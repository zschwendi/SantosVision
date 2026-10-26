import Foundation
import Metal
import simd

@main
struct OrderedSourceReuseMetalValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass repository and new result JSON")
        let root = URL(fileURLWithPath: CommandLine.arguments[1]), output = URL(fileURLWithPath: CommandLine.arguments[2])
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!, otherQueue = device.makeCommandQueue()!
        let library = try device.makeLibrary(source: String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8), options: nil)
        let helper = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        let reference = GameWorldOrderedCorners(device: device, library: library)!
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            if !value() { FileHandle.standardError.write(Data(("FAIL: " + message + "\n").utf8)); preconditionFailure(message) }
            checks += 1
        }
        func texture(_ width: Int = 16, _ height: Int = 12, value: Float = 0.25) -> any MTLTexture {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float, width: width, height: height, mipmapped: false)
            d.storageMode = .shared; d.usage = .shaderRead
            let t = device.makeTexture(descriptor: d)!
            let pixels = [Float](repeating: value, count: width * height)
            pixels.withUnsafeBytes { t.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
                withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
            return t
        }
        let projection = simd_float4x4(columns: (SIMD4(1,0,0,0), SIMD4(0,1,0,0),
            SIMD4(0,0,0.0001,-1), SIMD4(0,0,0.1,0)))
        func uniforms(_ depth: any MTLTexture) -> GameWorldUniforms {
            GameWorldUniforms(sourceEyeFromClip: projection.inverse, currentEyeFromSourceEye: matrix_identity_float4x4,
                currentClipFromEye: projection, sourceSize: SIMD2(UInt32(depth.width), UInt32(depth.height)))
        }
        func identity(_ id: UInt64, depth: any MTLTexture, epoch: UInt64 = 1, eye: UInt32 = 0) -> GameWorldOrderedSourceIdentity {
            let columns = [projection.columns.0, projection.columns.1, projection.columns.2, projection.columns.3]
            return .init(sourceID: id, generation: epoch, epoch: epoch, gameFrame: 2, requestFrame: 3, headFrame: 4,
                eye: eye, viewCount: 2, depthHandle: UInt64(UInt(bitPattern: Unmanaged.passUnretained(depth as AnyObject).toOpaque())),
                width: UInt32(depth.width), height: UInt32(depth.height), presentationBits: Double(5).bitPattern,
                projectionBits: columns.flatMap { [$0.x.bitPattern,$0.y.bitPattern,$0.z.bitPattern,$0.w.bitPattern] })
        }
        struct Prepared {
            let resources: GameWorldOrderedCorners.Resources
            let indices: [UInt32]
            let corners: [UInt32]
        }
        func read(_ h: GameWorldOrderedCorners, depth: any MTLTexture, id: GameWorldOrderedSourceIdentity?,
                  via q: any MTLCommandQueue, u: GameWorldUniforms? = nil) -> Prepared {
            let command = q.makeCommandBuffer()!
            let resources = h.prepare(commands: command, uniforms: u ?? uniforms(depth), depth: depth, source: id)!
            let indexCount = depth.width * depth.height * 6
            let indices = device.makeBuffer(length: indexCount * 4, options: .storageModeShared)!
            let rowBytes = ((depth.width + 1) * 4 + 255) & ~255
            let corners = device.makeBuffer(length: rowBytes * (depth.height + 1), options: .storageModeShared)!
            let blit = command.makeBlitCommandEncoder()!
            blit.copy(from: resources.indices, sourceOffset: 0, to: indices, destinationOffset: 0, size: indices.length)
            blit.copy(from: resources.corners, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(),
                sourceSize: MTLSize(width: depth.width + 1, height: depth.height + 1, depth: 1), to: corners,
                destinationOffset: 0, destinationBytesPerRow: rowBytes, destinationBytesPerImage: corners.length)
            blit.endEncoding(); command.commit(); command.waitUntilCompleted()
            require(command.status == .completed, "Actual preparation/cache read completed")
            let ip = indices.contents().bindMemory(to: UInt32.self, capacity: indexCount)
            let cp = corners.contents().bindMemory(to: UInt32.self, capacity: corners.length / 4)
            return Prepared(resources: resources, indices: (0..<indexCount).map { ip[$0] },
                corners: (0..<(depth.width + 1) * (depth.height + 1)).map { cp[$0 / (depth.width + 1) * (rowBytes / 4) + $0 % (depth.width + 1)] })
        }
        let a = texture(), keyA = identity(1, depth: a)
        let writer = autoreleasepool { read(helper, depth: a, id: keyA, via: queue) }
        require(helper.statistics.readyCached == 1 && helper.statistics.cacheHits == 0, "Only completed successful writer becomes cache-ready")
        autoreleasepool {
            let unqualified = queue.makeCommandBuffer()!
            require(helper.prepare(commands: unqualified, uniforms: uniforms(a), depth: a) == nil,
                "Missing fresh native proof retains the original fine draw")
            let wrongDepth = texture()
            require(helper.prepare(commands: unqualified, uniforms: uniforms(wrongDepth), depth: wrongDepth, source: keyA) == nil,
                "A live proof for another texture cannot invoke preparation")
        }
        require(helper.statistics.readyCached == 1 && helper.statistics.sets == 1 && helper.statistics.busy == 0,
            "Unqualified requests cannot allocate or evict another valid source cache")
        helper.validationRejectEncoderOrdinal = 1
        var moved = uniforms(a); moved.currentEyeFromSourceEye[3].x = 0.1; moved.currentClipFromEye[0].x *= 1.2
        let hit = autoreleasepool { read(helper, depth: a, id: keyA, via: queue, u: moved) }
        require(hit.resources.indices === writer.resources.indices && hit.indices == writer.indices && hit.corners == writer.corners,
            "Different display pose/projection reuses exact completed source data without invoking even the first encoder")
        require(helper.statistics.cacheHits == 1, "One exact cache hit observed")
        helper.validationRejectEncoderOrdinal = nil
        // Completed data may serve several independently leased GPU readers.
        let b = texture(value: 0.75), keyB = identity(2, depth: b)
        autoreleasepool {
            let gate = device.makeSharedEvent()!, one = queue.makeCommandBuffer()!, two = queue.makeCommandBuffer()!
            one.encodeWaitForEvent(gate, value: 1)
            let r1 = helper.prepare(commands: one, uniforms: uniforms(a), depth: a, source: keyA)!
            let r2 = helper.prepare(commands: two, uniforms: uniforms(a), depth: a, source: keyA)!
            require(r1.indices === r2.indices && r1.indices === writer.resources.indices,
                "Multiple in-flight readers share one immutable derived allocation")
            let count = a.width * a.height * 6
            let firstRead = device.makeBuffer(length: count * 4, options: .storageModeShared)!
            let secondRead = device.makeBuffer(length: count * 4, options: .storageModeShared)!
            for (command, resources, target) in [(one,r1,firstRead),(two,r2,secondRead)] {
                let blit = command.makeBlitCommandEncoder()!
                blit.copy(from: resources.indices, sourceOffset: 0, to: target, destinationOffset: 0, size: target.length)
                blit.endEncoding()
            }
            one.commit(); two.commit()
            let next = otherQueue.makeCommandBuffer()!
            let replacement = helper.prepare(commands: next, uniforms: uniforms(b), depth: b, source: keyB)!
            require(replacement.indices !== r1.indices && helper.statistics.readerLeases == 2,
                "Source change cannot overwrite an allocation with outstanding GPU readers")
            require(helper.statistics.busy == 2, "Old readers and new writer remain independently leased")
            next.commit(); next.waitUntilCompleted()
            require(next.status == .completed, "New source writes finish on another queue while old readers are blocked")
            gate.signaledValue = 1; one.waitUntilCompleted(); two.waitUntilCompleted()
            for target in [firstRead,secondRead] {
                let actual = target.contents().bindMemory(to: UInt32.self, capacity: count)
                require((0..<count).map { actual[$0] } == writer.indices,
                    "Both real GPU readers retain original indices despite another queue preparing the replacement source")
            }
            require(next.status == .completed && helper.statistics.busy == 0 && helper.statistics.readyCached == 1,
                "Old source retired and only successful current writer remains reusable")
        }
        let reusedB = autoreleasepool { read(helper, depth: b, id: keyB, via: queue) }
        let referenceB = autoreleasepool { read(reference, depth: b, id: nil, via: queue) }
        require(reusedB.indices == referenceB.indices && reusedB.corners == referenceB.corners,
            "Cache hit equals freshly generated row-major indices and corner depths bit-for-bit")
        let pending = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            let canonical = pending.prepare(commands: command, uniforms: uniforms(a), depth: a, source: keyA)!
            require(!canonical.pendingDuplicatePreparation, "First writer remains canonical")
            let transient = read(pending, depth: a, id: keyA, via: queue)
            let original = read(reference, depth: a, id: nil, via: queue)
            require(transient.resources.indices !== canonical.indices && transient.resources.corners !== canonical.corners,
                "Pending source uses independent fresh resources, never unfinished canonical data")
            require(transient.resources.pendingDuplicatePreparation && !transient.resources.reusedCompletedSource,
                "The actual pending-transient path is labelled explicitly")
            require(transient.indices == original.indices && transient.corners == original.corners,
                "A completed transient equals fresh ordered baseline bit-for-bit")
            require(pending.statistics.readyCached == 0 && pending.statistics.busy == 1,
                "Transient completing before uncommitted canonical writer cannot publish a ready cache")
            let newer = queue.makeCommandBuffer()!
            let recycled = pending.prepare(commands: newer, uniforms: uniforms(a), depth: a, source: keyA)!
            require(recycled.indices === transient.resources.indices && recycled.indices !== canonical.indices,
                "Only completed idle unkeyed transient resources may be recycled")
            require(pending.statistics.sets == 2 && pending.statistics.readyCached == 0
                && pending.statistics.pendingTransientAdmissions == 2, "No partial source admitted; transient admissions bounded")
        }
        require(pending.statistics.busy == 0 && pending.statistics.readyCached == 0,
            "Abandoning a writer never publishes cached data")
        // A real committed writer blocked on a GPU event exercises overlapping
        // compositor cadence, including preserving the other eye's ready cache.
        let overlap = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        let eyeB = identity(30, depth: b, eye: 1)
        let otherEye = read(overlap, depth: b, id: eyeB, via: queue)
        autoreleasepool {
            let gate = device.makeSharedEvent()!, canonical = queue.makeCommandBuffer()!
            canonical.encodeWaitForEvent(gate, value: 1)
            let first = overlap.prepare(commands: canonical, uniforms: uniforms(a), depth: a, source: keyA)!
            canonical.commit()
            require(canonical.status != .completed, "Canonical writer is actually committed and GPU-blocked")
            var commands: [any MTLCommandBuffer] = [], resources: [GameWorldOrderedCorners.Resources] = []
            let retainedEye = queue.makeCommandBuffer()!
            for ordinal in 0..<4 {
                let command = queue.makeCommandBuffer()!
                let fresh = overlap.prepare(commands: command, uniforms: uniforms(a), depth: a, source: keyA)!
                require(fresh.pendingDuplicatePreparation && fresh.indices !== first.indices
                    && fresh.indices !== otherEye.resources.indices && !resources.contains { $0.indices === fresh.indices },
                    "Every pending duplicate owns a distinct allocation, preserving canonical and other-eye data")
                command.commit(); commands.append(command); resources.append(fresh)
                if ordinal == 0 {
                    require(overlap.statistics.readyCached == 1,
                        "Idle other-eye cache remains preferred over eviction while spare capacity exists")
                    let reader = overlap.prepare(commands: retainedEye, uniforms: uniforms(b), depth: b, source: eyeB)!
                    require(reader.indices === otherEye.resources.indices, "Other-eye reader pins original ready data for pressure check")
                    let target = device.makeBuffer(length: b.width * b.height * 6 * 4, options: .storageModeShared)!
                    let blit = retainedEye.makeBlitCommandEncoder()!
                    blit.copy(from: reader.indices, sourceOffset: 0, to: target, destinationOffset: 0, size: target.length)
                    blit.endEncoding()
                }
            }
            let excess = queue.makeCommandBuffer()!
            require(overlap.prepare(commands: excess, uniforms: uniforms(a), depth: a, source: keyA) == nil,
                "Six sets remain the hard limit, with one busy canonical and four transients plus other-eye cache")
            require(overlap.statistics.readyCached == 1 && overlap.statistics.sets == 6,
                "Spare-budget transient preparation never evicts the other eye's ready cache")
            gate.signaledValue = 1; canonical.waitUntilCompleted()
            for command in commands { command.waitUntilCompleted(); require(command.status == .completed, "Queued transient GPU work completes") }
            retainedEye.commit(); retainedEye.waitUntilCompleted()
            require(overlap.statistics.readyCached == 2 && overlap.statistics.busy == 0,
                "Only canonical writers publish cache entries after all overlapping work completes")
            let current = read(overlap, depth: a, id: keyA, via: queue)
            require(current.resources.indices === first.indices && current.resources.reusedCompletedSource,
                "Completed canonical writer remains the sole cached source, never overwritten by transients")
            let preserved = read(overlap, depth: b, id: eyeB, via: queue)
            require(preserved.resources.indices === otherEye.resources.indices && preserved.resources.reusedCompletedSource,
                "Other-eye cache survives transient overlap while capacity was available")
        }
        for ordinal in [1,2] { for commit in [false,true] {
            let failures = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
            autoreleasepool {
                let canonical = queue.makeCommandBuffer()!
                let original = failures.prepare(commands: canonical, uniforms: uniforms(a), depth: a, source: keyA)!
                autoreleasepool {
                    failures.validationRejectEncoderOrdinal = ordinal
                    let partial = queue.makeCommandBuffer()!
                    require(failures.prepare(commands: partial, uniforms: uniforms(a), depth: a, source: keyA) == nil,
                        "Transient encoder refusal returns complete fine fallback")
                    require(failures.statistics.busy == (ordinal == 1 ? 1 : 2),
                        "Only already-encoded transient work retains a lease until completion/disposal")
                    if commit { partial.commit(); partial.waitUntilCompleted(); require(partial.status == .completed, "Partial transient command completes") }
                }
                require(failures.statistics.busy == 1 && failures.statistics.readyCached == 0,
                    "Transient failure or abandonment cannot release or publish the canonical writer")
                failures.validationRejectEncoderOrdinal = nil
                canonical.commit(); canonical.waitUntilCompleted()
                let cached = read(failures, depth: a, id: keyA, via: queue)
                require(cached.resources.indices === original.indices && cached.resources.reusedCompletedSource,
                    "Canonical source survives each transient encoder failure/disposal")
            }
        } }
        pending.validationRejectEncoderOrdinal = 2
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(pending.prepare(commands: command, uniforms: uniforms(a), depth: a, source: keyA) == nil, "Partial writer declines")
            command.commit(); command.waitUntilCompleted()
            require(command.status == .completed && pending.statistics.readyCached == 0,
                "Successful command status cannot certify a preparation that failed its second encoder")
        }
        pending.validationRejectEncoderOrdinal = nil
        _ = autoreleasepool { read(pending, depth: a, id: keyA, via: queue) }
        autoreleasepool {
            let reader = queue.makeCommandBuffer()!
            require(pending.prepare(commands: reader, uniforms: uniforms(a), depth: a, source: keyA) != nil, "Ready reader acquired")
        }
        require(pending.statistics.readyCached == 0 && pending.statistics.busy == 0,
            "An abandoned reader retires the cache without a leaked lease")
        // Simulate native slot reuse after its previous ordinary source lease
        // ended: identical texture address and metadata, distinct publication ID.
        let oldPixels = reusedB.corners, previousHits = helper.statistics.cacheHits
        let pixels = [Float](repeating: 0.5, count: b.width * b.height)
        pixels.withUnsafeBytes { b.replace(region: MTLRegionMake2D(0, 0, b.width, b.height), mipmapLevel: 0,
            withBytes: $0.baseAddress!, bytesPerRow: b.width * 4) }
        let newPublication = autoreleasepool { read(helper, depth: b, id: identity(3, depth: b), via: queue) }
        require(helper.statistics.cacheHits == previousHits && newPublication.corners != oldPixels,
            "New publication ID misses despite reused raw texture pointer and identical frame metadata")
        let newReference = autoreleasepool { read(reference, depth: b, id: nil, via: queue) }
        require(newPublication.indices == newReference.indices && newPublication.corners == newReference.corners,
            "Reused source storage produces newly derived exact contents")
        let beforeQueue = helper.statistics.cacheHits
        _ = autoreleasepool { read(helper, depth: b, id: identity(3, depth: b), via: otherQueue) }
        require(helper.statistics.cacheHits == beforeQueue, "Different command queue cannot use a prior queue's entry")
        let epochHelper = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        _ = autoreleasepool { read(epochHelper, depth: a, id: identity(10, depth: a), via: queue) }
        _ = autoreleasepool { read(epochHelper, depth: b, id: identity(11, depth: b, eye: 1), via: queue) }
        require(epochHelper.statistics.readyCached == 2, "Two distinct eye sources remain separate")
        _ = autoreleasepool { read(epochHelper, depth: a, id: identity(12, depth: a, epoch: 2), via: queue) }
        require(epochHelper.statistics.readyCached == 1, "New native epoch retires both previous eye entries")
        let readerLimit = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        _ = autoreleasepool { read(readerLimit, depth: a, id: keyA, via: queue) }
        autoreleasepool {
            var commands: [any MTLCommandBuffer] = []
            for _ in 0..<12 {
                let command = queue.makeCommandBuffer()!
                require(readerLimit.prepare(commands: command, uniforms: uniforms(a), depth: a, source: keyA) != nil,
                    "Bounded independent reader admitted")
                commands.append(command)
            }
            let excess = queue.makeCommandBuffer()!
            require(readerLimit.prepare(commands: excess, uniforms: uniforms(a), depth: a, source: keyA) == nil,
                "Thirteenth outstanding reader declines without growing tracking state")
            readerLimit.retireSourceCache()
            require(readerLimit.statistics.busy == 1 && readerLimit.statistics.readerLeases == 12,
                "Retirement never removes data still held by GPU reader commands")
            for command in commands { command.commit(); command.waitUntilCompleted() }
        }
        require(readerLimit.statistics.sets == 0 && readerLimit.statistics.bytes == 0,
            "Terminal retirement frees all derived resources after the last reader")
        let memory = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        let tallA = texture(2560,2304), tallB = texture(2560,2304,value:0.5)
        _ = autoreleasepool { read(memory, depth: tallA, id: identity(20, depth: tallA), via: queue) }
        _ = autoreleasepool { read(memory, depth: tallB, id: identity(21, depth: tallB, eye: 1), via: queue) }
        let peakBytes = memory.statistics.bytes
        require(memory.statistics.readyCached == 2 && peakBytes <= 384 << 20,
            "Idle reusable derived data is included in the unchanged total byte budget")
        memory.retireSourceCache()
        require(memory.statistics.sets == 0 && memory.statistics.bytes == 0, "Idle cached storage retires immediately")
        // Two tall sets fill the byte budget well before the six-set cap.
        // A transient must decline rather than recycle the other eye's cache;
        // a new canonical source still retains the original pressure LRU.
        let pressure = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        autoreleasepool {
            let other = queue.makeCommandBuffer()!, otherID = identity(61, depth: tallB, eye: 1)
            let saved = pressure.prepare(commands: other, uniforms: uniforms(tallB), depth: tallB, source: otherID)!
            other.commit(); other.waitUntilCompleted()
            let canonical = queue.makeCommandBuffer()!, canonicalID = identity(60, depth: tallA)
            let original = pressure.prepare(commands: canonical, uniforms: uniforms(tallA), depth: tallA, source: canonicalID)!
            let before = pressure.statistics, duplicate = queue.makeCommandBuffer()!
            require(pressure.prepare(commands: duplicate, uniforms: uniforms(tallA), depth: tallA, source: canonicalID) == nil,
                "Pending transient declines byte pressure instead of evicting idle keyed other-eye data")
            require(pressure.statistics.readyCached == 1 && pressure.statistics.bytes == before.bytes
                && pressure.statistics.pendingTransientAdmissions == 0, "Declined transient preserves all keyed cache resources")
            let reader = queue.makeCommandBuffer()!
            let preserved = pressure.prepare(commands: reader, uniforms: uniforms(tallB), depth: tallB, source: otherID)!
            require(preserved.reusedCompletedSource && preserved.indices === saved.indices,
                "Other-eye cache remains a real hit after pressure refusal")
            reader.commit(); reader.waitUntilCompleted()
            let newSource = queue.makeCommandBuffer()!
            let replacement = pressure.prepare(commands: newSource, uniforms: uniforms(tallA), depth: tallA,
                source: identity(62, depth: tallA))!
            require(!replacement.pendingDuplicatePreparation && replacement.indices === saved.indices && replacement.indices !== original.indices,
                "Normal new canonical source retains existing idle keyed pressure LRU")
            newSource.commit(); canonical.commit(); newSource.waitUntilCompleted(); canonical.waitUntilCompleted()
        }
        pressure.retireSourceCache()
        // Find a real driver-padding boundary: two actual allocations plus a
        // third nominal allocation fit, but three actual allocations do not.
        // No production budget/refusal hook is used.
        var paddingFixture: (height: Int, nominal: Int, actual: Int)?
        for h in stride(from: 1872, through: 1840, by: -1) {
            let candidate: (Int,Int)? = autoreleasepool {
                let layout = GameWorldOrderedCornersBudget.layout(width: 2560, height: h, maxBufferLength: device.maxBufferLength)!
                let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float, width: 2561, height: h + 1, mipmapped: false)
                d.storageMode = .private; d.usage = [.shaderRead,.shaderWrite]
                guard let t = device.makeTexture(descriptor: d), let b = device.makeBuffer(length: layout.indexBytes, options: .storageModePrivate) else { return nil }
                let actual = max(layout.cornerBytes,t.allocatedSize) + max(layout.indexBytes,b.allocatedSize)
                return (layout.bytes,actual)
            }
            if let (nominal,actual) = candidate, 2 * actual + nominal <= 384 << 20 && 3 * actual > 384 << 20 {
                paddingFixture = (h,nominal,actual); break
            }
        }
        require(paddingFixture != nil,"Actual M4 driver allocation padding boundary found within bounded 33 probes")
        let padding = paddingFixture!, padded = GameWorldOrderedCorners(device: device, library: library, reuseSources: true)!
        autoreleasepool {
            let d = texture(2560,padding.height), one = queue.makeCommandBuffer()!, otherID = identity(71,depth:d,eye:1)
            let saved = padded.prepare(commands: one, uniforms: uniforms(d), depth: d, source: otherID)!
            one.commit(); one.waitUntilCompleted()
            let canonical = queue.makeCommandBuffer()!, id = identity(70,depth:d)
            _ = padded.prepare(commands: canonical, uniforms: uniforms(d), depth: d, source: id)!
            let before = padded.statistics
            require(before.bytes + padding.nominal <= 384 << 20 && before.bytes + padding.actual > 384 << 20,
                "Nominal admission passes while the real padded allocation must be refused")
            let transient = queue.makeCommandBuffer()!
            require(padded.prepare(commands: transient, uniforms: uniforms(d), depth: d, source: id) == nil,
                "Actual-allocation-size refusal returns fine fallback")
            require(padded.statistics.bytes == before.bytes && padded.statistics.sets == 2 && padded.statistics.readyCached == 1,
                "Actual padding refusal preserves keyed resources and the canonical writer")
            let reader = queue.makeCommandBuffer()!
            let preserved = padded.prepare(commands: reader, uniforms: uniforms(d), depth: d, source: otherID)!
            require(preserved.reusedCompletedSource && preserved.indices === saved.indices,
                "Other-eye cache remains a hit after actual padded-byte refusal")
            reader.commit(); canonical.commit(); reader.waitUntilCompleted(); canonical.waitUntilCompleted()
        }
        padded.retireSourceCache()
        let report: [String:Any] = ["schema":1,"device":device.name,"checks":checks,"pass":true,
            "actual_two_tall_cached_bytes":peakBytes,"completed_success_only":true,
            "multi_inflight_reader_no_overwrite":true,"source_pointer_reuse_simulation":true,
            "pending_transients_never_evict_keyed_cache":true,"normal_canonical_pressure_lru_retained":true,
            "actual_padding_refusal_preserves_keyed_cache":true,"padding_fixture":["height":padding.height,"nominal":padding.nominal,"actual":padding.actual],
            "native_publication_liveness_proof":"Separate actual Rust world_frame source-ID slot-reuse test",
            "bounds":["sets":6,"bytes":384 << 20,"outstanding_checkouts":12],
            "physical_performance_or_quality_acceptance":false]
        try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:output,options:.withoutOverwriting)
        print("PASS actual Metal ordered-source reuse: \(checks) identity, multi-inflight, failure, retirement, content and memory checks")
    }
}

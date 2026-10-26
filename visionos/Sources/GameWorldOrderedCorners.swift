import Foundation

/// A freshly checked native publication identity. This value owns no texture
/// or native lease; the current caller still owns its ordinary source lease.
struct GameWorldOrderedSourceIdentity: Hashable, Sendable {
    let sourceID: UInt64
    let generation: UInt64
    let epoch: UInt64
    let gameFrame: UInt64
    let requestFrame: UInt64
    let headFrame: UInt64
    let eye: UInt32
    let viewCount: UInt32
    let depthHandle: UInt64
    let width: UInt32
    let height: UInt32
    let presentationBits: UInt64
    let projectionBits: [UInt32]

    var qualified: Bool {
        sourceID != 0 && epoch != 0 && depthHandle != 0 && width > 0 && height > 0
            && (1...2).contains(viewCount) && eye < viewCount && projectionBits.count == 16
            && projectionBits.allSatisfy { Float(bitPattern: $0).isFinite }
            && Double(bitPattern: presentationBits).isFinite
    }
}

/// Exact source-only inputs: display pose, display projection and color transfer
/// are deliberately absent because neither preparation kernel reads them.
struct GameWorldOrderedSourceKey: Hashable, Sendable {
    let identity: GameWorldOrderedSourceIdentity
    let device: UInt64
    let queue: ObjectIdentifier
    let depthFormat: UInt64
    let inverseProjectionBits: [UInt32]
    let depthReversed: UInt32
    let qualifierBits: [UInt32]
}

/// Pure admission arithmetic shared by the actual pool and CPU overflow tests.
enum GameWorldOrderedCornersBudget {
    static let maximumSets = 6
    static let maximumBytes = 384 << 20
    static let maximumCheckouts = 12

    struct Layout: Equatable {
        let width: Int
        let height: Int
        let cornerWidth: Int
        let cornerHeight: Int
        let indexCount: Int
        let indexBytes: Int
        let cornerBytes: Int
        let bytes: Int
    }

    static func layout(width: Int, height: Int, maxBufferLength: Int) -> Layout? {
        // The additional corner row/column must fit a 16384-wide 2D texture.
        guard width > 0, height > 0, width < 16_384, height < 16_384,
              maxBufferLength > 0 else { return nil }
        func product(_ lhs: Int, _ rhs: Int) -> Int? {
            let result = lhs.multipliedReportingOverflow(by: rhs)
            return result.overflow ? nil : result.partialValue
        }
        guard let pixels = product(width, height),
              let count = product(pixels, 6), let indexBytes = product(count, 4),
              let corners = product(width + 1, height + 1), let cornerBytes = product(corners, 4),
              let fallbackVertices = product(pixels, 4) else { return nil }
        let vertexCount = corners.addingReportingOverflow(fallbackVertices)
        let total = indexBytes.addingReportingOverflow(cornerBytes)
        guard !vertexCount.overflow, vertexCount.partialValue <= Int(UInt32.max),
              !total.overflow, total.partialValue <= maximumBytes,
              indexBytes <= maxBufferLength else { return nil }
        return Layout(width: width, height: height, cornerWidth: width + 1, cornerHeight: height + 1,
            indexCount: count, indexBytes: indexBytes, cornerBytes: cornerBytes, bytes: total.partialValue)
    }

    static func canAdd(bytes: Int, heldBytes: Int, heldSets: Int) -> Bool {
        guard bytes > 0, heldBytes >= 0, heldSets >= 0, heldSets < maximumSets else { return false }
        let total = heldBytes.addingReportingOverflow(bytes)
        return !total.overflow && total.partialValue <= maximumBytes
    }
}

#if SV_GAME_RUNTIME
import Metal
import simd

/// Resources returned by prepare belong to that command. The optional cache
/// may share completed immutable derived data with other commands via separate
/// read leases; callers still must not transfer Resources between commands.
final class GameWorldOrderedCorners: @unchecked Sendable {
    struct Resources: @unchecked Sendable {
        let corners: any MTLTexture
        let indices: any MTLBuffer
        var reusedCompletedSource = false
    }

    struct Statistics: Sendable {
        let sets: Int
        let busy: Int
        let bytes: Int
        let releasedLeases: UInt64
        let cacheHits: UInt64
        let cacheMisses: UInt64
        let readyCached: Int
        let readerLeases: Int
    }

    private final class Pool: @unchecked Sendable {
        private struct Entry {
            let resources: Resources
            let width: Int
            let height: Int
            let bytes: Int
            var checkouts: Set<UInt64>
            var writer: UInt64?
            var key: GameWorldOrderedSourceKey?
            var ready: Bool
            // Retain the queue object while its ObjectIdentifier is in a key.
            // This is not a native source/slot lease or a source texture pin.
            var queue: (any MTLCommandQueue)?
            var lastUse: UInt64
            var discardWhenIdle: Bool
        }
        private let lock = NSLock()
        private var entries: [Entry] = []
        private var nextCheckout: UInt64 = 0
        private var released: UInt64 = 0
        private var hits: UInt64 = 0
        private var misses: UInt64 = 0
        private var acceptsCache = true

        func acquire(device: any MTLDevice, layout: GameWorldOrderedCornersBudget.Layout,
                     key requestedKey: GameWorldOrderedSourceKey?, queue: any MTLCommandQueue)
            -> (Resources, CompletionLease, Bool)? {
            lock.lock()
            defer { lock.unlock() }
            let key = acceptsCache ? requestedKey : nil
            guard nextCheckout < .max else { return nil }
            guard entries.reduce(0, { $0 + $1.checkouts.count }) < GameWorldOrderedCornersBudget.maximumCheckouts else { return nil }
            nextCheckout += 1
            let checkout = nextCheckout
            func claim(_ index: Int) -> (Resources, CompletionLease, Bool) {
                assert(entries[index].checkouts.isEmpty)
                entries[index].checkouts.insert(checkout); entries[index].writer = checkout
                entries[index].key = key; entries[index].ready = false
                entries[index].queue = key == nil ? nil : queue; entries[index].lastUse = checkout
                entries[index].discardWhenIdle = !acceptsCache
                return (entries[index].resources,
                    CompletionLease(pool: self, checkout: checkout, prepared: false), false)
            }
            if let key {
                // Retire prior source/epoch/qualifier data for this eye. Busy
                // GPU readers keep their resources until their own completion.
                for index in entries.indices {
                    if let old = entries[index].key,
                       old.identity.epoch != key.identity.epoch
                        || (old.identity.eye == key.identity.eye && old != key) {
                        entries[index].key = nil; entries[index].ready = false; entries[index].queue = nil
                    }
                }
                if let index = entries.firstIndex(where: { $0.ready && $0.key == key && $0.writer == nil }) {
                    entries[index].checkouts.insert(checkout); entries[index].lastUse = checkout
                    if hits < .max { hits += 1 }
                    return (entries[index].resources,
                        CompletionLease(pool: self, checkout: checkout, prepared: true), true)
                }
                if misses < .max { misses += 1 }
                // A queued/incomplete writer is never a cache hit. Avoid a
                // duplicate preparation; the caller retains its fine draw.
                if entries.contains(where: { $0.key == key && $0.writer != nil }) { return nil }
            }
            if let index = entries.indices.filter({ entries[$0].checkouts.isEmpty
                && entries[$0].width == layout.width && entries[$0].height == layout.height
                && (key == nil || entries[$0].key == nil) })
                .min(by: { entries[$0].lastUse < entries[$1].lastUse }) {
                return claim(index)
            }
            // Idle sets of another extent are disposable. Busy resources cannot
            // be evicted to make room, even when their source size has changed.
            entries.removeAll { $0.checkouts.isEmpty && (key == nil || $0.key == nil) }
            var heldBytes = entries.reduce(0) { $0 + $1.bytes }
            if !GameWorldOrderedCornersBudget.canAdd(bytes: layout.bytes, heldBytes: heldBytes, heldSets: entries.count) {
                // Preserve the other eye's ready cache while spare capacity
                // exists. Under pressure, evict only an idle allocation.
                if let index = entries.indices.filter({ entries[$0].checkouts.isEmpty
                    && entries[$0].width == layout.width && entries[$0].height == layout.height })
                    .min(by: { entries[$0].lastUse < entries[$1].lastUse }) { return claim(index) }
                entries.removeAll { $0.checkouts.isEmpty }
                heldBytes = entries.reduce(0) { $0 + $1.bytes }
            }
            guard GameWorldOrderedCornersBudget.canAdd(bytes: layout.bytes,
                heldBytes: heldBytes, heldSets: entries.count) else { return nil }
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float,
                width: layout.cornerWidth, height: layout.cornerHeight, mipmapped: false)
            descriptor.storageMode = .private
            descriptor.usage = [.shaderRead, .shaderWrite]
            guard let corners = device.makeTexture(descriptor: descriptor),
                  let indices = device.makeBuffer(length: layout.indexBytes, options: .storageModePrivate) else { return nil }
            // Driver allocation sizes may include row/page padding. Keep the
            // budget at least nominal and recheck the actual returned resources.
            let textureBytes = max(layout.cornerBytes, corners.allocatedSize)
            let bufferBytes = max(layout.indexBytes, indices.allocatedSize)
            let actual = textureBytes.addingReportingOverflow(bufferBytes)
            guard !actual.overflow, GameWorldOrderedCornersBudget.canAdd(bytes: actual.partialValue,
                heldBytes: heldBytes, heldSets: entries.count) else { return nil }
            corners.label = "Ordered game world qualified corner depths"
            indices.label = "Ordered game world source-pixel triangle indices"
            let resources = Resources(corners: corners, indices: indices)
            entries.append(Entry(resources: resources, width: layout.width, height: layout.height,
                bytes: actual.partialValue, checkouts: [checkout], writer: checkout,
                key: key, ready: false, queue: key == nil ? nil : queue, lastUse: checkout, discardWhenIdle: !acceptsCache))
            return (resources, CompletionLease(pool: self, checkout: checkout, prepared: false), false)
        }

        func release(checkout: UInt64, success: Bool) {
            lock.lock()
            defer { lock.unlock() }
            guard let index = entries.firstIndex(where: { $0.checkouts.contains(checkout) }) else { return }
            entries[index].checkouts.remove(checkout)
            if entries[index].writer == checkout {
                entries[index].writer = nil
                entries[index].ready = success && entries[index].key != nil
            }
            if !success {
                entries[index].key = nil; entries[index].ready = false; entries[index].queue = nil
            }
            if released < .max { released += 1 }
            if entries[index].discardWhenIdle && entries[index].checkouts.isEmpty { entries.remove(at: index) }
        }

        func retire() {
            lock.lock()
            defer { lock.unlock() }
            acceptsCache = false
            entries.removeAll { $0.checkouts.isEmpty }
            for index in entries.indices {
                entries[index].key = nil; entries[index].ready = false; entries[index].queue = nil
                entries[index].discardWhenIdle = true
            }
        }

        var statistics: Statistics {
            lock.lock()
            defer { lock.unlock() }
            return Statistics(sets: entries.count, busy: entries.filter { !$0.checkouts.isEmpty }.count,
                bytes: entries.reduce(0) { $0 + $1.bytes }, releasedLeases: released,
                cacheHits: hits, cacheMisses: misses, readyCached: entries.filter { $0.ready && $0.key != nil }.count,
                readerLeases: entries.reduce(0) { $0 + $1.checkouts.count - ($1.writer == nil ? 0 : 1) })
        }
    }

    /// The command's completion block is the owner after the first encoder is
    /// created. A discarded uncommitted command destroys that block and its
    /// lease, returning the set without a pool-to-command retain cycle.
    private final class CompletionLease: @unchecked Sendable {
        private let lock = NSLock()
        private var pool: Pool?
        private let checkout: UInt64
        private var prepared: Bool
        init(pool: Pool, checkout: UInt64, prepared: Bool) {
            self.pool = pool; self.checkout = checkout; self.prepared = prepared
        }
        func markPrepared() { lock.lock(); prepared = true; lock.unlock() }
        func release(success: Bool) {
            lock.lock()
            let owned = pool
            let valid = success && prepared
            pool = nil
            lock.unlock()
            owned?.release(checkout: checkout, success: valid)
        }
        deinit { release(success: false) }
    }

    private let device: any MTLDevice
    private let cornerKernel: any MTLComputePipelineState
    private let indexKernel: any MTLComputePipelineState
    private let pool = Pool()
    let reuseSources: Bool

    #if SV_ORDERED_CORNERS_VALIDATION
    // Test-only refusal at the real encoder boundary, absent from production.
    var validationRejectEncoderOrdinal: Int?
    #endif

    init?(device: any MTLDevice, library: any MTLLibrary, reuseSources: Bool = false) {
        guard library.device.registryID == device.registryID,
              let corners = library.makeFunction(name: "gameWorldCornerDepths"),
              let indices = library.makeFunction(name: "orderedCornerIndices"),
              let cornerKernel = try? device.makeComputePipelineState(function: corners),
              let indexKernel = try? device.makeComputePipelineState(function: indices) else { return nil }
        self.device = device
        self.cornerKernel = cornerKernel
        self.indexKernel = indexKernel
        self.reuseSources = reuseSources
    }

    var statistics: Statistics { pool.statistics }
    func retireSourceCache() { pool.retire() }

    func prepare(commands: any MTLCommandBuffer, uniforms: GameWorldUniforms,
                 depth: any MTLTexture, source: GameWorldOrderedSourceIdentity? = nil) -> Resources? {
        guard commands.commandQueue.device.registryID == device.registryID,
              depth.device.registryID == device.registryID,
              commands.status == .notEnqueued,
              depth.textureType == .type2D, depth.pixelFormat == .r32Float,
              depth.sampleCount == 1, depth.arrayLength == 1, depth.depth == 1,
              depth.usage.contains(.shaderRead), uniforms.sourceDepthReversed <= 1,
              depth.width == Int(uniforms.sourceSize.x), depth.height == Int(uniforms.sourceSize.y),
              let layout = GameWorldOrderedCornersBudget.layout(width: depth.width, height: depth.height,
                  maxBufferLength: device.maxBufferLength) else { return nil }
        var key: GameWorldOrderedSourceKey?
        if reuseSources, let source, source.qualified,
           source.depthHandle == UInt64(UInt(bitPattern: Unmanaged.passUnretained(depth as AnyObject).toOpaque())),
           source.width == uniforms.sourceSize.x, source.height == uniforms.sourceSize.y {
            let columns = [uniforms.sourceEyeFromClip.columns.0, uniforms.sourceEyeFromClip.columns.1,
                uniforms.sourceEyeFromClip.columns.2, uniforms.sourceEyeFromClip.columns.3]
            let bits = columns.flatMap { [$0.x.bitPattern, $0.y.bitPattern, $0.z.bitPattern, $0.w.bitPattern] }
            if bits.allSatisfy({ Float(bitPattern: $0).isFinite }) {
                key = GameWorldOrderedSourceKey(identity: source, device: device.registryID,
                    queue: ObjectIdentifier(commands.commandQueue), depthFormat: UInt64(depth.pixelFormat.rawValue),
                    inverseProjectionBits: bits, depthReversed: uniforms.sourceDepthReversed,
                    qualifierBits: [uniforms.reserved.x.bitPattern, uniforms.reserved.y.bitPattern])
            }
        }
        // Opt-in reuse requires the caller's freshly checked live native
        // publication proof. If unavailable, preserve the complete fine draw
        // without allocating or evicting another eye's derived cache.
        if reuseSources && key == nil { return nil }
        guard let (resources, lease, hit) = pool.acquire(device: device, layout: layout,
            key: key, queue: commands.commandQueue) else { return nil }
        if hit {
            commands.addCompletedHandler { [lease] command in lease.release(success: command.status == .completed) }
            var result = resources
            result.reusedCompletedSource = true
            return result
        }
        func encoder(_ ordinal: Int) -> (any MTLComputeCommandEncoder)? {
            #if SV_ORDERED_CORNERS_VALIDATION
            if validationRejectEncoderOrdinal == ordinal { return nil }
            #endif
            return commands.makeComputeCommandEncoder()
        }
        guard let corners = encoder(1) else { return nil }
        // From here even a later encoder failure can leave work referencing the
        // set in this command. Return it only on completion or command disposal.
        commands.addCompletedHandler { [lease] command in lease.release(success: command.status == .completed) }
        var values = uniforms
        corners.label = "Ordered corners: shared depth qualification"
        corners.setComputePipelineState(cornerKernel)
        corners.setBytes(&values, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        corners.setTexture(depth, index: 0)
        corners.setTexture(resources.corners, index: 1)
        dispatch(corners, kernel: cornerKernel, width: layout.cornerWidth, height: layout.cornerHeight)
        corners.endEncoding()
        guard let indices = encoder(2) else { return nil }
        indices.label = "Ordered corners: original source-pixel triangle sequence"
        indices.setComputePipelineState(indexKernel)
        indices.setBytes(&values, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        indices.setBuffer(resources.indices, offset: 0, index: 1)
        indices.setTexture(resources.corners, index: 0)
        dispatch(indices, kernel: indexKernel, width: layout.width, height: layout.height)
        indices.endEncoding()
        lease.markPrepared()
        return resources
    }

    private func dispatch(_ encoder: any MTLComputeCommandEncoder, kernel: any MTLComputePipelineState,
                          width: Int, height: Int) {
        let x = min(16, kernel.threadExecutionWidth)
        let y = max(1, min(16, kernel.maxTotalThreadsPerThreadgroup / x))
        encoder.dispatchThreads(MTLSize(width: width, height: height, depth: 1),
            threadsPerThreadgroup: MTLSize(width: x, height: y, depth: 1))
    }
}
#endif

import Foundation
import Metal
import simd

@main
struct OrderedCornersMetalPoolValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass repository and new result JSON")
        let root = URL(fileURLWithPath: CommandLine.arguments[1])
        let output = URL(fileURLWithPath: CommandLine.arguments[2])
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let source = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
        let library = try device.makeLibrary(source: source, options: nil)
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            if !value() {
                FileHandle.standardError.write(Data(("FAIL: " + message + "\n").utf8))
                preconditionFailure(message)
            }
            checks += 1
        }
        func helper() -> GameWorldOrderedCorners { GameWorldOrderedCorners(device: device, library: library)! }
        func depth(_ width: Int, _ height: Int, format: MTLPixelFormat = .r32Float,
                   usage: MTLTextureUsage = .shaderRead) -> any MTLTexture {
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format,
                width: width, height: height, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = usage
            let texture = device.makeTexture(descriptor: descriptor)!
            if format == .r32Float {
                let pixels = [Float](repeating: 0.5, count: width * height)
                pixels.withUnsafeBytes { texture.replace(region: MTLRegionMake2D(0, 0, width, height),
                    mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
            }
            return texture
        }
        func uniforms(_ width: Int, _ height: Int) -> GameWorldUniforms {
            let projection = simd_float4x4(columns: (SIMD4(1, 0, 0, 0), SIMD4(0, 1, 0, 0),
                SIMD4(0, 0, 0.0001, -1), SIMD4(0, 0, 0.1, 0)))
            return GameWorldUniforms(sourceEyeFromClip: projection.inverse,
                currentEyeFromSourceEye: matrix_identity_float4x4,
                currentClipFromEye: projection,
                sourceSize: SIMD2(UInt32(width), UInt32(height)))
        }
        let small = depth(3, 3), u = uniforms(3, 3)
        // Verify actual production bindings and the complete immutable index
        // sequence by copying the private GPU buffer after real execution.
        let actual = helper()
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            let resources = actual.prepare(commands: command, uniforms: u, depth: small)!
            let readback = device.makeBuffer(length: 3 * 3 * 6 * 4, options: .storageModeShared)!
            let blit = command.makeBlitCommandEncoder()!
            blit.copy(from: resources.indices, sourceOffset: 0, to: readback, destinationOffset: 0, size: readback.length)
            blit.endEncoding(); command.commit(); command.waitUntilCompleted()
            require(command.status == .completed, "Actual ordered preparation command completed")
            let indices = readback.contents().bindMemory(to: UInt32.self, capacity: 54)
            for y in 0..<3 { for x in 0..<3 {
                let base = UInt32(y * 4 + x)
                let expected = [base, base + 4, base + 1, base + 1, base + 4, base + 5]
                require((0..<6).map { indices[(y * 3 + x) * 6 + $0] } == expected,
                    "GPU returned exact row-major triangle indices for qualified shared corners")
            } }
            require(actual.statistics.busy == 0 && actual.statistics.releasedLeases == 1,
                "Completion returns its checkout exactly once")
        }
        // One blocked command proves non-reuse while the GPU still owns it.
        // Five further uncommitted commands fill the explicit six-set bound.
        let flight = helper()
        autoreleasepool {
            let gate = device.makeSharedEvent()!, blocked = queue.makeCommandBuffer()!
            blocked.encodeWaitForEvent(gate, value: 1)
            let first = flight.prepare(commands: blocked, uniforms: u, depth: small)!
            blocked.commit()
            require(blocked.status != .completed, "Shared event retains actual in-flight ownership")
            var others: [any MTLCommandBuffer] = []
            var buffers = [first.indices]
            for _ in 0..<5 {
                let command = queue.makeCommandBuffer()!
                let resources = flight.prepare(commands: command, uniforms: u, depth: small)!
                require(!buffers.contains { $0 === resources.indices }, "No busy index allocation is reused")
                buffers.append(resources.indices); others.append(command)
            }
            let exhausted = queue.makeCommandBuffer()!
            require(flight.prepare(commands: exhausted, uniforms: u, depth: small) == nil,
                "Seventh busy set declines cleanly")
            require(flight.statistics.sets == 6 && flight.statistics.busy == 6
                && flight.statistics.bytes <= GameWorldOrderedCornersBudget.maximumBytes, "Set and byte bounds retained")
            gate.signaledValue = 1
            blocked.waitUntilCompleted()
            require(blocked.status == .completed && flight.statistics.busy == 5, "Only completed set is returned")
            let reuse = queue.makeCommandBuffer()!
            let recycled = flight.prepare(commands: reuse, uniforms: u, depth: small)!
            require(recycled.indices === first.indices, "Completed matching extent can be reused")
            reuse.commit(); reuse.waitUntilCompleted()
            for command in others { command.commit(); command.waitUntilCompleted() }
            require(flight.statistics.busy == 0 && flight.statistics.releasedLeases == 7,
                "All original and recycled checkouts return once")
        }
        let failure = helper()
        failure.validationRejectEncoderOrdinal = 1
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(failure.prepare(commands: command, uniforms: u, depth: small) == nil,
                "First real encoder boundary can decline")
            require(failure.statistics.busy == 0, "Failure before work returns its unowned checkout immediately")
        }
        failure.validationRejectEncoderOrdinal = 2
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(failure.prepare(commands: command, uniforms: u, depth: small) == nil,
                "Second encoder refusal cannot return partially initialized resources")
            require(failure.statistics.busy == 1, "Already encoded corner work retains exclusive checkout")
            command.commit(); command.waitUntilCompleted()
            require(command.status == .completed && failure.statistics.busy == 0,
                "Partial work releases only when its real command completes")
        }
        let abandoned = helper()
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(abandoned.prepare(commands: command, uniforms: u, depth: small) != nil,
                "Create a real uncommitted preparation")
            require(abandoned.statistics.busy == 1, "Uncommitted work holds its checkout")
        }
        require(abandoned.statistics.busy == 0 && abandoned.statistics.releasedLeases == 1,
            "Discarding an uncommitted Metal command destroys its once-only completion lease")
        abandoned.validationRejectEncoderOrdinal = 2
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(abandoned.prepare(commands: command, uniforms: u, depth: small) == nil,
                "Create partial uncommitted corner work")
            require(abandoned.statistics.busy == 1, "Partial uncommitted work remains leased")
        }
        require(abandoned.statistics.busy == 0 && abandoned.statistics.releasedLeases == 2,
            "Discarding partial work also returns its checkout once")
        let invalid = helper()
        autoreleasepool {
            let command = queue.makeCommandBuffer()!
            require(invalid.prepare(commands: command, uniforms: uniforms(4, 3), depth: small) == nil,
                "Source extent mismatch declines without allocation")
            require(invalid.prepare(commands: command, uniforms: u, depth: depth(3, 3, format: .rgba8Unorm)) == nil,
                "Incorrect depth format declines")
            require(invalid.prepare(commands: command, uniforms: u, depth: depth(3, 3, usage: .shaderWrite)) == nil,
                "Non-readable depth declines")
            var bad = u; bad.sourceDepthReversed = 2
            require(invalid.prepare(commands: command, uniforms: bad, depth: small) == nil, "Invalid depth convention declines")
            command.commit(); command.waitUntilCompleted()
            require(invalid.prepare(commands: command, uniforms: u, depth: small) == nil,
                "Completed command cannot receive new preparation")
            require(invalid.statistics.sets == 0 && invalid.statistics.releasedLeases == 0,
                "Rejected input never changes pool ownership")
        }
        // Exercise byte admission against actual returned Metal allocation
        // sizes, rather than just the six-set limit or a copied budget model.
        let bytes = helper(), tall = depth(2560, 2304), tallU = uniforms(2560, 2304)
        var peakBytes = 0
        autoreleasepool {
            let a = queue.makeCommandBuffer()!, b = queue.makeCommandBuffer()!, c = queue.makeCommandBuffer()!
            require(bytes.prepare(commands: a, uniforms: tallU, depth: tall) != nil, "First real tall set admitted")
            require(bytes.prepare(commands: b, uniforms: tallU, depth: tall) != nil, "Second real tall set admitted")
            peakBytes = bytes.statistics.bytes
            require(peakBytes >= 2 * 165_170_180 && peakBytes <= GameWorldOrderedCornersBudget.maximumBytes,
                "Actual padded allocations fit the total 384 MiB cap")
            require(bytes.prepare(commands: c, uniforms: tallU, depth: tall) == nil && bytes.statistics.busy == 2,
                "Third tall set declines before exceeding the total byte cap")
            a.commit(); b.commit(); a.waitUntilCompleted(); b.waitUntilCompleted()
            require(a.status == .completed && b.status == .completed && bytes.statistics.busy == 0,
                "Actual tall preparations complete and return their resources")
        }
        let report: [String: Any] = ["schema": 1, "device": device.name, "checks": checks,
            "max_sets": 6, "max_bytes": GameWorldOrderedCornersBudget.maximumBytes,
            "actual_two_tall_set_bytes": peakBytes,
            "production_helper_kernels_executed": true, "exact_small_indices_readback": true,
            "real_shared_event_inflight_no_reuse": true, "six_busy_sets_exhaustion": true,
            "abandoned_complete_and_partial_command_cleanup": true,
            "encoder_refusal": "Test-only refusal at real first/second encoder boundary; partial corner command executed",
            "pass": true, "physical_fps_or_visual_acceptance": false]
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output, options: .withoutOverwriting)
        print("PASS actual Mac Metal ordered-corner pool: \(checks) ownership, exhaustion, abandonment, encoder-boundary, index-readback and byte-limit checks")
    }
}

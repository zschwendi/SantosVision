import Foundation
import Metal
import simd

@main
struct OrderedCornersIntegrationValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass repository and new result JSON")
        let root = URL(fileURLWithPath: CommandLine.arguments[1])
        let destination = URL(fileURLWithPath: CommandLine.arguments[2])
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
        func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int,
                     _ usage: MTLTextureUsage) -> any MTLTexture {
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format,
                width: width, height: height, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = usage
            return device.makeTexture(descriptor: descriptor)!
        }
        let width = 32, height = 24, outWidth = 128, outHeight = 96
        let color = texture(.rgba8Unorm, width, height, .shaderRead)
        let depth = texture(.r32Float, width, height, .shaderRead)
        var rgb = [UInt8](repeating: 255, count: width * height * 4)
        var z = [Float](repeating: 0, count: width * height)
        for y in 0..<height { for x in 0..<width {
            let i = y * width + x
            rgb[i * 4] = UInt8(x * 7); rgb[i * 4 + 1] = UInt8(y * 10); rgb[i * 4 + 2] = UInt8((x + y) * 4)
            z[i] = y < 4 ? 0 : (x > 8 && x < 18 && y > 10 ? 0.25 : 0.05)
        } }
        rgb.withUnsafeBytes { color.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
            withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
        z.withUnsafeBytes { depth.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
            withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
        let projection = simd_float4x4(columns: (SIMD4(1, 0, 0, 0), SIMD4(0, 1.2, 0, 0),
            SIMD4(0, 0, 0.0001, -1), SIMD4(0, 0, 0.1, 0)))
        var relative = simd_float4x4(simd_quatf(angle: 0.02, axis: SIMD3(0, 1, 0)))
        relative[3] = SIMD4(0.03, 0, 0, 1)
        let original = GameWorldUniforms(sourceEyeFromClip: projection.inverse,
            currentEyeFromSourceEye: relative, currentClipFromEye: projection,
            sourceSize: SIMD2(UInt32(width), UInt32(height)))
        struct Case {
            let name: String
            var precompute = false
            var verticalPipeline = false
            var allowVertical = false
            var smooth: Float = 0
            var refuseEncoder: Int? = nil
            let expectedOrdered: Bool
            var expectedVertical = false
            var expectedPrecomputed = false
        }
        let cases = [
            Case(name: "ordinary", expectedOrdered: true),
            Case(name: "hud_allowed_without_vertical_pipeline", allowVertical: true, expectedOrdered: true),
            Case(name: "actual_vertical_reconstruction", verticalPipeline: true, allowVertical: true,
                 expectedOrdered: false, expectedVertical: true),
            Case(name: "smooth_policy_original", smooth: 1, expectedOrdered: false),
            Case(name: "ordered_priority_over_precompute", precompute: true, expectedOrdered: true),
            Case(name: "first_encoder_refusal_original", refuseEncoder: 1, expectedOrdered: false),
            Case(name: "second_encoder_refusal_original", refuseEncoder: 2, expectedOrdered: false),
            Case(name: "vertical_available_but_not_allowed", verticalPipeline: true, expectedOrdered: true),
            Case(name: "active_vertical_with_precompute", precompute: true, verticalPipeline: true, allowVertical: true,
                 expectedOrdered: false, expectedVertical: true, expectedPrecomputed: true)
        ]
        struct Rendered {
            let color: [UInt8]
            let depth: [UInt32]
            let owner: [UInt8]
            let ordered: Bool
            let vertical: Bool
            let precomputed: Bool
            let busyBeforeCompletion: Int
            let busyAfterCompletion: Int
        }
        func render(_ item: Case, ordered: Bool) -> Rendered {
            let helper = GameWorldFiniteSkySupport(device: device, library: library,
                colorFormat: .rgba8Unorm, depthFormat: .depth32Float, precomputeCorners: item.precompute,
                orderedCorners: ordered, verticalReconstruction: item.verticalPipeline)!
            if ordered { helper.validationRejectOrderedEncoderOrdinal(item.refuseEncoder) }
            let target = texture(.rgba8Unorm, outWidth, outHeight, .renderTarget)
            let targetDepth = texture(.depth32Float, outWidth, outHeight, .renderTarget)
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = target
            pass.colorAttachments[0].loadAction = .clear; pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1)
            pass.depthAttachment.texture = targetDepth
            pass.depthAttachment.loadAction = .clear; pass.depthAttachment.storeAction = .store
            pass.depthAttachment.clearDepth = 0
            let viewport = MTLViewport(originX: 0, originY: 0, width: Double(outWidth),
                height: Double(outHeight), znear: 0, zfar: 1)
            let command = queue.makeCommandBuffer()!
            var values = original; values.reserved.x = item.smooth
            let result = helper.encode(commands: command, pass: pass, viewport: viewport,
                projection: projection, far: 1000, uniforms: values, color: color, depth: depth,
                allowVerticalReconstruction: item.allowVertical)!
            let before = helper.orderedCornerStatus()["busy_sets"] as? Int ?? 0
            let rowBytes = 256
            let readback = device.makeBuffer(length: rowBytes * outHeight, options: .storageModeShared)!
            let blit = command.makeBlitCommandEncoder()!
            blit.copy(from: result.ownership, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(),
                sourceSize: MTLSize(width: outWidth, height: outHeight, depth: 1), to: readback,
                destinationOffset: 0, destinationBytesPerRow: rowBytes, destinationBytesPerImage: rowBytes * outHeight)
            blit.endEncoding(); command.commit(); command.waitUntilCompleted()
            require(command.status == .completed, "Assembled finite-sky command completed: " + item.name)
            var pixels = [UInt8](repeating: 0, count: outWidth * outHeight * 4)
            var depths = [UInt32](repeating: 0, count: outWidth * outHeight)
            pixels.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: outWidth * 4,
                from: MTLRegionMake2D(0, 0, outWidth, outHeight), mipmapLevel: 0) }
            depths.withUnsafeMutableBytes { targetDepth.getBytes($0.baseAddress!, bytesPerRow: outWidth * 4,
                from: MTLRegionMake2D(0, 0, outWidth, outHeight), mipmapLevel: 0) }
            let raw = readback.contents().bindMemory(to: UInt8.self, capacity: readback.length)
            let owners = (0..<outWidth * outHeight).map { raw[$0 / outWidth * rowBytes + $0 % outWidth] }
            let after = helper.orderedCornerStatus()["busy_sets"] as? Int ?? 0
            return Rendered(color: pixels, depth: depths, owner: owners,
                ordered: result.orderedCorners, vertical: result.verticalReconstruction,
                precomputed: result.cornersPrecomputed, busyBeforeCompletion: before, busyAfterCompletion: after)
        }
        var rows: [[String: Any]] = []
        for item in cases {
            let reference = autoreleasepool { render(item, ordered: false) }
            let candidate = autoreleasepool { render(item, ordered: true) }
            require(candidate.ordered == item.expectedOrdered, "Actual ordered selection: " + item.name)
            require(candidate.vertical == item.expectedVertical, "Actual vertical selection: " + item.name)
            require(candidate.precomputed == item.expectedPrecomputed, "Actual precomputed selection: " + item.name)
            require(candidate.color == reference.color, "Full RGB fallback/parity: " + item.name)
            require(candidate.depth == reference.depth, "Exact depth fallback/parity: " + item.name)
            require(candidate.owner == reference.owner, "Final ownership fallback/parity: " + item.name)
            require(candidate.owner.contains(1) && candidate.owner.contains(2), "Fixture exercises both finite and sky owners")
            let expectedBusy = item.expectedOrdered || item.refuseEncoder == 2 ? 1 : 0
            require(candidate.busyBeforeCompletion == expectedBusy && candidate.busyAfterCompletion == 0,
                "No early resource return or leaked checkout: " + item.name)
            rows.append(["case": item.name, "ordered": candidate.ordered,
                "vertical": candidate.vertical, "precomputed": candidate.precomputed,
                "rgb_depth_owner_byte_identical_to_same_original_policy": true,
                "busy_before_completion": candidate.busyBeforeCompletion,
                "busy_after_completion": candidate.busyAfterCompletion])
        }
        let report: [String: Any] = ["schema": 1, "device": device.name, "checks": checks,
            "completed_assembled_renders": cases.count * 2, "cases": rows, "pass": true,
            "scope": "Actual assembled finite-sky encoder, synthetic finite/sky/depth discontinuity fixture. No physical-game quality or FPS acceptance."]
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: destination, options: .withoutOverwriting)
        print("PASS assembled ordered corners: \(checks) checks across \(cases.count * 2) completed actual Metal finite-sky renders")
    }
}

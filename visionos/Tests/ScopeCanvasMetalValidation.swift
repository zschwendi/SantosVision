// Actual offscreen Mac Metal execution of the production HUD shader. This is
// an angular registration regression, not game or headset visual acceptance.
import Foundation
import Metal
import simd

@main
struct ScopeCanvasMetalValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass repository and captured 288-byte physical request")
        let root = URL(fileURLWithPath: CommandLine.arguments[1])
        let packet = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
        precondition(packet.count == 288)
        func matrix(_ offset: Int) -> simd_float4x4 {
            let v = (0..<16).map { index in Float(bitPattern: packet.withUnsafeBytes {
                $0.loadUnaligned(fromByteOffset: offset + index * 4, as: UInt32.self)
            }) }
            return .init(columns: (SIMD4(v[0], v[1], v[2], v[3]), SIMD4(v[4], v[5], v[6], v[7]),
                SIMD4(v[8], v[9], v[10], v[11]), SIMD4(v[12], v[13], v[14], v[15])))
        }
        let heads = [matrix(32), matrix(160)], projections = [matrix(96), matrix(224)]
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let source = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
        let library = try device.makeLibrary(source: source, options: nil)
        let pipelineDescriptor = MTLRenderPipelineDescriptor()
        pipelineDescriptor.vertexFunction = library.makeFunction(name: "gameWorldHUDVertex")!
        pipelineDescriptor.fragmentFunction = library.makeFunction(name: "gameWorldHUDFragment")!
        pipelineDescriptor.colorAttachments[0].pixelFormat = .rgba8Unorm
        let pipeline = try device.makeRenderPipelineState(descriptor: pipelineDescriptor)
        // The runner extracts this type directly from the production private
        // uniform declaration rather than maintaining a second ABI mirror.
        precondition(MemoryLayout<ScopeCanvasUniforms>.stride == 80)
        let textureDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
            width: 128, height: 128, mipmapped: false)
        textureDescriptor.storageMode = .shared
        textureDescriptor.usage = .shaderRead
        let sourceTexture = device.makeTexture(descriptor: textureDescriptor)!
        var pixels = [UInt8](repeating: 0, count: 128 * 128 * 4)
        // A compact symmetric graticule, with actual coverage and no scene-color
        // subtraction. Its center is the known common texture angular origin.
        for y in 56..<72 { for x in 56..<72 {
            if (62..<66).contains(x) || (62..<66).contains(y) {
                for c in 0..<4 { pixels[(y * 128 + x) * 4 + c] = 255 }
            }
        } }
        pixels.withUnsafeBytes { sourceTexture.replace(region: MTLRegionMake2D(0, 0, 128, 128), mipmapLevel: 0,
            withBytes: $0.baseAddress!, bytesPerRow: 128 * 4) }
        let extent = 2048
        let half = GameWorldHUDPolicy.halfSize(width: 1920, height: 1080, angularWidth: 60, distance: 2)!
        func render(_ eye: Int, scoped: Bool) -> SIMD3<Double> {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
                width: extent, height: extent, mipmapped: false)
            d.storageMode = .shared; d.usage = .renderTarget
            let target = device.makeTexture(descriptor: d)!
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = target
            pass.colorAttachments[0].loadAction = .clear; pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
            let command = queue.makeCommandBuffer()!
            let encoder = command.makeRenderCommandEncoder(descriptor: pass)!
            encoder.setRenderPipelineState(pipeline); encoder.setCullMode(.none)
            var uniforms = ScopeCanvasUniforms(clipFromHead: GameWorldHUDPolicy.clipFromHead(headFromEye: heads[eye],
                projection: projections[eye], placementY: 0, nativeScope: scoped), halfSize: half,
                distance: 2, decodeForLinearTarget: 0)
            encoder.setVertexBytes(&uniforms, length: MemoryLayout<ScopeCanvasUniforms>.stride, index: 0)
            encoder.setFragmentBytes(&uniforms, length: MemoryLayout<ScopeCanvasUniforms>.stride, index: 0)
            encoder.setFragmentTexture(sourceTexture, index: 0)
            encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
            encoder.endEncoding(); command.commit(); command.waitUntilCompleted()
            precondition(command.status == .completed, "Production HUD shader did not complete actual Metal execution")
            var out = [UInt8](repeating: 0, count: extent * extent * 4)
            out.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: extent * 4,
                from: MTLRegionMake2D(0, 0, extent, extent), mipmapLevel: 0) }
            var mass = 0.0, sumX = 0.0, sumY = 0.0
            for y in 0..<extent { for x in 0..<extent {
                let weight = Double(out[(y * extent + x) * 4 + 3])
                mass += weight; sumX += (Double(x) + 0.5) * weight; sumY += (Double(y) + 0.5) * weight
            } }
            precondition(mass > 100_000, "Graticule pixels were not rendered")
            let ndc = SIMD4<Float>(Float(2 * sumX / mass / Double(extent) - 1),
                Float(1 - 2 * sumY / mass / Double(extent)), 0.5, 1)
            let position = projections[eye].inverse * ndc
            var rotation = heads[eye]; rotation[3] = SIMD4(0, 0, 0, 1)
            let direction = rotation * SIMD4(position.x, position.y, position.z, 0)
            return simd_normalize(SIMD3(Double(direction.x), Double(direction.y), Double(direction.z)))
        }
        func angle(_ l: SIMD3<Double>, _ r: SIMD3<Double>) -> Double {
            acos(max(-1, min(1, simd_dot(l, r)))) * 180 / .pi
        }
        let old = [render(0, scoped: false), render(1, scoped: false)]
        let fixed = [render(0, scoped: true), render(1, scoped: true)]
        let oldDisparity = angle(old[0], old[1]), fixedDisparity = angle(fixed[0], fixed[1])
        precondition(oldDisparity > 1.5, "The actual old finite-plane shader path must expose the recorded-eye regression")
        precondition(fixedDisparity < 0.02, "The same scope graticule must describe a common calibrated direction in both eyes")
        for direction in fixed {
            precondition(angle(direction, SIMD3(0, 0, -1)) < 0.02, "Collimated center must remain forward")
        }
        print("PASS actual Mac Metal production HUD shader: old scope convergence \(oldDisparity) degrees; collimated \(fixedDisparity) degrees. Four completed 2048x2048 renders. No physical acceptance.")
    }
}

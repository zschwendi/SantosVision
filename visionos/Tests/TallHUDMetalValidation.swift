// Real captured HUD contribution textures through the production Metal HUD
// shaders and calibrated physical-eye projections. This is offscreen geometry/
// coverage evidence, not running-game or physical-headset quality acceptance.
import Foundation
import Metal
import simd

@main
struct TallHUDMetalValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 6,
            "Pass repo, physical request, baseline1440 capture, tall2304 capture and output JSON")
        let args = CommandLine.arguments
        let root = URL(fileURLWithPath: args[1])
        let packet = try Data(contentsOf: URL(fileURLWithPath: args[2]))
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
        let metal = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
        let library = try device.makeLibrary(source: metal, options: nil)
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = library.makeFunction(name: "gameWorldHUDVertex")!
        descriptor.fragmentFunction = library.makeFunction(name: "gameWorldHUDFragment")!
        descriptor.colorAttachments[0].pixelFormat = .rgba8Unorm
        descriptor.colorAttachments[0].isBlendingEnabled = true
        descriptor.colorAttachments[0].sourceRGBBlendFactor = .one
        descriptor.colorAttachments[0].destinationRGBBlendFactor = .oneMinusSourceAlpha
        descriptor.colorAttachments[0].sourceAlphaBlendFactor = .one
        descriptor.colorAttachments[0].destinationAlphaBlendFactor = .oneMinusSourceAlpha
        let pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
        precondition(MemoryLayout<ScopeCanvasUniforms>.stride == 80)
        let extent = 2048
        struct Result {
            let bytes: [UInt8]
            let bounds: [Int]
            let alphaPixels: Int
        }
        var rows: [[String: Any]] = []
        var ordinaryBounds = [[[Int]]](repeating: [], count: 2)
        for (profile, height, path) in [("baseline1440", 1440, args[3]), ("tall2304", 2304, args[4])] {
            let width = 2560
            let pixels = try Data(contentsOf: URL(fileURLWithPath: path))
            precondition(pixels.count == width * height * 8, "Actual captured RGBA16Float extent differs")
            var sourceBounds = [width, height, -1, -1], sourceAlphaPixels = 0
            pixels.withUnsafeBytes { raw in
                for y in 0..<height { for x in 0..<width {
                    let alpha = Float(Float16(bitPattern: raw.loadUnaligned(fromByteOffset: (y * width + x) * 8 + 6, as: UInt16.self)))
                    precondition(alpha.isFinite && alpha >= 0 && alpha <= 1)
                    if alpha > 0 {
                        sourceAlphaPixels += 1
                        sourceBounds[0] = min(sourceBounds[0], x); sourceBounds[1] = min(sourceBounds[1], y)
                        sourceBounds[2] = max(sourceBounds[2], x); sourceBounds[3] = max(sourceBounds[3], y)
                    }
                } }
            }
            precondition(sourceAlphaPixels > 80_000, "This test requires actual nonempty game HUD coverage")
            let textureDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba16Float,
                width: width, height: height, mipmapped: false)
            textureDescriptor.storageMode = .shared; textureDescriptor.usage = .shaderRead
            let texture = device.makeTexture(descriptor: textureDescriptor)!
            pixels.withUnsafeBytes { texture.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
                withBytes: $0.baseAddress!, bytesPerRow: width * 8) }
            let half = GameWorldHUDPolicy.halfSize(width: width, height: height, angularWidth: 60, distance: 2)!
            func render(eye: Int, scope: Bool, legacy: Bool) -> Result {
                let targetDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
                    width: extent, height: extent, mipmapped: false)
                targetDescriptor.storageMode = .shared; targetDescriptor.usage = .renderTarget
                let target = device.makeTexture(descriptor: targetDescriptor)!
                let pass = MTLRenderPassDescriptor()
                pass.colorAttachments[0].texture = target
                pass.colorAttachments[0].loadAction = .clear; pass.colorAttachments[0].storeAction = .store
                pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
                let command = queue.makeCommandBuffer()!, encoder = command.makeRenderCommandEncoder(descriptor: pass)!
                encoder.setRenderPipelineState(pipeline); encoder.setCullMode(.none)
                // Frozen former ordinary geometry, independent of the revised policy.
                let old = scope ? (half, Float(0))
                    : (half * GameWorldHUDPolicy.immersiveScale, half.y * GameWorldHUDPolicy.immersiveVerticalFraction)
                let current = GameWorldHUDPolicy.placement(halfSize: half, nativeScope: scope,
                    sourceSize: SIMD2(UInt32(width), UInt32(height)))
                let layout = legacy ? old : (current.halfSize, current.y)
                let clip = GameWorldHUDPolicy.clipFromHead(headFromEye: heads[eye], projection: projections[eye],
                    placementY: layout.1, nativeScope: scope)
                var uniforms = ScopeCanvasUniforms(clipFromHead: clip, halfSize: layout.0,
                    distance: 2, decodeForLinearTarget: 0)
                encoder.setVertexBytes(&uniforms, length: MemoryLayout<ScopeCanvasUniforms>.stride, index: 0)
                encoder.setFragmentBytes(&uniforms, length: MemoryLayout<ScopeCanvasUniforms>.stride, index: 0)
                encoder.setFragmentTexture(texture, index: 0)
                encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
                encoder.endEncoding(); command.commit(); command.waitUntilCompleted()
                precondition(command.status == .completed, "Actual Metal render failed")
                var out = [UInt8](repeating: 0, count: extent * extent * 4)
                out.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: extent * 4,
                    from: MTLRegionMake2D(0, 0, extent, extent), mipmapLevel: 0) }
                var bounds = [extent, extent, -1, -1], count = 0
                for y in 0..<extent { for x in 0..<extent {
                    if out[(y * extent + x) * 4 + 3] > 0 {
                        count += 1
                        bounds[0] = min(bounds[0], x); bounds[1] = min(bounds[1], y)
                        bounds[2] = max(bounds[2], x); bounds[3] = max(bounds[3], y)
                    }
                } }
                precondition(count > 10_000)
                if !scope && !legacy {
                    // Qualify the whole [0,1] UV quad, not only the existing
                    // lower-left alpha bbox. New upper-right HUD remains allowed.
                    for x: Float in [-1, 1] { for y: Float in [-1, 1] {
                        let p = clip * SIMD4(x * layout.0.x, y * layout.0.y, -2, 1), ndc = p / p.w
                        precondition(p.w > 0 && abs(ndc.x) < 1 && abs(ndc.y) < 1)
                    } }
                    var expected = SIMD4<Float>(.infinity, .infinity, -.infinity, -.infinity)
                    for x in [sourceBounds[0], sourceBounds[2] + 1] {
                        for y in [sourceBounds[1], sourceBounds[3] + 1] {
                            let p = clip * SIMD4((Float(x) / Float(width) * 2 - 1) * layout.0.x,
                                (1 - Float(y) / Float(height) * 2) * layout.0.y, -2, 1)
                            let screen = SIMD2((p.x / p.w + 1) * Float(extent) / 2,
                                (1 - p.y / p.w) * Float(extent) / 2)
                            expected.x = min(expected.x, screen.x); expected.y = min(expected.y, screen.y)
                            expected.z = max(expected.z, screen.x); expected.w = max(expected.w, screen.y)
                        }
                    }
                    precondition(zip(bounds, [expected.x, expected.y, expected.z, expected.w]).allSatisfy {
                        abs(Float($0.0) - $0.1) <= 3
                    }, "Actual captured alpha coverage was clipped, misplaced or bounds changed beyond filtering")
                }
                return Result(bytes: out, bounds: bounds, alphaPixels: count)
            }
            for eye in 0..<2 {
                let old = render(eye: eye, scope: false, legacy: true)
                let current = render(eye: eye, scope: false, legacy: false)
                let oldScope = render(eye: eye, scope: true, legacy: true)
                let currentScope = render(eye: eye, scope: true, legacy: false)
                precondition(oldScope.bytes == currentScope.bytes, "Native-scope output must be bit-identical at either extent")
                if height == 1440 { precondition(old.bytes == current.bytes, "Baseline output changed") }
                else { precondition(old.bytes != current.bytes, "Regression fixture must expose old tall HUD enlargement") }
                ordinaryBounds[eye].append(current.bounds)
                rows.append(["profile": profile, "eye": eye, "source_dimensions": [width, height],
                    "source_alpha_pixels": sourceAlphaPixels, "source_alpha_bounds": sourceBounds,
                    "old_output_alpha_bounds": old.bounds, "new_output_alpha_bounds": current.bounds,
                    "new_output_alpha_pixels": current.alphaPixels, "native_scope_bit_identical": true,
                    "ordinary_baseline_bit_identical": height == 1440,
                    "full_uv_quad_inside_measured_frustum": true, "completed_metal_status": 4])
            }
        }
        for eye in 0..<2 {
            let base = ordinaryBounds[eye][0], tall = ordinaryBounds[eye][1]
            let widths = Float(tall[2] - tall[0] + 1) / Float(base[2] - base[0] + 1)
            let heights = Float(tall[3] - tall[1] + 1) / Float(base[3] - base[1] + 1)
            precondition(abs(widths - 1) < 0.02 && abs(heights - 1) < 0.02,
                "Actual captured minimap does not retain baseline angular size")
        }
        let report: [String: Any] = ["schema": 1, "device": device.name, "completed_renders": 16,
            "render_extent": [extent, extent], "actual_captured_rgba16f_alpha": true,
            "rows": rows, "pass": true,
            "scope": "Offscreen production shader, actual captured alpha and calibrated eyes; different scenes. No full-gameplay, physical placement, sharpness, stereo or FPS acceptance."]
        let json = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
        try json.write(to: URL(fileURLWithPath: args[5]), options: .withoutOverwriting)
        print("PASS actual Metal tall HUD normalization: 16 completed renders, actual captured alpha, both calibrated eyes, baseline and native scope byte-identical.")
    }
}

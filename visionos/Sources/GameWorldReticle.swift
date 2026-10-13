import simd

enum GameWorldAimProjection {
    static func clip(origin: SIMD3<Double>, direction: SIMD3<Double>,
                     gameFromCurrentEye: simd_double4x4, projection: simd_float4x4) -> SIMD4<Float>? {
        guard (0..<3).allSatisfy({ origin[$0].isFinite && direction[$0].isFinite }),
              abs(simd_length(direction) - 1) < 1e-4 else { return nil }
        let point = origin + direction * 40
        let eyePoint = gameFromCurrentEye.inverse * SIMD4<Double>(point.x, point.y, point.z, 1)
        let clip = projection * SIMD4<Float>(eyePoint)
        guard clip.w > 0, clip.x.isFinite, clip.y.isFinite, clip.z.isFinite, clip.w.isFinite,
              clip.z >= 0, clip.z <= clip.w, abs(clip.x) <= clip.w, abs(clip.y) <= clip.w else { return nil }
        return clip
    }
}

#if SV_GAME_RUNTIME
import Foundation
import Metal
import simd

/// Project the original gameplay ray independently in each eye. It never
/// borrows world depth under a flat HUD pixel or changes the bullet ray.
final class GameWorldReticle: @unchecked Sendable {
    private let pipeline: any MTLRenderPipelineState
    private let depthState: any MTLDepthStencilState
    private struct Uniforms {
        var clipPosition: SIMD4<Float>
        var halfSizeNDC: SIMD2<Float>
        var padding = SIMD2<Float>.zero
    }

    init?(device: any MTLDevice, library: any MTLLibrary, colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat) {
        guard let vertex = library.makeFunction(name: "gameWorldReticleVertex"),
              let fragment = library.makeFunction(name: "gameWorldReticleFragment") else { return nil }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Controller gameplay aim projected independently in each eye"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = colorFormat
        descriptor.depthAttachmentPixelFormat = depthFormat
        let depth = MTLDepthStencilDescriptor()
        depth.depthCompareFunction = .always
        depth.isDepthWriteEnabled = false
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor),
              let state = device.makeDepthStencilState(descriptor: depth) else { return nil }
        self.pipeline = pipeline
        self.depthState = state
    }

    static func snapshot() -> SVXRAimSnapshot? {
        var aim = SVXRAimSnapshot()
        guard MemoryLayout<SVXRAimSnapshot>.size == 104,
              sv_xr_get_aim_snapshot(&aim, 104) == 0,
              aim.version == 1, aim.size == 104, aim.reserved == 0,
              aim.flags & 7 == 7, aim.age_seconds >= 0, aim.age_seconds <= 0.1 else { return nil }
        return aim
    }

    func encode(_ encoder: any MTLRenderCommandEncoder, aim: SVXRAimSnapshot,
                gameFromCurrentEye: simd_double4x4, projection: simd_float4x4, viewport: MTLViewport) {
        guard viewport.width > 0, viewport.height > 0 else { return }
        func vector(_ tuple: (Double, Double, Double)) -> SIMD3<Double> { SIMD3(tuple.0, tuple.1, tuple.2) }
        let origin = vector(aim.origin), direction = vector(aim.direction)
        // A finite distance provides stereo convergence for this direction
        // marker. This does not claim a raycast surface or weapon impact point.
        guard let clip = GameWorldAimProjection.clip(origin: origin, direction: direction,
                    gameFromCurrentEye: gameFromCurrentEye, projection: projection) else { return }
        var uniforms = Uniforms(clipPosition: clip,
                                halfSizeNDC: SIMD2(Float(8 / viewport.width), Float(8 / viewport.height)))
        encoder.setRenderPipelineState(pipeline)
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
    }
}
#endif

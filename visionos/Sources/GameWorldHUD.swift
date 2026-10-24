import simd

enum GameWorldHUDPolicy {
    struct Candidate {
        let lease: UInt64
        let epoch: UInt64
        let gameFrame: UInt64
        let sourceEye: Int
    }

    /// A missing/decomposed eye must never be combined with another eye's full
    /// game image. One logical canvas is selected once for the whole drawable.
    static func selectedIndex(_ candidates: [Candidate?], viewCount: Int) -> Int? {
        guard (1...2).contains(viewCount), candidates.count == viewCount,
              let first = candidates.first ?? nil, first.epoch != 0 else { return nil }
        for index in 0..<viewCount {
            guard let item = candidates[index], item.lease != 0,
                  item.epoch == first.epoch, item.sourceEye == index else { return nil }
        }
        return candidates.indices.max { lhs, rhs in
            candidates[lhs]!.gameFrame < candidates[rhs]!.gameFrame
        }
    }

    static func halfSize(width: Int, height: Int, angularWidth: Float, distance: Float) -> SIMD2<Float>? {
        guard width > 1, height > 1, width <= 16_384, height <= 16_384,
              angularWidth.isFinite, (20...90).contains(angularWidth),
              distance.isFinite, (0.5...10).contains(distance) else { return nil }
        let x = tan(angularWidth * .pi / 360) * distance
        let y = x * Float(height) / Float(width)
        guard x.isFinite, y.isFinite, x > 0, y > 0 else { return nil }
        return SIMD2(x, y)
    }

    // Bring both outside HUD corners inward and lower the whole canvas.
    // Flat-window rendering keeps the original game coordinates.
    static let immersiveScale: Float = 0.94
    static let immersiveVerticalFraction: Float = -0.17
}

#if SV_GAME_RUNTIME
import Foundation
import Metal

/// Coverage-bearing HUD pixels are isolated by the native renderer. This class
/// never infers transparency from the final game color or scene depth.
final class GameWorldHUD: @unchecked Sendable {
    final class Snapshot: @unchecked Sendable {
        let native: SVWorldHudFrame
        let color: any MTLTexture
        let worldColor: any MTLTexture
        let halfSize: SIMD2<Float>

        init?(world: SVWorldFrame, device: any MTLDevice) {
            var value = SVWorldHudFrame()
            value.abi_version = 1
            value.byte_size = UInt32(MemoryLayout<SVWorldHudFrame>.size)
            guard MemoryLayout<SVWorldHudFrame>.size == 104,
                  sv_world_frame_hud_v1(world.lease, &value) == 0,
                  value.abi_version == 1, value.byte_size == 104,
                  value.lease == world.lease, value.provenance_epoch != 0,
                  value.game_frame == world.game_frame, value.request_frame == world.request_frame,
                  value.head_frame == world.head_frame, value.source_eye == world.view_index,
                  value.width == world.width, value.height == world.height,
                  value.presentation_time.isFinite, value.presentation_time == world.presentation_time,
                  value.flags == 1, value.reserved.0 == 0, value.reserved.1 == 0,
                  let half = GameWorldHUDPolicy.halfSize(width: Int(value.width), height: Int(value.height),
                      angularWidth: value.angular_width_degrees, distance: value.distance_metres),
                  let colorPointer = UnsafeRawPointer(bitPattern: UInt(value.color_texture)),
                  let worldPointer = UnsafeRawPointer(bitPattern: UInt(value.world_color_texture)),
                  let color = Unmanaged<AnyObject>.fromOpaque(colorPointer).takeUnretainedValue() as? any MTLTexture,
                  let worldColor = Unmanaged<AnyObject>.fromOpaque(worldPointer).takeUnretainedValue() as? any MTLTexture,
                  color.device.registryID == device.registryID, worldColor.device.registryID == device.registryID,
                  color.textureType == .type2D, worldColor.textureType == .type2D,
                  color.pixelFormat == .rgba16Float,
                  worldColor.pixelFormat == .bgra8Unorm || worldColor.pixelFormat == .rgba8Unorm,
                  color.width == Int(value.width), color.height == Int(value.height),
                  worldColor.width == color.width, worldColor.height == color.height else { return nil }
            native = value
            self.color = color
            self.worldColor = worldColor
            halfSize = half
        }

        var candidate: GameWorldHUDPolicy.Candidate {
            .init(lease: native.lease, epoch: native.provenance_epoch,
                  gameFrame: native.game_frame, sourceEye: Int(native.source_eye))
        }
    }

    private let pipeline: any MTLRenderPipelineState
    private let depth: any MTLDepthStencilState
    private let decodeForLinearTarget: UInt32
    private struct Uniforms {
        var clipFromHead: simd_float4x4
        var halfSize: SIMD2<Float>
        var distance: Float
        var decodeForLinearTarget: UInt32
    }

    init?(device: any MTLDevice, library: any MTLLibrary,
          colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat) {
        guard let vertex = library.makeFunction(name: "gameWorldHUDVertex"),
              let fragment = library.makeFunction(name: "gameWorldHUDFragment") else { return nil }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "One coverage-bearing GTA HUD canvas for both physical eyes"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = colorFormat
        descriptor.colorAttachments[0].isBlendingEnabled = true
        descriptor.colorAttachments[0].sourceRGBBlendFactor = .one
        descriptor.colorAttachments[0].destinationRGBBlendFactor = .oneMinusSourceAlpha
        descriptor.colorAttachments[0].sourceAlphaBlendFactor = .one
        descriptor.colorAttachments[0].destinationAlphaBlendFactor = .oneMinusSourceAlpha
        descriptor.depthAttachmentPixelFormat = depthFormat
        let d = MTLDepthStencilDescriptor()
        d.depthCompareFunction = .always
        d.isDepthWriteEnabled = false
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor),
              let depth = device.makeDepthStencilState(descriptor: d) else { return nil }
        self.pipeline = pipeline
        self.depth = depth
        decodeForLinearTarget = colorFormat == .bgra8Unorm || colorFormat == .rgba8Unorm ? 0 : 1
    }

    func encode(_ encoder: any MTLRenderCommandEncoder, snapshot: Snapshot,
                headFromEye: simd_float4x4, projection: simd_float4x4) {
        var placement = matrix_identity_float4x4
        placement.columns.3.y = snapshot.halfSize.y * GameWorldHUDPolicy.immersiveVerticalFraction
        var uniforms = Uniforms(clipFromHead: projection * headFromEye.inverse * placement,
            halfSize: snapshot.halfSize * GameWorldHUDPolicy.immersiveScale, distance: snapshot.native.distance_metres,
            decodeForLinearTarget: decodeForLinearTarget)
        encoder.setRenderPipelineState(pipeline)
        encoder.setDepthStencilState(depth)
        encoder.setCullMode(.none)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setFragmentTexture(snapshot.color, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
    }
}
#endif

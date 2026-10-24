import simd

enum GameWorldAimProjection {
    static func doubleMatrix(_ matrix: simd_float4x4) -> simd_double4x4 {
        .init(columns: (SIMD4<Double>(matrix[0]), SIMD4<Double>(matrix[1]),
                        SIMD4<Double>(matrix[2]), SIMD4<Double>(matrix[3])))
    }
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

/// The native ray and reference belong to one qualified sampler transaction.
/// A camera-sequence comparison reports source lag; it does not certify the
/// foreground's draw pose or reconstruct it from the final mixed image.
enum GameWorldAimReferencePolicy {
    static func valid(epoch: UInt64, producer: UInt64, reference: UInt64,
                      request: UInt64, head: UInt64, gameFromStage: simd_double4x4) -> Bool {
        guard epoch != 0, producer != 0, reference >= producer, request != 0, head != 0,
              (0..<4).allSatisfy({ c in (0..<4).allSatisfy { gameFromStage[c][$0].isFinite } }),
              abs(gameFromStage[0].w) < 1e-6, abs(gameFromStage[1].w) < 1e-6,
              abs(gameFromStage[2].w) < 1e-6, abs(gameFromStage[3].w - 1) < 1e-6 else { return false }
        let rotation = simd_double3x3(columns: (
            SIMD3(gameFromStage[0].x, gameFromStage[0].y, gameFromStage[0].z),
            SIMD3(gameFromStage[1].x, gameFromStage[1].y, gameFromStage[1].z),
            SIMD3(gameFromStage[2].x, gameFromStage[2].y, gameFromStage[2].z)))
        guard abs(rotation.determinant - 1) <= 0.002 else { return false }
        for a in 0..<3 { for b in 0..<3 {
            if abs(simd_dot(rotation[a], rotation[b]) - (a == b ? 1 : 0)) > 0.002 { return false }
        } }
        return true
    }

    static func displayedSourceLag(producer: UInt64, source: UInt64) -> UInt64? {
        source != 0 && source <= producer ? producer - source : nil
    }
}

#if SV_GAME_RUNTIME
import Foundation
import Metal
import simd

/// Project the original gameplay ray independently in each eye. It never
/// borrows world depth under a flat HUD pixel or changes the bullet ray.
final class GameWorldReticle: @unchecked Sendable {
    struct Reference {
        let gameFromStage: simd_double4x4
        let camera: UInt64
        let request: UInt64
        let head: UInt64
        let producer: UInt64
        let scope: UInt64
    }
    struct Snapshot {
        let aim: SVXRAimSnapshot
        let reference: Reference?
        func gameFromStage(fallback: simd_float4x4) -> simd_double4x4 {
            reference?.gameFromStage ?? GameWorldAimProjection.doubleMatrix(fallback)
        }
    }
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

    private static func validAim(_ aim: SVXRAimSnapshot) -> Bool {
        aim.version == 1 && aim.size == 104 && aim.reserved == 0 && aim.view_epoch != 0
            && aim.flags & 7 == 7 && aim.age_seconds.isFinite
            && aim.age_seconds >= 0 && aim.age_seconds <= 0.1
    }

    static func snapshot() -> Snapshot? {
        var native = SVXRAimReferenceSnapshotV2()
        if MemoryLayout<SVXRAimReferenceSnapshotV2>.size == 280,
           sv_xr_get_aim_reference_snapshot_v2(&native, 280) == 0,
           native.version == 2, native.size == 280, validAim(native.aim),
           native.aim.flags & 64 != 0, native.aim.sequence == native.producer_camera_sequence {
            let values = withUnsafeBytes(of: native.game_from_stage) { raw in
                (0..<16).map { raw.loadUnaligned(fromByteOffset: $0 * 8, as: Double.self) }
            }
            let matrix = simd_double4x4(columns: (
                SIMD4(values[0],values[1],values[2],values[3]), SIMD4(values[4],values[5],values[6],values[7]),
                SIMD4(values[8],values[9],values[10],values[11]), SIMD4(values[12],values[13],values[14],values[15])))
            if GameWorldAimReferencePolicy.valid(epoch: native.aim.view_epoch, producer: native.producer_camera_sequence,
                reference: native.reference_camera_sequence, request: native.reference_request_frame,
                head: native.reference_head_frame, gameFromStage: matrix) {
                return Snapshot(aim: native.aim, reference: Reference(gameFromStage: matrix,
                    camera: native.reference_camera_sequence, request: native.reference_request_frame,
                    head: native.reference_head_frame, producer: native.producer_camera_sequence, scope: native.scope_sequence))
            }
        }
        var aim = SVXRAimSnapshot()
        guard MemoryLayout<SVXRAimSnapshot>.size == 104,
              sv_xr_get_aim_snapshot(&aim, 104) == 0,
              validAim(aim), aim.flags & 64 == 0 else { return nil }
        // Ordinary DualSense/gameplay keeps its established V1 path. A tracked
        // ray cannot fall back to unrelated per-eye old source references.
        return Snapshot(aim: aim, reference: nil)
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

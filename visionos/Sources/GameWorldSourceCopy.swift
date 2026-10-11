#if SV_GAME_RUNTIME
import ARKit
import Foundation
import Metal
import simd

// BEGIN PureSourceChecks (the standalone harness compiles this block verbatim on macOS; no ARKit, no native symbols)
/// Where a source eye image came from. Only `.actual` may ever be acknowledged
/// to the native side or reported as a game presentation.
enum GameWorldSourceKind: String, Sendable {
    /// A leased native game frame.
    case actual
    /// The analytic probe scene rendered from a real retained ARKit anchor.
    case analytic
    /// The analytic probe scene rendered from a simulator pose fixture; there is
    /// no ARKit anchor, so the system's display-time adjustment is untestable.
    case synthetic
}

/// Pure verification of a lease against the retained pose it names. No Metal,
/// so the rules are unit-testable: the retained record is keyed by the consumed
/// `head_frame`; the request frame is a separate key for the eye geometry, and
/// both must agree before the retained eye transforms are trusted.
enum GameWorldSourcePoseCheck {
    struct Result: Sendable {
        let passed: Bool
        let reason: String
        let maximumRotationError: Float
        let maximumTranslationError: Float
        let maximumProjectionError: Float
    }

    static let rotationTolerance: Float = 2e-3
    static let translationTolerance: Float = 2e-3
    static let projectionTolerance: Float = 1e-4

    /// `retainedStageFromHead`, `retainedEyes` and `retainedRequestFrame` come from the record
    /// keyed by the lease's consumed `head_frame`; `requestFrame` is the lease's own request.
    static func verify(retainedStageFromHead: simd_float4x4, retainedEyes: [XRRetainedEyeView], retainedRequestFrame: UInt64,
                       eyeIndex: Int, requestFrame: UInt64,
                       leaseGameFromStage: simd_float4x4, leaseGameFromEye: simd_float4x4,
                       leaseProjection: simd_float4x4, leaseDepthReversed: Bool) -> Result {
        guard retainedRequestFrame != 0, retainedRequestFrame == requestFrame else {
            return Result(passed: false, reason: "request_frame_mismatch", maximumRotationError: .infinity,
                          maximumTranslationError: .infinity, maximumProjectionError: .infinity)
        }
        guard eyeIndex >= 0, eyeIndex < retainedEyes.count else {
            return Result(passed: false, reason: "eye_not_retained", maximumRotationError: .infinity,
                          maximumTranslationError: .infinity, maximumProjectionError: .infinity)
        }
        let eye = retainedEyes[eyeIndex]
        // The game recorded game_from_stage = game_from_eye * inverse(stage_from_eye), so the
        // retained pose must reproduce game_from_eye through the lease's own stage mapping.
        let expected = leaseGameFromStage * retainedStageFromHead * eye.headFromEye
        var rotation: Float = 0
        for column in 0..<3 {
            let delta = expected[column] - leaseGameFromEye[column]
            rotation = max(rotation, abs(delta.x), abs(delta.y), abs(delta.z), abs(delta.w))
        }
        let shift = expected.columns.3 - leaseGameFromEye.columns.3
        let translation = max(abs(shift.x), abs(shift.y), abs(shift.z), abs(shift.w))
        let projection = projectionError(retained: eye.projectionFromEye, lease: leaseProjection, leaseDepthReversed: leaseDepthReversed)
        let finite = rotation.isFinite && translation.isFinite && projection.isFinite
        let passed = finite && rotation <= rotationTolerance && translation <= translationTolerance
            && projection <= projectionTolerance
        let reason = !finite ? "non_finite" : rotation > rotationTolerance ? "rotation_mismatch"
            : translation > translationTolerance ? "translation_mismatch"
            : projection > projectionTolerance ? "projection_mismatch" : "ok"
        return Result(passed: passed, reason: reason, maximumRotationError: rotation,
                      maximumTranslationError: translation, maximumProjectionError: projection)
    }

    /// Relative element error between a compositor projection and the game's copy of
    /// it. Identical except the depth row, which the bridge flips for a forward-Z
    /// game (row 2 becomes row 3 minus row 2).
    static func projectionError(retained: simd_float4x4, lease: simd_float4x4, leaseDepthReversed: Bool) -> Float {
        var error: Float = 0
        for column in 0..<4 {
            let r = retained[column], l = lease[column]
            let expectedZ = leaseDepthReversed ? r.z : r.w - r.z
            let scale = max(1, abs(r.x), abs(r.y), abs(r.z), abs(r.w))
            error = max(error, abs(r.x - l.x) / scale, abs(r.y - l.y) / scale, abs(expectedZ - l.z) / scale, abs(r.w - l.w) / scale)
        }
        return error
    }
}

/// Pure gate for the fullscreen copy. The copy reads the nearest source depth and
/// reconstructs at the output pixel's ray, which equals the mesh's interpolated
/// plane only when every output pixel centre is exactly a source texel centre:
/// same extent, integral viewport origin, the same projection (hence the same
/// view rays and depth range) up to the game's depth-row convention, and the same
/// head-from-eye transform (the ray origin under the labelled source anchor; a
/// changed IPD or eye calibration moves it with the projection unchanged). No SDK
/// guarantee of per-view constancy is cited, so it is checked. Anything else is
/// not a copy and must use a mesh path.
enum GameWorldCopyLatticeCheck {
    struct Result: Sendable {
        let passed: Bool
        let reason: String
        let projectionError: Float
        var eyeTransformError: Float = 0
    }

    static let projectionTolerance: Float = 1e-6
    /// Rigid head-from-eye agreement: rotation elements and metres of translation.
    static let eyeRotationTolerance: Float = 1e-5
    static let eyeTranslationTolerance: Float = 1e-4

    static func verify(sourceWidth: Int, sourceHeight: Int, viewport: MTLViewport,
                       sourceProjection: simd_float4x4, sourceDepthReversed: Bool,
                       targetProjection: simd_float4x4,
                       retainedHeadFromEye: simd_float4x4, currentHeadFromEye: simd_float4x4) -> Result {
        var eyeError: Float = 0
        for column in 0..<3 {
            let delta = retainedHeadFromEye[column] - currentHeadFromEye[column]
            eyeError = max(eyeError, abs(delta.x), abs(delta.y), abs(delta.z), abs(delta.w))
        }
        let shift = retainedHeadFromEye.columns.3 - currentHeadFromEye.columns.3
        let eyeShift = max(abs(shift.x), abs(shift.y), abs(shift.z), abs(shift.w))
        guard eyeError.isFinite, eyeShift.isFinite else {
            return Result(passed: false, reason: "eye_transform_non_finite", projectionError: .infinity, eyeTransformError: .infinity)
        }
        guard eyeError <= eyeRotationTolerance, eyeShift <= eyeTranslationTolerance else {
            return Result(passed: false, reason: "eye_transform_mismatch", projectionError: .infinity, eyeTransformError: max(eyeError, eyeShift))
        }
        guard viewport.width.isFinite, viewport.height.isFinite, viewport.originX.isFinite, viewport.originY.isFinite,
              viewport.width > 0, viewport.height > 0 else {
            return Result(passed: false, reason: "viewport_invalid", projectionError: .infinity, eyeTransformError: max(eyeError, eyeShift))
        }
        guard viewport.width == Double(sourceWidth), viewport.height == Double(sourceHeight) else {
            return Result(passed: false, reason: "extent_mismatch", projectionError: .infinity)
        }
        guard viewport.originX == viewport.originX.rounded(), viewport.originY == viewport.originY.rounded() else {
            return Result(passed: false, reason: "viewport_origin_fractional", projectionError: .infinity)
        }
        // The target is always reverse-Z (the compositor documents 1 near, 0 far); the source may be either.
        let error = GameWorldSourcePoseCheck.projectionError(retained: targetProjection, lease: sourceProjection,
                                                             leaseDepthReversed: sourceDepthReversed)
        guard error.isFinite else { return Result(passed: false, reason: "projection_non_finite", projectionError: error, eyeTransformError: max(eyeError, eyeShift)) }
        guard error <= projectionTolerance else { return Result(passed: false, reason: "projection_mismatch", projectionError: error, eyeTransformError: max(eyeError, eyeShift)) }
        return Result(passed: true, reason: "ok", projectionError: error, eyeTransformError: max(eyeError, eyeShift))
    }
}

/// Pure pair qualification. A pair is one scene epoch, one consumed head frame,
/// one request, two distinct eyes; coincident counters from independent frames
/// are the producer's responsibility to withhold, but every field is re-checked.
enum GameWorldPairValidation {
    static func reject(_ pair: SVWorldPair) -> String? {
        guard pair.abi_version == 1 else { return "pair_abi_version" }
        guard Int(pair.byte_size) == MemoryLayout<SVWorldPair>.size else { return "pair_byte_size" }
        guard pair.scene_epoch != 0 else { return "pair_scene_epoch_zero" }
        let left = pair.eyes.0, right = pair.eyes.1
        for (index, eye) in [left, right].enumerated() {
            guard eye.abi_version == 1, eye.byte_size == UInt32(MemoryLayout<SVWorldFrame>.size) else { return "eye\(index)_abi" }
            guard eye.lease != 0 else { return "eye\(index)_lease_zero" }
            guard eye.view_count == 2, eye.view_index == UInt32(index) else { return "eye\(index)_view_identity" }
            guard eye.depth_convention == 0, eye.reserved == 0 else { return "eye\(index)_reserved" }
            guard eye.presentation_time.isFinite else { return "eye\(index)_time" }
        }
        guard left.lease != right.lease else { return "pair_shared_lease" }
        guard left.head_frame == right.head_frame, left.head_frame != 0 else { return "pair_head_frame" }
        guard left.request_frame == right.request_frame else { return "pair_request_frame" }
        guard left.presentation_time == right.presentation_time else { return "pair_presentation_time" }
        guard left.width == right.width, left.height == right.height else { return "pair_size" }
        guard matricesClose(left.game_from_stage, right.game_from_stage, tolerance: 1e-4) else { return "pair_game_from_stage" }
        return nil
    }

    static func matricesClose(_ a: (Float, Float, Float, Float, Float, Float, Float, Float,
                                    Float, Float, Float, Float, Float, Float, Float, Float),
                              _ b: (Float, Float, Float, Float, Float, Float, Float, Float,
                                    Float, Float, Float, Float, Float, Float, Float, Float),
                              tolerance: Float) -> Bool {
        var same = true
        withUnsafeBytes(of: a) { left in
            withUnsafeBytes(of: b) { right in
                let l = left.bindMemory(to: Float.self), r = right.bindMemory(to: Float.self)
                for index in 0..<16 where !(abs(l[index] - r[index]) <= tolerance) { same = false }
            }
        }
        return same
    }
}
// END PureSourceChecks

/// `sv_world_acquire_pair` from WorldBridge.h, called directly. Codes follow the
/// header: 0 both eyes ready, 1 no qualified pair, -1 bad arguments or ABI. The
/// documented 592-byte layout is checked once; a mismatch disables pairing.
enum GameWorldPairAcquisition {
    static let documentedPairSize = 592
    static let layoutMatchesDocumentation = MemoryLayout<SVWorldPair>.size == documentedPairSize
        && MemoryLayout<SVWorldFrame>.size == 288

    /// Header code plus the filled structure; -3 when the Swift layout does not match the header.
    static func acquire() -> (code: Int32, pair: SVWorldPair) {
        var pair = SVWorldPair()
        pair.abi_version = 1
        pair.byte_size = UInt32(MemoryLayout<SVWorldPair>.size)
        guard layoutMatchesDocumentation else { return (-3, pair) }
        let code = sv_world_acquire_pair(&pair)
        return (code, pair)
    }
}

/// Uniforms for the fullscreen copy; mirrors GameWorldCopyUniforms in GameWorld.metal.
struct GameWorldCopyUniforms {
    var sourceEyeFromClip: simd_float4x4
    var sourceClipFromEye: simd_float4x4
    var currentEyeFromClip: simd_float4x4
    var currentClipFromEye: simd_float4x4
    /// Present head relative to the source eye: used for the near fade only, never for geometry.
    var currentEyeFromSourceEye: simd_float4x4
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32
    var colorTransfer: UInt32
    var nearFadeDistances: SIMD2<Float>
    var reserved = SIMD2<Float>.zero
}

/// The one-triangle colour/depth conversion pass (MUGUET_COMPOSITOR_SOURCE_ANCHOR_COPY).
/// Same colour and depth attachments, depth state and fade rule as the mesh path.
final class GameWorldSourceCopy: @unchecked Sendable {
    let pipeline: any MTLRenderPipelineState

    init?(device: any MTLDevice, library: any MTLLibrary, colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat) {
        guard let vertex = library.makeFunction(name: "gameWorldCopyVertex"),
              let fragment = library.makeFunction(name: "gameWorldSourceCopyFragment") else { return nil }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Game world source-anchor fullscreen copy"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = colorFormat
        descriptor.depthAttachmentPixelFormat = depthFormat
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor) else { return nil }
        self.pipeline = pipeline
    }

    func encode(_ encoder: any MTLRenderCommandEncoder, uniforms: GameWorldCopyUniforms,
                depthState: any MTLDepthStencilState, color: any MTLTexture, depth: any MTLTexture) {
        var uniforms = uniforms
        encoder.setRenderPipelineState(pipeline)
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldCopyUniforms>.stride, index: 0)
        encoder.setFragmentTexture(color, index: 0)
        encoder.setFragmentTexture(depth, index: 1)
        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
    }
}
#endif

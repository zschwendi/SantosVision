import Foundation
import simd

/// Extra source coverage for display-time head reprojection. The physical display projection is never
/// changed: the expanded matrix must travel through the request and source
/// lease so reprojection still uses the camera that actually rendered it.
enum GameWorldSourceGuardBand {
    // The user can outrun the old source image during a turn. Render a real
    // margin on both axes instead of stretching the edge or delaying the head.
    // 1.30 contains the angular directions of both registered head-turn captures
    // (maximum required scales 1.191 and 1.276). This is a bounded margin, not a
    // guarantee for arbitrary turns or translation disocclusions. The original
    // 1.0 projection remains available for controlled clarity comparisons.
    static let defaultScale: Float = 1.3
    static let allowedScales: ClosedRange<Float> = 1...1.35
    static let configuredScale = resolveScale(ProcessInfo.processInfo.environment)

    static func resolveScale(_ environment: [String: String]) -> Float {
        guard let value = environment["MUGUET_XR_SOURCE_GUARD_BAND"],
              let scale = Float(value), scale.isFinite, allowedScales.contains(scale) else { return defaultScale }
        return scale
    }

    static func projection(_ physical: simd_float4x4, scale: Float = configuredScale) -> simd_float4x4? {
        guard scale.isFinite, allowedScales.contains(scale),
              let converted = GameWorldProjection.nativePinhole(physical) else { return nil }
        var source = converted.matrix
        if scale == 1 { return source }
        // Scale complete clip X/Y rows, including asymmetric offsets. This
        // preserves the original frustum center while placing its boundaries
        // inside the source image by the requested NDC margin. Depth and eye
        // origins are unchanged. It trades source angular resolution for
        // coverage at the same texture size; it cannot reveal a whole unseen
        // hemisphere or repair translation disocclusions.
        for column in 0..<4 {
            source[column][0] /= scale
            source[column][1] /= scale
        }
        return source
    }
}

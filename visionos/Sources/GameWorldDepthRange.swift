import Foundation

/// Keep the finite compositor/source range consistent with a qualified native
/// game lens. The old fixed1km range clipped a native10km world. Unknown/stale
/// native authority preserves the prior1km fallback; infinity is never admitted.
enum GameWorldDepthRange {
    static let fallbackFar: Float = 1_000
    static let maximumNativeFar: Float = 20_000
    // Forward-Z f32 regeneration rounds p22 near-1, then the guest flips it
    // back. At nominal20km and near>=.05 this can reconstruct up to2.5% farther.
    // This bounds representability only; the whole-clip matrix budget is unchanged.
    static let maximumReconstructedFar: Float = 20_512

    static func selectedFar(nativeFar: Float) -> Float {
        guard nativeFar.isFinite, (1...maximumNativeFar).contains(nativeFar) else { return fallbackFar }
        return max(fallbackFar, nativeFar)
    }
}

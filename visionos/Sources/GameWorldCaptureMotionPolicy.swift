import Foundation
import simd

/// Optional diagnostic gate: capture the actual moving view, rather than the
/// first still frame. It changes no game camera, rendering or tracking policy.
struct GameWorldCaptureMotionPolicy {
    let minimumDegrees: Double
    let minimumTranslation: Double

    init?(arguments: [String]) {
        func value(_ key: String, limit: Double) -> Double? {
            guard let index = arguments.firstIndex(of: key) else { return 0 }
            guard arguments.indices.contains(index + 1), let result = Double(arguments[index + 1]),
                  result.isFinite, (0...limit).contains(result) else { return nil }
            return result
        }
        guard let angle = value("--sv-world-debug-min-pose-angle", limit: 90),
              let translation = value("--sv-world-debug-min-pose-translation", limit: 2) else { return nil }
        minimumDegrees = angle
        minimumTranslation = translation
    }

    func allows(_ currentEyeFromSourceEye: simd_float4x4) -> Bool {
        guard (0..<4).allSatisfy({ c in (0..<4).allSatisfy { currentEyeFromSourceEye[c][$0].isFinite } }) else { return false }
        let rotation = simd_float3x3(columns: (SIMD3(currentEyeFromSourceEye[0].x, currentEyeFromSourceEye[0].y, currentEyeFromSourceEye[0].z),
            SIMD3(currentEyeFromSourceEye[1].x, currentEyeFromSourceEye[1].y, currentEyeFromSourceEye[1].z),
            SIMD3(currentEyeFromSourceEye[2].x, currentEyeFromSourceEye[2].y, currentEyeFromSourceEye[2].z)))
        guard abs(rotation.determinant - 1) < 0.002,
              abs(currentEyeFromSourceEye[0].w) < 1e-5, abs(currentEyeFromSourceEye[1].w) < 1e-5,
              abs(currentEyeFromSourceEye[2].w) < 1e-5, abs(currentEyeFromSourceEye[3].w - 1) < 1e-5 else { return false }
        if minimumDegrees == 0 && minimumTranslation == 0 { return true }
        let q = simd_normalize(SIMD4<Double>(simd_quatf(rotation).vector))
        let angle = 2 * acos(min(1, abs(q.w))) * 180 / .pi
        let offset = currentEyeFromSourceEye[3]
        let translation = simd_length(SIMD3<Double>(Double(offset.x), Double(offset.y), Double(offset.z)))
        return (minimumDegrees > 0 && angle >= minimumDegrees)
            || (minimumTranslation > 0 && translation >= minimumTranslation)
    }
}

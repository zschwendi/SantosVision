import Foundation
import simd

@main
struct CaptureMotionPolicyValidation {
    static func main() {
        let key = "--sv-world-debug-min-pose-angle"
        let shift = "--sv-world-debug-min-pose-translation"
        precondition(GameWorldCaptureMotionPolicy(arguments: [])!.allows(matrix_identity_float4x4))
        for args in [[key], [key,"nan"], [key,"inf"], [key,"-1"], [key,"91"], [shift,"2.1"]] {
            precondition(GameWorldCaptureMotionPolicy(arguments: args) == nil)
        }
        let policy = GameWorldCaptureMotionPolicy(arguments: [key,"8",shift,"0.1"])!
        precondition(!policy.allows(matrix_identity_float4x4))
        for axis in [SIMD3<Float>(1,0,0), SIMD3<Float>(0,1,0), SIMD3<Float>(0,0,1)] {
            for degrees: Float in [-45,-9,-7,0,7,9,45] {
                let transform = simd_float4x4(simd_quatf(angle: degrees * .pi / 180, axis: axis))
                precondition(policy.allows(transform) == (abs(degrees) >= 8))
            }
        }
        for distance: Float in [-0.2,-0.09,0,0.09,0.2] {
            var transform = matrix_identity_float4x4; transform[3].x = distance
            precondition(policy.allows(transform) == (abs(distance) >= 0.1))
        }
        var invalid = matrix_identity_float4x4; invalid[3].x = .nan
        precondition(!policy.allows(invalid))
        invalid = matrix_identity_float4x4; invalid[0].x = 2
        precondition(!policy.allows(invalid))
        print("PASS: 36 diagnostic motion-gate checks; no camera or rendering changes")
    }
}

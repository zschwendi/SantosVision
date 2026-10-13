import simd

@main
struct AimProjectionValidation {
    static func main() {
        let origin = SIMD3<Double>(100_000, -80_000, 15)
        func camera(_ eyeX: Double) -> simd_double4x4 {
            simd_double4x4(columns: (SIMD4(1, 0, 0, 0), SIMD4(0, 0, 1, 0),
                                     SIMD4(0, -1, 0, 0), SIMD4(origin.x + eyeX, origin.y, origin.z, 1)))
        }
        func projection(_ offset: Float) -> simd_float4x4 {
            simd_float4x4(columns: (SIMD4(0.731, 0, 0, 0), SIMD4(0, 0.911, 0, 0),
                                    SIMD4(offset, -0.087, 0.0001, -1), SIMD4(0, 0, 0.1, 0)))
        }
        let left = GameWorldAimProjection.clip(origin: origin, direction: SIMD3(0, 1, 0),
                          gameFromCurrentEye: camera(-0.034), projection: projection(-0.268))!
        let right = GameWorldAimProjection.clip(origin: origin, direction: SIMD3(0, 1, 0),
                           gameFromCurrentEye: camera(0.034), projection: projection(0.268))!
        let expected = Float(0.268 + 0.731 * 0.034 / 40)
        precondition(abs(left.x / left.w - expected) < 1e-6)
        precondition(abs(right.x / right.w + expected) < 1e-6)
        precondition(abs(left.y / left.w - 0.087) < 1e-6)
        precondition(GameWorldAimProjection.clip(origin: origin, direction: SIMD3(0, -1, 0),
                          gameFromCurrentEye: camera(0), projection: projection(0)) == nil)
        precondition(GameWorldAimProjection.clip(origin: origin, direction: SIMD3(.nan, 1, 0),
                          gameFromCurrentEye: camera(0), projection: projection(0)) == nil)
        precondition(GameWorldAimProjection.clip(origin: origin, direction: SIMD3(0, 1, 0),
                          gameFromCurrentEye: simd_double4x4(0), projection: projection(0)) == nil)
        print("PASS: one original gameplay ray projects into both asymmetric eyes with IPD, large game origins, behind-camera and malformed-ray rejection")
    }
}

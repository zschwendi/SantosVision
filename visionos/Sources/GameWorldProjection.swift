import simd

/// GTA's camera shaders store an asymmetric pinhole projection as eleven
/// scalars. Physical compositor projections also contain calibration residuals
/// that this representation cannot encode. Bound the resulting ray error before
/// converting; the compositor target keeps its full original projection.
enum GameWorldProjection {
    // Whole-frustum error budget: at most 0.125 display pixels at 4096 pixels
    // across. This is a bound on the conversion, not raw matrix coefficients.
    static let maximumNDCError: Double = 1.0 / 16_384.0
    struct Conversion {
        let matrix: simd_float4x4
        let maximumRayError: Float
        let maximumDepthError: Float
    }

    static func nativePinhole(_ original: simd_float4x4) -> Conversion? {
        let values = (0..<4).flatMap { column in (0..<4).map { original[column][$0] } }
        guard values.allSatisfy(\.isFinite), original[0][0] > 0, original[1][1] > 0,
              abs(original[2][3] + 1) <= 1e-4 else { return nil }
        let scale = -original[2][3]
        var projection = original * (1 / scale)
        // Keep focal lengths, asymmetric eye offsets and the finite depth row.
        // Never silently replace a materially skewed or oblique projection.
        for index in [1, 2, 3, 4, 6, 7, 12, 13, 15] {
            let column = index / 4, row = index % 4
            projection[column][row] = 0
        }
        projection[2][3] = -1
        let near = projection[3][2] / (projection[2][2] + 1)
        let far = projection[3][2] / projection[2][2]
        guard near.isFinite, far.isFinite, near > 0, far > near, far <= 1002 else { return nil }

        let inverse = original.inverse
        guard (0..<4).allSatisfy({ c in (0..<4).allSatisfy { inverse[c][$0].isFinite } }) else { return nil }
        var rayError: Float = 0
        var depthError: Float = 0
        // Check the physical frustum's boundaries and center at near, middle,
        // and far depth. The analytic check below also bounds every point
        // between these samples, including the calibrated physical edges.
        for z: Float in [0, 0.5, 1] {
            for y: Float in [-1, 0, 1] {
                for x: Float in [-1, 0, 1] {
                    let eye = inverse * SIMD4<Float>(x, y, z, 1)
                    let clip = projection * eye
                    guard clip.w.isFinite, abs(clip.w) > 1e-8 else { return nil }
                    let ndc = SIMD3<Float>(clip.x, clip.y, clip.z) / clip.w
                    guard ndc.x.isFinite, ndc.y.isFinite, ndc.z.isFinite else { return nil }
                    rayError = max(rayError, abs(ndc.x - x), abs(ndc.y - y))
                    depthError = max(depthError, abs(ndc.z - z))
                }
            }
        }
        guard rayError <= Float(maximumNDCError), depthError <= Float(maximumNDCError) else { return nil }
        // Certify the entire clip box, including points between the samples.
        // After normalizing M's constant W term, its denominator is at least
        // 1 - sum(abs(W.xyz)). The numerator below bounds each NDC error for
        // x/y in [-1, 1] and reverse depth z in [0, 1].
        func doubleMatrix(_ matrix: simd_float4x4) -> simd_double4x4 {
            simd_double4x4(columns: (SIMD4<Double>(matrix.columns.0), SIMD4<Double>(matrix.columns.1),
                                     SIMD4<Double>(matrix.columns.2), SIMD4<Double>(matrix.columns.3)))
        }
        var mapping = doubleMatrix(projection) * doubleMatrix(original).inverse
        guard mapping[3][3].isFinite, mapping[3][3] > 0 else { return nil }
        mapping *= 1 / mapping[3][3]
        let denominatorError = (0..<3).reduce(0.0) { $0 + abs(mapping[$1][3]) }
        guard denominatorError.isFinite, denominatorError < 1 else { return nil }
        var bounds = SIMD3<Double>.zero
        for row in 0..<3 {
            let numeratorError = (0..<4).reduce(0.0) { sum, column in
                sum + abs(mapping[column][row] - (column == row ? 1 : 0))
            }
            bounds[row] = (numeratorError + denominatorError) / (1 - denominatorError)
        }
        guard bounds.x.isFinite, bounds.y.isFinite, bounds.z.isFinite,
              max(bounds.x, bounds.y) <= maximumNDCError, bounds.z <= maximumNDCError else { return nil }
        rayError = max(rayError, Float(max(bounds.x, bounds.y)))
        depthError = max(depthError, Float(bounds.z))
        return Conversion(matrix: projection, maximumRayError: rayError, maximumDepthError: depthError)
    }
}

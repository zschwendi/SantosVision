import Foundation
import simd

@main
struct ProjectionCalibrationValidation {
    static func main() throws {
        let input = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        precondition(input.count == 288)
        var output = input
        for eye in 0..<2 {
            let offset = 96 + eye * 128
            let values = (0..<16).map { index in
                Float(bitPattern: input.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset + index * 4, as: UInt32.self) })
            }
            let raw = simd_float4x4(columns: (
                SIMD4(values[0], values[1], values[2], values[3]),
                SIMD4(values[4], values[5], values[6], values[7]),
                SIMD4(values[8], values[9], values[10], values[11]),
                SIMD4(values[12], values[13], values[14], values[15])))
            let conversion = GameWorldProjection.nativePinhole(raw)!
            let canonical = conversion.matrix
            precondition(canonical[2][0] != 0 && canonical[2][1] != 0, "Stereo asymmetry must survive")
            // Independently sample a denser frustum lattice than the production
            // admission check, including points between its checked depths.
            func doubleMatrix(_ matrix: simd_float4x4) -> simd_double4x4 {
                simd_double4x4(columns: (SIMD4<Double>(matrix.columns.0), SIMD4<Double>(matrix.columns.1),
                                         SIMD4<Double>(matrix.columns.2), SIMD4<Double>(matrix.columns.3)))
            }
            let inverse = doubleMatrix(raw).inverse
            let candidate = doubleMatrix(canonical)
            var maximumRayError = 0.0, maximumDepthError = 0.0
            for zi in 0...10 {
                for yi in 0...40 {
                    for xi in 0...40 {
                        let point = SIMD4<Double>(Double(xi) / 20 - 1, Double(yi) / 20 - 1, Double(zi) / 10, 1)
                        let clip = candidate * (inverse * point)
                        let ndc = clip / clip.w
                        maximumRayError = max(maximumRayError, abs(ndc.x - point.x), abs(ndc.y - point.y))
                        maximumDepthError = max(maximumDepthError, abs(ndc.z - point.z))
                    }
                }
            }
            precondition(maximumRayError < 2e-5 && maximumDepthError < 2e-5)
            var oblique = raw; oblique[0][2] = 0.01
            precondition(GameWorldProjection.nativePinhole(oblique) == nil)
            var skew = raw; skew[1][0] = 0.01
            precondition(GameWorldProjection.nativePinhole(skew) == nil)
            var infinite = raw; infinite[2][2] = 0
            precondition(GameWorldProjection.nativePinhole(infinite) == nil)
            var nonfinite = raw; nonfinite[0][0] = .nan
            precondition(GameWorldProjection.nativePinhole(nonfinite) == nil)
            precondition(GameWorldProjection.nativePinhole(canonical)!.matrix == canonical)
            var matrix = canonical
            output.replaceSubrange(offset..<(offset + 64), with: withUnsafeBytes(of: &matrix) { Data($0) })
            print("eye\(eye): 18,491 physical rays/depths checked; maximum NDC ray error=\(maximumRayError), depth error=\(maximumDepthError)")
        }
        try output.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
        print("PASS: physical eye conversion, preserved stereo offsets, malformed projection rejection, idempotence")
    }
}

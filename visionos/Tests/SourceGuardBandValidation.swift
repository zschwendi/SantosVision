import Foundation
import simd

@main
struct SourceGuardBandValidation {
    static func matrix(_ values: [Double]) -> simd_float4x4 {
        precondition(values.count == 16)
        return simd_float4x4(columns: (
            SIMD4(values[0...3].map(Float.init)), SIMD4(values[4...7].map(Float.init)),
            SIMD4(values[8...11].map(Float.init)), SIMD4(values[12...15].map(Float.init))))
    }

    static func main() throws {
        let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        let report = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let physical = matrix(report["current_clip_from_eye"] as! [Double])
        var otherAsymmetricEye = physical
        otherAsymmetricEye[2][0] = -physical[2][0]
        var checked = 0
        for original in [physical, otherAsymmetricEye] {
            let conversion = GameWorldProjection.nativePinhole(original)!
            let canonical = conversion.matrix
            precondition(GameWorldSourceGuardBand.projection(original, scale: 1)! == canonical)
            let inverse = original.inverse
            for scale: Float in [1.05, 1.1, 1.2, 1.3, 1.35] {
                let source = GameWorldSourceGuardBand.projection(original, scale: scale)!
                precondition(source[2][2] == canonical[2][2] && source[3][2] == canonical[3][2])
                for zi in 0...10 {
                    for yi in 0...40 {
                        for xi in 0...40 {
                            let ndc = SIMD4<Float>(Float(xi) / 20 - 1, Float(yi) / 20 - 1, Float(zi) / 10, 1)
                            let eye = inverse * ndc
                            let expandedClip = source * eye
                            let expanded = expandedClip / expandedClip.w
                            let xyBudget = conversion.maximumRayError / scale + 2e-6
                            let depthBudget = conversion.maximumDepthError + 2e-6
                            precondition(abs(expanded.x - ndc.x / scale) <= xyBudget)
                            precondition(abs(expanded.y - ndc.y / scale) <= xyBudget)
                            precondition(abs(expanded.z - ndc.z) <= depthBudget)
                            precondition(abs(expanded.x) <= 1 / scale + xyBudget && abs(expanded.y) <= 1 / scale + xyBudget)
                            checked += 1
                        }
                    }
                }
            }
        }
        for value in ["", "nan", "inf", "0.9", "1.36", "garbage"] {
            precondition(GameWorldSourceGuardBand.resolveScale(["MUGUET_XR_SOURCE_GUARD_BAND": value]) == 1.3)
        }
        precondition(GameWorldSourceGuardBand.resolveScale([:]) == 1.3)
        precondition(GameWorldSourceGuardBand.resolveScale(["MUGUET_XR_SOURCE_GUARD_BAND": "1"]) == 1)
        precondition(GameWorldSourceGuardBand.resolveScale(["MUGUET_XR_SOURCE_GUARD_BAND": "1.1"]) == 1.1)
        precondition(GameWorldSourceGuardBand.resolveScale(["MUGUET_XR_SOURCE_GUARD_BAND": "1.35"]) == 1.35)
        for scale: Float in [.nan, .infinity, 0.9, 1.36] {
            precondition(GameWorldSourceGuardBand.projection(physical, scale: scale) == nil)
        }
        var oblique = physical; oblique[0][2] = 0.01
        precondition(GameWorldSourceGuardBand.projection(oblique, scale: 1.1) == nil)
        // These are the maximum angular budgets from the registered normal
        // gameplay and initial-turn captures. Containment does not synthesize
        // missing source color/depth or establish physical latency.
        for required: Float in [1.190764, 1.275312] {
            precondition(required < GameWorldSourceGuardBand.defaultScale)
        }
        print("PASS: \(checked) source coverage/depth rays, default 1.30 margin, explicit identity override, asymmetric frusta, invalid configuration and oblique rejection")
        print("Physical capture supplies one eye; the second case mirrors its horizontal asymmetry. No device or latency claim.")
    }
}

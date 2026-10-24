import Foundation
import simd

@main
struct SourceCoverageValidation {
    static var checks = 0
    static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    static func main() throws {
        let p = simd_float4x4(columns: (SIMD4(1.1, 0, 0, 0), SIMD4(0, 1.2, 0, 0),
            SIMD4(-0.075, -0.025, 0.1 / 999.9, -1), SIMD4(0, 0, 100 / 999.9, 0)))
        // Independent angular samples throughout every spherical cap. Compare
        // actual rotated rays against the analytic, deliberately conservative
        // projective bound, without clamping it to the production scale cap.
        let recorded = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))) as! [String: Any]
        let values = (recorded["current_clip_from_eye"] as! [NSNumber]).map(\.floatValue)
        let actual = simd_float4x4(columns: (SIMD4(values[0],values[1],values[2],values[3]), SIMD4(values[4],values[5],values[6],values[7]), SIMD4(values[8],values[9],values[10],values[11]), SIMD4(values[12],values[13],values[14],values[15])))
        var other = actual; other[2][0] = -actual[2][0]
        for physical in [p, actual, other] {
        for zoom: Float in [1, 2, 5, 9] {
            var optical = GameWorldProjection.nativePinhole(physical)!.matrix; optical[0][0] *= zoom; optical[1][1] *= zoom
            for degrees in [0.0, 2, 5, 10, 15, 25] {
                let angle = degrees * .pi / 180
                let bound = GameWorldSourceCoverage.requiredScale(physical, magnification: zoom, angle: angle)
                for x: Float in [-1, -0.5, 0, 0.5, 1] { for y: Float in [-1, -0.5, 0, 0.5, 1] {
                    let h = optical.inverse * SIMD4(x, y, 0.5, 1)
                    let ray = simd_normalize(SIMD3(h.x, h.y, h.z))
                    let tangent = simd_normalize(simd_cross(ray, SIMD3<Float>(0, 1, 0)))
                    let bitangent = simd_cross(ray, tangent)
                    for step in 0..<48 {
                        let azimuth = Float(step) * .pi / 24
                        for fraction: Float in [0, 0.5, 1] {
                            let theta = Float(angle) * fraction
                            let direction = cos(theta) * ray + sin(theta) * (cos(azimuth) * tangent + sin(azimuth) * bitangent)
                            let clip = optical * SIMD4(direction, 0)
                            if clip.w > 0 {
                                check(Double(max(abs(clip.x), abs(clip.y)) / clip.w) <= bound + 2e-5,
                                      "angular cone escaped the requested scale bound")
                            } else { check(!bound.isFinite, "behind-eye cone must be explicit saturation") }
                        }
                    }
                }}
            }
        }
        }
        for physical in [actual, other] {
            check(GameWorldSourceCoverage.requiredScale(physical, magnification: 1, angle: 0) < 1.00001, "actual calibrated asymmetry must cancel at rest")
        }
        var state = GameWorldSourceCoverage()
        let initial = state.sample(timestamp: 1, pose: matrix_identity_float4x4, projections: [actual, other], magnification: 1)
        check(initial == 1.3, "stationary view keeps previous source sharpness")
        let turn = simd_float4x4(simd_quatf(angle: .pi / 30, axis: SIMD3(0, 1, 0)))
        let widened = state.sample(timestamp: 1.02, pose: turn, projections: [actual, other], magnification: 1)
        check(widened > initial, "fast real turn must widen the actual game frustum")
        check(widened <= 3, "coverage memory/raster bound")
        check(state.sample(timestamp: 1.01, pose: turn, projections: [actual, other], magnification: 1) == widened,
              "out-of-order pose cannot reset motion history")
        for i in 2...400 { _ = state.sample(timestamp: 1 + Double(i) * 0.02, pose: turn, projections: [actual, other], magnification: 1) }
        check(state.scale == 1.3, "source sharpness recovers after stopping")
        state.observeSourceAge(.nan); state.observeSourceAge(-1); state.observeSourceAge(100)
        check(state.sample(timestamp: 10, pose: turn, projections: [actual, other], magnification: 1) == 1.3,
              "invalid source age cannot force persistent blur")
        let normal = GameWorldSourceCoverage.requiredScale(p, magnification: 1, angle: 0.05)
        let scoped = GameWorldSourceCoverage.requiredScale(p, magnification: 5, angle: 0.05)
        check(scoped > normal, "narrow sniper frustum needs a larger proportional margin")
        check(!GameWorldSourceCoverage.requiredScale(p, magnification: 1, angle: .pi).isFinite,
              "a cone containing behind-eye directions cannot claim finite coverage")
        var measuredPose = turn; measuredPose[0][0] += 0.000001
        var still = GameWorldSourceCoverage()
        for i in 0...1000 {
            _ = still.sample(timestamp: 20 + Double(i) / 90, pose: measuredPose,
                             projections: [actual, other], magnification: 1)
        }
        check(still.scale == 1.3, "non-unit f32 quaternion roundoff must not invent stationary motion")
        print("PASS: \(checks) independent angular containment and adaptive coverage lifecycle checks. Geometry only; no physical latency/performance claim.")
    }
}

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
        state.observeSourceAges([.nan, -1, 100], timestamp: 10)
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
        validateAgeHistory(projection: p)
        print("PASS: \(checks) independent angular containment, adaptive coverage lifecycle, and source-age temporal consistency checks. Geometry only; no physical latency/performance claim.")
    }

    static func validateAgeHistory(projection: simd_float4x4) {
        // Query the actual coverage result after the same pose interval. Keep
        // the pose samples fixed so these cases isolate source-age history.
        func initial() -> GameWorldSourceCoverage {
            var state = GameWorldSourceCoverage()
            _ = state.sample(timestamp: 1, pose: matrix_identity_float4x4,
                             projections: [projection, projection], magnification: 1)
            return state
        }
        func result(_ state: inout GameWorldSourceCoverage, after interval: Double = 0.2, speed: Double = 0.7) -> Float {
            let pose = simd_float4x4(simd_quatf(angle: Float(interval * speed), axis: SIMD3(0, 1, 0)))
            return state.sample(timestamp: 1 + interval, pose: pose,
                                projections: [projection, projection], magnification: 1)
        }
        var oldestOnly = initial()
        oldestOnly.observeSourceAges([0.24], timestamp: 1)
        let oldestScale = result(&oldestOnly)
        for ages in [[0.24, 0.01], [0.01, 0.24], [0.24, 0.01, 0.01], [0.01, 0.24, 0.24]] {
            var state = initial()
            state.observeSourceAges(ages, timestamp: 1)
            check(result(&state) == oldestScale, "eye order/count cannot weaken oldest-eye coverage or change the pixel grid")
        }
        var repeated = initial()
        repeated.observeSourceAges([0.24], timestamp: 1)
        for _ in 0..<120 { repeated.observeSourceAges([0.01, 0.01], timestamp: 1) }
        check(result(&repeated) == oldestScale, "duplicate display timestamps cannot decay source-age coverage")
        var split = initial()
        split.observeSourceAges([0.01], timestamp: 1)
        split.observeSourceAges([0.24], timestamp: 1)
        check(result(&split) == oldestScale, "same-time observations may conservatively raise the oldest-eye peak")

        // Derive the expected elapsed-time decay independently from the old
        // one-observation-per-60-Hz factor. A fixed pose query must yield the
        // same source projection at every age-observation cadence.
        let expectedHorizon = 0.3 * pow(0.98, 60 * 0.2)
        let expectedScale = Float(GameWorldSourceCoverage.requiredScale(projection, magnification: 1,
                                                                       angle: 0.7 * expectedHorizon))
        for frequency in [30, 60, 90, 120] {
            var state = initial()
            state.observeSourceAges([0.27], timestamp: 1)
            for i in 1...(frequency / 5) {
                state.observeSourceAges([0.1, 0.1], timestamp: 1 + Double(i) / Double(frequency))
            }
            check(abs(result(&state) - expectedScale) < 0.00001,
                  "elapsed source-age decay must be independent of refresh cadence")
        }
        var reference = initial()
        reference.observeSourceAges([0.27], timestamp: 1)
        reference.observeSourceAges([0.1, 0.1], timestamp: 1.1)
        var invalid = reference
        invalid.observeSourceAges([0.5], timestamp: 1.09)
        invalid.observeSourceAges([0.5], timestamp: .nan)
        invalid.observeSourceAges([0.5], timestamp: .infinity)
        invalid.observeSourceAges([.nan, -.infinity, -1, 1.001], timestamp: 1.15)
        invalid.observeSourceAges([], timestamp: 1.18)
        reference.observeSourceAges([0.1], timestamp: 1.2)
        invalid.observeSourceAges([0.1], timestamp: 1.2)
        check(result(&invalid) == result(&reference), "stale/invalid observations cannot alter the peak or its clock")

        let cappedExpected = Float(GameWorldSourceCoverage.requiredScale(projection, magnification: 1, angle: 0.7 * 0.3))
        for age in [0.5, Double(0.5).nextUp, 1.0] {
            var capped = initial()
            capped.observeSourceAges([age, 0.01], timestamp: 1)
            check(abs(result(&capped) - cappedExpected) < 0.00001,
                  "every admitted stalled source age must raise the horizon to its 300 ms cap")
        }
        var overLimit = initial()
        overLimit.observeSourceAges([Double(1).nextUp], timestamp: 1)
        var unobserved = initial()
        check(result(&overLimit) == result(&unobserved), "ages beyond native lease admission cannot change coverage")
        var floored = initial()
        floored.observeSourceAges([0.0], timestamp: 0)
        floored.observeSourceAges([0.0], timestamp: 1)
        let floorExpected = Float(GameWorldSourceCoverage.requiredScale(projection, magnification: 1, angle: 1.5 * 0.1))
        check(abs(result(&floored, speed: 1.5) - floorExpected) < 0.00001, "source-age horizon retains its 100 ms floor")

        var contraction = initial()
        let fastTurn = simd_float4x4(simd_quatf(angle: 0.3, axis: SIMD3(0, 1, 0)))
        let high = contraction.sample(timestamp: 1.02, pose: fastTurn, projections: [projection], magnification: 1)
        check(high == 3, "large motion still raises coverage immediately to the existing cap")
        // The long pose gap clears motion; the source-scale reduction itself
        // must remain bounded by the pre-existing 0.75 units per second.
        let lower = contraction.sample(timestamp: 1.32, pose: fastTurn, projections: [projection], magnification: 1)
        check(abs(lower - (high - 0.3 * 0.75)) < 0.000001, "source scale still contracts at exactly 0.75 per second")
    }
}

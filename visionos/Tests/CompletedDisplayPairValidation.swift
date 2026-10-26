import Foundation
import simd

@main struct Validation {
    static func main() throws {
        var checks = 0
        func check(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message)
            checks += 1
        }
        var physical = matrix_identity_float4x4
        physical[0][0] = 0.75; physical[1][1] = 0.9; physical[2][3] = -1
        var source = physical
        source[0][0] /= 3; source[1][1] /= 3
        let density = GameWorldSamplingDensity.measure(eye: 0, displayProjection: physical,
            displayPixels: SIMD2(1888, 1792), sourceProjection: source, sourcePixels: SIMD2(2560, 1440))!
        func eye(_ index: Int, lease: UInt64? = nil, path: String = "finite_sky_ordered_corners") -> GameWorldCompletedPair.Eye {
            GameWorldCompletedPair.Eye(displayEye: index, sourceEye: index, lease: lease ?? UInt64(index + 1),
                game: UInt64(100 + index), request: UInt64(200 + index), head: UInt64(300 + index),
                sourcePresentationTime: 10_000 + Double(index), retainedPoseTime: 700 + Double(index),
                requestedGuard: index == 0 ? 3 : nil, guardProvenance: index == 0 ? "matched" : "unavailable",
                inferredFocalGuard: GameWorldCompletedPair.focalRatio(display: physical, source: source),
                sourceProjection: GameWorldCompletedPair.bits(source), sourceGameFromEye: GameWorldCompletedPair.bits(source),
                sourceGameFromStage: GameWorldCompletedPair.bits(source), displayProjection: GameWorldCompletedPair.bits(physical),
                displayHeadFromEye: GameWorldCompletedPair.bits(physical), displayWorldHeadFromEye: GameWorldCompletedPair.bits(physical),
                currentEyeFromSourceEye: GameWorldCompletedPair.bits(physical), viewport: [0, 0, 1888, 1792], density: density,
                path: path, orderedPreparation: index == 0 ? "completed_source_cache_hit" : "fresh_preparation")
        }
        func pair(_ frame: UInt64, eyes: [GameWorldCompletedPair.Eye]? = nil) -> GameWorldCompletedPair {
            GameWorldCompletedPair(compositorFrame: frame, predictedDisplayTime: 500, displayRequest: 900, displayHead: 901,
                displayStageFromHead: GameWorldCompletedPair.bits(physical), pairAcquiredAtomically: false, eyes: eyes ?? [eye(0), eye(1)])
        }
        func rows(_ telemetry: GameWorldCompletedPairTelemetry) -> [[String: Any]] { telemetry.reportValue["pairs"] as! [[String: Any]] }
        let disabled = GameWorldCompletedPairTelemetry(environment: [:])
        check(!disabled.enabled && !disabled.shouldSample(now: 1), "default off")
        disabled.complete(pair(1), successful: true, acknowledged: true, at: 1)
        check(rows(disabled).isEmpty, "disabled collector retains nothing")
        let telemetry = GameWorldCompletedPairTelemetry(environment: ["MUGUET_COMPOSITOR_COMPLETED_PAIR_TELEMETRY": "1"])
        check(!telemetry.shouldSample(now: .nan), "reject invalid sampling clock")
        check(telemetry.shouldSample(now: 1), "first reservation")
        check(!telemetry.shouldSample(now: 1.999), "one second rate bound")
        check(telemetry.shouldSample(now: 2), "next interval")
        check(!telemetry.shouldSample(now: 1), "clock regression does not accelerate samples")
        telemetry.discardUnqualifiedSample()
        check(telemetry.reportValue["reserved_samples"] as? UInt64 == 2
            && telemetry.reportValue["unqualified_reserved_samples"] as? UInt64 == 1, "incomplete reserved samples counted")
        telemetry.complete(pair(1), successful: false, acknowledged: false, at: 5)
        check(rows(telemetry).isEmpty, "GPU error does not publish density")
        check(telemetry.reportValue["failed_sampled_commands"] as? UInt64 == 1, "sample failure counted")
        for invalid in [pair(1, eyes: [eye(0)]), pair(1, eyes: [eye(0), eye(1, lease: 1)]),
                        pair(1, eyes: [eye(0), eye(1, path: "not_encoded")]), pair(1, eyes: [eye(1), eye(0)])] {
            check(!invalid.qualified, "partial, duplicate, unencoded or unordered eyes rejected")
            telemetry.complete(invalid, successful: true, acknowledged: true, at: 5)
        }
        check(rows(telemetry).isEmpty, "invalid candidates not published")
        telemetry.complete(pair(40), successful: true, acknowledged: false, at: 1_000)
        telemetry.complete(pair(39), successful: true, acknowledged: true, at: 1_001)
        let sampleRows = rows(telemetry)
        check(sampleRows.count == 2, "successful commands publish independently of native ack")
        check(sampleRows[0]["compositor_frame"] as? UInt64 == 40 && sampleRows[1]["compositor_frame"] as? UInt64 == 39,
              "callback order preserves actual submission IDs")
        let eyes = sampleRows[0]["eyes"] as! [[String: Any]]
        check(eyes[0]["game_frame"] as? UInt64 == 100 && eyes[1]["game_frame"] as? UInt64 == 101, "independent generations retained")
        check(eyes[0]["source_native_presentation_time_seconds"] as? Double == 10_000, "native timestamp unchanged")
        check(eyes[0]["source_retained_pose_timestamp_seconds"] as? Double == 700, "retained pose timestamp unchanged")
        check(sampleRows[0]["predicted_display_timestamp_seconds"] as? Double == 500, "prediction timestamp unchanged")
        check(sampleRows[0]["completion_observed_at_uptime_seconds"] as? Double == 1_000, "completion clock separate")
        check(eyes[0]["source_requested_guard_scale"] as? Float == 3 && eyes[1]["source_requested_guard_scale"] == nil,
              "missing requested guard is not replaced by inference")
        check(eyes[0]["ordered_preparation"] as? String == "completed_source_cache_hit"
            && eyes[1]["ordered_preparation"] as? String == "fresh_preparation", "per-eye cache outcomes retained")
        check(eyes[0]["source_projection_float_bits"] as? [UInt32] == GameWorldCompletedPair.bits(source), "exact source matrix bits")
        check(eyes[0]["source_projection_float_bits"] as? [UInt32] != eyes[0]["display_projection_float_bits"] as? [UInt32],
              "source and display matrix remain distinct")
        let ratio = GameWorldCompletedPair.focalRatio(display: physical, source: source)!
        check(abs(ratio.x - 3) < 1e-6 && abs(ratio.y - 3) < 1e-6, "known guard inference")
        var nonfinite = source; nonfinite[0][0] = .nan
        check(GameWorldCompletedPair.focalRatio(display: physical, source: nonfinite) == nil, "invalid inference excluded")
        check(GameWorldCompletedPair.bits(nonfinite)[0] == nonfinite[0][0].bitPattern, "nonfinite bits retained safely")
        for frame in 41..<80 { telemetry.complete(pair(UInt64(frame)), successful: true, acknowledged: true, at: Double(frame)) }
        check(rows(telemetry).count == 16 && rows(telemetry).first?["compositor_frame"] as? UInt64 == 64, "bounded newest16 eviction")
        check(telemetry.reportValue["successful_sampled_commands"] as? UInt64 == 41, "successful counter survives ring eviction")
        let data = try JSONSerialization.data(withJSONObject: telemetry.reportValue, options: [.sortedKeys])
        check(data.count < 150_000, "bounded payload under150KB including all exact matrices")
        check(JSONSerialization.isValidJSONObject(disabled.reportValue), "disabled legacy export JSON compatible")
        let legacy = GameWorldImageQualityTelemetry(fillRequested: false, fillAvailable: false)
        legacy.record(density: density)
        legacy.record(view: 0, outcome: .notRequested)
        let legacyReport = legacy.reportValue
        check(legacyReport["schema"] as? Int == 1, "legacy schema unchanged")
        check((legacyReport["sampling_density"] as? [[String: Any]])?.count == 1, "legacy density retained")
        check(legacyReport["depth_edge_fill"] is [String: Any], "legacy fill counters retained")
        check(legacyReport["sampling_density_stage"] is String && legacyReport["completed_display_pairs"] is [String: Any],
              "new additive qualification fields exported")
        let compositorClock = GameWorldCompletedPair.Clock.compositorPresentationEpoch.rawValue
        func age(_ display: Double, _ retained: Double?, displayClock: String? = nil,
                 retainedClock: String? = nil, qualified: Bool = true) -> Double? {
            GameWorldCompletedPair.sourcePoseAge(display: display, displayClock: displayClock ?? compositorClock,
                retained: retained, retainedClock: retainedClock ?? compositorClock, sourcePoseQualified: qualified)
        }
        check(age(10, 9) == 1, "same audited clock permits matched pose age")
        check(age(10, 11) == -1, "future source pose remains negative, never clamped")
        check(age(10, 10) == 0, "zero pose age retained")
        check(age(10, 9, displayClock: "unknown") == nil, "unknown display clock suppresses age")
        check(age(10, 9, retainedClock: "unknown") == nil, "unknown source clock suppresses age")
        check(age(10, 9, retainedClock: GameWorldCompletedPair.Clock.systemUptime.rawValue) == nil,
              "mismatched clock suppresses age")
        check(age(10, 9, displayClock: "unknown", retainedClock: "unknown") == nil, "matching unknown clocks insufficient")
        check(age(10, 9, qualified: false) == nil, "matched clocks cannot replace source pose verification")
        check(age(.nan, 9) == nil && age(10, .infinity) == nil && age(10, nil) == nil, "nonfinite/missing age inputs suppressed")
        check(age(.greatestFiniteMagnitude, -.greatestFiniteMagnitude) == nil, "nonfinite age difference suppressed")
        check(GameWorldCompletedPair.commandGPUInterval(start: 100, end: 100.25) == 0.25, "same-command GPU interval")
        for (start, end): (Double?, Double?) in [(nil, 1), (1, nil), (0, 1), (2, 1), (1, 1), (.nan, 2), (1, .infinity)] {
            check(GameWorldCompletedPair.commandGPUInterval(start: start, end: end) == nil, "invalid/missing GPU interval suppressed")
        }
        var clockEye0 = eye(0), clockEye1 = eye(1)
        clockEye0.retainedPoseClock = compositorClock; clockEye0.retainedPoseQualified = true
        clockEye1.retainedPoseClock = "unknown"; clockEye1.retainedPoseQualified = true
        var clockPair = pair(81, eyes: [clockEye0, clockEye1])
        check(clockPair.predictedDisplayClock == "unknown", "other/test pair constructors default to unknown clock")
        clockPair.predictedDisplayClock = compositorClock
        let clockTelemetry = GameWorldCompletedPairTelemetry(environment: ["MUGUET_COMPOSITOR_COMPLETED_PAIR_TELEMETRY": "1"])
        clockTelemetry.complete(clockPair, successful: true, acknowledged: true, at: 200, gpuStart: 100, gpuEnd: 100.25)
        let clockRow = rows(clockTelemetry)[0], clockEyes = clockRow["eyes"] as! [[String: Any]]
        check(clockEyes[0]["source_pose_age_at_predicted_display_seconds"] as? Double == -200, "export preserves qualified negative pose age")
        check(clockEyes[1]["source_pose_age_at_predicted_display_seconds"] == nil, "unknown retained clock omits derived field")
        check(clockRow["command_gpu_interval_seconds"] as? Double == 0.25, "completed pair retains its command interval")
        clockTelemetry.complete(clockPair, successful: true, acknowledged: true, at: 201, gpuStart: 0, gpuEnd: 1)
        check(rows(clockTelemetry)[1]["command_gpu_interval_seconds"] == nil, "unavailable command timing keeps record without interval")
        clockTelemetry.complete(clockPair, successful: false, acknowledged: false, at: 202, gpuStart: 100, gpuEnd: 101)
        check(rows(clockTelemetry).count == 2, "failed command cannot publish GPU interval or pose age")
        check(JSONSerialization.isValidJSONObject(clockTelemetry.reportValue), "tagged clock/GPU export JSON valid")
        let concurrent = GameWorldCompletedPairTelemetry(environment: ["MUGUET_COMPOSITOR_COMPLETED_PAIR_TELEMETRY": "1"])
        let immutable = pair(80)
        DispatchQueue.concurrentPerform(iterations: 64) { _ in
            concurrent.complete(immutable, successful: true, acknowledged: true, at: 200)
            _ = concurrent.reportValue
        }
        check(rows(concurrent).count == 16 && JSONSerialization.isValidJSONObject(concurrent.reportValue),
              "concurrent callbacks/report snapshots remain bounded and complete")
        print("PASS: \(checks) completed-pair checks; maximum retained fixture report \(data.count) bytes. CPU only; no Metal execution.")
    }
}

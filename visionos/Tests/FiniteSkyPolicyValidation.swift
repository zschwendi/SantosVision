import Foundation
import Dispatch

@main
enum FiniteSkyPolicyValidation {
    static func main() throws {
        func require(_ condition: @autoclosure () -> Bool, _ message: String) {
            precondition(condition(), message)
        }
        let key = "MUGUET_COMPOSITOR_FINITE_SKY_SUPPORT"
        let enable = "--sv-world-finite-sky-support"
        let disable = "--sv-world-disable-finite-sky-support"
        let cases: [([String: String], [String], Bool)] = [
            ([:], [], true), ([key: "0"], [], false), ([:], [disable], false),
            ([key: "1"], [], true), ([:], [enable], true),
            ([key: "0"], [enable], false), ([key: "1"], [disable], false),
            ([:], [disable, enable], false), ([:], [enable, disable], false),
            ([key: "invalid"], [], true)
        ]
        for (environment, arguments, expected) in cases {
            require(GameWorldFiniteSkyPolicy.requested(environment: environment, arguments: arguments) == expected,
                    "Normal default/explicit QA disable precedence failed")
        }

        let telemetry = GameWorldFiniteSkyTelemetry(requested: true, available: true, unavailableReason: nil)
        telemetry.record(view: 0, outcome: .encoded, source: .init(eye: 0, game: 41, request: 7, head: 9))
        telemetry.record(view: 1, outcome: .encodeUnavailable, source: .init(eye: 1, game: 42, request: 8, head: 10))
        var report = telemetry.reportValue
        var views = report["views"] as! [[String: Any]]
        require(views[0]["views_encoded"] as? UInt64 == 1 && views[0]["views_fell_back"] as? UInt64 == 0,
                "Encoded view was misreported")
        require(views[1]["views_encoded"] as? UInt64 == 0 && views[1]["views_fell_back"] as? UInt64 == 1,
                "Encoding failure was misreported as success")
        require((views[1]["last_source"] as? [String: Any])?["game_frame"] as? UInt64 == 42,
                "Source frame provenance was lost")
        telemetry.record(view: 0, outcome: .noQualifiedActualSource, source: nil)
        telemetry.record(view: -1, outcome: .encoded, source: nil)
        telemetry.record(view: 2, outcome: .encoded, source: nil)
        views = telemetry.reportValue["views"] as! [[String: Any]]
        require(views.count == 2 && views[0]["last_source"] == nil,
                "Invalid view grew telemetry or stale source provenance survived")
        require(views[0]["views_bypassed"] as? UInt64 == 1 && views[0]["views_encoded"] as? UInt64 == 1,
                "Missing source was counted as an encoded result")

        let unavailable = GameWorldFiniteSkyTelemetry(requested: true, available: false, unavailableReason: .pipelineUnavailable)
        unavailable.record(view: 0, outcome: .pipelineUnavailable, source: nil)
        report = unavailable.reportValue
        require(report["requested"] as? Bool == true && report["available"] as? Bool == false
            && report["unavailable_reason"] as? String == "pipeline_unavailable", "Request and availability were conflated")
        let disabled = GameWorldFiniteSkyTelemetry(requested: false, available: false, unavailableReason: .qaDisabled)
        disabled.record(view: 0, outcome: .qaDisabled, source: nil)
        require(disabled.reportValue["requested"] as? Bool == false, "QA-disabled report claims requested")

        let concurrent = GameWorldFiniteSkyTelemetry(requested: true, available: true, unavailableReason: nil)
        DispatchQueue.concurrentPerform(iterations: 1000) { index in
            concurrent.record(view: index % 2, outcome: index % 2 == 0 ? .encoded : .encodeUnavailable, source: nil)
        }
        views = concurrent.reportValue["views"] as! [[String: Any]]
        require(views[0]["views_encoded"] as? UInt64 == 500 && views[1]["views_fell_back"] as? UInt64 == 500,
                "Concurrent encoding observations lost outcomes")
        for _ in 0..<10_000 { concurrent.record(view: 1, outcome: .noQualifiedActualSource, source: nil) }
        let bytes = try JSONSerialization.data(withJSONObject: concurrent.reportValue, options: [.sortedKeys])
        require(bytes.count < 2048, "Compact telemetry grew an event history")
        require((concurrent.reportValue["scope"] as? String)?.contains("does not prove GPU completion") == true,
                "Encoding telemetry lost its GPU acceptance qualification")
        print("PASS \(cases.count) finite sky policy cases; bounded per-view and concurrent CPU telemetry")
    }
}

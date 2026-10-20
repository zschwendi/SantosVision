import Foundation

/// The physical build7 A/B improved the sky holes without eliminating them.
/// Use the same bounded support normally; explicit QA disable always wins.
enum GameWorldFiniteSkyPolicy {
    static func requested(environment: [String: String], arguments: [String]) -> Bool {
        environment["MUGUET_COMPOSITOR_FINITE_SKY_SUPPORT"] != "0"
            && !arguments.contains("--sv-world-disable-finite-sky-support")
    }
}

/// CPU encoding outcomes only. Fixed two-view storage and a fixed reason enum
/// prevent diagnostic history growth. GPU/pixel/display success is separate.
final class GameWorldFiniteSkyTelemetry: @unchecked Sendable {
    enum Outcome: String, CaseIterable, Sendable {
        case encoded
        case qaDisabled = "qa_disabled"
        case incompatibleConfiguration = "incompatible_path_flags"
        case pipelineUnavailable = "pipeline_unavailable"
        case sourceCopyActive = "source_anchor_copy"
        case singleSourceDiagnostic = "single_source_diagnostic"
        case noQualifiedActualSource = "no_qualified_actual_source"
        case encodeUnavailable = "input_resource_or_encoder_unavailable"

        var isFallback: Bool {
            switch self {
            case .incompatibleConfiguration, .pipelineUnavailable, .sourceCopyActive,
                 .singleSourceDiagnostic, .encodeUnavailable: true
            default: false
            }
        }
    }

    struct Source: Sendable {
        let eye: Int
        let game: UInt64
        let request: UInt64
        let head: UInt64
    }

    private static let outcomes = Outcome.allCases
    let requested: Bool
    private let available: Bool
    private let unavailableReason: Outcome?
    private let lock = NSLock()
    private struct View {
        var counts = [UInt64](repeating: 0, count: GameWorldFiniteSkyTelemetry.outcomes.count)
        var last: Outcome?
        var source: Source?
    }
    private var views = [View(), View()]

    init(requested: Bool, available: Bool, unavailableReason: Outcome?) {
        self.requested = requested
        self.available = available
        self.unavailableReason = unavailableReason
    }

    func record(view: Int, outcome: Outcome, source: Source?) {
        guard (0..<2).contains(view), let index = Self.outcomes.firstIndex(of: outcome) else { return }
        lock.lock()
        defer { lock.unlock() }
        if views[view].counts[index] != .max { views[view].counts[index] += 1 }
        views[view].last = outcome
        views[view].source = source
    }

    var reportValue: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        let rows: [[String: Any]] = views.enumerated().map { index, view in
            var encoded: UInt64 = 0, bypassed: UInt64 = 0, fallback: UInt64 = 0
            var reasons: [String: UInt64] = [:]
            for (reason, count) in zip(Self.outcomes, view.counts) where count > 0 {
                if reason == .encoded { encoded = count }
                else {
                    reasons[reason.rawValue] = count
                    if reason.isFallback { fallback = sum(fallback, count) }
                    else { bypassed = sum(bypassed, count) }
                }
            }
            var row: [String: Any] = ["view_index": index, "views_encoded": encoded,
                "views_bypassed": bypassed, "views_fell_back": fallback, "reasons": reasons]
            if let last = view.last { row["last_outcome"] = last.rawValue }
            if let source = view.source {
                row["last_source"] = ["eye": source.eye, "game_frame": source.game,
                                      "request_frame": source.request, "head_frame": source.head] as [String: Any]
            }
            return row
        }
        var result: [String: Any] = ["schema": 1, "requested": requested, "available": available,
            "scope": "Per-view CPU pass-encode outcomes; encoded does not prove GPU completion, repaired pixel coverage, display acceptance or elimination of holes.",
            "counter_scope": "since renderer construction", "observed_at_uptime_seconds": ProcessInfo.processInfo.systemUptime,
            "support_radius_fraction_of_far": 0.8, "views": rows]
        if let unavailableReason { result["unavailable_reason"] = unavailableReason.rawValue }
        return result
    }

    private func sum(_ a: UInt64, _ b: UInt64) -> UInt64 {
        let value = a.addingReportingOverflow(b)
        return value.overflow ? .max : value.partialValue
    }
}

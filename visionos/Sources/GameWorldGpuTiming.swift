import Foundation

/// Cold QA setting for a matched queue-pressure comparison. The ordinary
/// three-command limit is unchanged; this is not a frame-rate or quality knob.
enum GameWorldSubmissionBudget {
    static let defaultMaximum = 3

    static func maximum(environment: [String: String]) -> Int {
        guard let raw = environment["MUGUET_COMPOSITOR_MAX_IN_FLIGHT"],
              let value = Int(raw), (1...defaultMaximum).contains(value) else { return defaultMaximum }
        return value
    }
}

/// Measures only the existing compositor command buffer. Guest rendering has
/// its own queue and is deliberately excluded from this timing interval.
struct GameWorldGpuTimingSummary: Sendable {
    let windowIndex: UInt64
    let startUptime: Double
    let endUptime: Double
    let callbacks: UInt64
    let completedCommands: UInt64
    let errorCommands: UInt64
    let otherCommands: UInt64
    let qualifiedPresentations: UInt64
    let debugBlitCommands: UInt64
    let validSamples: UInt64
    let unavailableSamples: UInt64
    let invalidSamples: UInt64
    let sumMilliseconds: Double
    let minMilliseconds: Double?
    let maxMilliseconds: Double?

    var reportValue: [String: Any] {
        ["schema": 1,
         "scope": "elapsed GPU interval of the compositor command buffer: depth reprojection, optional debug blit, encodePresent. Guest/wgpu commands and system display are not encoded here; cross-queue GPU scheduling may overlap this interval. Not isolated busy time or reciprocal FPS.",
         "window_kind": "nonoverlapping completion-callback windows; counters reset after each report",
         "clock": "ProcessInfo.systemUptime",
         "window_index": windowIndex,
         "window_start_uptime_seconds": startUptime,
         "window_end_uptime_seconds": endUptime,
         "window_seconds": endUptime - startUptime,
         "command_callbacks": callbacks,
         "completed_command_buffers": completedCommands,
         "error_command_buffers": errorCommands,
         "other_command_buffers": otherCommands,
         "qualified_presentations": qualifiedPresentations,
         "debug_blit_command_buffers": debugBlitCommands,
         "valid_gpu_samples": validSamples,
         "unavailable_gpu_samples": unavailableSamples,
         "invalid_gpu_samples": invalidSamples,
         "gpu_sum_ms": validSamples > 0 ? sumMilliseconds as Any : NSNull(),
         "gpu_mean_ms": validSamples > 0 ? sumMilliseconds / Double(validSamples) as Any : NSNull(),
         "gpu_min_ms": minMilliseconds.map { $0 as Any } ?? NSNull(),
         "gpu_max_ms": maxMilliseconds.map { $0 as Any } ?? NSNull()]
    }
}

/// Fixed-size counters behind a short lock; no GPU work, waits, or polling.
final class GameWorldGpuTiming: @unchecked Sendable {
    private let lock = NSLock()
    private let interval: Double
    private let clock: @Sendable () -> Double
    private var windowIndex: UInt64 = 0
    private var startUptime: Double?
    private var callbacks: UInt64 = 0
    private var completedCommands: UInt64 = 0
    private var errorCommands: UInt64 = 0
    private var otherCommands: UInt64 = 0
    private var qualifiedPresentations: UInt64 = 0
    private var debugBlitCommands: UInt64 = 0
    private var validSamples: UInt64 = 0
    private var unavailableSamples: UInt64 = 0
    private var invalidSamples: UInt64 = 0
    private var sumMilliseconds: Double = 0
    private var minMilliseconds: Double?
    private var maxMilliseconds: Double?

    init(interval: Double = 10, clock: @escaping @Sendable () -> Double = { ProcessInfo.processInfo.systemUptime }) {
        precondition(interval.isFinite && interval > 0)
        self.interval = interval
        self.clock = clock
    }

    func record(completed: Bool, error: Bool, qualified: Bool,
                debugBlit: Bool, gpuStart: Double, gpuEnd: Double) -> GameWorldGpuTimingSummary? {
        lock.lock()
        defer { lock.unlock() }
        // Serialize the callback clock with the counters, so concurrent
        // completion handlers cannot drop samples by arriving out of order.
        let now = clock()
        guard now.isFinite, now >= 0 else { return nil }
        if startUptime == nil { startUptime = now }
        guard let startUptime, now >= startUptime else { return nil }
        callbacks &+= 1
        if completed { completedCommands &+= 1 }
        else if error { errorCommands &+= 1 }
        else { otherCommands &+= 1 }
        if debugBlit { debugBlitCommands &+= 1 }
        if completed && qualified {
            qualifiedPresentations &+= 1
            if !gpuStart.isFinite || !gpuEnd.isFinite || gpuStart < 0 || gpuEnd < 0 {
                invalidSamples &+= 1
            } else if gpuStart == 0 || gpuEnd == 0 {
                unavailableSamples &+= 1
            } else {
                let milliseconds = (gpuEnd - gpuStart) * 1_000
                if gpuEnd <= gpuStart || !milliseconds.isFinite {
                    invalidSamples &+= 1
                } else {
                    validSamples &+= 1
                    sumMilliseconds += milliseconds
                    minMilliseconds = min(minMilliseconds ?? milliseconds, milliseconds)
                    maxMilliseconds = max(maxMilliseconds ?? milliseconds, milliseconds)
                }
            }
        }
        guard now - startUptime >= interval else { return nil }
        windowIndex &+= 1
        let summary = GameWorldGpuTimingSummary(windowIndex: windowIndex,
            startUptime: startUptime, endUptime: now, callbacks: callbacks,
            completedCommands: completedCommands, errorCommands: errorCommands,
            otherCommands: otherCommands, qualifiedPresentations: qualifiedPresentations,
            debugBlitCommands: debugBlitCommands, validSamples: validSamples,
            unavailableSamples: unavailableSamples, invalidSamples: invalidSamples,
            sumMilliseconds: sumMilliseconds, minMilliseconds: minMilliseconds,
            maxMilliseconds: maxMilliseconds)
        clearCounters(startUptime: now)
        return summary
    }

    func reset() {
        lock.lock()
        defer { lock.unlock() }
        clearCounters(startUptime: nil)
    }

    private func clearCounters(startUptime: Double?) {
        self.startUptime = startUptime
        callbacks = 0; completedCommands = 0; errorCommands = 0; otherCommands = 0
        qualifiedPresentations = 0; debugBlitCommands = 0
        validSamples = 0; unavailableSamples = 0; invalidSamples = 0
        sumMilliseconds = 0; minMilliseconds = nil; maxMilliseconds = nil
    }
}

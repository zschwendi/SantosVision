import Foundation

/// One deliberate left-hand calibration requested from Settings. It may start
/// in the window (GTA paused) and waits for the next qualified immersive
/// first-person entry. The card succeeds only when the native side reports an
/// actual post-IK capture for this exact session and that capture is then
/// explicitly committed; a UI click is not a result. Every other exit closes
/// the session, which discards an uncommitted capture.
struct LeftWristCalibrationFlow: Equatable {
    /// Native snapshot from sv_xr_left_wrist_state_v1.
    struct Native: Equatable {
        var supported = false, eligible = false, nativeHandShown = false
        /// Fingerprint = GameController product category, vendor and fixed left
        /// hand. It distinguishes controller models, not physical units.
        var calibrationPresent = false, controllerFingerprintMatches = false
        var session: UInt64 = 0, resultSession: UInt64 = 0
        var phase: UInt32 = 0, reason: UInt32 = 0, failure: UInt32 = 0, samples: UInt32 = 0, requiredSamples: UInt32 = 0
        var translationMetres: Float = 0, rotationDegrees: Float = 0
    }
    enum Stage: Equatable { case idle, pending, opening, card(UInt64), capturing(UInt64), committing(UInt64), finished(UInt64, Double) }
    /// `close` always discards an uncommitted capture; only `commit` adopts one.
    enum Command: Equatable { case none, open, capture(UInt64), commit(UInt64), close(UInt64) }
    enum Exit: Equatable { case playerCancelled, leftImmersion, unsupported, expired }
    /// The short acknowledgement after an actual native capture.
    static let successDisplaySeconds = 1.5
    private(set) var stage = Stage.idle
    private(set) var lastOutcome = ""

    var pending: Bool { stage == .pending }
    var cardVisible: Bool {
        switch stage { case .card, .capturing, .committing, .finished: true; default: false }
    }
    var session: UInt64? {
        switch stage { case .card(let s), .capturing(let s), .committing(let s), .finished(let s, _): s; default: nil }
    }

    mutating func request() -> Command {
        guard stage == .idle else { return .none }
        stage = .pending
        lastOutcome = ""
        return .none
    }

    /// Any stage returns to idle; an open native session is always closed so
    /// position-only left tracking resumes.
    mutating func cancel(_ exit: Exit) -> Command {
        let open = session
        if stage != .idle {
            lastOutcome = switch exit {
            case .playerCancelled: "Calibration cancelled. Your previous left-hand setting is unchanged."
            case .leftImmersion: "Calibration stopped because immersive view closed. Your previous left-hand setting is unchanged."
            case .unsupported: "This build can't calibrate the left hand."
            case .expired: "Calibration timed out. Your previous left-hand setting is unchanged."
            }
        }
        stage = .idle
        return open.map { .close($0) } ?? .none
    }

    /// `ready` is the app's immersive gate: presented world frames, no GTA
    /// pause menu, no Start panel, no pending window return, app active.
    mutating func update(ready: Bool, native: Native, now: Double) -> Command {
        switch stage {
        case .idle, .opening, .committing: return .none
        case .pending:
            if !native.supported { return cancel(.unsupported) }
            guard ready, native.eligible else { return .none }
            stage = .opening
            return .open
        case .card(let s), .capturing(let s):
            if !ready { return cancel(.leftImmersion) }
            if native.session != s { return cancel(.expired) }
            if case .capturing = stage {
                if native.phase == 4, native.resultSession == s {
                    stage = .committing(s)
                    return .commit(s)
                }
                if native.phase != 3, native.failure != 0, native.resultSession == s { stage = .card(s) }
            }
            return .none
        case .finished(let s, let since):
            guard now - since >= Self.successDisplaySeconds || !ready else { return .none }
            stage = .idle
            // Already closed by the commit; closing a stale serial is a no-op.
            return .close(s)
        }
    }

    /// Native commit outcome. Only a committed capture is acknowledged.
    mutating func commitResult(_ committed: Bool, now: Double) -> Command {
        guard case .committing(let s) = stage else { return .none }
        if committed {
            stage = .finished(s, now)
            lastOutcome = "Left hand calibrated."
            return .none
        }
        stage = .idle
        lastOutcome = "Calibration was not completed. Your previous left-hand setting is unchanged."
        return .close(s)
    }

    mutating func opened(_ session: UInt64) {
        guard stage == .opening else { return }
        if session == 0 { stage = .idle; lastOutcome = "This build can't calibrate the left hand."; return }
        stage = .card(session)
    }

    mutating func confirm(native: Native) -> Command {
        guard case .card(let s) = stage, native.session == s, native.phase == 2 else { return .none }
        stage = .capturing(s)
        return .capture(s)
    }

    /// Native refused Confirm (not ready / controller missing): stay open.
    mutating func captureRefused() { if case .capturing(let s) = stage { stage = .card(s) } }

    static func canConfirm(_ stage: Stage, native: Native) -> Bool {
        if case .card(let s) = stage { return native.session == s && native.phase == 2 }
        return false
    }

    static func reasonText(_ reason: UInt32) -> String {
        switch reason {
        case 1: "Switch to first person and hold a gun."
        case 2: "Keep your left controller in view of the headset."
        case 3: "The game's left hand isn't available yet."
        case 4: "Showing the game's own left hand…"
        case 5: "The left Sense controller isn't recognised. Reconnect it, then try again."
        case 6: "Hold still…"
        case 7: "Couldn't get a steady reading. Hold still, then confirm again."
        case 8: "Put your left controller where the game's left hand is, then confirm again."
        case 9: "Tracking changed. Hold still, then confirm again."
        case 10: "Calibration timed out."
        case 11: "This build can't calibrate the left hand."
        case 12: "The character changed. Confirm again."
        case 13: "Tracking was unsteady. Confirm again."
        case 14: "No first-person gun frames arrived. Stay in first person holding the gun, then confirm again."
        case 15: "The left controller changed. Hold still, then confirm again."
        default: ""
        }
    }

    /// The single card line for the current native state.
    static func cardText(_ stage: Stage, native: Native) -> String {
        switch stage {
        case .finished, .committing: return "Left hand calibrated."
        case .capturing:
            return native.phase == 3 && (native.reason == 0 || native.reason == 6)
                ? "Hold still… \(native.samples)/\(max(native.requiredSamples, 1))"
                : reasonText(native.reason)
        case .card(let s):
            if native.failure != 0, native.resultSession == s { return reasonText(native.failure) }
            if native.phase == 2 {
                return "Hold your left controller exactly where the game's left hand is, at the same angle. Hold still, look at Confirm and pull the right trigger."
            }
            return reasonText(native.reason).isEmpty ? "Showing the game's own left hand…" : reasonText(native.reason)
        default: return ""
        }
    }

    func settingsText(native: Native) -> String {
        if stage == .pending {
            return "Close GTA's menu and enter immersive view. Switch to first person holding a gun; the calibration card opens by itself."
        }
        if !lastOutcome.isEmpty, lastOutcome != "Left hand calibrated." { return lastOutcome }
        if native.calibrationPresent {
            return native.controllerFingerprintMatches
                ? String(format: "Calibrated (%.1f cm, %.0f°): the game's left hand follows your controller's position and angle. It applies to any left controller of this model.",
                         native.translationMetres * 100, native.rotationDegrees)
                : "Calibrated for another left controller model. Calibrate again to use this one."
        }
        return "Not calibrated: the game's left hand follows your controller's position and keeps the game's wrist angle."
    }
}

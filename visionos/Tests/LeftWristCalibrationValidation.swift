import Foundation

@main struct LeftWristCalibrationValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message)
            checks += 1
        }
        typealias Flow = LeftWristCalibrationFlow
        var native = Flow.Native(supported: true, requiredSamples: 12)
        var flow = Flow()

        // Settings request from the window waits for the next qualified entry.
        require(flow.request() == .none && flow.pending && !flow.cardVisible, "Settings action only arms the request")
        require(flow.update(ready: false, native: native, now: 0) == .none, "No card while GTA's 2D pause/window owns play")
        require(flow.update(ready: true, native: native, now: 0) == .none, "No card before first person with a gun is observed")
        native.eligible = true
        require(flow.update(ready: false, native: native, now: 0) == .none, "Eligible native frames alone do not open a card outside immersion")
        require(flow.update(ready: true, native: native, now: 0) == .open && !flow.cardVisible, "Qualified immersive entry opens exactly one native session")
        require(flow.update(ready: true, native: native, now: 0) == .none, "Opening is not repeated while in flight")
        flow.opened(7)
        native.session = 7; native.phase = 1; native.reason = 4
        require(flow.cardVisible && Flow.cardText(flow.stage, native: native).contains("own left hand"), "Card shows GTA's own hand appearing")
        require(!Flow.canConfirm(flow.stage, native: native) && flow.confirm(native: native) == .none,
                "Confirm is unavailable until the native hand is actually shown")
        native.phase = 2; native.reason = 0; native.nativeHandShown = true
        require(Flow.canConfirm(flow.stage, native: native), "Native hand shown enables Confirm")
        require(flow.confirm(native: native) == .capture(7), "Confirm requests a native capture for this exact session")
        require(flow.confirm(native: native) == .none, "Confirm cannot be repeated during capture")
        native.phase = 3; native.samples = 4
        require(flow.update(ready: true, native: native, now: 1) == .none && Flow.cardText(flow.stage, native: native).contains("4/12"),
                "Capture progress is native sample progress")
        // A UI click is never success: only phase4 for this session finishes.
        native.phase = 4; native.resultSession = 6
        require(flow.update(ready: true, native: native, now: 1) == .none && !(flow.stage == .finished(7, 1)), "Another session's result is ignored")
        native.resultSession = 7
        require(flow.update(ready: true, native: native, now: 2) == .none && flow.stage == .finished(7, 2), "Actual native capture acknowledges")
        require(flow.cardVisible && Flow.cardText(flow.stage, native: native) == "Left hand calibrated.", "Brief acknowledgement")
        require(flow.update(ready: true, native: native, now: 3) == .none, "Acknowledgement stays briefly")
        require(flow.update(ready: true, native: native, now: 3.6) == .close(7, succeeded: true) && flow.stage == .idle,
                "Card closes the session and persists after the acknowledgement")

        // Failure keeps the card open and Confirm available again when ready.
        flow = Flow(); native = Flow.Native(supported: true, eligible: true, requiredSamples: 12)
        _ = flow.request(); _ = flow.update(ready: true, native: native, now: 0); flow.opened(9)
        native.session = 9; native.phase = 2
        _ = flow.confirm(native: native)
        native.phase = 2; native.failure = 8; native.resultSession = 9
        require(flow.update(ready: true, native: native, now: 1) == .none && flow.stage == .card(9), "Failed capture returns to the open card")
        require(Flow.cardText(flow.stage, native: native).contains("where the game's left hand is"), "Failure is explained")
        require(Flow.canConfirm(flow.stage, native: native), "Player can confirm again")
        flow.captureRefused()
        require(flow.stage == .card(9), "Refusal outside capture is harmless")
        _ = flow.confirm(native: native); flow.captureRefused()
        require(flow.stage == .card(9), "A native refusal keeps the card open")

        // Every exit closes the native session: Crown/Window/GTA pause, expiry, Cancel.
        require(flow.update(ready: false, native: native, now: 2) == .close(9, succeeded: false) && flow.stage == .idle,
                "Leaving immersion closes the session and the card")
        require(flow.settingsText(native: native).contains("immersive view closed"), "Settings explains the stop")
        _ = flow.request(); _ = flow.update(ready: true, native: native, now: 3); flow.opened(10)
        native.session = 11
        require(flow.update(ready: true, native: native, now: 3) == .close(10, succeeded: false), "A lost native session is never reused")
        _ = flow.request(); _ = flow.update(ready: true, native: native, now: 4); flow.opened(12); native.session = 12
        require(flow.cancel(.playerCancelled) == .close(12, succeeded: false) && !flow.cardVisible, "Cancel closes the session")
        require(flow.cancel(.playerCancelled) == .none, "Cancel is idempotent")
        _ = flow.request()
        require(flow.cancel(.playerCancelled) == .none && flow.stage == .idle, "Cancelling a pending request needs no native call")
        _ = flow.request(); _ = flow.update(ready: true, native: native, now: 5); flow.opened(0)
        require(flow.stage == .idle && !flow.cardVisible, "An unsupported native open fails clearly")
        _ = flow.request()
        require(flow.update(ready: true, native: Flow.Native(), now: 6) == .none && flow.stage == .idle, "Unsupported builds never wait forever")

        // Settings wording distinguishes fallback, calibrated and other controller.
        var idle = Flow()
        require(idle.settingsText(native: Flow.Native()).contains("keeps the game's wrist angle"), "Position-only fallback explained")
        require(idle.settingsText(native: Flow.Native(calibrationPresent: true, controllerMatches: false)).contains("different left controller"),
                "Controller mismatch keeps fallback")
        require(idle.settingsText(native: Flow.Native(calibrationPresent: true, controllerMatches: true, translationMetres: 0.061, rotationDegrees: 23))
            .contains("6.1 cm"), "Calibrated summary")
        _ = idle.request()
        require(idle.settingsText(native: Flow.Native()).contains("opens by itself"), "Pending request explains the next immersive entry")
        print("PASS left wrist calibration flow: \(checks) checks")
    }
}

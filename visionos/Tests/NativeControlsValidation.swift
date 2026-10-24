import Foundation

@main struct NativeControlsValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        NativeControlsOwnership.shared.set(true)
        require(svNativeControlsOwnsInputV1() == 1, "Rust must observe modal ownership")
        NativeControlsOwnership.shared.set(false)
        require(svNativeControlsOwnsInputV1() == 0, "Ownership releases on dismiss")
        require(svNativeControlsInputPolicyV2() == 0, "V2 starts in normal gameplay")
        NativeControlsOwnership.shared.set(modal: false, utility: true)
        require(svNativeControlsOwnsInputV1() == 1 && svNativeControlsInputPolicyV2() == 2,
                "Utility retains V1 boolean compatibility and distinct V2 navigation policy")
        NativeControlsOwnership.shared.set(modal: true, utility: true)
        require(svNativeControlsOwnsInputV1() == 1 && svNativeControlsInputPolicyV2() == 1,
                "A full modal always takes priority over utility navigation")
        NativeControlsOwnership.shared.set(modal: false, utility: false)
        require(svNativeControlsOwnsInputV1() == 0 && svNativeControlsInputPolicyV2() == 0,
                "Releasing both owners restores normal gameplay policy")
        var router = NativeControlsRouter()
        let heldStart = NativeControlsInput(pause: true)
        require(!router.update(heldStart, selection: 0, now: 0).dismiss, "Opening Start cannot close its own panel")
        _ = router.update(NativeControlsInput(), selection: 0, now: 0.01)
        require(router.update(NativeControlsInput(x: 1), selection: 0, now: 1).selection == 1, "Right chooses next actual action")
        require(router.update(NativeControlsInput(x: 1), selection: 1, now: 1.2).selection == 1, "Held direction initial repeat delay")
        require(router.update(NativeControlsInput(x: 1), selection: 1, now: 1.36).selection == 2, "Repeat advances highlight")
        _ = router.update(NativeControlsInput(), selection: 2, now: 2)
        require(router.update(NativeControlsInput(y: -1), selection: 2, now: 2.1).selection == 0, "Down chooses row below")
        _ = router.update(NativeControlsInput(), selection: 0, now: 3)
        require(router.update(NativeControlsInput(confirm: true), selection: 0, now: 3.1).activate, "Cross activates highlighted action")
        require(!router.update(NativeControlsInput(confirm: true), selection: 0, now: 3.2).activate, "Held Cross cannot queue repeated actions")
        _ = router.update(NativeControlsInput(), selection: 0, now: 4)
        require(router.update(NativeControlsInput(back: true), selection: 0, now: 4.1).dismiss, "Circle resumes or dismisses settings")
        require(SantosControlAction.allCases == [.resume, .window, .settings, .quit], "Only four actual app pause actions remain")
        require(SantosSenseUtilityAction.phone.rawValue == 7 && SantosSenseUtilityAction.changeView.rawValue == 6,
                "Gameplay phone and view actions preserve their actual queue IDs")
        require(SantosSenseUtilityAction.allCases.count == 6, "Gameplay utility strip contains only phone, view and directions")
        require([SantosSenseUtilityAction.up, .down, .left, .right].map(\.rawValue) == [8, 9, 10, 11],
                "Onscreen D-pad uses the real native directional queue")
        var gate = NativeControlsActivationGate()
        let selectedWindow = gate.deferController(.window, now: 1)!
        require(gate.resolve(selectedWindow, now: 1.08) == .window,
                "Raw Cross captures the selected action, rather than reading a later changing highlight")
        require(!gate.direct(now: 1.1), "Late system event cannot duplicate a raw controller activation")
        let selectedResume = gate.deferController(.resume, now: 2)!
        require(gate.direct(now: 2.01), "Actual gaze/pinch Button action gets priority")
        require(gate.resolve(selectedResume, now: 2.08) == nil, "Gaze/pinch cancels the raw Cross fallback")
        require(gate.deferController(.resume, now: 2.02) == nil,
                "A late raw snapshot cannot activate a different highlight after actual Button UI activation")
        let stale = gate.deferController(.quit, now: 3)!
        let current = gate.deferController(.settings, now: 3.01)!
        require(gate.resolve(stale, now: 3.08) == nil && gate.resolve(current, now: 3.09) == .settings,
                "Superseded controller confirmation cannot dispatch stale Quit")
        let cancelled = gate.deferController(.quit, now: 4)!
        gate.cancel()
        require(gate.resolve(cancelled, now: 4.08) == nil, "Closing controls cancels pending activation")
        var gazeOnly = NativeControlsRouter()
        _ = gazeOnly.update(NativeControlsInput(), selection: 0, now: 0)
        require(!gazeOnly.update(NativeControlsInput(confirm: true), selection: 0, now: 0.1).activate,
                "Gaze click without stick navigation cannot speculatively activate default Resume")
        require(gazeOnly.update(NativeControlsInput(), selection: 0, now: 1).confirmReleased,
                "Controller release is observable after a long held native gaze click")
        var directed = NativeControlsRouter()
        _ = directed.update(NativeControlsInput(), selection: 0, now: 0)
        _ = directed.update(NativeControlsInput(x: 1), selection: 0, now: 0.1)
        let pressed = directed.update(NativeControlsInput(confirm: true), selection: 1, now: 0.2)
        require(pressed.activate && !pressed.confirmReleased, "Navigated raw Cross captures action on press, but is not a release")
        require(directed.update(NativeControlsInput(), selection: 1, now: 1).confirmReleased,
                "Fallback waits for release, so a held UIKit Button has time to receive its real action")
        var presses = NativeMenuPressObserver()
        require(presses.consume(1)?.toggle == true, "Fast Start poll opens controls immediately")
        require(presses.consume(1) == nil && presses.consume(0) == nil,
                "Duplicate and stale full status snapshots cannot double-toggle controls")
        require(presses.consume(3)?.toggle == false && presses.count == 3,
                "Two presses arriving between UI samples preserve parity")
        require(presses.consume(4)?.toggle == true, "Next fresh physical press is handled once")
        typealias Pause = NativePauseCompanionPolicy
        let grace = Pause.pauseOpenGrace
        var legacy = Pause()
        _ = legacy.observe(.unknown, visible: false)
        var shown = legacy.startPressed(visible: false, now: 0)
        require(shown && legacy.panelOwnsController(visible: shown, now: 0),
                "Unverified game state keeps the established modal Start panel")
        require(!legacy.startPressed(visible: true, now: 1), "Unverified Start still toggles the panel closed")
        var pause = Pause()
        _ = pause.observe(.gameplay, visible: false)
        shown = pause.startPressed(visible: false, now: 10)
        require(shown && !pause.panelOwnsController(visible: shown, now: 10.05),
                "Start opens the panel without cutting GTA's own pause Escape short")
        shown = pause.observe(.paused, visible: shown)
        require(shown && !pause.panelOwnsController(visible: shown, now: 10 + grace + 5),
                "With GTA's pause/Settings menu open the controller belongs to GTA, indefinitely")
        require(pause.startPressed(visible: shown, now: 20),
                "Start inside GTA's menu is GTA's Escape; the panel follows GTA's actual state")
        shown = pause.observe(.paused, visible: shown)
        require(shown, "Backing out of a GTA submenu keeps the companion")
        shown = pause.observe(.gameplay, visible: shown)
        require(!shown, "The panel closes when GTA's own menu closes, by Start or Circle")
        require(!pause.panelOwnsController(visible: shown, now: 21), "Closed panel never owns input")
        var noPause = Pause()
        _ = noPause.observe(.gameplay, visible: false)
        shown = noPause.startPressed(visible: false, now: 30)
        shown = noPause.observe(.gameplay, visible: shown)
        require(shown && !noPause.panelOwnsController(visible: shown, now: 30 + grace - 0.01), "Pause grace stays non-modal")
        require(noPause.panelOwnsController(visible: shown, now: 30 + grace),
                "If GTA never opens a menu, the app-only panel takes the controller as before")
        shown = noPause.observe(.paused, visible: shown)
        require(shown && !noPause.panelOwnsController(visible: shown, now: 40),
                "A late GTA pause still returns the controller to GTA's menu")
        var doubleTap = Pause()
        _ = doubleTap.observe(.gameplay, visible: false)
        shown = doubleTap.startPressed(visible: false, now: 50)
        require(!doubleTap.startPressed(visible: shown, now: 50.2),
                "A second Start before GTA pauses closes the panel")
        var external = Pause()
        _ = external.observe(.paused, visible: false)
        require(!external.startPressed(visible: false, now: 59),
                "Start inside a GTA menu opened without the panel is GTA's Escape, not a panel toggle")
        require(!external.panelOwnsController(visible: true, now: 60),
                "A panel shown after a failed quit beside GTA's menu still leaves GTA the controller")
        _ = external.observe(.gameplay, visible: false)
        require(external.panelOwnsController(visible: true, now: 61),
                "An app-opened panel over gameplay remains modal")
        require(Pause.GuestMenu(rawValue: 2) == .paused && Pause.GuestMenu(rawValue: 1) == .gameplay
                && Pause.GuestMenu(rawValue: 0) == .unknown && Pause.GuestMenu(rawValue: 9) == nil,
                "Swift reads sv_native_menu_context_v1 values exactly")
        require(SantosControlAction.companionActions(immersive: true) == [.window, .settings, .quit]
                && SantosControlAction.companionActions(immersive: false) == [.settings, .quit],
                "Beside GTA's menu no Resume duplicates GTA's Back, and flat view has no Window no-op")
        require(SantosControlAction.settings.title != "Settings", "App settings are not labelled like GTA's own Settings tab")
        var cross = Pause()
        require(cross.acceptsCompanionActivation(now: 0), "Look-and-trigger or pinch activates companion buttons")
        cross.observeConfirm(true, now: 1)
        require(!cross.acceptsCompanionActivation(now: 1.5), "A held Cross belongs to GTA's menu")
        cross.observeConfirm(false, now: 1.6)
        require(!cross.acceptsCompanionActivation(now: 1 + Pause.confirmGuard - 0.01),
                "A system Button event coinciding with Cross is GTA's selection")
        require(cross.acceptsCompanionActivation(now: 1 + Pause.confirmGuard), "Guard expires after a short bound")
        cross.observeConfirm(false, now: 3)
        require(cross.acceptsCompanionActivation(now: 3), "Released state does not refresh the guard")
        print("Native controls policy: \(checks) checks passed")
    }
}

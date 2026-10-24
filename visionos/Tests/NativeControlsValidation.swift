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
        var router = NativeControlsRouter()
        let heldStart = NativeControlsInput(pause: true)
        require(!router.update(heldStart, selection: 0, now: 0).dismiss, "Opening Start cannot close its own panel")
        _ = router.update(NativeControlsInput(), selection: 0, now: 0.01)
        require(router.update(NativeControlsInput(x: 1), selection: 0, now: 1).selection == 1, "Right chooses next actual action")
        require(router.update(NativeControlsInput(x: 1), selection: 1, now: 1.2).selection == 1, "Held direction initial repeat delay")
        require(router.update(NativeControlsInput(x: 1), selection: 1, now: 1.36).selection == 2, "Repeat advances highlight")
        _ = router.update(NativeControlsInput(), selection: 2, now: 2)
        require(router.update(NativeControlsInput(y: -1), selection: 2, now: 2.1).selection == 6, "Down chooses row below")
        _ = router.update(NativeControlsInput(), selection: 6, now: 3)
        require(router.update(NativeControlsInput(confirm: true), selection: 6, now: 3.1).activate, "Cross activates highlighted action")
        require(!router.update(NativeControlsInput(confirm: true), selection: 6, now: 3.2).activate, "Held Cross cannot queue repeated actions")
        _ = router.update(NativeControlsInput(), selection: 6, now: 4)
        require(router.update(NativeControlsInput(back: true), selection: 6, now: 4.1).dismiss, "Circle resumes or dismisses settings")
        require(SantosControlAction.camera.menuAction == 6, "Camera routes to gameplay-aware native key path")
        require(SantosControlAction.phone.menuAction == 7, "Phone routes to gameplay-aware native key path")
        require(SantosControlAction.resume.menuAction == 13, "Resume is state aware, never a blind Escape toggle")
        require(Set(SantosControlAction.allCases.map(\.rawValue)).count == 18, "Every visible cell has unique selection")
        require(SantosControlAction.zoomIn.menuAction == 15 && SantosControlAction.zoomOut.menuAction == 16, "Map zoom reaches the real wheel queue")
        print("Native controls policy: \(checks) checks passed")
    }
}

import Foundation

@main struct GameSceneTransitionsValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message)
            checks += 1
        }
        let old = UUID(), new = UUID()
        var policy = GameWindowReturnPolicy()
        policy.request(generation: old, reason: "Start")
        let first = policy.serial
        policy.request(generation: old, reason: "watchdog")
        require(policy.serial == first && policy.reason == "Start", "Two callbacks coalesce into one return")
        require(policy.begin(active: false, quitting: false, now: 0) == nil, "No scene operation while app inactive")
        require(policy.pending, "Foreground can resume the pending return")
        let ticket = policy.begin(active: true, quitting: false, now: 10)!
        require(policy.begin(active: true, quitting: false, now: 10) == nil, "Only one task may open the window")
        require(policy.poll(ticket, windowReady: false, active: true, quitting: false, now: 10) == .waiting,
                "Queued openWindow does not release the compositor")
        require(policy.poll(ticket, windowReady: true, active: false, quitting: false, now: 11) == .waiting,
                "Attached background window cannot release active presentation")
        require(policy.poll(ticket, windowReady: true, active: true, quitting: false, now: 12) == .closeSpace,
                "Real active attached game window allows one close")
        require(policy.poll(ticket, windowReady: true, active: true, quitting: false, now: 12) == .stale,
                "Repeated callbacks cannot close another space")
        policy.request(generation: new, reason: "onDisappear")
        require(policy.serial == first, "onDisappear during close never reopens another window")
        policy.finish(ticket)
        require(!policy.pending, "Successful transition retires the ticket")
        policy.sceneDisappeared(generation: new)
        require(!policy.pending && policy.serial == first,
                "Actual delayed onDisappear after await completion never queues another window")
        policy.sceneAppeared()
        policy.sceneDisappeared(generation: new)
        require(policy.pending && policy.serial > first,
                "An unexpected dismissal of a later immersive session still restores the window")
        policy.cancel()
        policy.request(generation: new, reason: "next return")
        let second = policy.begin(active: true, quitting: false, now: 20)!
        policy.finish(ticket)
        require(policy.ticket == second && policy.pending, "Late old completion cannot retire a new return")
        require(policy.poll(ticket, windowReady: true, active: true, quitting: false, now: 21) == .stale,
                "Old task cannot release a new compositor")
        require(policy.poll(second, windowReady: false, active: true, quitting: false, now: 24.99) == .waiting,
                "Attachment has a bounded opportunity to finish")
        require(policy.poll(second, windowReady: false, active: true, quitting: false, now: 25) == .timedOut,
                "Failed attachment does not silently destroy the only presentation")
        require(!policy.pending && policy.stage == .failed, "Failure stops the loop and allows explicit retry")
        policy.request(generation: new, reason: "retry")
        let third = policy.begin(active: true, quitting: false, now: 30)!
        require(third.serial > second.serial, "Retry has a new identity")
        require(policy.poll(third, windowReady: true, active: true, quitting: true, now: 31) == .cancelled,
                "Quit cancels before another scene can open or close")
        policy.request(generation: new, reason: "late disappear during quit")
        require(policy.begin(active: true, quitting: true, now: 40) == nil && !policy.pending,
                "Late teardown request never starts a scene operation")
        print("Game scene transitions: \(checks) checks passed")
    }
}

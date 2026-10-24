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
        var kept = GameWindowReturnPolicy()
        kept.sceneAppeared()
        kept.request(generation: new, reason: "gta_pause_menu_in_window")
        let keptTicket = kept.begin(active: true, quitting: false, now: 50)!
        require(kept.poll(keptTicket, windowReady: false, active: true, quitting: false, now: 55) == .timedOut,
                "Attachment failure keeps immersion open")
        kept.sceneDisappeared(generation: new)
        require(kept.pending && kept.serial > keptTicket.serial,
                "A later system dismissal of that kept immersion still restores the game window")
        var completed = GameWindowReturnPolicy()
        completed.sceneAppeared()
        completed.request(generation: new, reason: "Start")
        let done = completed.begin(active: true, quitting: false, now: 60)!
        _ = completed.poll(done, windowReady: true, active: true, quitting: false, now: 60)
        completed.finish(done)
        completed.sceneDisappeared(generation: new)
        require(!completed.pending && completed.serial == done.serial,
                "The failure exception does not reopen after a completed return")
        var suspended = GameWindowReturnPolicy()
        suspended.request(generation: new, reason: "Window")
        let suspendedTicket = suspended.begin(active: true, quitting: false, now: 70)!
        require(suspended.poll(suspendedTicket, windowReady: false, active: false, quitting: false, now: 71) == .waiting,
                "Taking off the headset pauses the window attachment deadline")
        require(suspended.poll(suspendedTicket, windowReady: false, active: true, quitting: false, now: 700) == .waiting,
                "Reactivation gives the actual window a fresh bounded attachment opportunity")
        require(suspended.poll(suspendedTicket, windowReady: false, active: true, quitting: false, now: 704.99) == .waiting,
                "Background time is never mistaken for failed window restoration")
        require(suspended.poll(suspendedTicket, windowReady: true, active: true, quitting: false, now: 705) == .closeSpace,
                "The live reactivated window can complete the return")
        print("Game scene transitions: \(checks) checks passed")
    }
}

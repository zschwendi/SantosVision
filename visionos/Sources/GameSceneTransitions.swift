import Foundation

/// A return owns one transition, even if Start, the frame watchdog and the
/// compositor's disappearance all request it. Opening a window is asynchronous:
/// never release the immersive scene until the real game view is attached.
struct GameWindowReturnPolicy {
    struct Ticket: Equatable, Sendable {
        let serial: UInt64
        let rendererGeneration: UUID
    }
    enum Stage: String { case idle, requested, openingWindow, closingSpace, failed }
    enum Decision { case waiting, closeSpace, stale, cancelled, timedOut }
    private(set) var serial: UInt64 = 0
    private(set) var stage = Stage.idle
    private(set) var reason = ""
    private(set) var ticket: Ticket?
    private var deadline: Double?
    private var returnRequestedForThisSpace = false
    var pending: Bool { stage == .requested || stage == .openingWindow || stage == .closingSpace }

    mutating func request(generation: UUID, reason: String) {
        guard !pending else { return }
        returnRequestedForThisSpace = true
        serial &+= 1
        ticket = Ticket(serial: serial, rendererGeneration: generation)
        self.reason = reason
        deadline = nil
        stage = .requested
    }
    mutating func sceneAppeared() { returnRequestedForThisSpace = false }
    mutating func sceneDisappeared(generation: UUID) {
        // dismissImmersiveSpace can return BEFORE onDisappear. That late
        // callback belongs to the completed return, not a second openWindow.
        // A timed-out return kept immersion open, so its later dismissal
        // still needs the game window.
        guard !returnRequestedForThisSpace || stage == .failed else { return }
        request(generation: generation, reason: "immersive_scene_disappeared")
    }
    mutating func begin(active: Bool, quitting: Bool, now: Double) -> Ticket? {
        guard stage == .requested else { return nil }
        if quitting { cancel(); return nil }
        guard active, now.isFinite else { return nil }
        deadline = now + 5
        stage = .openingWindow
        return ticket
    }
    mutating func poll(_ expected: Ticket, windowReady: Bool, active: Bool,
                       quitting: Bool, now: Double) -> Decision {
        guard ticket == expected, stage == .openingWindow else { return .stale }
        if quitting { cancel(); return .cancelled }
        // Removing the headset must not spend the attachment deadline. The
        // return task reads the live runtime activity, not its captured App value.
        if !active { deadline = nil; return .waiting }
        if windowReady { stage = .closingSpace; return .closeSpace }
        if deadline == nil, now.isFinite { deadline = now + 5 }
        if let deadline, now.isFinite, now >= deadline {
            stage = .failed
            return .timedOut
        }
        return .waiting
    }
    mutating func finish(_ expected: Ticket) {
        guard ticket == expected, stage == .closingSpace else { return }
        stage = .idle
        ticket = nil
        deadline = nil
    }
    mutating func cancel() { stage = .idle; ticket = nil; deadline = nil }
}

#if SV_GAME_RUNTIME
import Combine

@MainActor
final class GameSceneTransitions: ObservableObject {
    @Published private(set) var policy = GameWindowReturnPolicy()
    var pending: Bool { policy.pending }
    func sceneAppeared() { policy.sceneAppeared() }
    func sceneDisappeared(generation: UUID) { policy.sceneDisappeared(generation: generation) }
    func requestReturn(generation: UUID, reason: String) { policy.request(generation: generation, reason: reason) }
    func begin(active: Bool, quitting: Bool) -> GameWindowReturnPolicy.Ticket? {
        policy.begin(active: active, quitting: quitting, now: ProcessInfo.processInfo.systemUptime)
    }
    func poll(_ ticket: GameWindowReturnPolicy.Ticket, windowReady: Bool,
              active: Bool, quitting: Bool) -> GameWindowReturnPolicy.Decision {
        policy.poll(ticket, windowReady: windowReady, active: active, quitting: quitting,
                    now: ProcessInfo.processInfo.systemUptime)
    }
    func finish(_ ticket: GameWindowReturnPolicy.Ticket) { policy.finish(ticket) }
}
#endif

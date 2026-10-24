import Foundation

/// Modal ownership is a lock-protected value, never actor state read from Rust.
final class NativeControlsOwnership: @unchecked Sendable {
    static let shared = NativeControlsOwnership()
    private let lock = NSLock()
    private var ownsInput = false
    func set(_ value: Bool) { lock.lock(); ownsInput = value; lock.unlock() }
    func read() -> Bool { lock.lock(); defer { lock.unlock() }; return ownsInput }
}

@_cdecl("sv_native_controls_owns_input_v1")
func svNativeControlsOwnsInputV1() -> UInt32 {
    NativeControlsOwnership.shared.read() ? 1 : 0
}

struct NativeControlsInput: Sendable {
    var x: Float = 0
    var y: Float = 0
    var confirm = false
    var back = false
    var pause = false
    var neutral: Bool { abs(x) < 0.3 && abs(y) < 0.3 && !confirm && !back && !pause }
}

/// The Start panel contains only app-level pause actions.
enum SantosControlAction: Int, CaseIterable, Identifiable {
    case resume, window, settings, quit
    var id: Int { rawValue }
    var title: String {
        switch self {
        case .resume: "Resume"
        case .window: "Window"
        case .settings: "Settings"
        case .quit: "Quit game"
        }
    }
    var symbol: String {
        switch self {
        case .resume: "play.fill"
        case .window: "arrow.down.right.and.arrow.up.left"
        case .settings: "gearshape"
        case .quit: "power"
        }
    }
}

/// Shared by the flat SwiftUI strip and the immersive compositor hit targets.
/// Values intentionally match the existing additive native menu-action ABI.
enum SantosSenseUtilityAction: UInt32, CaseIterable, Identifiable, Sendable {
    case phone = 7, changeView = 6, up = 8, down = 9, left = 10, right = 11
    var id: UInt32 { rawValue }
    var title: String {
        switch self {
        case .phone: "Phone"
        case .changeView: "Change view"
        case .up: "D-pad up"
        case .down: "D-pad down"
        case .left: "D-pad left"
        case .right: "D-pad right"
        }
    }
    var isDirection: Bool { [.up, .down, .left, .right].contains(self) }
    var symbol: String {
        switch self {
        case .phone: "iphone"
        case .changeView: "camera.rotate"
        case .up: "arrow.up"
        case .down: "arrow.down"
        case .left: "arrow.left"
        case .right: "arrow.right"
        }
    }
}

/// A controller Cross can also arrive as the system's gaze/pinch Button event.
/// Prefer that actual hit target; the raw controller fallback captures its
/// selected action now and is cancelled by a direct UI activation.
struct NativeControlsActivationGate {
    struct Pending: Equatable { let sequence: UInt64; let action: SantosControlAction }
    private var sequence: UInt64 = 0
    private var pending: Pending?
    private var lastDirectTime = -Double.infinity
    mutating func deferController(_ action: SantosControlAction, now: Double) -> Pending? {
        guard now - lastDirectTime >= 0.15 else { return nil }
        sequence &+= 1
        let value = Pending(sequence: sequence, action: action)
        pending = value
        return value
    }
    mutating func direct(now: Double) -> Bool {
        pending = nil
        guard now - lastDirectTime >= 0.15 else { return false }
        lastDirectTime = now
        return true
    }
    mutating func resolve(_ value: Pending, now: Double) -> SantosControlAction? {
        guard pending == value else { return nil }
        pending = nil
        lastDirectTime = now
        return value.action
    }
    mutating func cancel() { pending = nil }
}

/// Fast atomic polling and slower full status snapshots share this observer.
/// Ignore duplicate/older snapshots, rather than wrapping subtraction toggling
/// the controls a second time after an already handled physical Start press.
struct NativeMenuPressObserver {
    private(set) var count: UInt64 = 0
    struct Event { let count: UInt64; let toggle: Bool }
    mutating func consume(_ next: UInt64) -> Event? {
        guard next > count else { return nil }
        let delta = next - count
        count = next
        return Event(count: next, toggle: delta % 2 == 1)
    }
}

/// Input edges and directional repeat used by the actual native controls view.
/// Opening a modal with Start held never immediately closes/activates it again.
struct NativeControlsRouter {
    private var armed = false
    private var previous = NativeControlsInput()
    private var lastDirection = 0
    private var usedNavigation = false
    private var nextRepeat: Double = 0
    struct Result { var selection: Int; var activate = false; var confirmReleased = false; var dismiss = false }
    mutating func update(_ input: NativeControlsInput, selection: Int, now: Double) -> Result {
        var result = Result(selection: min(max(selection, 0), SantosControlAction.allCases.count - 1))
        guard armed else {
            armed = input.neutral
            previous = input
            return result
        }
        result.confirmReleased = !input.confirm && previous.confirm
        result.dismiss = (input.back && !previous.back) || (input.pause && !previous.pause)
        let direction: Int
        if abs(input.y) >= abs(input.x), abs(input.y) > 0.5 { direction = input.y > 0 ? -2 : 2 }
        else if abs(input.x) > 0.5 { direction = input.x > 0 ? 1 : -1 }
        else { direction = 0 }
        if direction != 0, direction != lastDirection || now >= nextRepeat {
            let count = SantosControlAction.allCases.count
            result.selection = (result.selection + direction + count) % count
            nextRepeat = now + (direction == lastDirection ? 0.14 : 0.35)
        }
        usedNavigation = usedNavigation || direction != 0
        // Raw Cross is a fallback only after deliberate controller navigation.
        // Plain gaze clicks use UIKit, including a hold until Button release.
        result.activate = usedNavigation && input.confirm && !previous.confirm
        lastDirection = direction
        previous = input
        return result
    }
}

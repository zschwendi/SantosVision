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

enum SantosControlAction: Int, CaseIterable, Identifiable {
    case resume, window, settings, camera
    case phone, up, down, quit
    case left, right, select, back
    case apply, yes, no, tab
    var id: Int { rawValue }
    var title: String {
        switch self {
        case .resume: "Resume"
        case .window: "Window"
        case .settings: "Settings"
        case .camera: "Change camera"
        case .phone: "Phone"
        case .up: "D-pad up"
        case .down: "D-pad down"
        case .quit: "Quit game"
        case .left: "D-pad left"
        case .right: "D-pad right"
        case .select: "Select"
        case .back: "Back"
        case .apply: "Apply"
        case .yes: "Yes"
        case .no: "No"
        case .tab: "Tab"
        }
    }
    var symbol: String {
        switch self {
        case .resume: "play.fill"
        case .window: "arrow.down.right.and.arrow.up.left"
        case .settings: "gearshape"
        case .camera: "camera.rotate"
        case .phone: "iphone"
        case .up: "arrow.up"
        case .down: "arrow.down"
        case .quit: "power"
        case .left: "arrow.left"
        case .right: "arrow.right"
        case .select: "checkmark"
        case .back: "arrow.uturn.backward"
        case .apply: "checkmark.circle"
        case .yes: "y.circle"
        case .no: "n.circle"
        case .tab: "arrow.right.to.line"
        }
    }
    var menuAction: UInt32? {
        switch self {
        case .resume: 13
        case .camera: 6
        case .phone: 7
        case .up: 8
        case .down: 9
        case .left: 10
        case .right: 11
        case .select: 2
        case .back: 12
        case .apply: 3
        case .yes: 4
        case .no: 5
        case .tab: 14
        default: nil
        }
    }
}

/// Input edges and directional repeat used by the actual native controls view.
/// Opening a modal with Start held never immediately closes/activates it again.
struct NativeControlsRouter {
    private var armed = false
    private var previous = NativeControlsInput()
    private var lastDirection = 0
    private var nextRepeat: Double = 0
    struct Result { var selection: Int; var activate = false; var dismiss = false }
    mutating func update(_ input: NativeControlsInput, selection: Int, now: Double) -> Result {
        var result = Result(selection: selection)
        guard armed else {
            armed = input.neutral
            previous = input
            return result
        }
        result.activate = input.confirm && !previous.confirm
        result.dismiss = (input.back && !previous.back) || (input.pause && !previous.pause)
        let direction: Int
        if abs(input.y) >= abs(input.x), abs(input.y) > 0.5 { direction = input.y > 0 ? -4 : 4 }
        else if abs(input.x) > 0.5 { direction = input.x > 0 ? 1 : -1 }
        else { direction = 0 }
        if direction != 0, direction != lastDirection || now >= nextRepeat {
            let count = SantosControlAction.allCases.count
            result.selection = (selection + direction + count) % count
            nextRepeat = now + (direction == lastDirection ? 0.14 : 0.35)
        }
        lastDirection = direction
        previous = input
        return result
    }
}

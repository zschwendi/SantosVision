import ARKit
import Foundation
import GameController

/// The Sense controllers are separate GCController devices, not one extended
/// gamepad. Resolve their fixed hardware chirality once per connection. This
/// does not start accessory tracking or request access to tracked poses.
@MainActor
final class SpatialGamepadInput {
    static let shared = SpatialGamepadInput()
    static let accessoriesDidChange = Notification.Name("LosSantosSpatialAccessoriesDidChange")
    private var observations: [NSObjectProtocol] = []
    private var controllers: [ObjectIdentifier: GCController] = [:]
    private var resolving: [ObjectIdentifier: Task<Void, Never>] = [:]
    private var errors: [ObjectIdentifier: String] = [:]
    private var accessories: [ObjectIdentifier: Accessory] = [:]

    /// Already resolved fixed-hand identities for a separate, permissioned
    /// tracking session. Ambiguous duplicate same-hand devices are excluded.
    var trackingAccessories: [Accessory] {
        let connected = Set(GCController.controllers().map(ObjectIdentifier.init))
        let current = accessories.filter { connected.contains($0.key) }.map(\.value)
        return [Accessory.Chirality.left, .right].compactMap { hand in
            let matches = current.filter { $0.inherentChirality == hand }
            return matches.count == 1 ? matches[0] : nil
        }
    }

    func start() {
        guard observations.isEmpty else { return }
        for name in [Notification.Name.GCControllerDidConnect, .GCControllerDidDisconnect] {
            observations.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { _ in
                Task { @MainActor in SpatialGamepadInput.shared.refresh() }
            })
        }
        refresh()
    }

    private func refresh() {
        let current = GCController.controllers().filter { $0.productCategory == GCProductCategorySpatialController }
        let currentIDs = Set(current.map(ObjectIdentifier.init))
        let removed = Set(controllers.keys).subtracting(currentIDs)
        for id in removed {
            resolving.removeValue(forKey: id)?.cancel()
            controllers.removeValue(forKey: id)
            errors.removeValue(forKey: id)
            accessories.removeValue(forKey: id)
        }
        SpatialGamepadStore.shared.retainOnly(currentIDs)
        if !removed.isEmpty { NotificationCenter.default.post(name: Self.accessoriesDidChange, object: nil) }
        for controller in current {
            let id = ObjectIdentifier(controller)
            guard controllers[id] == nil else { continue }
            controllers[id] = controller
            resolving[id] = Task { [weak self, controller] in
                do {
                    let accessory = try await Self.resolveAccessory(for: id)
                    guard let self, !Task.isCancelled, self.controllers[id] === controller else { return }
                    let hand: UInt32
                    switch accessory.inherentChirality {
                    case .left: hand = 1
                    case .right: hand = 2
                    case .unspecified:
                        self.errors[id] = "Controller has no fixed left/right identity"
                        self.resolving.removeValue(forKey: id)
                        return
                    @unknown default:
                        self.errors[id] = "Controller hand identity is unsupported"
                        self.resolving.removeValue(forKey: id)
                        return
                    }
                    SpatialGamepadStore.shared.bind(controller, hand: hand)
                    self.accessories[id] = accessory
                    self.resolving.removeValue(forKey: id)
                    NotificationCenter.default.post(name: Self.accessoriesDidChange, object: nil)
                } catch {
                    guard let self, !Task.isCancelled, self.controllers[id] === controller else { return }
                    self.errors[id] = String(error.localizedDescription.prefix(200))
                    self.resolving.removeValue(forKey: id)
                }
            }
        }
    }

    private enum ResolutionError: Error { case disconnected }

    /// Only a Sendable identity leaves the UI actor. Obtain the framework's
    /// current device reference in this nonisolated acquisition task and pass
    /// it directly to ARKit; never send our actor-owned registry reference to
    /// the @concurrent initializer. The UI actor verifies identity again after
    /// the await before binding any controls or keeping the returned Accessory.
    nonisolated private static func resolveAccessory(for id: ObjectIdentifier) async throws -> Accessory {
        guard let device = GCController.controllers().first(where: { ObjectIdentifier($0) == id }) else {
            throw ResolutionError.disconnected
        }
        return try await Accessory(device: device)
    }

    /// Native modal navigation uses the same fixed-hand snapshots as gameplay.
    func nativeControlsInput() -> NativeControlsInput {
        var values = [Float](repeating: 0, count: 6)
        var buttons: UInt32 = 0
        var leftDevice: UInt64 = 0
        let hands = values.withUnsafeMutableBufferPointer {
            SpatialGamepadStore.shared.read(values: $0.baseAddress!, buttons: &buttons, leftDevice: &leftDevice)
        }
        if hands != 0 {
            return NativeControlsInput(x: values[0], y: values[1], confirm: buttons & 1 != 0,
                                       back: buttons & 2 != 0, pause: buttons & (1 << 9) != 0)
        }
        guard let pad = GCController.controllers().compactMap({ $0.extendedGamepad }).first else {
            return NativeControlsInput()
        }
        let x = abs(pad.dpad.xAxis.value) > 0.3 ? pad.dpad.xAxis.value : pad.leftThumbstick.xAxis.value
        let y = abs(pad.dpad.yAxis.value) > 0.3 ? pad.dpad.yAxis.value : pad.leftThumbstick.yAxis.value
        return NativeControlsInput(x: x, y: y, confirm: pad.buttonA.isPressed,
                                   back: pad.buttonB.isPressed, pause: pad.buttonMenu.isPressed)
    }

    var report: [String: Any] {
        let state = SpatialGamepadStore.shared.state()
        return [
            "profile": "spatial_gamepad_buttons_v1",
            "discovered_controller_count": controllers.count,
            "resolving_count": resolving.count,
            "left_controls_available": state.left,
            "right_controls_available": state.right,
            "left_camera_button_available": state.leftMenu,
            "right_pause_button_available": state.rightMenu,
            "left_dpad_modifier_available": state.leftClick,
            "right_stick_click_available": state.rightClick,
            "mapping": "left_move_right_look_l3_normal_click_phone_ornament_phone_dpad_popup_both_stick_click_camera_left_menu_camera_right_menu_pause",
            "motion_aim_enabled": false,
            "errors": Array(errors.values.sorted().prefix(8))
        ]
    }
}

private struct SpatialGamepadDevice {
    let controller: GCController
    let hand: UInt32
}

/// Only this bounded registry crosses between Swift's UI actor and Rust's
/// polling thread. Read live values; never cache a held stick or trigger.
private final class SpatialGamepadStore: @unchecked Sendable {
    static let shared = SpatialGamepadStore()
    private let lock = NSLock()
    private var devices: [ObjectIdentifier: SpatialGamepadDevice] = [:]

    func retainOnly(_ ids: Set<ObjectIdentifier>) {
        lock.lock()
        devices = devices.filter { ids.contains($0.key) }
        lock.unlock()
    }

    func bind(_ controller: GCController, hand: UInt32) {
        lock.lock()
        devices[ObjectIdentifier(controller)] = SpatialGamepadDevice(controller: controller, hand: hand)
        lock.unlock()
    }

    private func availableDevices() -> [SpatialGamepadDevice] {
        // Also check the framework's live collection at every sample, so a
        // disconnect cannot retain a last pressed value while the UI actor is busy.
        let connected = Set(GCController.controllers().map(ObjectIdentifier.init))
        lock.lock()
        let result = devices.filter { connected.contains($0.key) }.map(\.value)
        lock.unlock()
        return result
    }

    func state() -> (left: Bool, right: Bool, leftMenu: Bool, rightMenu: Bool, leftClick: Bool, rightClick: Bool) {
        let devices = availableDevices()
        func input(_ hand: UInt32) -> GCControllerLiveInput? {
            let matches = devices.filter { $0.hand == hand }
            guard matches.count == 1, Self.hasControls(matches[0].controller.input) else { return nil }
            return matches[0].controller.input
        }
        let left = input(1), right = input(2)
        return (left != nil, right != nil,
                left?.buttons[.menu] != nil, right?.buttons[.menu] != nil,
                left?.buttons[.thumbstickButton] != nil, right?.buttons[.thumbstickButton] != nil)
    }

    private static func hasControls(_ input: GCControllerInputState) -> Bool {
        // Menu/stick click are optional capabilities. Their absence must not
        // disable movement, face buttons, aim or fire for the whole half.
        input.dpads[.thumbstick] != nil && [.a, .b, .grip, .trigger].allSatisfy {
            input.buttons[$0] != nil
        }
    }

    func read(values: UnsafeMutablePointer<Float>, buttons: UnsafeMutablePointer<UInt32>,
              leftDevice: UnsafeMutablePointer<UInt64>) -> UInt32 {
        for index in 0..<6 { values[index] = 0 }
        buttons.pointee = 0
        leftDevice.pointee = 0
        var hands: UInt32 = 0
        let devices = availableDevices()
        // Duplicate same-hand devices are ambiguous; leave that side neutral
        // instead of selecting by discovery order or merging different people.
        for hand: UInt32 in [1, 2] {
            let matches = devices.filter { $0.hand == hand }
            guard matches.count == 1 else { continue }
            let input = matches[0].controller.input
            guard Self.hasControls(input), let stick = input.dpads[.thumbstick] else { continue }
            let left = hand == 1
            if left { leftDevice.pointee = UInt64(UInt(bitPattern: ObjectIdentifier(matches[0].controller))) }
            let stickIndex = left ? 0 : 2
            values[stickIndex] = stick.xAxis.value
            values[stickIndex + 1] = stick.yAxis.value
            values[left ? 4 : 5] = input.buttons[.trigger]?.pressedInput.value ?? 0
            let controls: [(GCButtonElementName, Int)] = [
                (.a, left ? 2 : 0), (.b, left ? 3 : 1), (.grip, left ? 4 : 5),
                (.thumbstickButton, left ? 6 : 7), (.menu, left ? 8 : 9)
            ]
            for (name, bit) in controls where input.buttons[name]?.pressedInput.isPressed == true {
                buttons.pointee |= 1 << bit
            }
            hands |= hand
        }
        return hands
    }
}

/// ABI v1: returns left/right availability bits. Called only from the native
/// visionOS input thread; zero/invalid buffers produce a disconnected sample.
@_cdecl("sv_spatial_gamepad_read_v1")
func svSpatialGamepadReadV1(_ values: UnsafeMutablePointer<Float>?, _ count: UInt32,
                          _ buttons: UnsafeMutablePointer<UInt32>?, _ leftDevice: UnsafeMutablePointer<UInt64>?) -> UInt32 {
    guard count == 6, let values, let buttons, let leftDevice else { return 0 }
    return SpatialGamepadStore.shared.read(values: values, buttons: buttons, leftDevice: leftDevice)
}

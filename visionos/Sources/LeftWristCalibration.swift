import Foundation
import SwiftUI

/// Settings action, next-immersive-entry wait, temporary card and persistence
/// for the explicit left grip-to-native-hand calibration. Capture, validation
/// and the native pose transport live in Rust; this model never invents a pose.
@MainActor
final class LeftWristCalibrationModel: ObservableObject {
    @Published private(set) var flow = LeftWristCalibrationFlow()
    @Published private(set) var native = LeftWristCalibrationFlow.Native()
    private var ready = false
    private var engineRunning = false
    private var polling: Task<Void, Never>?
    private var accessoryObservation: NSObjectProtocol?
    static let defaultsKey = "LosSantos.leftWristCalibration.v1"

    var cardVisible: Bool { flow.cardVisible }
    var supported: Bool { native.supported }
    var cardText: String { LeftWristCalibrationFlow.cardText(flow.stage, native: native) }
    var canConfirm: Bool { LeftWristCalibrationFlow.canConfirm(flow.stage, native: native) }
    var settingsText: String { flow.settingsText(native: native) }
    var finished: Bool { if case .finished = flow.stage { true } else { false } }

    /// Restores the saved calibration once the engine accepted startup.
    func engineStarted() {
        guard !engineRunning else { return }
        engineRunning = true
        restore()
        refreshController()
        accessoryObservation = NotificationCenter.default.addObserver(
            forName: SpatialGamepadInput.accessoriesDidChange, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.refreshController() }
        }
        refresh()
    }

    func request() {
        guard engineRunning else { return }
        _ = flow.request()
        tick()
    }
    func cancel() { perform(flow.cancel(.playerCancelled)); refresh(); updatePolling() }
    func confirm() { refresh(); perform(flow.confirm(native: native)); updatePolling() }
    /// App-scope immersive gate; losing it closes an open card.
    func setImmersiveReady(_ value: Bool) {
        guard value != ready else { return }
        ready = value
        tick()
    }
    func refresh() { native = Self.readNative() }

    private func tick() {
        refresh()
        perform(flow.update(ready: ready, native: native, now: ProcessInfo.processInfo.systemUptime))
        updatePolling()
    }

    private func perform(_ command: LeftWristCalibrationFlow.Command) {
        switch command {
        case .none: return
        case .open: flow.opened(Self.open())
        case .capture(let session): if Self.capture(session) != 0 { flow.captureRefused() }
        case .commit(let session):
            // Persist only what native actually adopted for this session.
            let committed = Self.commit(session) == 0
            if committed { persist() }
            refresh()
            perform(flow.commitResult(committed, now: ProcessInfo.processInfo.systemUptime))
            return
        case .close(let session): Self.close(session)
        }
        refresh()
    }

    /// While a request or card exists, poll (and so heartbeat) the native state.
    private func updatePolling() {
        guard flow.stage != .idle else { polling?.cancel(); polling = nil; return }
        guard polling == nil else { return }
        polling = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .milliseconds(100)) } catch { return }
                guard let self, !Task.isCancelled else { return }
                self.tick()
            }
        }
    }

    private func refreshController() {
        #if SV_GAME_RUNTIME
        _ = sv_xr_left_wrist_controller_v1(SpatialGamepadInput.shared.leftControllerFingerprint())
        #endif
        refresh()
    }

    private func persist() {
        #if SV_GAME_RUNTIME
        var value = SVLeftWristCalibration()
        guard sv_xr_left_wrist_export_v1(&value, UInt32(MemoryLayout<SVLeftWristCalibration>.size)) == 0 else { return }
        UserDefaults.standard.set(withUnsafeBytes(of: &value) { Data($0) }, forKey: Self.defaultsKey)
        #endif
    }

    private func restore() {
        #if SV_GAME_RUNTIME
        guard let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
              data.count == MemoryLayout<SVLeftWristCalibration>.size else { return }
        var value = data.withUnsafeBytes { $0.loadUnaligned(as: SVLeftWristCalibration.self) }
        // Native re-validates rigidity, the controller fingerprint and the measured forearm bound.
        if sv_xr_left_wrist_import_v1(&value, UInt32(data.count)) != 0 {
            UserDefaults.standard.removeObject(forKey: Self.defaultsKey)
        }
        #endif
    }

    private static func readNative() -> LeftWristCalibrationFlow.Native {
        #if SV_GAME_RUNTIME
        var state = SVLeftWristState()
        guard sv_xr_left_wrist_state_v1(&state, UInt32(MemoryLayout<SVLeftWristState>.size)) == 0 else { return .init() }
        return .init(supported: state.flags & 1 != 0, eligible: state.flags & 2 != 0, nativeHandShown: state.flags & 16 != 0,
                     calibrationPresent: state.flags & 4 != 0, controllerFingerprintMatches: state.flags & 8 != 0,
                     session: state.session, resultSession: state.result_session, phase: state.phase, reason: state.reason,
                     failure: state.failure, samples: state.samples, requiredSamples: state.required_samples,
                     translationMetres: state.translation_m, rotationDegrees: state.rotation_degrees)
        #else
        return .init()
        #endif
    }
    private static func open() -> UInt64 {
        #if SV_GAME_RUNTIME
        sv_xr_left_wrist_open_v1()
        #else
        0
        #endif
    }
    private static func capture(_ session: UInt64) -> Int32 {
        #if SV_GAME_RUNTIME
        sv_xr_left_wrist_capture_v1(session)
        #else
        -3
        #endif
    }
    private static func close(_ session: UInt64) {
        #if SV_GAME_RUNTIME
        _ = sv_xr_left_wrist_close_v1(session)
        #endif
    }
    private static func commit(_ session: UInt64) -> Int32 {
        #if SV_GAME_RUNTIME
        sv_xr_left_wrist_commit_v1(session)
        #else
        -1
        #endif
    }
}

/// The temporary card shown in the existing controls window while immersive.
struct LeftWristCalibrationCard: View {
    @ObservedObject var model: LeftWristCalibrationModel
    var body: some View {
        VStack(spacing: 14) {
            Text("Left hand calibration").font(.headline)
            Text(model.cardText)
                .font(.callout).multilineTextAlignment(.center)
                .frame(maxWidth: .infinity, minHeight: 60)
            if !model.finished {
                HStack(spacing: 12) {
                    Button("Cancel", role: .cancel) { model.cancel() }
                        .frame(maxWidth: .infinity)
                    Button("Confirm") { model.confirm() }
                        .buttonStyle(.borderedProminent)
                        .disabled(!model.canConfirm)
                        .frame(maxWidth: .infinity)
                }
            }
        }
        .frame(width: 340)
        .padding(18)
        .glassBackgroundEffect()
    }
}

/// The single Settings action. Hidden unless the running build supports it.
struct LeftWristCalibrationSettingsRow: View {
    @ObservedObject var model: LeftWristCalibrationModel
    let closeSettings: () -> Void
    var body: some View {
        Group {
            if model.supported || model.flow.pending {
                Button(model.flow.pending ? "Cancel left-hand calibration" : "Calibrate left hand", systemImage: "hand.raised") {
                    if model.flow.pending { model.cancel() } else { model.request(); closeSettings() }
                }
                .disabled(model.cardVisible)
                Text(model.settingsText).font(.caption).foregroundStyle(.secondary)
            }
        }
        .onAppear { model.refresh() }
    }
}

import Combine
import Foundation
import GameController
import Metal
import OSLog
import QuartzCore
import UIKit

private let runtimeLog = Logger(subsystem: "com.zephyrstudios.santosvision", category: "NativeRuntime")

private struct NativeRuntimeResult: Sendable {
    let code: Int32
    let payload: String
}

/// All calls use their own buffers. Module loading never blocks the UI actor.
private enum NativeRuntime {
    static var isLinked: Bool {
        #if SV_GAME_RUNTIME
        true
        #else
        false
        #endif
    }

    static func probe() -> NativeRuntimeResult {
        #if SV_GAME_RUNTIME
        guard sv_runtime_abi_version() == 1 else {
            return NativeRuntimeResult(code: -1, payload: "{\"error\":\"Unsupported native runtime ABI\"}")
        }
        return readBuffer { sv_runtime_probe($0, $1) }
        #else
        return NativeRuntimeResult(code: -1, payload: "{\"error\":\"Native game runtime is not linked in the diagnostic build\"}")
        #endif
    }

    static func start(gameRoot: String, saveRoot: String, frozenDepthHandoff: Bool,
                      drawSetupOverhead: Bool) -> NativeRuntimeResult {
        #if SV_GAME_RUNTIME
        let configCode = sv_world_set_frozen_depth_transfer(frozenDepthHandoff ? 1 : 0)
        guard configCode == 0 else {
            return NativeRuntimeResult(code: configCode, payload: "Frozen-depth setting could not be applied before native startup (\(configCode)).")
        }
        let drawSetupCode = sv_runtime_set_draw_setup_overhead(drawSetupOverhead ? 1 : 0)
        guard drawSetupCode == 0 else {
            return NativeRuntimeResult(code: drawSetupCode, payload: "Draw-setup setting could not be applied before native startup (\(drawSetupCode)).")
        }
        return gameRoot.withCString { game in
            saveRoot.withCString { saves in
                readBuffer { sv_runtime_start(game, saves, $0, $1) }
            }
        }
        #else
        return NativeRuntimeResult(code: -1, payload: "Native game runtime is not linked.")
        #endif
    }

    static func status() -> NativeRuntimeResult {
        #if SV_GAME_RUNTIME
        return readBuffer { sv_runtime_status($0, $1) }
        #else
        return NativeRuntimeResult(code: -1, payload: "{\"phase\":\"unlinked\"}")
        #endif
    }

    static func captureNext(path: String) -> Int32 {
        #if SV_GAME_RUNTIME
        return path.withCString { sv_gpu_capture_next($0) }
        #else
        return -1
        #endif
    }

    static func setActive(_ active: Bool) -> Int32 {
        #if SV_GAME_RUNTIME
        return sv_runtime_set_active(active ? 1 : 0)
        #else
        return -1
        #endif
    }

    #if SV_GAME_RUNTIME
    private static func readBuffer(_ operation: (UnsafeMutablePointer<CChar>, Int) -> Int32) -> NativeRuntimeResult {
        var buffer = [CChar](repeating: 0, count: 65_536)
        let code = buffer.withUnsafeMutableBufferPointer { pointer in
            operation(pointer.baseAddress!, pointer.count)
        }
        let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
        return NativeRuntimeResult(code: code, payload: String(decoding: bytes, as: UTF8.self))
    }
    #endif
}

// BEGIN NativeSenseUtilityOwnership (standalone validation uses this actual block)
struct NativeSenseUtilityOwnership {
    private(set) var flatActions = Set<SantosSenseUtilityAction>()
    private(set) var flatInteractions = Set<SantosSenseUtilityAction>()
    private(set) var immersiveDirections = Set<SantosSenseUtilityAction>()
    private(set) var immersiveActive = false
    var directions: Set<SantosSenseUtilityAction> {
        immersiveDirections.union(flatActions.filter(\.isDirection))
    }
    var interactionActive: Bool { immersiveActive || !immersiveDirections.isEmpty || !flatActions.isEmpty || !flatInteractions.isEmpty }
    mutating func setFlat(_ action: SantosSenseUtilityAction, pressed: Bool) {
        if pressed { flatActions.insert(action) } else { flatActions.remove(action) }
    }
    mutating func setFlatInteraction(_ action: SantosSenseUtilityAction, pressed: Bool) {
        if pressed { flatInteractions.insert(action) } else { flatInteractions.remove(action) }
    }
    mutating func clearFlatPhoneInteractions() { flatInteractions = flatInteractions.filter { !$0.isDirection } }
    mutating func clearFlat() { flatActions.removeAll(); flatInteractions.removeAll() }
    mutating func setImmersiveDirection(_ action: SantosSenseUtilityAction, pressed: Bool) {
        guard action.isDirection else { return }
        if pressed { immersiveDirections.insert(action) } else { immersiveDirections.remove(action) }
    }
    mutating func setImmersiveActive(_ active: Bool) {
        immersiveActive = active
        if !active { immersiveDirections.removeAll() }
    }
    mutating func clear() {
        flatActions.removeAll(); flatInteractions.removeAll(); immersiveDirections.removeAll(); immersiveActive = false
    }
}
// END NativeSenseUtilityOwnership

// BEGIN NativePhoneTransitionPolicy (standalone validation uses this actual block)
struct NativePhoneTransitionPolicy {
    enum Step: Equatable { case ready, wait, close, blocked }
    let deadline: Double
    private(set) var closeCommands = 0
    private var closePoll: UInt64?
    private var nextCloseTime = -Double.infinity
    mutating func next(phone: Bool?, paused: Bool?, poll: UInt64?, active: Bool, now: Double) -> Step {
        guard active, now.isFinite, now < deadline else { return .blocked }
        guard let phone, let poll else { return .wait }
        if !phone {
            if let closePoll, poll <= closePoll { return .wait }
            return .ready
        }
        // Backspace in a GTA menu can dismiss settings. Never substitute menu
        // navigation for a verified gameplay-phone close.
        guard paused == false else { return .blocked }
        guard now >= nextCloseTime else { return .wait }
        if let closePoll, poll <= closePoll { return .wait }
        guard closeCommands < 3 else { return .blocked }
        closeCommands += 1
        closePoll = poll
        nextCloseTime = now + 0.5
        return .close
    }
}
// END NativePhoneTransitionPolicy

@MainActor
final class NativeRuntimeModel: ObservableObject {
    @Published private(set) var status = "The native game runtime has not been checked."
    @Published private(set) var nativeDetails = ""
    @Published private(set) var isBusy = false
    @Published private(set) var hasStarted = false
    @Published private(set) var startupAttempted = false
    @Published private(set) var probePassed = false
    @Published private(set) var engineFailed = false
    @Published private(set) var graphicsSettings = NativeGraphicsSettings()
    @Published private(set) var graphicsStatus = ""
    @Published private(set) var playSettings = NativePlaySettings()
    @Published private(set) var playSettingsStatus = ""
    @Published private(set) var adaptiveRenderingEnabled = RenderingOptimizations.storedAdaptive
    @Published private(set) var frozenDepthHandoffEnabled = RenderingOptimizations.storedFrozenDepth
    @Published private(set) var drawSetupOverheadEnabled = RenderingOptimizations.storedDrawSetup
    @Published private(set) var depthEdgeFillEnabled = GameWorldDepthEdgeFillPolicy.stored
    @Published private(set) var higherImmersiveResolutionEnabled = GameWorldDisplayQuality.stored
    @Published private(set) var windowSurfaceReady = false
    @Published private(set) var controllerSummary = "Controller not connected"
    @Published private(set) var menuPressCount: UInt64 = 0
    @Published var gameControlsVisible = false { didSet { updateNativeControlsOwnership() } }
    @Published var nativeSettingsPresented = false { didSet { updateNativeControlsOwnership() } }
    @Published private(set) var nativeControlsSelection = 0
    /// The Start panel sits beside GTA's own open menu and leaves it the controller.
    @Published private(set) var pauseControlsFollowGame = false
    @Published private(set) var gtaPauseMenuOpen = false
    private var pauseCompanion = NativePauseCompanionPolicy()
    @Published private(set) var senseGameplayUtilitiesVisible = false
    @Published private(set) var sensePhoneNavigationVisible = false
    @Published private(set) var windowGameplayUtilitiesVisible = false
    @Published private(set) var immersivePhonePreparationActive = false
    @Published private(set) var immersivePhonePreparationStatus = ""
    private var sensePhoneInstantiated = false
    private var sensePhoneContextKnown = false
    private var senseGameplayUIEligible: Bool?
    private var immersivePhonePreparationToken: UUID?
    @Published private(set) var nativeControlsStatus = ""
    private var fastMenuPolling: Task<Void, Never>?
    private var nativeMenuPressObserver = NativeMenuPressObserver()
    private var nativeControlsTask: Task<Void, Never>?
    private var nativeControlsRouter = NativeControlsRouter()
    private var nativeControlsActivationGate = NativeControlsActivationGate()
    private var pendingNativeControlsConfirmation: NativeControlsActivationGate.Pending?
    private var nativeControlsConfirmation: Task<Void, Never>?
    private var senseUtilityOwnership = NativeSenseUtilityOwnership()
    private var senseUtilityInteractionActive: Bool { senseUtilityOwnership.interactionActive }
    private var heldSenseUtilityDirections: Set<SantosSenseUtilityAction> = []
    private var releasedSenseUtilityDirections: [SantosSenseUtilityAction: Double] = [:]
    private var flatSenseUtilityPrimaryActions: UInt64 = 0
    private var flatSenseUtilityQueuedActions: UInt64 = 0
    private var flatSenseUtilityRejectedActions: UInt64 = 0
    private var flatSenseUtilityLastAction: UInt32 = 0
    private var flatSenseUtilityLastActionUptime: Double = 0
    private var pauseWindowActions: [Bool: (token: UUID, action: () -> Void)] = [:]
    let windowGameMode: Bool
    let isLinked = NativeRuntime.isLinked
    let gameRoot: URL
    let saveRoot: URL
    let reportURL: URL
    private let reportSessionID = UUID().uuidString
    let cloudSaves: CloudSaveModel
    /// Explicit left grip-to-native-hand calibration (Settings action + card).
    let leftWrist = LeftWristCalibrationModel()
    /// Mirrors the card so scenes and modal ownership react to it.
    @Published private(set) var leftWristCardVisible = false
    private var leftWristObservation: AnyCancellable?
    @Published private(set) var isQuitting = false
    private var didBootstrap = false
    private var polling: Task<Void, Never>?
    private var simulatedPoseTask: Task<Void, Never>?
    private var captureTask: Task<Void, Never>?
    private var lastProbe: NativeRuntimeResult?
    private var lastStart: NativeRuntimeResult?
    private var lastStatus: NativeRuntimeResult?
    private var captureReturnCode: Int32?
    private var activeReturnCode: Int32?
    private var isActive = true
    private var appSceneIsActive = true
    private var windowIsAttached = false
    private var windowSceneIsActive = true
    private var immersiveSceneIsActive = false
    private var nativeControlsSceneIsActive = false
    private var capturePaths = Set<String>()
    private var captureRequests: [[String: Any]] = []
    private var compositorOwnsHeadTracking = false
    private var worldPresentation: [String: Any]?
    private var worldGpuTiming: [String: Any]?
    private var worldLifecycle: [String: Any] = [:]
    private var worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
    private var metalCapabilities: [String: Any] = [:]
    private var graphicsStartupError: String?
    private var graphicsConfigWritten = false
    private var playSettingsStartupError: String?

    var graphicsConfigURL: URL { saveRoot.appendingPathComponent("native-graphics.json") }
    var canChangeGraphics: Bool { isLinked && !hasStarted && !isBusy }
    var canChangePlaySettings: Bool { isLinked && !startupAttempted && !hasStarted && !isBusy }
    var canChangeRenderingOptimizations: Bool { isLinked && !startupAttempted && !hasStarted && !isBusy }

    /// Only used by an explicit device QA launch; ordinary play is unchanged.
    var presentedWindowFrames: UInt64 {
        guard let bytes = lastStatus?.payload.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              let value = object["window_frames_presented"] as? NSNumber else { return 0 }
        return value.uint64Value
    }

    init(arguments: [String] = ProcessInfo.processInfo.arguments) {
        windowGameMode = !arguments.contains("--sv-open-game") && !arguments.contains("--sv-open-tracking")
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        func value(_ flag: String) -> String? {
            guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else { return nil }
            return arguments[index + 1]
        }
        gameRoot = value("--sv-game-root").map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? documents.appendingPathComponent("game", isDirectory: true)
        saveRoot = value("--sv-save-root").map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? documents.appendingPathComponent("saves", isDirectory: true)
        reportURL = value("--sv-report").map { URL(fileURLWithPath: $0) }
            ?? documents.appendingPathComponent("native-runtime-report.json")
        cloudSaves = CloudSaveModel(saveRoot: saveRoot)
        do {
            graphicsSettings = try NativeGraphicsSettings.load(from: graphicsConfigURL)
            try graphicsSettings.apply(arguments: arguments)
            graphicsStatus = graphicsSettings.summary
        } catch {
            graphicsStartupError = error.localizedDescription
            graphicsStatus = "Graphics settings need attention: \(error.localizedDescription)"
        }
        do {
            playSettings = try NativePlaySettings.load(from: saveRoot)
        } catch {
            playSettingsStartupError = error.localizedDescription
            playSettingsStatus = "Sound and controls need attention: \(error.localizedDescription)"
        }
        if let device = MTLCreateSystemDefaultDevice() { recordMetalCapabilities(device) }
        SpatialGamepadInput.shared.start()
        leftWristObservation = leftWrist.$flow.map(\.cardVisible).removeDuplicates().dropFirst().sink { [weak self] visible in
            guard let self else { return }
            self.leftWristCardVisible = visible
            self.updateNativeControlsOwnership()
        }
        if !isLinked { status = "This build contains the native immersive tracking test. Build game mode to link the GTA engine." }
    }

    var hasGameData: Bool {
        FileManager.default.fileExists(atPath: gameRoot.appendingPathComponent("data/manifest.json").path)
            && FileManager.default.fileExists(atPath: gameRoot.appendingPathComponent("b").path)
    }

    func attachWindowLayer(_ layer: CAMetalLayer) -> Bool {
        guard windowGameMode, !windowSurfaceReady, !startupAttempted else { return false }
        #if SV_GAME_RUNTIME
        let code = sv_runtime_set_window_layer(Unmanaged.passUnretained(layer).toOpaque())
        guard code == 0 else {
            status = "The game window could not start (\(code)). Quit and reopen to retry."
            recordReport()
            return false
        }
        windowSurfaceReady = true
        return true
        #else
        return false
        #endif
    }

    func bootstrap() async {
        guard !didBootstrap else { return }
        didBootstrap = true
        let arguments = ProcessInfo.processInfo.arguments
        if isLinked || arguments.contains("--sv-probe-runtime") { await probe() }
        if arguments.contains("--sv-start-runtime"), probePassed { await start() }
    }

    func probe() async {
        guard !isBusy else { return }
        isBusy = true
        status = "Loading the embedded native game module…"
        let result = await Task.detached(priority: .userInitiated) { NativeRuntime.probe() }.value
        lastProbe = result
        nativeDetails = result.payload
        probePassed = result.code == 0
        status = probePassed ? "The embedded game module loaded in the native visionOS runtime."
            : "Native game module loading failed. See the runtime details below."
        isBusy = false
        recordReport()
    }

    func start() async {
        guard !isBusy, isLinked, probePassed, !hasStarted, isActive,
              !windowGameMode || windowSurfaceReady else { return }
        guard hasGameData else {
            status = "Game data is missing from this app’s game folder."
            recordReport()
            return
        }
        if let graphicsStartupError {
            status = "Choose supported graphics settings before starting. \(graphicsStartupError)"
            recordReport()
            return
        }
        if let playSettingsStartupError {
            status = "Check sound and controller settings before starting. \(playSettingsStartupError)"
            recordReport()
            return
        }
        isBusy = true
        guard await cloudSaves.prepareForLaunch() else {
            status = cloudSaves.status
            isBusy = false
            recordReport()
            return
        }
        do {
            try persistGraphics()
            try playSettings.save(to: saveRoot)
        } catch {
            status = "Could not save game settings: \(error.localizedDescription)"
            isBusy = false
            recordReport()
            return
        }
        status = "Starting the native GTA engine…"
        startupAttempted = true
        let game = gameRoot.path, saves = saveRoot.path
        let frozenDepthHandoff = frozenDepthHandoffEnabled
        let drawSetupOverhead = drawSetupOverheadEnabled
        let result = await Task.detached(priority: .userInitiated) {
            NativeRuntime.start(gameRoot: game, saveRoot: saves, frozenDepthHandoff: frozenDepthHandoff,
                                drawSetupOverhead: drawSetupOverhead)
        }.value
        lastStart = result
        hasStarted = result.code == 0
        nativeDetails = result.payload
        status = hasStarted ? "Native engine startup was accepted. Checking the engine state…"
            : "Native engine startup failed. See the runtime details below."
        isBusy = false
        recordReport()
        if hasStarted {
            await cloudSaves.gameplayStarted()
            leftWrist.engineStarted()
            activeReturnCode = NativeRuntime.setActive(isActive)
            configureCaptures()
            startPolling()
            if isActive { resumeHeadTracking() }
        }
    }

    func setGraphicsResolution(_ resolution: NativeGraphicsSettings.Resolution) {
        guard canChangeGraphics else { return }
        graphicsSettings.width = resolution.dimensions.width
        graphicsSettings.height = resolution.dimensions.height
        saveGraphicsSelection()
    }

    func setMasterVolume(_ volume: Double) {
        guard canChangePlaySettings, volume.isFinite, (0...1).contains(volume) else { return }
        playSettings.masterVolume = volume
        savePlaySettings()
    }

    func setLookSensitivity(_ percent: Double) {
        guard canChangePlaySettings, percent.isFinite, (0...100).contains(percent) else { return }
        playSettings.lookSensitivityPercent = percent
        savePlaySettings()
    }

    func setDeveloperMode(_ enabled: Bool) {
        guard canChangePlaySettings else { return }
        playSettings.developerMode = enabled
        savePlaySettings()
    }

    private func savePlaySettings() {
        do {
            try playSettings.save(to: saveRoot)
            playSettingsStartupError = nil
            playSettingsStatus = ""
        } catch {
            playSettingsStartupError = error.localizedDescription
            playSettingsStatus = "Could not save sound and controls: \(error.localizedDescription)"
        }
        recordReport()
    }

    func setGraphicsFrameCap(_ cap: Int) {
        guard canChangeGraphics, cap == 30 || cap == 60 else { return }
        graphicsSettings.fpsLimit = cap
        saveGraphicsSelection()
    }
    func setGraphicsPreset(_ preset: NativeGraphicsSettings.Preset) {
        guard canChangeGraphics else { return }
        graphicsSettings.graphicsPreset = preset.rawValue
        saveGraphicsSelection()
    }

    func setAdaptiveRendering(_ enabled: Bool) {
        guard canChangeRenderingOptimizations else { return }
        adaptiveRenderingEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: RenderingOptimizations.adaptiveKey)
        recordReport()
    }

    func setFrozenDepthHandoff(_ enabled: Bool) {
        guard canChangeRenderingOptimizations else { return }
        frozenDepthHandoffEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: RenderingOptimizations.frozenDepthKey)
        recordReport()
    }

    func setDrawSetupOverhead(_ enabled: Bool) {
        guard canChangeRenderingOptimizations else { return }
        drawSetupOverheadEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: RenderingOptimizations.drawSetupKey)
        recordReport()
    }

    func setDepthEdgeFill(_ enabled: Bool) {
        guard canChangeImmersiveDisplay else { return }
        depthEdgeFillEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: GameWorldDepthEdgeFillPolicy.storedKey)
        recordReport()
    }

    var canChangeImmersiveDisplay: Bool { !compositorOwnsHeadTracking && !isQuitting }

    func setHigherImmersiveResolution(_ enabled: Bool) {
        guard canChangeImmersiveDisplay else { return }
        higherImmersiveResolutionEnabled = enabled
        UserDefaults.standard.set(enabled, forKey: GameWorldDisplayQuality.storedKey)
        recordReport()
    }

    private func saveGraphicsSelection() {
        do {
            try persistGraphics()
            graphicsStartupError = nil
        } catch {
            graphicsStartupError = error.localizedDescription
            graphicsStatus = "Could not save graphics settings: \(error.localizedDescription)"
        }
        recordReport()
    }

    private func persistGraphics() throws {
        try graphicsSettings.save(to: graphicsConfigURL)
        graphicsConfigWritten = true
        graphicsStatus = graphicsSettings.summary
        runtimeLog.info("Saved graphics configuration: \(self.graphicsConfigURL.path, privacy: .public)")
    }

    func setActive(_ active: Bool) {
        appSceneIsActive = active
        updateActivity()
    }

    func setWindowSceneActive(_ active: Bool) {
        windowSceneIsActive = active
        updateActivity()
    }

    /// setWindowAttached comes from the real retained GameMetalView and the
    /// scene phase comes from its containing window, not an openWindow request.
    var windowReadyForReturn: Bool { windowIsAttached && windowSceneIsActive }
    var appActiveForSceneTransitions: Bool { appSceneIsActive }
    var immersiveWindowReturnNeedsDismissal: Bool { compositorOwnsHeadTracking || immersiveSceneIsActive }

    func setImmersiveSceneActive(_ active: Bool) {
        immersiveSceneIsActive = active
        updateActivity()
    }

    func setNativeControlsSceneActive(_ active: Bool) {
        nativeControlsSceneIsActive = active
        updateActivity()
    }

    private var nativeControlsModal: Bool {
        // The calibration card blocks gameplay input like the app pause panel;
        // pad.rs then withholds Start/Escape from GTA as well.
        nativeSettingsPresented || leftWristCardVisible || (gameControlsVisible && !pauseControlsFollowGame)
    }

    private func updateNativeControlsOwnership() {
        if !gameControlsVisible { pauseCompanion.closed() }
        let companion = gameControlsVisible
            && !pauseCompanion.panelOwnsController(visible: true, now: ProcessInfo.processInfo.systemUptime)
        if pauseControlsFollowGame != companion { pauseControlsFollowGame = companion }
        let modal = nativeControlsModal
        NativeControlsOwnership.shared.set(modal: modal || immersivePhonePreparationActive, utility: senseUtilityInteractionActive)
        nativeControlsRouter = NativeControlsRouter()
        nativeControlsSelection = 0
        nativeControlsActivationGate.cancel()
        pendingNativeControlsConfirmation = nil
        nativeControlsConfirmation?.cancel()
        nativeControlsConfirmation = nil
        updateSenseUtilitiesVisibility()
        nativeControlsTask?.cancel()
        nativeControlsTask = nil
        guard modal || companion else { return }
        nativeControlsTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let input = SpatialGamepadInput.shared.nativeControlsInput()
                guard modal else {
                    // GTA's menu owns this input. Only remember Cross, so a
                    // coincident system Button event cannot also hit the panel.
                    self.pauseCompanion.observeConfirm(input.confirm, now: ProcessInfo.processInfo.systemUptime)
                    do { try await Task.sleep(for: .milliseconds(16)) } catch { return }
                    continue
                }
                let event = self.nativeControlsRouter.update(input, selection: self.nativeControlsSelection,
                    now: ProcessInfo.processInfo.systemUptime)
                self.nativeControlsSelection = event.selection
                if self.nativeSettingsPresented {
                    if event.dismiss { self.nativeSettingsPresented = false }
                } else if self.leftWristCardVisible, !self.gameControlsVisible {
                    // Circle or Start cancels the card; Confirm is only a real
                    // gaze/trigger hit on its button.
                    if event.dismiss { self.leftWrist.cancel() }
                } else if event.dismiss {
                    self.performPauseAction(.resume)
                } else {
                    if event.activate, let action = SantosControlAction(rawValue: event.selection) {
                        self.pendingNativeControlsConfirmation = self.nativeControlsActivationGate.deferController(action,
                            now: ProcessInfo.processInfo.systemUptime)
                    }
                    if event.confirmReleased, let activation = self.pendingNativeControlsConfirmation {
                        self.pendingNativeControlsConfirmation = nil
                        self.nativeControlsConfirmation?.cancel()
                        self.nativeControlsConfirmation = Task { @MainActor [weak self] in
                            // UIKit Button actions happen on release, including
                            // held gaze clicks. Give that real hit first choice.
                            do { try await Task.sleep(for: .milliseconds(80)) } catch { return }
                            guard let self, self.gameControlsVisible, !self.nativeSettingsPresented,
                                  let selected = self.nativeControlsActivationGate.resolve(activation,
                                      now: ProcessInfo.processInfo.systemUptime) else { return }
                            self.performPauseAction(selected)
                        }
                    }
                }
                do { try await Task.sleep(for: .milliseconds(16)) } catch { return }
            }
        }
    }

    func registerPauseWindowAction(token: UUID, immersive: Bool, action: @escaping () -> Void) {
        pauseWindowActions[immersive] = (token, action)
    }

    func unregisterPauseWindowAction(token: UUID, immersive: Bool) {
        if pauseWindowActions[immersive]?.token == token { pauseWindowActions.removeValue(forKey: immersive) }
    }

    func activatePauseButton(_ action: SantosControlAction) {
        guard gameControlsVisible, !nativeSettingsPresented, !isQuitting else { return }
        if pauseControlsFollowGame {
            guard SantosControlAction.companionActions(immersive: compositorOwnsHeadTracking).contains(action),
                  pauseCompanion.acceptsCompanionActivation(now: ProcessInfo.processInfo.systemUptime) else { return }
        }
        guard nativeControlsActivationGate.direct(now: ProcessInfo.processInfo.systemUptime) else { return }
        pendingNativeControlsConfirmation = nil
        nativeControlsConfirmation?.cancel()
        nativeControlsConfirmation = nil
        nativeControlsSelection = action.rawValue
        performPauseAction(action)
    }

    private func performPauseAction(_ action: SantosControlAction) {
        guard hasStarted, !isQuitting else { return }
        switch action {
        case .resume:
            guard sendMenuAction(13) else { return }
            gameControlsVisible = false
        case .window:
            guard let handler = pauseWindowActions[compositorOwnsHeadTracking] else {
                nativeControlsStatus = "Window is not ready. Try Resume, then Start."
                return
            }
            _ = sendMenuAction(13)
            handler.action()
        case .settings:
            nativeSettingsPresented = true
        case .quit:
            Task { await quitGame() }
        }
    }

    func setSenseUtilityInteractionActive(_ active: Bool) {
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.setImmersiveActive(active)
        synchronizeSenseUtilityDirections()
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
    }

    /// Flat buttons are independent owners. Releasing one must not release a
    /// second flat button or the same direction held in the immersive scene.
    func setFlatSenseUtilityPressed(_ action: SantosSenseUtilityAction, pressed: Bool) {
        if pressed, !senseGameplayUtilitiesVisible { return }
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.setFlat(action, pressed: pressed)
        synchronizeSenseUtilityDirections()
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
    }

    func clearFlatSenseUtilityPressed() {
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.clearFlat()
        synchronizeSenseUtilityDirections()
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
    }

    /// A native arrow is a Peel-style one-shot key press, while its touch
    /// lifecycle still owns controller input. It must never create a held key.
    func setFlatSenseUtilityInteractionPressed(_ action: SantosSenseUtilityAction, pressed: Bool) {
        if pressed, !(windowGameplayUtilitiesVisible && isActive) || immersivePhonePreparationActive
            || (action.isDirection && !sensePhoneNavigationVisible) { return }
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.setFlatInteraction(action, pressed: pressed)
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
    }

    func clearFlatSensePhoneInteractions() {
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.clearFlatPhoneInteractions()
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
    }

    /// Called only by the flat system control's primary-action event. Counts
    /// distinguish UIKit delivery from a native queue/lifecycle rejection.
    func activateFlatSenseUtility(_ action: SantosSenseUtilityAction) {
        flatSenseUtilityPrimaryActions &+= 1
        flatSenseUtilityLastAction = action.rawValue
        flatSenseUtilityLastActionUptime = ProcessInfo.processInfo.systemUptime
        if performSenseUtility(action) { flatSenseUtilityQueuedActions &+= 1 }
        else { flatSenseUtilityRejectedActions &+= 1 }
    }

    /// Returns true for directions even when a lifecycle gate rejects the hold,
    /// so release handlers never turn a rejected/finished hold into a tap.
    @discardableResult
    func setSenseUtilityPressed(_ action: SantosSenseUtilityAction, pressed: Bool) -> Bool {
        guard action.isDirection else { return false }
        if pressed, !senseGameplayUtilitiesVisible { return true }
        let owned = senseUtilityInteractionActive
        senseUtilityOwnership.setImmersiveDirection(action, pressed: pressed)
        synchronizeSenseUtilityDirections()
        if owned != senseUtilityInteractionActive { updateNativeControlsOwnership() }
        return true
    }

    private func synchronizeSenseUtilityDirections() {
        let wanted = senseUtilityOwnership.directions
        for action in heldSenseUtilityDirections.subtracting(wanted) {
            releasedSenseUtilityDirections[action] = ProcessInfo.processInfo.systemUptime
            #if SV_GAME_RUNTIME
            _ = sv_native_utility_dpad_v1(action.rawValue, 0)
            #endif
        }
        for action in wanted.subtracting(heldSenseUtilityDirections) {
            #if SV_GAME_RUNTIME
            _ = sv_native_utility_dpad_v1(action.rawValue, 1)
            #endif
        }
        heldSenseUtilityDirections = wanted
    }

    private func clearSenseUtilityDirections() {
        senseUtilityOwnership.clear()
        synchronizeSenseUtilityDirections()
        NativeControlsOwnership.shared.set(modal: nativeControlsModal || immersivePhonePreparationActive, utility: false)
    }

    private func refreshSenseUtilityDirections() {
        guard senseGameplayUtilitiesVisible else { clearSenseUtilityDirections(); return }
        #if SV_GAME_RUNTIME
        for action in heldSenseUtilityDirections { _ = sv_native_utility_dpad_v1(action.rawValue, 1) }
        #endif
    }

    @discardableResult
    func performSenseUtility(_ action: SantosSenseUtilityAction) -> Bool {
        guard (senseGameplayUtilitiesVisible || windowGameplayUtilitiesVisible), isActive,
              !immersivePhonePreparationActive else { return false }
        guard !action.isDirection || sensePhoneNavigationVisible else { return false }
        if action.isDirection, heldSenseUtilityDirections.contains(action)
            || ProcessInfo.processInfo.systemUptime - (releasedSenseUtilityDirections[action] ?? -.infinity) < 0.2 {
            return true // Button release must not append a second tap after its hold.
        }
        let command: UInt32 = action == .phone && sensePhoneNavigationVisible ? 12 : action.rawValue
        return sendMenuAction(command) // Phone toggles with the original Backspace action.
    }

    private func updateSenseUtilitiesVisibility() {
        let spatial = SpatialGamepadInput.shared.report
        let connected = spatial["left_controls_available"] as? Bool == true
            || spatial["right_controls_available"] as? Bool == true
        let ready = hasStarted && !engineFailed && !isQuitting && !gameControlsVisible && !nativeSettingsPresented
        // Phone close remains reachable with DualSense, Sense or hand pinch.
        // Inactive focus keeps the flat ornament present; accepting an action
        // still requires the real active lifecycle above.
        // Only a fresh native local-ped death observation can hide gameplay
        // utilities. Unknown state keeps them available; an open phone always
        // retains its escape controls.
        windowGameplayUtilitiesVisible = ready && (connected || sensePhoneInstantiated)
            && (senseGameplayUIEligible != false || sensePhoneInstantiated)
        senseGameplayUtilitiesVisible = windowGameplayUtilitiesVisible && isActive
        sensePhoneNavigationVisible = windowGameplayUtilitiesVisible && sensePhoneInstantiated
        if !senseGameplayUtilitiesVisible { clearSenseUtilityDirections() }
        else if !sensePhoneNavigationVisible { clearFlatSensePhoneInteractions() }
    }

    /// Close only a freshly observed gameplay phone, then require a new native
    /// poll to confirm it is gone before changing presentation ownership.
    func preparePhoneForImmersiveTransition() async -> Bool {
        guard hasStarted, !engineFailed, !isQuitting, isActive, !immersivePhonePreparationActive else { return false }
        let token = UUID()
        immersivePhonePreparationToken = token
        immersivePhonePreparationActive = true
        immersivePhonePreparationStatus = "Checking the game phone before immersive view…"
        clearSenseUtilityDirections()
        updateNativeControlsOwnership()
        defer {
            immersivePhonePreparationToken = nil
            immersivePhonePreparationActive = false
            updateNativeControlsOwnership()
        }
        var policy = NativePhoneTransitionPolicy(deadline: ProcessInfo.processInfo.systemUptime + 4)
        while immersivePhonePreparationToken == token, !Task.isCancelled {
            let result = await Task.detached { NativeRuntime.status() }.value
            let object = result.payload.data(using: .utf8)
                .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
            let controller = object?["controller"] as? [String: Any]
            let phone = result.code == 0 ? controller?["phone_instantiated"] as? Bool : nil
            let paused = controller?["menu_navigation"] as? Bool
            let poll = (controller?["poll_samples"] as? NSNumber)?.uint64Value
            guard immersivePhonePreparationToken == token, !Task.isCancelled else { break }
            if result.code == 0 {
                lastStatus = result
                nativeDetails = result.payload
                sensePhoneContextKnown = phone != nil
                if let phone { sensePhoneInstantiated = phone }
            }
            switch policy.next(phone: phone, paused: paused, poll: poll,
                               active: isActive && !engineFailed && !isQuitting,
                               now: ProcessInfo.processInfo.systemUptime) {
            case .ready:
                sensePhoneInstantiated = false
                sensePhoneContextKnown = true
                immersivePhonePreparationStatus = ""
                updateSenseUtilitiesVisibility()
                return true
            case .close:
                immersivePhonePreparationStatus = "Putting the phone away before immersive view…"
                guard sendMenuAction(12) else { return false }
            case .wait: break
            case .blocked:
                immersivePhonePreparationStatus = "Close the phone in the game, then try immersive view again."
                return false
            }
            do { try await Task.sleep(for: .milliseconds(100)) } catch { break }
        }
        immersivePhonePreparationStatus = "Immersive view cancelled. The game is still in its window."
        return false
    }

    func cancelPhonePreparation() { immersivePhonePreparationToken = nil }

    func setWindowAttached(_ attached: Bool) {
        windowIsAttached = attached
        // UIKit attachment callbacks can run during SwiftUI construction.
        // Native suspension is immediate; observable/report updates defer.
        updateActivity(deferReport: true)
    }

    private func updateActivity(deferReport: Bool = false) {
        let active = !isQuitting && (nativeControlsSceneIsActive || (compositorOwnsHeadTracking && immersiveSceneIsActive)
            || (appSceneIsActive && (!windowGameMode || (windowIsAttached && windowSceneIsActive))))
        let changed = active != isActive
        if active != isActive {
            worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
            worldGpuTiming = nil
        }
        isActive = active
        if !active, senseUtilityInteractionActive { clearSenseUtilityDirections() }
        updateSenseUtilitiesVisibility()
        #if SV_GAME_RUNTIME
        _ = sv_runtime_set_window_visible(windowIsAttached && windowSceneIsActive ? 1 : 0)
        #endif
        if !active { suspendHeadTracking(); worldPresentation = nil; worldGpuTiming = nil }
        // Repeating the native setter invalidates the current XR frame epoch.
        if isLinked, changed { activeReturnCode = NativeRuntime.setActive(active) }
        if active {
            if hasStarted { resumeHeadTracking() }
            else if probePassed, ProcessInfo.processInfo.arguments.contains("--sv-start-runtime") {
                Task { await start() }
            }
        }
        if deferReport {
            Task { @MainActor [weak self] in self?.recordReport() }
        } else {
            recordReport()
        }
    }

    func suspendHeadTracking() {
        simulatedPoseTask?.cancel()
        simulatedPoseTask = nil
        XRHeadTracking.shared.suspend()
    }

    func resumeHeadTracking() {
        guard isActive else { return }
        guard !windowGameMode || compositorOwnsHeadTracking else {
            suspendHeadTracking()
            #if SV_GAME_RUNTIME
            _ = sv_xr_world_camera_set_enabled(0)
            #endif
            return
        }
        XRHeadTracking.shared.resume()
        guard !compositorOwnsHeadTracking, hasStarted, simulatedPoseTask == nil,
              let fixture = SimulatorPoseFixture.shared else { return }
        runtimeLog.info("Starting explicit simulator headset pose fixture. Real headset tracking is not being tested.")
        simulatedPoseTask = Task {
            while !Task.isCancelled {
                let timestamp = ProcessInfo.processInfo.systemUptime
                XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: fixture.pose(at: timestamp), tracked: true)
                do { try await Task.sleep(for: .milliseconds(16)) } catch { return }
            }
        }
    }

    func beginWorldTracking() {
        compositorOwnsHeadTracking = true
        simulatedPoseTask?.cancel()
        simulatedPoseTask = nil
        worldPresentation = nil
        worldGpuTiming = nil
        worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
        updateActivity()
        if isActive { XRHeadTracking.shared.resume() }
        recordReport()
    }

    func endWorldTracking() {
        guard compositorOwnsHeadTracking else { return }
        compositorOwnsHeadTracking = false
        worldPresentation = nil
        worldGpuTiming = nil
        worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
        suspendHeadTracking()
        updateActivity()
        recordReport()
    }

    @discardableResult
    func sendMenuAction(_ action: UInt32) -> Bool {
        #if SV_GAME_RUNTIME
        let code = sv_runtime_menu_action(action)
        nativeControlsStatus = code == 0 ? "" : "Game action could not be queued (\(code))."
        return code == 0
        #else
        nativeControlsStatus = "Game runtime is not linked."
        return false
        #endif
    }

    func quitGame() async {
        guard !isQuitting else { return }
        isQuitting = true
        setActive(false)
        setImmersiveSceneActive(false)
        guard await cloudSaves.finishBackup() else {
            isQuitting = false
            status = cloudSaves.status
            gameControlsVisible = true
            setActive(true)
            if compositorOwnsHeadTracking { setImmersiveSceneActive(true) }
            recordReport()
            return
        }
        Darwin.exit(EXIT_SUCCESS)
    }

    #if SV_GAME_RUNTIME
    func recordWorldState(_ value: [String: Any]) {
        worldLifecycle = value
        recordReport()
    }

    func recordWorldPresentation(_ presentation: GameWorldPresentation) {
        guard isActive, compositorOwnsHeadTracking else { return }
        worldPresentation = ["view_count": presentation.viewCount,
                             "game_frames": presentation.gameFrames,
                             "request_frames": presentation.requestFrames,
                             "head_frames": presentation.headFrames,
                             "completed_at": ISO8601DateFormatter().string(from: Date())]
        recordReport()
    }

    func recordWorldGpuTiming(_ timing: GameWorldGpuTimingSummary) {
        guard isActive, compositorOwnsHeadTracking,
              timing.startUptime >= worldGpuTimingNotBeforeUptime else { return }
        if let lastEnd = worldGpuTiming?["window_end_uptime_seconds"] as? Double,
           timing.endUptime <= lastEnd { return }
        worldGpuTiming = timing.reportValue
        recordReport()
    }
    #endif

    func recordMetalCapabilities(_ device: any MTLDevice) {
        metalCapabilities = ["device_name": device.name, "registry_id": device.registryID,
                             "apple3": device.supportsFamily(.apple3),
                             "mac1": device.supportsFamily(.mac1), "mac2": device.supportsFamily(.mac2),
                             "common3": device.supportsFamily(.common3),
                             "float32_filtering": device.supports32BitFloatFiltering,
                             "bc_texture_compression": device.supportsBCTextureCompression]
    }

    func captureCurrentFrame() {
        guard hasStarted else { return }
        requestCapture(base: reportURL.deletingLastPathComponent().appendingPathComponent("game-frame.ppm"), numbered: true)
    }

    private func configureCaptures() {
        let arguments = ProcessInfo.processInfo.arguments
        func argument(_ flag: String) -> String? {
            guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else { return nil }
            return arguments[index + 1]
        }
        guard let path = argument("--sv-capture") else { return }
        let delay = Double(argument("--sv-capture-delay") ?? "0")
        let count = Int(argument("--sv-capture-count") ?? "1")
        let interval = Double(argument("--sv-capture-interval") ?? "10")
        guard let delay, delay.isFinite, delay >= 0,
              let count, (1...20).contains(count),
              let interval, interval.isFinite, interval > 0 else {
            runtimeLog.error("Invalid capture options: delay must be nonnegative, count must be 1...20, interval must be positive.")
            return
        }
        let base = URL(fileURLWithPath: path)
        captureTask = Task {
            if delay > 0 {
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
            }
            for index in 0..<count {
                guard !Task.isCancelled else { return }
                requestCapture(base: base, numbered: count > 1)
                if index + 1 < count {
                    do { try await Task.sleep(for: .seconds(interval)) } catch { return }
                }
            }
        }
    }

    private func requestCapture(base: URL, numbered: Bool) {
        var candidate = base
        var number = 1
        while numbered || capturePaths.contains(candidate.path) || FileManager.default.fileExists(atPath: candidate.path) {
            let suffix = String(format: "-%03d", number)
            candidate = URL(fileURLWithPath: base.deletingPathExtension().path + suffix + "." + (base.pathExtension.isEmpty ? "ppm" : base.pathExtension))
            if !capturePaths.contains(candidate.path), !FileManager.default.fileExists(atPath: candidate.path) { break }
            number += 1
        }
        capturePaths.insert(candidate.path)
        let code = NativeRuntime.captureNext(path: candidate.path)
        captureReturnCode = code
        captureRequests.append(["path": candidate.path, "return_code": code,
                                "timestamp": ISO8601DateFormatter().string(from: Date())])
        runtimeLog.info("Game frame capture requested: \(candidate.path, privacy: .public), return=\(code)")
        recordReport()
    }

    private func observeMenuPressCount(_ presses: UInt64) {
        guard let event = nativeMenuPressObserver.consume(presses) else { return }
        menuPressCount = event.count
        if event.toggle {
            let visible = pauseCompanion.startPressed(visible: gameControlsVisible,
                                                      now: ProcessInfo.processInfo.systemUptime)
            if visible != gameControlsVisible { gameControlsVisible = visible }
        }
        Task { await cloudSaves.backupNow() }
    }

    /// The panel closes with GTA's own menu; grace expiry and GTA's menu
    /// opening change the controller owner without changing visibility.
    private func observeGuestMenu(_ raw: UInt32) {
        let guest = NativePauseCompanionPolicy.GuestMenu(rawValue: raw) ?? .unknown
        if gtaPauseMenuOpen != (guest == .paused) { gtaPauseMenuOpen = guest == .paused }
        let visible = pauseCompanion.observe(guest,
                                             visible: gameControlsVisible)
        if visible != gameControlsVisible { gameControlsVisible = visible; return }
        let companion = gameControlsVisible
            && !pauseCompanion.panelOwnsController(visible: true, now: ProcessInfo.processInfo.systemUptime)
        if companion != pauseControlsFollowGame { updateNativeControlsOwnership() }
    }

    private func startFastMenuPolling() {
        fastMenuPolling?.cancel()
        fastMenuPolling = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                #if SV_GAME_RUNTIME
                self.observeMenuPressCount(sv_native_menu_press_count_v1())
                self.observeGuestMenu(sv_native_menu_context_v1())
                #endif
                self.refreshSenseUtilityDirections()
                do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
            }
        }
    }

    private func startPolling() {
        startFastMenuPolling()
        polling?.cancel()
        polling = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let result = await Task.detached { NativeRuntime.status() }.value
                self.lastStatus = result
                self.nativeDetails = result.payload
                if let data = result.payload.data(using: .utf8),
                   let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   let state = object["state"] as? String {
                    if let controller = object["controller"] as? [String: Any],
                       let presses = (controller["menu_press_count"] as? NSNumber)?.uint64Value,
                       presses > self.menuPressCount {
                        self.observeMenuPressCount(presses)
                    }
                    self.engineFailed = state == "failed"
                    self.status = "Native engine state: \(state)."
                    if let error = object["last_error"] as? String {
                        self.status += " \(error)"
                    }
                }
                self.recordReport()
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
            }
        }
    }

    private func recordReport() {
        func payload(_ result: NativeRuntimeResult?) -> Any {
            guard let result else { return NSNull() }
            let parsed: Any = result.payload.data(using: .utf8)
                .flatMap { try? JSONSerialization.jsonObject(with: $0) } ?? result.payload
            return ["return_code": result.code, "payload": parsed] as [String: Any]
        }
        let nativeStatus = lastStatus?.payload.data(using: .utf8)
            .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
        senseGameplayUIEligible = (nativeStatus?["immersive_aim"] as? [String: Any])?["gameplay_ui_eligible"] as? Bool
        // Unknown context cannot certify that a previously open phone closed.
        // Retain its escape controls until the native getter reports false.
        let phone = (nativeStatus?["controller"] as? [String: Any])?["phone_instantiated"] as? Bool
        sensePhoneContextKnown = phone != nil
        if let phone { sensePhoneInstantiated = phone }
        let controllers = GCController.controllers().filter { $0.extendedGamepad != nil }
        let controllerNames = controllers.map { $0.vendorName ?? "Game controller" }
        updateSenseUtilitiesVisibility()
        let spatialControllers = SpatialGamepadInput.shared.report
        let leftSpatial = spatialControllers["left_controls_available"] as? Bool == true
        let rightSpatial = spatialControllers["right_controls_available"] as? Bool == true
        controllerSummary = leftSpatial && rightSpatial ? "Spatial controls ready (left + right)"
            : leftSpatial ? "Left spatial controls ready; connect right"
            : rightSpatial ? "Right spatial controls ready; connect left"
            : controllerNames.first.map { "\($0) connected" } ?? "Controller not connected"
        // A historical completion is evidence, but the native publisher also
        // verifies a fresh ready frame and current lifecycle epoch.
        let currentWorldConnected = isActive && compositorOwnsHeadTracking && worldPresentation != nil
            && nativeStatus?["immersive_game_renderer_connected"] as? Bool == true
        let report: [String: Any] = [
            "schema": 1, "timestamp": ISO8601DateFormatter().string(from: Date()),
            "client_build": Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown",
            "client_version": Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown",
            "report_session_id": reportSessionID,
            "report_system_uptime_seconds": ProcessInfo.processInfo.systemUptime,
            "native_runtime_linked": isLinked, "game_root": gameRoot.path,
            "presentation_mode": compositorOwnsHeadTracking ? "immersive" : (windowGameMode ? "window" : "diagnostic"),
            "window_surface_ready": windowSurfaceReady,
            "window_scene_policy": "single_instance_main",
            "native_controls_owns_input": NativeControlsOwnership.shared.read(),
            "native_controls_input_policy_v2": NativeControlsOwnership.shared.readPolicy(),
            "native_controls_selection": nativeControlsSelection,
            "pause_controls_follow_game": pauseControlsFollowGame,
            "guest_menu_context": pauseCompanion.guest.rawValue,
            "gta_pause_menu_open": gtaPauseMenuOpen,
            "sense_gameplay_utilities_visible": senseGameplayUtilitiesVisible,
            "sense_gameplay_ui_eligible": senseGameplayUIEligible.map { $0 as Any } ?? NSNull(),
            "sense_phone_navigation_visible": sensePhoneNavigationVisible,
            "window_gameplay_utilities_visible": windowGameplayUtilitiesVisible,
            "phone_context_known": sensePhoneContextKnown,
            "immersive_phone_preparation_active": immersivePhonePreparationActive,
            "immersive_phone_preparation_status": immersivePhonePreparationStatus,
            "flat_sense_utility_ui": [
                "primary_action_events": flatSenseUtilityPrimaryActions,
                "queued_actions": flatSenseUtilityQueuedActions,
                "rejected_actions": flatSenseUtilityRejectedActions,
                "last_action": flatSenseUtilityLastAction,
                "last_action_uptime_seconds": flatSenseUtilityLastActionUptime,
                "system_select": "gaze_and_either_sense_trigger",
            ],
            "native_controls_status": nativeControlsStatus,
            "window_attached": windowIsAttached,
            "window_scene_active": windowSceneIsActive,
            "app_scene_active": appSceneIsActive,
            "immersive_scene_active": immersiveSceneIsActive,
            "compositor_owns_head_tracking": compositorOwnsHeadTracking,
            "connected_window_scene_count": UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.count,
            "controllers": ["connected": !controllers.isEmpty, "names": controllerNames],
            "spatial_controllers": spatialControllers,
            "save_root": saveRoot.path, "has_game_data": hasGameData,
            "save_sync_status": cloudSaves.status,
            "graphics_requested": graphicsSettings.reportValue,
            "play_settings_requested": playSettings.reportValue,
            "play_settings_status": playSettingsStatus,
            "graphics_config_path": graphicsConfigURL.path,
            "graphics_config_written": graphicsConfigWritten,
            "graphics_settings_status": graphicsStatus,
            "rendering_optimizations_requested": [
                "adaptive_mesh_and_sky_fan": adaptiveRenderingEnabled,
                "frozen_depth_handoff": frozenDepthHandoffEnabled,
                "draw_setup_overhead": drawSetupOverheadEnabled
            ],
            "rendering_optimizations_effective": [
                "adaptive_mesh": RenderingOptimizations.adaptiveMeshEnabled,
                "sky_fan": RenderingOptimizations.skyFanEnabled,
                "render_prepare": RenderingOptimizations.renderPrepareEnabled,
                "uniform_stage_pack": RenderingOptimizations.uniformStagePackEnabled,
                "bind_group_prepare": RenderingOptimizations.bindGroupPrepareEnabled,
                "uniform_snapshot": RenderingOptimizations.uniformSnapshotEnabled,
                "xr_emission_fastpath": RenderingOptimizations.xrEmissionFastpathEnabled,
                "metal_skip_empty_transit": RenderingOptimizations.metalSkipEmptyTransitEnabled,
                "metal_skip_empty_prepass": RenderingOptimizations.metalSkipEmptyPrepassEnabled
            ],
            "rendering_quality_requested": ["depth_edge_fill": depthEdgeFillEnabled,
                                            "vertical_reconstruction": GameWorldVerticalReconstructionPolicy.requested,
                                            "higher_display_resolution": higherImmersiveResolutionEnabled],
            "rendering_quality_effective": ["higher_display_resolution": GameWorldDisplayQuality.requested,
                "smooth_surface_continuity": GameWorldSurfaceContinuityPolicy.requested,
                "depth_edge_fill": GameWorldDepthEdgeFillPolicy.requested(
                environment: ProcessInfo.processInfo.environment, arguments: ProcessInfo.processInfo.arguments,
                stored: GameWorldDepthEdgeFillPolicy.stored)],
            "probe": payload(lastProbe), "start": payload(lastStart), "status": payload(lastStatus),
            "head_pose_source": windowGameMode && !compositorOwnsHeadTracking ? "disabled_in_window" : (SimulatorPoseFixture.shared == nil ? "ARKit" : "simulator_fixture"),
            "simulator_pose_fixture": SimulatorPoseFixture.shared?.reportValue ?? [:],
            "capture_request_return_code": captureReturnCode.map { $0 as Any } ?? NSNull(),
            "capture_requests": captureRequests,
            "app_active": isActive,
            "set_active_return_code": activeReturnCode.map { $0 as Any } ?? NSNull(),
            "metal_capabilities": metalCapabilities,
            "immersive_game_renderer_connected": currentWorldConnected,
            "immersive_lifecycle": worldLifecycle,
            "immersive_game_presentation": worldPresentation ?? [:],
            "immersive_compositor_gpu_timing": worldGpuTiming ?? [:]
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: reportURL, options: .atomic)
            runtimeLog.info("Native runtime report: \(self.reportURL.path, privacy: .public)")
            runtimeLog.info("\(self.status, privacy: .public) \(self.nativeDetails, privacy: .public)")
        } catch {
            runtimeLog.error("Could not write runtime report: \(error.localizedDescription, privacy: .public)")
        }
    }
}

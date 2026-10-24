import ARKit
import Combine
import CompositorServices
import OSLog
import SwiftUI

@main
struct SantosVisionApp: App {
    @StateObject private var model = DiagnosticModel()
    @StateObject private var runtime = NativeRuntimeModel()
    @StateObject private var windowSurface = WindowGameSurface()
    #if SV_GAME_RUNTIME
    @StateObject private var world = GameWorldModel()
    @StateObject private var gameScenes = GameSceneTransitions()
    @State private var windowReturnTask: Task<Void, Never>?
    @State private var controlsWindowRequested = false
    #endif
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace

    var body: some Scene {
        Window("Los Santos", id: "main") {
            #if SV_GAME_RUNTIME
            if runtime.windowGameMode {
                WindowGameView(runtime: runtime, surface: windowSurface)
                    .environmentObject(world)
                    .environmentObject(gameScenes)
            } else {
            LauncherView(model: model, runtime: runtime)
                .environmentObject(world)
            }
            #else
            LauncherView(model: model, runtime: runtime)
            #endif
        }
        .defaultSize(width: 1000, height: 680)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(.presented)
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setActive(phase == .active)
            #if SV_GAME_RUNTIME
            if phase == .active { restoreGameWindowIfRequested(); updateControlsWindow() }
            #endif
        }
        #if SV_GAME_RUNTIME
        .onChange(of: gameScenes.policy.serial) { _, _ in restoreGameWindowIfRequested() }
        .onChange(of: runtime.gameControlsVisible) { _, _ in updateControlsWindow() }
        .onChange(of: runtime.gtaPauseMenuOpen) { _, _ in updateControlsWindow() }
        .onChange(of: runtime.pauseControlsFollowGame) { _, _ in updateControlsWindow() }
        .onChange(of: world.isImmersiveActive) { _, _ in updateControlsWindow() }
        #endif

        ImmersiveSpace(id: DiagnosticModel.spaceID) {
            CompositorLayer(configuration: CalibrationConfiguration()) { layer in
                model.startRenderer(layer)
            }
            .onDisappear {
                // Also handles dismissal before ARKit startup can launch a render thread.
                runtime.suspendHeadTracking()
                model.stopRenderer()
                model.isImmersiveActive = false
            }
        }
        .immersionStyle(selection: .constant(.full), in: .full)

        #if SV_GAME_RUNTIME
        Window("Los Santos controls", id: "game-controls") {
            ImmersiveGameControls(runtime: runtime, world: world, gameScenes: gameScenes)
        }
        .defaultSize(width: 460, height: 260)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(.suppressed)

        ImmersiveSpace(id: GameWorldModel.spaceID) {
            GameImmersiveScene(runtime: runtime, world: world, gameScenes: gameScenes)
            .onDisappear {
                if world.isImmersiveActive || world.isOpening { world.stopRenderer(reason: "immersive_scene_disappeared") }
                // Also releases diagnostic-launcher ownership if no layer
                // renderer ever started. endWorldTracking is idempotent.
                runtime.endWorldTracking()
                world.isImmersiveActive = false
                // Queue restoration at App scope; never create a new scene
                // from the disappearing compositor's environment actions.
                if !runtime.isQuitting {
                    gameScenes.sceneDisappeared(generation: world.sceneGeneration)
                }
            }
        }
        .immersionStyle(selection: .constant(.full), in: .full)
        #endif
    }

    #if SV_GAME_RUNTIME
    private func updateControlsWindow() {
        guard scenePhase == .active, !runtime.isQuitting else { return }
        if world.isImmersiveActive, runtime.gtaPauseMenuOpen {
            // GTA's front end is an ordinary 2D presentation, without a new
            // qualified stereo world/depth pair. Show the actual game menu in
            // its game window instead of going black until the world watchdog
            // fires. Keep GTA paused and keep the controller routed to GTA.
            gameScenes.requestReturn(generation: world.sceneGeneration, reason: "gta_pause_menu_in_window")
            return
        }
        guard !gameScenes.pending else { return }
        // During the verified game's pause-open grace, wait for its own menu
        // before creating an app-only controls scene.
        if runtime.gameControlsVisible, runtime.pauseControlsFollowGame { return }
        let needed = runtime.gameControlsVisible && world.isImmersiveActive
        guard needed != controlsWindowRequested else { return }
        controlsWindowRequested = needed
        if needed { openWindow(id: "game-controls") }
        else { dismissWindow(id: "game-controls") }
    }

    /// App owns scene operations for the whole transition. The source window
    /// and compositor may disappear while an openWindow request is in flight.
    private func restoreGameWindowIfRequested() {
        guard windowReturnTask == nil,
              let ticket = gameScenes.begin(active: runtime.appActiveForSceneTransitions, quitting: runtime.isQuitting) else { return }
        world.recordSceneTransition("main_window_requested", serial: ticket.serial)
        openWindow(id: "main")
        windowReturnTask = Task { @MainActor in
            defer { windowReturnTask = nil }
            while !Task.isCancelled {
                let gameWindowConnected = runtime.windowReadyForReturn
                    && windowSurface.view.window?.windowScene?.activationState == .foregroundActive
                switch gameScenes.poll(ticket, windowReady: gameWindowConnected,
                                       active: runtime.appActiveForSceneTransitions, quitting: runtime.isQuitting) {
                case .waiting:
                    do { try await Task.sleep(for: .milliseconds(50)) } catch { return }
                case .closeSpace:
                    // A late request must never stop a new immersive renderer.
                    // openImmersiveSpace returns before the layer starts its
                    // renderer, so a GTA-menu return can predate that
                    // generation; GTA's still-open menu keeps it valid.
                    if world.isImmersiveActive, world.sceneGeneration != ticket.rendererGeneration,
                       !runtime.gtaPauseMenuOpen {
                        gameScenes.finish(ticket)
                        world.recordSceneTransition("stale_window_return_ignored", serial: ticket.serial)
                        return
                    }
                    world.recordSceneTransition("main_window_attached_before_immersive_close", serial: ticket.serial)
                    if world.isImmersiveActive || world.isOpening || runtime.immersiveWindowReturnNeedsDismissal {
                        world.stopRenderer(reason: "coordinated_window_return")
                        await dismissImmersiveSpace()
                        world.isImmersiveActive = false
                        world.isOpening = false
                    }
                    // Keep the native Start companion when GTA is still paused.
                    // Its ornament now belongs to the actual returned game window.
                    dismissWindow(id: "game-controls")
                    controlsWindowRequested = false
                    gameScenes.finish(ticket)
                    world.recordSceneTransition("window_return_complete", serial: ticket.serial)
                    return
                case .timedOut:
                    world.status = "The game window could not reconnect. Immersive view has been kept open."
                    world.recordSceneTransition("main_window_attach_timeout", serial: ticket.serial)
                    return
                case .stale, .cancelled: return
                }
            }
        }
    }
    #endif
}

@MainActor
final class DiagnosticModel: ObservableObject {
    static let spaceID = "SantosVisionTrackingDiagnostic"
    @Published var isImmersiveActive = false
    @Published var isOpening = false
    @Published var status = "Headset tracking has not been tested."
    private var renderer: CalibrationRenderer?
    private var startup: Task<Void, Never>?
    private var rendererGeneration = UUID()
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "TrackingStatus")

    func startRenderer(_ layer: LayerRenderer) {
        stopRenderer()
        let generation = rendererGeneration
        do {
            let renderer = try CalibrationRenderer(layer: layer) { [weak self] event in
                Task { @MainActor [weak self] in
                    guard let self, self.rendererGeneration == generation else { return }
                    switch event {
                    case .status(let message):
                        self.status = message
                        self.log.info("\(message, privacy: .public)")
                    case .ended:
                        XRHeadTracking.shared.suspend()
                        self.renderer = nil
                        self.startup = nil
                        self.isImmersiveActive = false
                    }
                }
            }
            self.renderer = renderer
            XRHeadTracking.shared.resume()
            startup = Task {
                do {
                    try await renderer.start()
                } catch {
                    renderer.stop()
                    guard rendererGeneration == generation else { return }
                    status = "Tracking test could not start: \(error.localizedDescription)"
                    XRHeadTracking.shared.suspend()
                    log.error("\(self.status, privacy: .public)")
                }
            }
        } catch {
            status = "Tracking test could not start: \(error.localizedDescription)"
            log.error("\(self.status, privacy: .public)")
        }
    }

    func stopRenderer() {
        // Late callbacks from an old renderer cannot reset a new immersive session.
        rendererGeneration = UUID()
        startup?.cancel()
        startup = nil
        renderer?.stop()
        renderer = nil
        XRHeadTracking.shared.suspend()
    }
}

private struct LauncherView: View {
    @ObservedObject var model: DiagnosticModel
    @ObservedObject var runtime: NativeRuntimeModel
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    #if SV_GAME_RUNTIME
    @EnvironmentObject private var world: GameWorldModel
    #endif

    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 20) {
            Text("Los Santos").font(.largeTitle.bold())
            Text("Native engine and headset tracking").font(.title2)
            Text("The compatible GTA web-port engine is compiled for visionOS. The immersive game renderer requests the headset’s actual views and scene depth from the native engine.")

            Text("Game runtime").font(.headline)
            Text(runtime.status).font(.callout)
            if runtime.isLinked {
                nativeGraphicsControls
                renderingOptimizationControls
                HStack {
                    Button("Check native game module") { Task { await runtime.probe() } }
                        .disabled(runtime.isBusy || runtime.hasStarted)
                    Button("Start native engine") { Task { await runtime.start() } }
                        .disabled(runtime.isBusy || !runtime.probePassed || runtime.hasStarted || !runtime.hasGameData)
                    Button("Capture next game frame") { runtime.captureCurrentFrame() }
                        .disabled(!runtime.hasStarted)
                }
                if !runtime.hasGameData {
                    Text("Game data has not been installed in this app’s game folder.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if !runtime.nativeDetails.isEmpty {
                    DisclosureGroup("Runtime diagnostics") {
                        Text(runtime.nativeDetails).font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }
            }

            #if SV_GAME_RUNTIME
            Divider()
            Text("Immersive GTA world").font(.headline)
            Text("Open the game world around you. Head movement follows the current game camera, including third-person and cutscenes; choose camera modes with the game's standard controls. Qualified color and scene depth must be ready before the world can appear.")
            Text("Near geometry fades from 45 to 15 cm from the camera.")
                .font(.caption)
                .foregroundStyle(.secondary)
            if SimulatorPoseFixture.shared != nil {
                Text("This simulator uses synthetic head poses. Its compositor may provide one view; this does not verify physical headset tracking or stereo.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if world.isImmersiveActive {
                Button("Close immersive game") {
                    Task {
                        world.stopRenderer()
                        await dismissImmersiveSpace()
                        world.isImmersiveActive = false
                    }
                }
            } else {
                Button("Enter immersive GTA world") { Task { await openGame() } }
                    .disabled(!runtime.hasStarted || runtime.engineFailed || runtime.gtaPauseMenuOpen || world.isOpening || model.isOpening || model.isImmersiveActive)
            }
            Text(world.status).font(.callout).foregroundStyle(.secondary)
            #endif

            Divider()
            Text("Tracking test").font(.headline)
            Text(SimulatorPoseFixture.shared == nil
                 ? "Enter a 3D calibration scene with a grid and colored markers around you. Turn your head and move slightly: the markers should stay fixed in space. This scene contains no GTA content."
                 : "Open a 3D calibration scene with a grid and colored markers. The simulator receives explicit synthetic yaw and pitch inputs. This scene contains no GTA content.")

            if model.isImmersiveActive {
                Button("Close tracking test") {
                    Task {
                        runtime.suspendHeadTracking()
                        model.stopRenderer()
                        await dismissImmersiveSpace()
                        model.isImmersiveActive = false
                        model.status = "Tracking test closed."
                    }
                }
            } else {
                Button(SimulatorPoseFixture.shared == nil ? "Test headset tracking" : "Open simulated immersive scene") {
                    Task { await openTracking() }
                }
                .disabled(model.isOpening || gameIsOpen)
            }
            Text(model.status).font(.callout).foregroundStyle(.secondary)
            if runtime.isLinked {
                Button("Recenter native head tracking") { XRHeadTracking.shared.recenter() }
                    .disabled(!model.isImmersiveActive && !gameIsOpen)
            }
            Text("Use the Digital Crown or Close tracking test to leave immersion.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(30)
        }
        .task {
            await runtime.bootstrap()
            #if SV_GAME_RUNTIME
            if ProcessInfo.processInfo.arguments.contains("--sv-open-game") { await openGame() }
            else if ProcessInfo.processInfo.arguments.contains("--sv-open-tracking") { await openTracking() }
            #else
            if ProcessInfo.processInfo.arguments.contains("--sv-open-tracking") { await openTracking() }
            #endif
        }
    }

    private var nativeGraphicsControls: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Graphics").font(.headline)
            Picker("Quality", selection: Binding(
                get: { runtime.graphicsSettings.preset },
                set: { runtime.setGraphicsPreset($0) })) {
                ForEach(NativeGraphicsSettings.Preset.allCases) { preset in
                    Text(preset.label).tag(preset)
                }
            }
            .disabled(!runtime.canChangeGraphics)
            Text(runtime.graphicsSettings.preset.detail)
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Picker("Resolution", selection: Binding(
                    get: { runtime.graphicsSettings.resolution },
                    set: { runtime.setGraphicsResolution($0) })) {
                    ForEach(NativeGraphicsSettings.Resolution.allCases) { resolution in
                        Text(resolution.label).tag(resolution)
                    }
                }
                Picker("Frame cap", selection: Binding(
                    get: { runtime.graphicsSettings.fpsLimit },
                    set: { runtime.setGraphicsFrameCap($0) })) {
                    Text("30 FPS").tag(30)
                    Text("60 FPS").tag(60)
                }
            }
            .disabled(!runtime.canChangeGraphics)
            Text(runtime.graphicsStatus).font(.caption).foregroundStyle(.secondary)
            if runtime.hasStarted {
                Text("Quit and reopen Los Santos to change graphics.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var renderingOptimizationControls: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Rendering optimizations").font(.headline)
            Toggle("Adaptive rendering (experimental)", isOn: Binding(
                get: { runtime.adaptiveRenderingEnabled },
                set: { runtime.setAdaptiveRendering($0) }))
            Toggle("Reduce frame-copy overhead", isOn: Binding(
                get: { runtime.frozenDepthHandoffEnabled },
                set: { runtime.setFrozenDepthHandoff($0) }))
            Toggle("Reduce rendering overhead (experimental)", isOn: Binding(
                get: { runtime.drawSetupOverheadEnabled },
                set: { runtime.setDrawSetupOverhead($0) }))
            Text("Rendering overhead reductions reuse unchanged setup work and avoid empty GPU work while preserving your graphics settings. Frame-copy reduction starts on; the experimental options start off. Choose before starting the game, then quit and reopen Los Santos to change these settings.")
                .font(.caption).foregroundStyle(.secondary)
            Toggle("Fill gaps beside near objects (experimental)", isOn: Binding(
                get: { runtime.depthEdgeFillEnabled },
                set: { runtime.setDepthEdgeFill($0) }))
            Text("In the immersive world, head movement can open black gaps beside nearer objects. This fills them with the neighboring background from the same game frame. It starts off.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .disabled(!runtime.canChangeRenderingOptimizations)
    }

    private var gameIsOpen: Bool {
        #if SV_GAME_RUNTIME
        world.isImmersiveActive || world.isOpening
        #else
        false
        #endif
    }

    #if SV_GAME_RUNTIME
    private func openGame() async {
        guard runtime.hasStarted, !runtime.engineFailed, !runtime.gtaPauseMenuOpen, !world.isOpening, !world.isImmersiveActive,
              !model.isImmersiveActive, !model.isOpening else { return }
        world.isOpening = true
        runtime.beginWorldTracking()
        world.status = "Opening the immersive native game world…"
        switch await openImmersiveSpace(id: GameWorldModel.spaceID) {
        case .opened: world.isImmersiveActive = true
        case .userCancelled:
            world.stopRenderer()
            runtime.endWorldTracking()
            world.status = "Immersive game opening cancelled."
        case .error:
            world.stopRenderer()
            runtime.endWorldTracking()
            world.status = "visionOS could not open the immersive game space."
        @unknown default:
            world.stopRenderer()
            runtime.endWorldTracking()
            world.status = "visionOS returned an unknown immersive-space result."
        }
        world.isOpening = false
    }
    #endif

    private func openTracking() async {
        guard !model.isOpening, !model.isImmersiveActive else { return }
        model.isOpening = true
        runtime.resumeHeadTracking()
        model.status = "Opening the tracking test…"
        switch await openImmersiveSpace(id: DiagnosticModel.spaceID) {
        case .opened:
            model.isImmersiveActive = true
        case .userCancelled:
            runtime.suspendHeadTracking()
            model.stopRenderer()
            model.status = "Tracking test cancelled."
        case .error:
            runtime.suspendHeadTracking()
            model.stopRenderer()
            model.status = "visionOS could not open the immersive space."
        @unknown default:
            runtime.suspendHeadTracking()
            model.stopRenderer()
            model.status = "visionOS returned an unknown immersive-space result."
        }
        model.isOpening = false
    }
}

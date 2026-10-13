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
    #endif
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace

    var body: some Scene {
        Window("SantosVision", id: "main") {
            #if SV_GAME_RUNTIME
            if runtime.windowGameMode {
                WindowGameView(runtime: runtime, surface: windowSurface)
                    .environmentObject(world)
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
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setActive(phase == .active)
        }

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
            ImmersiveGameControls(runtime: runtime, world: world)
        }
        .defaultSize(width: 460, height: 260)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        ImmersiveSpace(id: GameWorldModel.spaceID) {
            GameImmersiveScene(runtime: runtime, world: world)
            .onDisappear {
                world.stopRenderer()
                runtime.endWorldTracking()
                world.isImmersiveActive = false
            }
        }
        .immersionStyle(selection: .constant(.full), in: .full)
        #endif
    }
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
            Text("SantosVision").font(.largeTitle.bold())
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
                    .disabled(!runtime.hasStarted || runtime.engineFailed || world.isOpening || model.isOpening || model.isImmersiveActive)
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
            Text("Low preset").font(.callout)
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
                Text("Quit and reopen SantosVision to change graphics.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                Button("Restore low defaults") { runtime.restoreLowGraphics() }
                    .disabled(!runtime.canChangeGraphics)
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
            Text("Rendering overhead reductions reuse unchanged setup work and avoid empty GPU work while preserving your graphics settings. Frame-copy reduction starts on; the experimental options start off. Choose before starting the game, then quit and reopen SantosVision to change these settings.")
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
        guard runtime.hasStarted, !runtime.engineFailed, !world.isOpening, !world.isImmersiveActive,
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

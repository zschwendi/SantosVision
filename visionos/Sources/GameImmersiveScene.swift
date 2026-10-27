#if SV_GAME_RUNTIME
import CompositorServices
import GameController
import SwiftUI

// Keep the Metal layer in the compositor content tree. Adapting it to View
// selects ImmersiveSpace's RealityKit/View scene path instead of its Metal path.
struct GameImmersiveScene: CompositorContent {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var world: GameWorldModel
    @ObservedObject var gameScenes: GameSceneTransitions
    @Environment(\.scenePhase) private var scenePhase
    @State private var fallbackTask: Task<Void, Never>?

    var body: some CompositorContent {
        CompositorLayer(configuration: GameWorldConfiguration()) { layer in
            world.startRenderer(layer, runtime: runtime, gameScenes: gameScenes)
        }
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setImmersiveSceneActive(phase == .active)
            world.setActive(phase == .active)
        }
        .onAppear {
            gameScenes.sceneAppeared()
            fallbackTask?.cancel()
            fallbackTask = Task { @MainActor in
                // The window drawable pauses when the immersive camera owns
                // presentation. Restore it after startup failure or sustained
                // loss of actual, successfully completed game frames.
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .milliseconds(250)) }
                    catch { return }
                    guard !Task.isCancelled, world.takeFrameRecoveryRequest() != nil else { continue }
                    gameScenes.requestReturn(generation: world.sceneGeneration, reason: "qualified_game_frame_timeout")
                    return
                }
            }
        }
        .onDisappear {
            fallbackTask?.cancel()
            fallbackTask = nil
            runtime.setImmersiveSceneActive(false)
        }
    }
}

struct ImmersiveGameControls: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var world: GameWorldModel
    @ObservedObject var gameScenes: GameSceneTransitions
    @Environment(\.scenePhase) private var controlsScenePhase

    var body: some View {
        VStack(spacing: 14) {
            if runtime.leftWristCardVisible, !runtime.gameControlsVisible {
                // The same single controls window hosts the temporary
                // calibration card; no new scene is created for it.
                LeftWristCalibrationCard(model: runtime.leftWrist)
            } else {
                Text("spatial-V").font(.title3.bold())
                SantosPauseControls(runtime: runtime,
                    windowAction: {
                        gameScenes.requestReturn(generation: world.sceneGeneration, reason: "player_requested_window")
                        // Window resumes play in the window; an app-only modal
                        // panel must not keep the controller after the return.
                        if !runtime.pauseControlsFollowGame { runtime.gameControlsVisible = false }
                    }, immersive: true)
                if !world.hasPresentedFrames {
                    Text(world.status).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .padding(18)
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(true))
        .onChange(of: controlsScenePhase, initial: true) { _, phase in
            runtime.setNativeControlsSceneActive(phase == .active)
        }
        .onDisappear { runtime.setNativeControlsSceneActive(false) }
        .sheet(isPresented: $runtime.nativeSettingsPresented) { SantosSettingsView(runtime: runtime) }
    }
}
#endif

#if SV_GAME_RUNTIME
import CompositorServices
import GameController
import SwiftUI

// Keep the Metal layer in the compositor content tree. Adapting it to View
// selects ImmersiveSpace's RealityKit/View scene path instead of its Metal path.
struct GameImmersiveScene: CompositorContent {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var world: GameWorldModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @Environment(\.scenePhase) private var scenePhase
    @State private var fallbackTask: Task<Void, Never>?

    var body: some CompositorContent {
        CompositorLayer(configuration: GameWorldConfiguration()) { layer in
            world.startRenderer(layer, runtime: runtime)
        }
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setImmersiveSceneActive(phase == .active)
            world.setActive(phase == .active)
        }
        .onChange(of: runtime.menuPressCount) { _, _ in
            if runtime.gameControlsVisible { openWindow(id: "game-controls") }
            else if world.hasPresentedFrames { dismissWindow(id: "game-controls") }
        }
        .onAppear {
            fallbackTask?.cancel()
            fallbackTask = Task { @MainActor in
                // The window drawable pauses when the immersive camera owns
                // presentation. Restore it if qualified game frames never arrive.
                do { try await Task.sleep(for: .seconds(8)) }
                catch { return }
                if !Task.isCancelled, !world.hasPresentedFrames {
                    world.failImmersiveTransition("Immersive view could not show a game frame. Returned to the game window.")
                    openWindow(id: "main")
                    world.stopRenderer(reason: "first game frame timed out after 8 seconds")
                    await dismissImmersiveSpace()
                    world.isImmersiveActive = false
                }
            }
        }
        .onDisappear {
            fallbackTask?.cancel()
            fallbackTask = nil
            runtime.setImmersiveSceneActive(false)
            dismissWindow(id: "game-controls")
            openWindow(id: "main")
        }
    }
}

struct ImmersiveGameControls: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var world: GameWorldModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @State private var showingSettings = false

    var body: some View {
        VStack(spacing: 14) {
            Text("Los Santos").font(.title3.bold())
            SantosPauseControls(runtime: runtime,
                windowAction: { Task { await returnToWindow() } },
                settingsAction: { showingSettings = true })
            if !world.hasPresentedFrames {
                Text(world.status).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(18)
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(false))
        .sheet(isPresented: $showingSettings) { SantosSettingsView(runtime: runtime) }
        .onChange(of: runtime.gameControlsVisible) { _, visible in
            if !visible, world.hasPresentedFrames { dismissWindow(id: "game-controls") }
        }
    }

    private func returnToWindow() async {
        openWindow(id: "main")
        world.stopRenderer()
        await dismissImmersiveSpace()
        world.isImmersiveActive = false
        runtime.gameControlsVisible = false
        dismissWindow(id: "game-controls")
    }
}
#endif

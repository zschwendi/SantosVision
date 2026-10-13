#if SV_GAME_RUNTIME
import CompositorServices
import GameController
import SwiftUI

struct GameImmersiveScene: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var world: GameWorldModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        CompositorLayer(configuration: GameWorldConfiguration()) { layer in
            world.startRenderer(layer, runtime: runtime)
        }
        ._viewRepresentation
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(false))
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setImmersiveSceneActive(phase == .active)
            world.setActive(phase == .active)
        }
        .onChange(of: runtime.menuPressCount) { _, _ in
            if runtime.gameControlsVisible { openWindow(id: "game-controls") }
            else if world.hasPresentedFrames { dismissWindow(id: "game-controls") }
        }
        .task {
            // Keep an escape route visible until actual game frames arrive.
            try? await Task.sleep(for: .seconds(8))
            if !Task.isCancelled, !world.hasPresentedFrames {
                openWindow(id: "game-controls")
            }
        }
        .onDisappear {
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

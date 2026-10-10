import ARKit
import Combine
import CompositorServices
import SwiftUI

@main
struct SantosVisionApp: App {
    @StateObject private var model = DiagnosticModel()

    var body: some Scene {
        WindowGroup {
            LauncherView(model: model)
        }
        .defaultSize(width: 580, height: 600)

        ImmersiveSpace(id: DiagnosticModel.spaceID) {
            CompositorLayer(configuration: CalibrationConfiguration()) { layer in
                model.startRenderer(layer)
            }
            .onDisappear {
                // Also handles dismissal before ARKit startup can launch a render thread.
                model.stopRenderer()
                model.isImmersiveActive = false
            }
        }
        .immersionStyle(selection: .constant(.full), in: .full)
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

    func startRenderer(_ layer: LayerRenderer) {
        stopRenderer()
        let generation = rendererGeneration
        do {
            let renderer = try CalibrationRenderer(layer: layer) { [weak self] event in
                Task { @MainActor [weak self] in
                    guard let self, self.rendererGeneration == generation else { return }
                    switch event {
                    case .status(let message): self.status = message
                    case .ended:
                        self.renderer = nil
                        self.startup = nil
                        self.isImmersiveActive = false
                    }
                }
            }
            self.renderer = renderer
            startup = Task {
                do {
                    try await renderer.start()
                } catch {
                    renderer.stop()
                    guard rendererGeneration == generation else { return }
                    status = "Tracking test could not start: \(error.localizedDescription)"
                }
            }
        } catch {
            status = "Tracking test could not start: \(error.localizedDescription)"
        }
    }

    func stopRenderer() {
        // Late callbacks from an old renderer cannot reset a new immersive session.
        rendererGeneration = UUID()
        startup?.cancel()
        startup = nil
        renderer?.stop()
        renderer = nil
    }
}

private struct LauncherView: View {
    @ObservedObject var model: DiagnosticModel
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 20) {
            Text("SantosVision").font(.largeTitle.bold())
            Text("Immersive rendering checkpoint").font(.title2)
            Text("GTA V needs a compatible web-port game ZIP. A retail PC or console copy cannot launch in this fork.")
            Text("Full immersive GTA gameplay is still being developed. The game is unavailable in this build.")
                .foregroundStyle(.secondary)

            Button("Play GTA V") {}
                .disabled(true)

            Divider()
            Text("Tracking test").font(.headline)
            Text("Enter a 3D calibration scene with a grid and colored markers around you. Turn your head and move slightly: the markers should stay fixed in space. This scene contains no GTA content.")

            if model.isImmersiveActive {
                Button("Close tracking test") {
                    Task {
                        model.stopRenderer()
                        await dismissImmersiveSpace()
                        model.isImmersiveActive = false
                        model.status = "Tracking test closed."
                    }
                }
            } else {
                Button("Test headset tracking") {
                    Task {
                        model.isOpening = true
                        model.status = "Opening the tracking test…"
                        switch await openImmersiveSpace(id: DiagnosticModel.spaceID) {
                        case .opened:
                            model.isImmersiveActive = true
                        case .userCancelled:
                            model.stopRenderer()
                            model.status = "Tracking test cancelled."
                        case .error:
                            model.stopRenderer()
                            model.status = "visionOS could not open the immersive space."
                        @unknown default:
                            model.stopRenderer()
                            model.status = "visionOS returned an unknown immersive-space result."
                        }
                        model.isOpening = false
                    }
                }
                .disabled(model.isOpening)
            }
            Text(model.status).font(.callout).foregroundStyle(.secondary)
            Text("Use the Digital Crown or Close tracking test to leave immersion.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(30)
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .background {
                model.stopRenderer()
                Task {
                    await dismissImmersiveSpace()
                    model.isImmersiveActive = false
                }
            }
        }
    }
}

#if SV_GAME_RUNTIME
import Combine
import CompositorServices
import Foundation
import OSLog

@MainActor
final class GameWorldModel: ObservableObject {
    static let spaceID = "SantosVisionNativeGameWorld"
    @Published var isImmersiveActive = false
    @Published var isOpening = false
    @Published private(set) var hasPresentedFrames = false
    @Published var status = "The immersive game world has not been opened."
    private var renderer: GameWorldRenderer?
    private var startup: Task<Void, Never>?
    private var generation = UUID()
    private var appActive = true
    private weak var runtime: NativeRuntimeModel?
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameWorldStatus")

    func startRenderer(_ layer: LayerRenderer, runtime: NativeRuntimeModel) {
        stopRenderer()
        self.runtime = runtime
        hasPresentedFrames = false
        runtime.beginWorldTracking()
        runtime.recordMetalCapabilities(layer.device)
        let token = generation
        do {
            let renderer = try GameWorldRenderer(layer: layer) { [weak self] event in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token else { return }
                    switch event {
                    case .status(let message):
                        self.status = message
                    case .presented(let presentation):
                        guard self.appActive else { return }
                        self.hasPresentedFrames = true
                        let source = SimulatorPoseFixture.shared == nil ? "predicted headset poses" : "explicit synthetic simulator poses"
                        self.status = "Rendering the actual GTA world in \(presentation.viewCount) compositor \(presentation.viewCount == 1 ? "view" : "views") with real scene depth and \(source)."
                        self.runtime?.recordWorldPresentation(presentation)
                    case .gpuTiming(let timing):
                        self.runtime?.recordWorldGpuTiming(timing)
                        return
                    case .ended:
                        self.renderer = nil
                        self.startup = nil
                        self.isImmersiveActive = false
                        self.runtime?.endWorldTracking()
                    }
                    self.log.info("\(self.status, privacy: .public)")
                }
            }
            self.renderer = renderer
            renderer.setActive(appActive)
            startup = Task {
                do { try await renderer.start() }
                catch {
                    renderer.stop()
                    guard generation == token else { return }
                    status = "The immersive game world could not start: \(error.localizedDescription)"
                    runtime.endWorldTracking()
                    log.error("\(self.status, privacy: .public)")
                }
            }
        } catch {
            status = "The immersive game world could not start: \(error.localizedDescription)"
            runtime.endWorldTracking()
            log.error("\(self.status, privacy: .public)")
        }
    }

    func setActive(_ active: Bool) {
        appActive = active
        renderer?.setActive(active)
        if active, renderer != nil { XRHeadTracking.shared.resume() }
    }

    func stopRenderer() {
        generation = UUID()
        startup?.cancel()
        startup = nil
        renderer?.stop()
        renderer = nil
        runtime?.endWorldTracking()
    }
}
#endif

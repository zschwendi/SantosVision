#if SV_GAME_RUNTIME
import Combine
import CompositorServices
import Foundation
import OSLog
import SwiftUI

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
    private var lifecycle: [[String: Any]] = []
    private var latestRendererState: [String: Any] = [:]
    private var latestFiniteSkySupport: [String: Any] = [:]
    private var latestHUDCanvas: [String: Any] = [:]
    private var lastFailure: String?
    private var latestViewRequestFailure: [String: Any] = [:]
    private var diagnosticRenderingMethod: String?
    private var recordedStatusKeys = Set<String>()
    private weak var runtime: NativeRuntimeModel?
    private var senseUtilityVisibilityObservation: AnyCancellable?
    private var senseUtilityLayer: LayerRenderer?
    private var senseUtilityInteraction = GameWorldSenseUtilityInteraction<SpatialEventCollection.Event.ID>()
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameWorldStatus")

    func startRenderer(_ layer: LayerRenderer, runtime: NativeRuntimeModel) {
        stopRenderer()
        self.runtime = runtime
        hasPresentedFrames = false
        runtime.beginWorldTracking()
        runtime.recordMetalCapabilities(layer.device)
        lastFailure = nil
        latestRendererState = [:]
        latestFiniteSkySupport = [:]
        latestHUDCanvas = [:]
        latestViewRequestFailure = [:]
        diagnosticRenderingMethod = nil
        recordedStatusKeys.removeAll(keepingCapacity: true)
        recordState("renderer_constructor_started")
        let token = generation
        do {
            let atlas = GameWorldSenseUtilities.makeAtlas(device: layer.device)
            let renderer = try GameWorldRenderer(layer: layer, utilityAtlas: atlas) { [weak self] event in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token else { return }
                    switch event {
                    case .lifecycle(let event):
                        self.latestRendererState = event.reportValue
                        if event.stage == "view_request_rejected" {
                            self.latestViewRequestFailure = event.reportValue
                        }
                        self.recordState(event.stage, detail: event.detail,
                                         appendEvent: event.stage != "frame_sample")
                        return
                    case .status(let message):
                        self.status = message
                        self.recordState("renderer_status", detail: message)
                    case .presented(let presentation):
                        guard self.appActive else { return }
                        self.hasPresentedFrames = true
                        let source = SimulatorPoseFixture.shared == nil ? "predicted headset poses" : "explicit synthetic simulator poses"
                        self.status = "Rendering the actual GTA world in \(presentation.viewCount) compositor \(presentation.viewCount == 1 ? "view" : "views") with real scene depth and \(source)."
                        self.runtime?.recordWorldPresentation(presentation)
                    case .singleSourcePresented(let presentation):
                        guard self.appActive else { return }
                        // Recenter/tracking loss may occur after GPU completion but before
                        // this MainActor delivery; the visibility watchdog must reject it.
                        guard XRHeadTracking.shared.retainedPose(headFrame: presentation.headFrame,
                            now: ProcessInfo.processInfo.systemUptime)?.epoch == presentation.retainedEpoch else {
                            self.recordState("single_source_completion_discarded", detail: "retained source epoch expired")
                            return
                        }
                        self.hasPresentedFrames = true
                        self.diagnosticRenderingMethod = "single_source_depth_stereo"
                        self.status = "Diagnostic: one actual source frame reprojected into both eyes. Hidden surfaces, depth-edge gaps and fellow-eye peripheral cropping remain; gaps can swap sides as the source eye alternates. This is not two rendered game views."
                        self.recordState("single_source_gpu_completed", detail:
                            "rendering_method=single_source_depth_stereo source_view_index=\(presentation.sourceViewIndex) target_view_count=\(presentation.targetViewCount) game_frame=\(presentation.gameFrame) request_frame=\(presentation.requestFrame) head_frame=\(presentation.headFrame) retained_epoch=\(presentation.retainedEpoch)")
                    case .gpuTiming(let timing):
                        self.runtime?.recordWorldGpuTiming(timing)
                        return
                    case .ended(let reason):
                        self.senseUtilityVisibilityObservation = nil
                        self.senseUtilityLayer?.onSpatialEvent = { _ in }
                        self.senseUtilityLayer = nil
                        self.clearSenseUtilityInteraction()
                        self.renderer = nil
                        self.startup = nil
                        self.isImmersiveActive = false
                        self.recordState("renderer_ended", detail: reason)
                        self.runtime?.endWorldTracking()
                    }
                    self.log.info("\(self.status, privacy: .public)")
                }
            }
            self.renderer = renderer
            senseUtilityLayer = layer
            layer.onSpatialEvent = { [weak self] events in
                guard let self, self.generation == token else { return }
                self.handleSenseUtilityEvents(events)
            }
            renderer.setSenseUtilitiesVisible(runtime.senseGameplayUtilitiesVisible)
            senseUtilityVisibilityObservation = runtime.$senseGameplayUtilitiesVisible.removeDuplicates().sink { [weak self] visible in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token else { return }
                    self.renderer?.setSenseUtilitiesVisible(visible)
                    if !visible { self.clearSenseUtilityInteraction() }
                }
            }
            recordState("renderer_constructor_succeeded")
            renderer.setActive(appActive)
            startup = Task {
                do { try await renderer.start() }
                catch {
                    renderer.stop()
                    guard generation == token else { return }
                    status = "The immersive game world could not start: \(error.localizedDescription)"
                    lastFailure = String(reflecting: error)
                    recordState("renderer_start_failed", detail: lastFailure)
                    runtime.endWorldTracking()
                    log.error("\(self.status, privacy: .public)")
                }
            }
        } catch {
            status = "The immersive game world could not start: \(error.localizedDescription)"
            lastFailure = String(reflecting: error)
            recordState("renderer_constructor_failed", detail: lastFailure)
            runtime.endWorldTracking()
            log.error("\(self.status, privacy: .public)")
        }
    }

    func setActive(_ active: Bool) {
        let changed = appActive != active
        appActive = active
        renderer?.setActive(active)
        if !active { clearSenseUtilityInteraction() }
        if changed { recordState("scene_activity_changed", detail: "active=\(active)") }
        if active, renderer != nil { XRHeadTracking.shared.resume() }
    }

    func failImmersiveTransition(_ message: String) {
        lastFailure = message
        status = message
        recordState("first_frame_timeout", detail: message)
    }

    func stopRenderer(reason: String = "requested") {
        recordState("renderer_stop_requested", detail: reason)
        generation = UUID()
        senseUtilityVisibilityObservation = nil
        senseUtilityLayer?.onSpatialEvent = { _ in }
        senseUtilityLayer = nil
        clearSenseUtilityInteraction()
        startup?.cancel()
        startup = nil
        renderer?.stop()
        renderer = nil
        runtime?.endWorldTracking()
        recordState("renderer_detached")
    }

    private func clearSenseUtilityInteraction() {
        for action in senseUtilityInteraction.activeActions {
            _ = runtime?.setSenseUtilityPressed(action, pressed: false)
        }
        senseUtilityInteraction.clear()
        runtime?.setSenseUtilityInteractionActive(false)
    }

    private func handleSenseUtilityEvents(_ events: SpatialEventCollection) {
        guard let runtime, runtime.senseGameplayUtilitiesVisible, renderer != nil, appActive else {
            clearSenseUtilityInteraction()
            return
        }
        for event in events {
            let previousActions = senseUtilityInteraction.activeActions
            let action = GameWorldSenseUtilityLayout.action(identifier: event.trackingAreaIdentifier.rawValue)
            switch event.phase {
            case .active:
                senseUtilityInteraction.update(event.id, action: action)
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            case .ended:
                if let pressed = senseUtilityInteraction.end(event.id, action: action) {
                    // The explicit native action bypasses physical ownership;
                    // release ownership only after queuing the intended action.
                    _ = runtime.performSenseUtility(pressed)
                }
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            case .cancelled:
                senseUtilityInteraction.cancel(event.id)
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            @unknown default: break
            }
            let currentActions = senseUtilityInteraction.activeActions
            for action in previousActions.subtracting(currentActions) {
                _ = runtime.setSenseUtilityPressed(action, pressed: false)
            }
            for action in currentActions.subtracting(previousActions) {
                _ = runtime.setSenseUtilityPressed(action, pressed: true)
            }
        }
    }

    private func recordState(_ stage: String, detail: String? = nil, appendEvent: Bool = true) {
        var appendEvent = appendEvent
        if ["renderer_status", "view_request_rejected", "view_request_recovered"].contains(stage) {
            // Repeated waiting/failure updates must not evict startup evidence.
            // Retain at most 64 distinct status keys per renderer generation.
            let key = stage + "|" + (detail ?? "")
            if recordedStatusKeys.contains(key) || recordedStatusKeys.count >= 64 {
                appendEvent = false
            } else {
                recordedStatusKeys.insert(key)
            }
        }
        if appendEvent {
            var event: [String: Any] = ["stage": stage, "generation": generation.uuidString,
                "uptime_seconds": ProcessInfo.processInfo.systemUptime]
            if let detail { event["detail"] = detail }
            lifecycle.append(event)
            if lifecycle.count > 32 { lifecycle.removeFirst(lifecycle.count - 32) }
        }
        if let renderer {
            latestFiniteSkySupport = renderer.finiteSkySupportStatus()
            latestHUDCanvas = renderer.hudCanvasStatus()
        }
        var value: [String: Any] = ["stage": stage, "status": status,
            "scene_active": appActive, "immersive_open": isImmersiveActive,
            "opening": isOpening, "has_presented_game_frames": hasPresentedFrames,
            "renderer_exists": renderer != nil, "generation": generation.uuidString,
            "renderer": latestRendererState, "events": lifecycle,
            "finite_sky_support": latestFiniteSkySupport,
            "hud_canvas": latestHUDCanvas,
            "latest_rejected_view_request": latestViewRequestFailure]
        if let lastFailure { value["last_failure"] = lastFailure }
        if let diagnosticRenderingMethod { value["rendering_method"] = diagnosticRenderingMethod }
        runtime?.recordWorldState(value)
    }
}
#endif

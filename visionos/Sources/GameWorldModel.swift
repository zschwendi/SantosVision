import Foundation

// BEGIN GameWorldFrameWatchdog (standalone validation uses this actual block)
struct GameWorldFrameWatchdog {
    enum Failure: String { case firstFrame = "first_frame_timeout", frameLoss = "game_frame_loss_timeout" }
    static let firstFrameTimeout: Double = 8
    static let frameLossTimeout: Double = 2
    private(set) var running = false
    private(set) var activeSince: Double?
    private(set) var lastCompletion: Double?
    private(set) var hasCompletedFrame = false
    private(set) var recoveryRequested = false

    mutating func start(at now: Double, active: Bool) {
        self = Self()
        running = true
        setActive(active, at: now)
    }
    mutating func stop() { self = Self() }
    mutating func setActive(_ active: Bool, at now: Double) {
        guard running else { return }
        activeSince = active && now.isFinite ? now : nil
        lastCompletion = nil
    }
    mutating func completed(at uptime: Double, observedAt now: Double) {
        guard running, !recoveryRequested, let activeSince,
              uptime.isFinite, now.isFinite, uptime >= activeSince, uptime <= now else { return }
        hasCompletedFrame = true
        lastCompletion = max(lastCompletion ?? uptime, uptime)
    }
    mutating func takeFailure(at now: Double) -> Failure? {
        guard running, !recoveryRequested, let activeSince, now.isFinite else { return nil }
        let reference = max(activeSince, lastCompletion ?? activeSince)
        let timeout = hasCompletedFrame ? Self.frameLossTimeout : Self.firstFrameTimeout
        guard now - reference >= timeout else { return nil }
        recoveryRequested = true
        return hasCompletedFrame ? .frameLoss : .firstFrame
    }
}
// END GameWorldFrameWatchdog

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
    var sceneGeneration: UUID { generation }
    private var appActive = true
    private var frameWatchdog = GameWorldFrameWatchdog()
    private var lifecycle: [[String: Any]] = []
    private var latestRendererState: [String: Any] = [:]
    private var latestFiniteSkySupport: [String: Any] = [:]
    private var latestHUDCanvas: [String: Any] = [:]
    private var latestImageQuality: [String: Any] = [:]
    private var lastFailure: String?
    private var latestViewRequestFailure: [String: Any] = [:]
    private var diagnosticRenderingMethod: String?
    private var recordedStatusKeys = Set<String>()
    private weak var runtime: NativeRuntimeModel?
    private weak var gameScenes: GameSceneTransitions?
    private var senseUtilityVisibilityObservation: AnyCancellable?
    private var senseUtilityPhoneObservation: AnyCancellable?
    private var senseUtilityLayer: LayerRenderer?
    private var senseUtilityInteraction = GameWorldSenseUtilityInteraction<SpatialEventCollection.Event.ID>()
    private var senseUtilityTrackingLive = false
    private var senseUtilityTrackingSequence: UInt64 = 0
    private var senseUtilityEventCounts: [String: UInt64] = [:]
    private var senseUtilityLastInput: [String: Any] = [:]
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameWorldStatus")

    func recordSceneTransition(_ stage: String, serial: UInt64) {
        recordState(stage, detail: "window_return_serial=\(serial)")
    }

    func startRenderer(_ layer: LayerRenderer, runtime: NativeRuntimeModel, gameScenes: GameSceneTransitions) {
        stopRenderer()
        self.runtime = runtime
        self.gameScenes = gameScenes
        hasPresentedFrames = false
        frameWatchdog.start(at: ProcessInfo.processInfo.systemUptime, active: appActive)
        runtime.beginWorldTracking()
        runtime.recordMetalCapabilities(layer.device)
        lastFailure = nil
        latestRendererState = [:]
        latestFiniteSkySupport = [:]
        latestHUDCanvas = [:]
        latestImageQuality = [:]
        senseUtilityTrackingLive = false
        senseUtilityTrackingSequence = 0
        senseUtilityEventCounts = [:]
        senseUtilityLastInput = [:]
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
                    case .gameFrameCompleted(let uptime):
                        self.frameWatchdog.completed(at: uptime, observedAt: ProcessInfo.processInfo.systemUptime)
                        return
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
                    case .senseUtilityTracking(let active, let sequence):
                        guard sequence > self.senseUtilityTrackingSequence else { return }
                        self.senseUtilityTrackingSequence = sequence
                        self.senseUtilityTrackingLive = active
                        if !active { self.clearSenseUtilityInteraction(preserveGestureCancellation: true) }
                        return
                    case .ended(let reason):
                        self.senseUtilityVisibilityObservation = nil
                        self.senseUtilityPhoneObservation = nil
                        self.senseUtilityLayer?.onSpatialEvent = { _ in }
                        self.senseUtilityLayer = nil
                        self.clearSenseUtilityInteraction()
                        self.renderer = nil
                        self.startup = nil
                        self.isImmersiveActive = false
                        self.recordState("renderer_ended", detail: reason)
                        self.runtime?.endWorldTracking()
                        self.gameScenes?.requestReturn(generation: self.generation, reason: "renderer_ended")
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
            renderer.setSenseUtilitiesPhoneOpen(runtime.sensePhoneNavigationVisible)
            senseUtilityVisibilityObservation = runtime.$senseGameplayUtilitiesVisible.removeDuplicates().sink { [weak self] visible in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token else { return }
                    self.renderer?.setSenseUtilitiesVisible(visible)
                    if !visible { self.clearSenseUtilityInteraction(preserveGestureCancellation: true) }
                }
            }
            senseUtilityPhoneObservation = runtime.$sensePhoneNavigationVisible.removeDuplicates().sink { [weak self] open in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token else { return }
                    self.renderer?.setSenseUtilitiesPhoneOpen(open)
                    self.clearSenseUtilityInteraction(preserveGestureCancellation: true)
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
        if changed { frameWatchdog.setActive(active, at: ProcessInfo.processInfo.systemUptime) }
        renderer?.setActive(active)
        if !active {
            senseUtilityTrackingLive = false
            clearSenseUtilityInteraction(preserveGestureCancellation: true)
        }
        if changed { recordState("scene_activity_changed", detail: "active=\(active)") }
        if active, renderer != nil { XRHeadTracking.shared.resume() }
    }

    func failImmersiveTransition(_ message: String) {
        lastFailure = message
        status = message
        recordState("first_frame_timeout", detail: message)
    }

    struct FrameRecoveryRequest {
        fileprivate let generation: UUID
        fileprivate let failure: GameWorldFrameWatchdog.Failure
    }

    func takeFrameRecoveryRequest() -> FrameRecoveryRequest? {
        guard let failure = frameWatchdog.takeFailure(at: ProcessInfo.processInfo.systemUptime) else { return nil }
        let message = failure == .firstFrame
            ? "Immersive view could not show a game frame. Returned to the game window."
            : "The immersive game view stopped updating. Returned to the game window."
        lastFailure = message
        status = message
        recordState(failure.rawValue, detail: message)
        return FrameRecoveryRequest(generation: generation, failure: failure)
    }

    /// The scene opens the main window before releasing immersive presentation.
    /// A stale request must never stop a newer renderer or a manual return.
    func stopRendererForFrameRecovery(_ request: FrameRecoveryRequest) -> UUID? {
        guard request.generation == generation, frameWatchdog.recoveryRequested else { return nil }
        let reason = request.failure == .firstFrame
            ? "first game frame timed out after 8 seconds"
            : "qualified game GPU completions stopped for 2 seconds"
        stopRenderer(reason: reason)
        return generation
    }

    func finishFrameRecovery(generation token: UUID) {
        guard generation == token else { return }
        isImmersiveActive = false
        isOpening = false
    }

    func stopRenderer(reason: String = "requested") {
        recordState("renderer_stop_requested", detail: reason)
        generation = UUID()
        frameWatchdog.stop()
        senseUtilityVisibilityObservation = nil
        senseUtilityPhoneObservation = nil
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

    private func clearSenseUtilityInteraction(preserveGestureCancellation: Bool = false) {
        if preserveGestureCancellation { senseUtilityInteraction.suspend() }
        else { senseUtilityInteraction.clear() }
        runtime?.setSenseUtilityInteractionActive(false)
    }

    private func handleSenseUtilityEvents(_ events: SpatialEventCollection) {
        guard let runtime, runtime.senseGameplayUtilitiesVisible, renderer != nil, appActive,
              senseUtilityTrackingLive else {
            senseUtilityEventCounts["unavailable", default: 0] &+= UInt64(events.count)
            clearSenseUtilityInteraction(preserveGestureCancellation: true)
            for event in events {
                switch event.phase {
                case .active: senseUtilityInteraction.rejectUntilEnd(event.id)
                case .ended, .cancelled: senseUtilityInteraction.cancel(event.id, timestamp: event.timestamp)
                @unknown default: break
                }
            }
            return
        }
        for event in events {
            let candidate = GameWorldSenseUtilityLayout.action(identifier: event.trackingAreaIdentifier.rawValue)
            let action = candidate.flatMap { $0.isDirection && !runtime.sensePhoneNavigationVisible ? nil : $0 }
            senseUtilityEventCounts[String(describing: event.phase), default: 0] &+= 1
            senseUtilityEventCounts[action == nil ? "unknown_hit" : "known_hit", default: 0] &+= 1
            senseUtilityLastInput = ["phase": String(describing: event.phase), "kind": String(describing: event.kind),
                "known_action": action?.rawValue ?? 0]
            switch event.phase {
            case .active:
                senseUtilityInteraction.update(event.id, action: action)
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            case .ended:
                if let pressed = senseUtilityInteraction.end(event.id, action: action, timestamp: event.timestamp) {
                    // The explicit native action bypasses physical ownership;
                    // release ownership only after queuing the intended action.
                    runtime.setSenseUtilityInteractionActive(true)
                    let accepted = runtime.performSenseUtility(pressed)
                    senseUtilityEventCounts[accepted ? "action_queued" : "action_rejected", default: 0] &+= 1
                }
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            case .cancelled:
                senseUtilityInteraction.cancel(event.id, timestamp: event.timestamp)
                runtime.setSenseUtilityInteractionActive(senseUtilityInteraction.isActive)
            @unknown default: break
            }
            if event.phase != .active { recordState("sense_utility_input", appendEvent: false) }
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
            latestImageQuality = renderer.imageQualityStatus()
        }
        var value: [String: Any] = ["stage": stage, "status": status,
            "scene_active": appActive, "immersive_open": isImmersiveActive,
            "opening": isOpening, "has_presented_game_frames": hasPresentedFrames,
            "renderer_exists": renderer != nil, "generation": generation.uuidString,
            "frame_watchdog": ["startup_timeout_seconds": GameWorldFrameWatchdog.firstFrameTimeout,
                "ongoing_timeout_seconds": GameWorldFrameWatchdog.frameLossTimeout,
                "has_completed_frame": frameWatchdog.hasCompletedFrame,
                "recovery_requested": frameWatchdog.recoveryRequested],
            "renderer": latestRendererState, "events": lifecycle,
            "finite_sky_support": latestFiniteSkySupport,
            "hud_canvas": latestHUDCanvas,
            "image_quality": latestImageQuality,
            "sense_utility_input": ["events": senseUtilityEventCounts, "last_event": senseUtilityLastInput,
                "tracking_live": senseUtilityTrackingLive, "input_scope": "system hit identifiers only; no gaze coordinates"],
            "latest_rejected_view_request": latestViewRequestFailure]
        if let lastFailure { value["last_failure"] = lastFailure }
        if let diagnosticRenderingMethod { value["rendering_method"] = diagnosticRenderingMethod }
        runtime?.recordWorldState(value)
    }
}
#endif

import Combine
import Foundation
import Metal
import OSLog

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
    @Published private(set) var adaptiveRenderingEnabled = RenderingOptimizations.storedAdaptive
    @Published private(set) var frozenDepthHandoffEnabled = RenderingOptimizations.storedFrozenDepth
    @Published private(set) var drawSetupOverheadEnabled = RenderingOptimizations.storedDrawSetup
    let isLinked = NativeRuntime.isLinked
    let gameRoot: URL
    let saveRoot: URL
    let reportURL: URL
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
    private var capturePaths = Set<String>()
    private var captureRequests: [[String: Any]] = []
    private var compositorOwnsHeadTracking = false
    private var worldPresentation: [String: Any]?
    private var worldGpuTiming: [String: Any]?
    private var worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
    private var metalCapabilities: [String: Any] = [:]
    private var graphicsStartupError: String?
    private var graphicsConfigWritten = false

    var graphicsConfigURL: URL { saveRoot.appendingPathComponent("native-graphics.json") }
    var canChangeGraphics: Bool { isLinked && !hasStarted && !isBusy }
    var canChangeRenderingOptimizations: Bool { isLinked && !startupAttempted && !hasStarted && !isBusy }

    init(arguments: [String] = ProcessInfo.processInfo.arguments) {
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
        do {
            graphicsSettings = try NativeGraphicsSettings.load(from: graphicsConfigURL)
            try graphicsSettings.apply(arguments: arguments)
            graphicsStatus = graphicsSettings.summary
        } catch {
            graphicsStartupError = error.localizedDescription
            graphicsStatus = "Graphics settings need attention: \(error.localizedDescription)"
        }
        if let device = MTLCreateSystemDefaultDevice() { recordMetalCapabilities(device) }
        if !isLinked { status = "This build contains the native immersive tracking test. Build game mode to link the GTA engine." }
    }

    var hasGameData: Bool {
        FileManager.default.fileExists(atPath: gameRoot.appendingPathComponent("data/manifest.json").path)
            && FileManager.default.fileExists(atPath: gameRoot.appendingPathComponent("b").path)
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
        guard !isBusy, isLinked, probePassed, !hasStarted, isActive else { return }
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
        isBusy = true
        do {
            try persistGraphics()
        } catch {
            status = "Could not save graphics settings: \(error.localizedDescription)"
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

    func setGraphicsFrameCap(_ cap: Int) {
        guard canChangeGraphics, cap == 30 || cap == 60 else { return }
        graphicsSettings.fpsLimit = cap
        saveGraphicsSelection()
    }

    func restoreLowGraphics() {
        guard canChangeGraphics else { return }
        graphicsSettings = NativeGraphicsSettings()
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
        if active != isActive {
            worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
            worldGpuTiming = nil
        }
        isActive = active
        if !active { suspendHeadTracking(); worldPresentation = nil; worldGpuTiming = nil }
        if isLinked { activeReturnCode = NativeRuntime.setActive(active) }
        if active {
            if hasStarted { resumeHeadTracking() }
            else if probePassed, ProcessInfo.processInfo.arguments.contains("--sv-start-runtime") {
                Task { await start() }
            }
        }
        recordReport()
    }

    func suspendHeadTracking() {
        simulatedPoseTask?.cancel()
        simulatedPoseTask = nil
        XRHeadTracking.shared.suspend()
    }

    func resumeHeadTracking() {
        guard isActive else { return }
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
        if isActive { XRHeadTracking.shared.resume() }
        recordReport()
    }

    func endWorldTracking() {
        compositorOwnsHeadTracking = false
        worldPresentation = nil
        worldGpuTiming = nil
        worldGpuTimingNotBeforeUptime = ProcessInfo.processInfo.systemUptime
        suspendHeadTracking()
        recordReport()
    }

    #if SV_GAME_RUNTIME
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

    private func startPolling() {
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
        // A historical completion is evidence, but the native publisher also
        // verifies a fresh ready frame and current lifecycle epoch.
        let currentWorldConnected = isActive && compositorOwnsHeadTracking && worldPresentation != nil
            && nativeStatus?["immersive_game_renderer_connected"] as? Bool == true
        let report: [String: Any] = [
            "schema": 1, "timestamp": ISO8601DateFormatter().string(from: Date()),
            "report_system_uptime_seconds": ProcessInfo.processInfo.systemUptime,
            "native_runtime_linked": isLinked, "game_root": gameRoot.path,
            "save_root": saveRoot.path, "has_game_data": hasGameData,
            "graphics_requested": graphicsSettings.reportValue,
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
            "probe": payload(lastProbe), "start": payload(lastStart), "status": payload(lastStatus),
            "head_pose_source": SimulatorPoseFixture.shared == nil ? "ARKit" : "simulator_fixture",
            "simulator_pose_fixture": SimulatorPoseFixture.shared?.reportValue ?? [:],
            "capture_request_return_code": captureReturnCode.map { $0 as Any } ?? NSNull(),
            "capture_requests": captureRequests,
            "app_active": isActive,
            "set_active_return_code": activeReturnCode.map { $0 as Any } ?? NSNull(),
            "metal_capabilities": metalCapabilities,
            "immersive_game_renderer_connected": currentWorldConnected,
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

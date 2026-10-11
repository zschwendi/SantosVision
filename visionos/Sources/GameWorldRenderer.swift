#if SV_GAME_RUNTIME
import ARKit
import CompositorServices
import Foundation
import Metal
import OSLog
import SwiftUI
import simd

struct GameWorldConfiguration: CompositorLayerConfiguration {
    func makeConfiguration(capabilities: LayerRenderer.Capabilities,
                           configuration: inout LayerRenderer.Configuration) {
        let colorFormats = capabilities.supportedColorFormats(options: [])
        // Some compositor/simulator configurations expose only sRGB targets.
        // Negotiate first; the shader matches the destination's transfer function.
        configuration.colorFormat = colorFormats.contains(.bgra8Unorm) ? .bgra8Unorm
            : colorFormats.contains(.bgra8Unorm_srgb) ? .bgra8Unorm_srgb
            : colorFormats.first ?? configuration.colorFormat
        if capabilities.supportedDepthFormats.contains(.depth32Float) {
            configuration.depthFormat = .depth32Float
        }
        configuration.isFoveationEnabled = false
        let layouts = capabilities.supportedLayouts(options: [])
        configuration.layout = layouts.contains(.dedicated) ? .dedicated
            : layouts.contains(.layered) ? .layered : .shared
        configuration.defaultDepthRange = SIMD2<Float>(1_000, max(0.05, capabilities.supportedMinimumNearPlaneDistance))
        let selectedColor = configuration.colorFormat.rawValue
        let selectedDepth = configuration.depthFormat.rawValue
        Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameCompositor")
            .info("Game layer capabilities: colors=\(colorFormats.map(\.rawValue), privacy: .public) selected_color=\(selectedColor) depth=\(selectedDepth) minimum_near=\(capabilities.supportedMinimumNearPlaneDistance)")
    }
}

struct GameWorldPresentation: Sendable {
    let viewCount: Int
    let gameFrames: [UInt64]
    let requestFrames: [UInt64]
    let headFrames: [UInt64]
}

enum GameWorldEvent: Sendable {
    case status(String)
    case presented(GameWorldPresentation)
    case gpuTiming(GameWorldGpuTimingSummary)
    case ended
}

private struct GameWorldUniforms {
    var sourceEyeFromClip: simd_float4x4
    var currentEyeFromSourceEye: simd_float4x4
    var currentClipFromEye: simd_float4x4
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32 = 1
    var colorTransfer: UInt32 = 0
    var nearFadeDistances = SIMD2<Float>(0.15, 0.45)
    var reserved = SIMD2<Float>.zero
}

private enum GameWorldError: LocalizedError {
    case unsupportedTracking, missingMetal, cameraEnable(Int32)
    var errorDescription: String? {
        switch self {
        case .unsupportedTracking: "Head tracking is unavailable. The simulator needs an explicit synthetic pose fixture."
        case .missingMetal: "The native game world Metal pipeline could not be created."
        case .cameraEnable(let code): "The native immersive camera could not be enabled (\(code))."
        }
    }
}

private func worldMatrix<T>(_ value: T) -> simd_float4x4 {
    var matrix = matrix_identity_float4x4
    withUnsafeMutableBytes(of: &matrix) { destination in
        withUnsafeBytes(of: value) { destination.copyMemory(from: $0) }
    }
    return matrix
}

private func worldDoubleMatrix(_ matrix: simd_float4x4) -> simd_double4x4 {
    func column(_ value: SIMD4<Float>) -> SIMD4<Double> {
        SIMD4<Double>(Double(value.x), Double(value.y), Double(value.z), Double(value.w))
    }
    return simd_double4x4(columns: (column(matrix.columns.0), column(matrix.columns.1),
                                    column(matrix.columns.2), column(matrix.columns.3)))
}

private func worldFloatMatrix(_ matrix: simd_double4x4) -> simd_float4x4 {
    func column(_ value: SIMD4<Double>) -> SIMD4<Float> {
        SIMD4<Float>(Float(value.x), Float(value.y), Float(value.z), Float(value.w))
    }
    return simd_float4x4(columns: (column(matrix.columns.0), column(matrix.columns.1),
                                  column(matrix.columns.2), column(matrix.columns.3)))
}

private func validWorldMatrix(_ matrix: simd_float4x4) -> Bool {
    let values = [matrix.columns.0, matrix.columns.1, matrix.columns.2, matrix.columns.3]
    return values.allSatisfy { $0.x.isFinite && $0.y.isFinite && $0.z.isFinite && $0.w.isFinite }
        && abs(simd_determinant(matrix)) > 1e-10
}

private final class GameWorldLease: @unchecked Sendable {
    let native: SVWorldFrame
    let color: any MTLTexture
    let depth: any MTLTexture
    let sourceProjection: simd_float4x4
    let gameFromEye: simd_float4x4
    let gameFromStage: simd_float4x4
    private let lock = NSLock()
    private var released = false

    init?(native: SVWorldFrame, device: any MTLDevice, eye: Int, viewCount: Int, timestamp: Double) {
        let projection = worldMatrix(native.projection_from_eye)
        let eyeTransform = worldMatrix(native.game_from_eye)
        let stageTransform = worldMatrix(native.game_from_stage)
        let supportedColor: Set<MTLPixelFormat> = [.rgba8Unorm, .bgra8Unorm, .rgba8Unorm_srgb, .bgra8Unorm_srgb]
        guard native.abi_version == 1, native.byte_size == MemoryLayout<SVWorldFrame>.size,
              native.lease != 0, native.view_index == UInt32(eye),
              native.view_count == UInt32(viewCount), native.reserved == 0,
              native.depth_convention == 0, native.width > 1, native.height > 1,
              native.presentation_time.isFinite,
              abs(timestamp - native.presentation_time) <= 1,
              validWorldMatrix(projection), validWorldMatrix(eyeTransform), validWorldMatrix(stageTransform),
              let colorPointer = UnsafeRawPointer(bitPattern: UInt(native.color_texture)),
              let depthPointer = UnsafeRawPointer(bitPattern: UInt(native.depth_texture)),
              let color = Unmanaged<AnyObject>.fromOpaque(colorPointer).takeUnretainedValue() as? any MTLTexture,
              let depth = Unmanaged<AnyObject>.fromOpaque(depthPointer).takeUnretainedValue() as? any MTLTexture,
              color.device.registryID == device.registryID, depth.device.registryID == device.registryID,
              color.textureType == .type2D, depth.textureType == .type2D,
              supportedColor.contains(color.pixelFormat),
              depth.pixelFormat == .r32Float,
              color.width == Int(native.width), color.height == Int(native.height),
              depth.width == color.width, depth.height == color.height else { return nil }
        self.native = native
        self.color = color
        self.depth = depth
        sourceProjection = projection
        gameFromEye = eyeTransform
        gameFromStage = stageTransform
    }

    func release() {
        lock.lock()
        defer { lock.unlock() }
        guard !released else { return }
        released = true
        _ = sv_world_release_frame(native.lease)
    }
    deinit { release() }
}

// BEGIN CornerScratchPool (the standalone GPU harness compiles this block verbatim)
/// The corner textures one command buffer holds until it completes.
struct CornerScratchLease: @unchecked Sendable {
    var textures: [any MTLTexture] = []
}

/// Bounded scratch textures for the optional corner-depth precompute. A texture
/// is handed to one command buffer and returns to the pool only from that
/// buffer's completion handler, so it is never rewritten while an earlier
/// submission can still read it. Nothing is cached between source frames.
final class CornerScratchPool: @unchecked Sendable {
    /// Three in-flight command buffers, two eyes each.
    static let maximumTextures = 6
    /// One corner texture. 3841x2161 R32Float, the 4K case, is 31.7 MiB.
    static let maximumTextureBytes = 64 << 20
    private let lock = NSLock()
    private var entries: [(texture: any MTLTexture, busy: Bool)] = []

    /// The (width + 1) x (height + 1) corner grid, or nil outside the bound.
    static func cornerSize(width: Int, height: Int) -> (width: Int, height: Int)? {
        guard width > 0, height > 0, width <= 16_384, height <= 16_384,
              (width + 1) * (height + 1) * 4 <= maximumTextureBytes else { return nil }
        return (width + 1, height + 1)
    }

    /// A texture no other submission is using, or nil when the bound is reached.
    func acquire(device: any MTLDevice, width: Int, height: Int) -> (any MTLTexture)? {
        guard let size = Self.cornerSize(width: width, height: height) else { return nil }
        lock.lock()
        defer { lock.unlock() }
        if let index = entries.firstIndex(where: { !$0.busy && $0.texture.width == size.width && $0.texture.height == size.height }) {
            entries[index].busy = true
            return entries[index].texture
        }
        // Idle textures of another size are dropped first, so a change of source size cannot grow the pool.
        entries.removeAll { !$0.busy }
        guard entries.count < Self.maximumTextures else { return nil }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float, width: size.width,
                                                                  height: size.height, mipmapped: false)
        descriptor.usage = [.shaderRead, .shaderWrite]
        descriptor.storageMode = .private
        guard let texture = device.makeTexture(descriptor: descriptor) else { return nil }
        texture.label = "Game world qualified corner depths"
        entries.append((texture, true))
        return texture
    }

    /// Called from the completion handler of the command buffer that used them.
    func release(_ lease: CornerScratchLease) {
        guard !lease.textures.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        for texture in lease.textures {
            if let index = entries.firstIndex(where: { $0.texture === texture }) { entries[index].busy = false }
        }
    }

    var statistics: (textures: Int, busy: Int, bytes: Int) {
        lock.lock()
        defer { lock.unlock() }
        return (entries.count, entries.filter(\.busy).count, entries.reduce(0) { $0 + $1.texture.width * $1.texture.height * 4 })
    }
}
// END CornerScratchPool

/// Native Metal snapshots are immutable and already GPU-complete when leased.
/// The render thread only calls host mailbox/lease APIs, never guest functions.
final class GameWorldRenderer: @unchecked Sendable {
    private let layer: LayerRenderer
    private let session = ARKitSession()
    private let tracking = WorldTrackingProvider()
    private let queue: any MTLCommandQueue
    private let pipeline: any MTLRenderPipelineState
    /// Optional exact corner-depth precompute (MUGUET_COMPOSITOR_CORNER_PRECOMPUTE=1); nil keeps the
    /// single-pass pipeline above. Off by default.
    private let cornerPrecompute: (kernel: any MTLComputePipelineState, pipeline: any MTLRenderPipelineState,
                                   scratch: CornerScratchPool)?
    private var loggedCornerFallback = false
    private let depthState: any MTLDepthStencilState
    private let report: @Sendable (GameWorldEvent) -> Void
    private let gpuTiming = ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_GPU_TIMING"] == "1"
        ? GameWorldGpuTiming() : nil
    private let stopLock = NSLock()
    private var stopped = false
    private var active = true
    private let sessionToken = UUID()
    private let inFlight = DispatchSemaphore(value: 3)
    private var lastStatus = ""
    private var frameNumber: UInt64 = 0
    private var loggedFrame = false
    private let debugPrefix = GameWorldDebugCapture.requestedPrefix()
    private var didDebugCapture = false
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameCompositor")

    init(layer: LayerRenderer, report: @escaping @Sendable (GameWorldEvent) -> Void) throws {
        self.layer = layer
        self.report = report
        guard let queue = layer.device.makeCommandQueue(), let library = layer.device.makeDefaultLibrary(),
              let vertex = library.makeFunction(name: "gameWorldVertex"),
              let fragment = library.makeFunction(name: "gameWorldFragment") else { throw GameWorldError.missingMetal }
        self.queue = queue
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Actual native GTA world depth reprojection"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = layer.configuration.colorFormat
        descriptor.depthAttachmentPixelFormat = layer.configuration.depthFormat
        pipeline = try layer.device.makeRenderPipelineState(descriptor: descriptor)
        let cornerRequested = ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_CORNER_PRECOMPUTE"] == "1"
        var precompute: (kernel: any MTLComputePipelineState, pipeline: any MTLRenderPipelineState, scratch: CornerScratchPool)?
        if cornerRequested, let kernelFunction = library.makeFunction(name: "gameWorldCornerDepths"),
           let precomputedVertex = library.makeFunction(name: "gameWorldPrecomputedVertex"),
           let kernel = try? layer.device.makeComputePipelineState(function: kernelFunction) {
            // Same fragment stage, attachments and depth state; only the vertex entry point differs.
            descriptor.label = "Actual native GTA world depth reprojection, precomputed corners"
            descriptor.vertexFunction = precomputedVertex
            if let precomputed = try? layer.device.makeRenderPipelineState(descriptor: descriptor) {
                precompute = (kernel, precomputed, CornerScratchPool())
            }
        }
        cornerPrecompute = precompute
        let depthDescriptor = MTLDepthStencilDescriptor()
        depthDescriptor.depthCompareFunction = .greaterEqual
        depthDescriptor.isDepthWriteEnabled = true
        guard let depthState = layer.device.makeDepthStencilState(descriptor: depthDescriptor) else {
            throw GameWorldError.missingMetal
        }
        self.depthState = depthState
        if cornerRequested {
            log.info("Corner-depth precompute requested: \(precompute == nil ? "unavailable, using the single-pass pipeline" : "enabled", privacy: .public)")
        }
    }

    func start() async throws {
        try Task.checkCancellation()
        if SimulatorPoseFixture.shared == nil {
            guard WorldTrackingProvider.isSupported else { throw GameWorldError.unsupportedTracking }
            try await session.run([tracking])
        }
        try Task.checkCancellation()
        guard !isStopped else { session.stop(); return }
        let result = WorldCameraOwnership.shared.acquire(sessionToken)
        guard result == 0 else { throw GameWorldError.cameraEnable(result) }
        reportStatus("Waiting for a qualified native game world frame with its actual depth.")
        let thread = Thread { [self] in renderLoop() }
        thread.name = "SantosVision native game immersive renderer"
        thread.qualityOfService = .userInteractive
        thread.start()
    }

    func stop() {
        stopLock.lock()
        stopped = true
        stopLock.unlock()
        WorldCameraOwnership.shared.release(sessionToken)
        session.stop()
    }

    func setActive(_ active: Bool) {
        stopLock.lock()
        if self.active != active { gpuTiming?.reset() }
        self.active = active
        stopLock.unlock()
    }

    private var isStopped: Bool {
        stopLock.lock()
        defer { stopLock.unlock() }
        return stopped
    }

    private var isActive: Bool {
        stopLock.lock()
        defer { stopLock.unlock() }
        return active
    }

    private func reportStatus(_ message: String) {
        guard message != lastStatus else { return }
        lastStatus = message
        report(.status(message))
        log.info("\(message, privacy: .public)")
    }

    private func renderLoop() {
        defer {
            session.stop()
            WorldCameraOwnership.shared.release(sessionToken)
            report(.ended)
        }
        while !isStopped {
            guard isActive else { Thread.sleep(forTimeInterval: 0.01); continue }
            switch layer.state {
            case .invalidated: return
            case .paused: Thread.sleep(forTimeInterval: 0.01)
            case .running: autoreleasepool { renderFrame() }
            @unknown default: return
            }
        }
    }

    private func renderFrame() {
        guard inFlight.wait(timeout: .now() + 0.1) == .success else { return }
        var ownsSlot = true
        defer { if ownsSlot { inFlight.signal() } }
        guard let frame = layer.queryNextFrame() else { return }
        frame.startUpdate()
        frame.endUpdate()
        guard let timing = frame.predictTiming() else { return }
        LayerRenderer.Clock().wait(until: timing.optimalInputTime)
        guard !isStopped, isActive, layer.state == .running else { return }
        frame.startSubmission()
        let drawables = frame.queryDrawables()
        guard !drawables.isEmpty else { return } // Empty query invalidates the frame.
        defer { frame.endSubmission() }
        guard let commands = queue.makeCommandBuffer() else { return }
        commands.label = "SantosVision actual game world compositor frame"
        var leases: [GameWorldLease] = []
        var cornerScratch = CornerScratchLease()
        var presentation: GameWorldPresentation?
        var debugCapture: GameWorldDebugCapture?
        for drawable in drawables {
            let duration = LayerRenderer.Clock.Instant.epoch.duration(to: drawable.frameTiming.presentationTime)
            let timestamp = Double(duration.components.seconds) + Double(duration.components.attoseconds) / 1e18
            let anchor = tracking.state == .running ? tracking.queryDeviceAnchor(atTimestamp: timestamp) : nil
            let trackedAnchor = anchor?.isTracked == true ? anchor : nil
            drawable.deviceAnchor = trackedAnchor
            let stageFromHead = SimulatorPoseFixture.shared?.pose(at: timestamp)
                ?? trackedAnchor?.originFromAnchorTransform
            // Capture drawables must not replace the headset's two-view request
            // with a one-view recording request.
            let builtIn = drawable.target == .builtIn
            var drawableLeases: [GameWorldLease] = []
            var acquisitionFailed = false
            if builtIn, let stageFromHead, (1...2).contains(drawable.views.count) {
                XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: stageFromHead, tracked: true)
                submitViews(drawable: drawable, timestamp: timestamp)
                for eye in drawable.views.indices {
                    var native = SVWorldFrame()
                    native.abi_version = 1
                    native.byte_size = UInt32(MemoryLayout<SVWorldFrame>.size)
                    let result = sv_world_acquire_frame(UInt32(eye), &native)
                    if result == 0 {
                        if let lease = GameWorldLease(native: native, device: layer.device,
                                                      eye: eye, viewCount: drawable.views.count, timestamp: timestamp) {
                            drawableLeases.append(lease)
                        } else {
                            _ = sv_world_release_frame(native.lease)
                            acquisitionFailed = true
                            reportStatus("A native frame failed the freshness, Metal device, view, depth, or matrix checks.")
                        }
                    } else if result < 0 {
                        acquisitionFailed = true
                        reportStatus("Native world frame acquisition failed (\(result)).")
                    }
                }
                if drawableLeases.isEmpty, !acquisitionFailed {
                    reportStatus("Waiting for a qualified native game world frame with its actual depth.")
                } else if drawableLeases.count < drawable.views.count, !acquisitionFailed {
                    reportStatus("Waiting for both distinct native game eye views and their matching depth.")
                }
            } else if builtIn, stageFromHead == nil {
                XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: nil, tracked: false)
                reportStatus("Head pose unavailable; native world drawing is suspended until tracking returns.")
            } else if builtIn {
                reportStatus("The compositor supplied an unsupported game view count (\(drawable.views.count)); this bridge supports one or two views.")
            }
            let encodedViews = encode(drawable: drawable, stageFromHead: stageFromHead,
                                      leases: drawableLeases, commands: commands, cornerScratch: &cornerScratch)
            if builtIn, encodedViews == drawable.views.count, !drawableLeases.isEmpty {
                presentation = GameWorldPresentation(viewCount: drawable.views.count,
                    gameFrames: drawableLeases.map { $0.native.game_frame },
                    requestFrames: drawableLeases.map { $0.native.request_frame },
                    headFrames: drawableLeases.map { $0.native.head_frame })
                if !didDebugCapture, let debugPrefix, let stageFromHead,
                   let lease = drawableLeases.first(where: { $0.native.view_index == 0 }),
                   lease.native.game_frame >= GameWorldDebugCapture.minimumGameFrame,
                   let view = drawable.views.first {
                    let uniforms = worldUniforms(lease: lease, stageFromHead: stageFromHead,
                        headFromEye: view.transform,
                        projection: drawable.computeProjection(convention: .rightUpBack, viewIndex: 0))
                    debugCapture = GameWorldDebugCapture(prefix: debugPrefix, device: layer.device, commands: commands,
                        color: lease.color, depth: lease.depth,
                        outputColor: drawable.colorTextures[view.textureMap.textureIndex],
                        outputDepth: drawable.depthTextures[view.textureMap.textureIndex],
                        slice: view.textureMap.sliceIndex, viewport: view.textureMap.viewport, native: lease.native,
                        sourceEyeFromClip: uniforms.sourceEyeFromClip,
                        currentEyeFromSourceEye: uniforms.currentEyeFromSourceEye,
                        currentClipFromEye: uniforms.currentClipFromEye, timestamp: timestamp)
                    didDebugCapture = true
                    if debugCapture == nil { log.error("Live world capture unsupported or exceeded its 96 MiB bound.") }
                }
            }
            leases += drawableLeases
            drawable.encodePresent(commandBuffer: commands)
        }
        let completedLeases = leases
        let completedCornerScratch = cornerScratch
        let cornerPool = cornerPrecompute?.scratch
        let completedPresentation = presentation
        let completedDebugCapture = debugCapture
        let reportThisFrame = !loggedFrame || frameNumber % 120 == 0
        if presentation != nil { loggedFrame = true }
        frameNumber &+= 1
        commands.addCompletedHandler { [inFlight, report, log, gpuTiming] buffer in
            defer {
                inFlight.signal()
                completedLeases.forEach { $0.release() }
                // Only now can a later submission write these corner textures.
                cornerPool?.release(completedCornerScratch)
            }
            var acknowledged = false
            if let completedPresentation {
                let tokens = completedLeases.map { $0.native.lease }
                let result = tokens.withUnsafeBufferPointer {
                    sv_world_complete_frame($0.baseAddress, UInt32(completedPresentation.viewCount),
                                            UInt32(buffer.status.rawValue))
                }
                if result != 0 {
                    log.debug("Native world presentation acknowledgement rejected (\(result)); session may have ended.")
                }
                acknowledged = result == 0
            }
            if let gpuTiming,
               let summary = gpuTiming.record(completed: buffer.status == .completed, error: buffer.status == .error,
                    qualified: acknowledged && completedPresentation != nil,
                    debugBlit: completedDebugCapture != nil,
                    gpuStart: buffer.status == .completed ? buffer.gpuStartTime : 0,
                    gpuEnd: buffer.status == .completed ? buffer.gpuEndTime : 0) {
                report(.gpuTiming(summary))
            }
            if buffer.status == .completed { completedDebugCapture?.write(compositorAcknowledged: acknowledged) }
            if buffer.status == .error {
                report(.status("Immersive game Metal command failed: \(buffer.error?.localizedDescription ?? "unknown GPU error")"))
            } else if buffer.status == .completed, acknowledged, let completedPresentation, reportThisFrame {
                log.info("Actual GTA compositor command completed: views=\(completedPresentation.viewCount) game_frames=\(String(describing: completedPresentation.gameFrames), privacy: .public) request_frames=\(String(describing: completedPresentation.requestFrames), privacy: .public) head_frames=\(String(describing: completedPresentation.headFrames), privacy: .public)")
                report(.presented(completedPresentation))
            }
        }
        ownsSlot = false
        commands.commit()
    }

    private func submitViews(drawable: LayerRenderer.Drawable, timestamp: Double) {
        var request = SVXRViewRequest()
        request.version = 1
        request.size = UInt32(MemoryLayout<SVXRViewRequest>.size)
        request.frame = WorldRequestCounter.shared.next()
        request.presentation_time_seconds = timestamp
        request.view_count = UInt32(drawable.views.count)
        withUnsafeMutableBytes(of: &request.views) { bytes in
            let entries = bytes.bindMemory(to: SVXREyeView.self)
            for index in drawable.views.indices {
                var headFromEye = drawable.views[index].transform
                var projection = drawable.computeProjection(convention: .rightUpBack, viewIndex: index)
                withUnsafeMutableBytes(of: &entries[index].head_from_eye) { destination in
                    withUnsafeBytes(of: &headFromEye) { destination.copyMemory(from: $0) }
                }
                withUnsafeMutableBytes(of: &entries[index].projection_from_eye) { destination in
                    withUnsafeBytes(of: &projection) { destination.copyMemory(from: $0) }
                }
            }
        }
        let result = sv_xr_submit_views(&request)
        if result != 0 { reportStatus("Native compositor view submission failed (\(result)).") }
    }

    /// Encodes the corner-depth kernel for one eye on `commands`, ahead of that eye's draw. Returns nil,
    /// leaving the single-pass pipeline in use for this eye, when no bounded scratch texture is free.
    private func encodeCornerDepths(lease: GameWorldLease, uniforms: GameWorldUniforms, commands: any MTLCommandBuffer,
                                    cornerScratch: inout CornerScratchLease) -> (any MTLTexture)? {
        guard let precompute = cornerPrecompute else { return nil }
        guard let corners = precompute.scratch.acquire(device: layer.device, width: Int(lease.native.width),
                                                       height: Int(lease.native.height)) else {
            if !loggedCornerFallback {
                loggedCornerFallback = true
                log.error("Corner-depth precompute had no bounded scratch texture for \(lease.native.width)x\(lease.native.height); that eye used the single-pass pipeline.")
            }
            return nil
        }
        // Held by this command buffer until its completion handler returns it.
        cornerScratch.textures.append(corners)
        guard let encoder = commands.makeComputeCommandEncoder() else { return nil }
        var uniforms = uniforms
        encoder.label = "Game world corner depths"
        encoder.setComputePipelineState(precompute.kernel)
        encoder.setBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setTexture(lease.depth, index: 0)
        encoder.setTexture(corners, index: 1)
        let groupWidth = precompute.kernel.threadExecutionWidth
        let groupHeight = max(1, precompute.kernel.maxTotalThreadsPerThreadgroup / groupWidth)
        encoder.dispatchThreadgroups(MTLSize(width: (corners.width + groupWidth - 1) / groupWidth,
                                             height: (corners.height + groupHeight - 1) / groupHeight, depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: groupWidth, height: groupHeight, depth: 1))
        encoder.endEncoding()
        return corners
    }

    private func encode(drawable: LayerRenderer.Drawable, stageFromHead: simd_float4x4?,
                        leases: [GameWorldLease], commands: any MTLCommandBuffer,
                        cornerScratch: inout CornerScratchLease) -> Int {
        var clearedTextures = Set<String>()
        var encodedViews = 0
        for (eye, view) in drawable.views.enumerated() {
            let map = view.textureMap
            let firstPass = clearedTextures.insert("\(map.textureIndex):\(map.sliceIndex)").inserted
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = drawable.colorTextures[map.textureIndex]
            pass.colorAttachments[0].slice = map.sliceIndex
            pass.colorAttachments[0].loadAction = firstPass ? .clear : .load
            pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
            pass.depthAttachment.texture = drawable.depthTextures[map.textureIndex]
            pass.depthAttachment.slice = map.sliceIndex
            pass.depthAttachment.loadAction = firstPass ? .clear : .load
            pass.depthAttachment.storeAction = .store
            pass.depthAttachment.clearDepth = 0
            var source: (lease: GameWorldLease, uniforms: GameWorldUniforms)?
            if let stageFromHead, let lease = leases.first(where: { $0.native.view_index == UInt32(eye) }) {
                source = (lease, worldUniforms(lease: lease, stageFromHead: stageFromHead, headFromEye: view.transform,
                    projection: drawable.computeProjection(convention: .rightUpBack, viewIndex: eye)))
            }
            // The kernel is encoded on this command buffer before the eye's render pass that reads its result.
            let corners = source.flatMap {
                encodeCornerDepths(lease: $0.lease, uniforms: $0.uniforms, commands: commands, cornerScratch: &cornerScratch)
            }
            guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { continue }
            encoder.label = "Actual GTA world eye \(eye)"
            encoder.setViewport(map.viewport)
            if let (lease, sourceUniforms) = source {
                var uniforms = sourceUniforms
                if let corners, let precompute = cornerPrecompute {
                    encoder.setRenderPipelineState(precompute.pipeline)
                    encoder.setVertexTexture(corners, index: 1)
                } else {
                    encoder.setRenderPipelineState(pipeline)
                }
                encoder.setDepthStencilState(depthState)
                encoder.setCullMode(.none)
                encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
                encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
                encoder.setVertexTexture(lease.depth, index: 0)
                encoder.setFragmentTexture(lease.color, index: 0)
                encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4,
                    instanceCount: Int(lease.native.width) * Int(lease.native.height))
                encodedViews += 1
                if !loggedFrame {
                    log.info("Actual GTA source eye \(eye): size=\(lease.native.width)x\(lease.native.height) format=\(lease.color.pixelFormat.rawValue) color_device=\(lease.color.device.registryID) compositor_device=\(self.layer.device.registryID) eye_x=\(view.transform.columns.3.x) projection_x=\(uniforms.currentClipFromEye.columns.2.x) source_near=\(lease.native.depth_near) source_far=\(lease.native.depth_far) corner_precompute=\(corners != nil)")
                }
            }
            encoder.endEncoding()
        }
        return encodedViews
    }

    private func worldUniforms(lease: GameWorldLease, stageFromHead: simd_float4x4,
                               headFromEye: simd_float4x4, projection: simd_float4x4) -> GameWorldUniforms {
        // Subtract large GTA world origins in Double on the CPU, then send the
        // small relative eye matrix to both the shader and its live diagnostic.
        let gameFromStage = worldDoubleMatrix(lease.gameFromStage)
        let stageFromHead = worldDoubleMatrix(stageFromHead)
        let headFromCurrentEye = worldDoubleMatrix(headFromEye)
        let gameFromCurrentEye = gameFromStage * stageFromHead * headFromCurrentEye
        let gameFromSourceEye = worldDoubleMatrix(lease.gameFromEye)
        let relativeEye = gameFromCurrentEye.inverse * gameFromSourceEye
        var uniforms = GameWorldUniforms(sourceEyeFromClip: lease.sourceProjection.inverse,
            currentEyeFromSourceEye: worldFloatMatrix(relativeEye), currentClipFromEye: projection,
            sourceSize: SIMD2<UInt32>(lease.native.width, lease.native.height))
        uniforms.sourceDepthReversed = lease.sourceProjection.columns.2.z >= 0 ? 1 : 0
        let sourceSRGB = lease.color.pixelFormat == .rgba8Unorm_srgb || lease.color.pixelFormat == .bgra8Unorm_srgb
        let destinationEncoded = layer.configuration.colorFormat == .bgra8Unorm
            || layer.configuration.colorFormat == .rgba8Unorm
        if sourceSRGB && destinationEncoded { uniforms.colorTransfer = 1 }
        else if !sourceSRGB && !destinationEncoded { uniforms.colorTransfer = 2 }
        return uniforms
    }
}

private final class WorldRequestCounter: @unchecked Sendable {
    static let shared = WorldRequestCounter()
    private let lock = NSLock()
    private var frame: UInt64 = 0
    func next() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        frame &+= 1
        return frame
    }
}

/// An old renderer's delayed teardown cannot disable a newly opened camera.
private final class WorldCameraOwnership: @unchecked Sendable {
    static let shared = WorldCameraOwnership()
    private let lock = NSLock()
    private var owner: UUID?
    func acquire(_ token: UUID) -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        let result = sv_xr_world_camera_set_enabled(1)
        if result == 0 { owner = token }
        return result
    }
    func release(_ token: UUID) {
        lock.lock()
        defer { lock.unlock() }
        guard owner == token else { return }
        owner = nil
        _ = sv_xr_world_camera_set_enabled(0)
    }
}
#endif

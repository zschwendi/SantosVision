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
        if GameWorldDisplayQuality.requested { configuration.maxRenderQuality = .init(rawValue: 1) }
        let interactionFormats = capabilities.supportedTrackingAreasFormats
        if interactionFormats.contains(.r16Uint) { configuration.trackingAreasFormat = .r16Uint }
        else if interactionFormats.contains(.r8Uint) { configuration.trackingAreasFormat = .r8Uint }
        if configuration.trackingAreasFormat != .invalid {
            configuration.trackingAreasUsage.formUnion(.renderTarget)
        }
        if GameWorldDebugCapture.requestedPrefix() != nil {
            configuration.colorUsage.formUnion([.renderTarget, .shaderRead])
            configuration.depthUsage.formUnion([.renderTarget, .shaderRead])
        }
        let layouts = capabilities.supportedLayouts(options: [])
        configuration.layout = layouts.contains(.dedicated) ? .dedicated
            : layouts.contains(.layered) ? .layered : .shared
        configuration.defaultDepthRange = SIMD2<Float>(GameWorldDepthRange.selectedFar(nativeFar: sv_xr_native_far_plane_metres()),
            max(0.05, capabilities.supportedMinimumNearPlaneDistance))
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

struct GameWorldSingleSourcePresentation: Sendable {
    let targetViewCount: Int
    let sourceViewIndex: Int
    let gameFrame: UInt64
    let requestFrame: UInt64
    let headFrame: UInt64
    let retainedEpoch: UInt64
}

struct GameWorldLifecycleEvent: Sendable {
    let stage: String
    let uptime: Double
    let layerState: String
    let trackingState: String
    let rendererActive: Bool
    let rendererStopped: Bool
    let ownsWorldCamera: Bool
    var detail: String? = nil
    var viewCount: Int? = nil
    var trackedPose: Bool? = nil
    var requestBytesHex: String? = nil
    var motionTrackingJSON: String? = nil
    var sourceGuardBandScale: Float? = nil
    var sourceCoverageSaturated: Bool? = nil
    var displayRenderQuality: Float? = nil
    var maximumDisplayRenderQuality: Float? = nil

    var reportValue: [String: Any] {
        var value: [String: Any] = ["stage": stage, "uptime_seconds": uptime,
            "layer_state": layerState, "tracking_state": trackingState,
            "renderer_active": rendererActive, "renderer_stopped": rendererStopped,
            "owns_world_camera_lease": ownsWorldCamera]
        if let detail { value["detail"] = detail }
        if let viewCount { value["view_count"] = viewCount }
        if let trackedPose { value["tracked_pose"] = trackedPose }
        if let requestBytesHex { value["request_bytes_hex"] = requestBytesHex }
        value["source_guard_band_scale"] = sourceGuardBandScale ?? GameWorldSourceGuardBand.configuredScale
        if let sourceCoverageSaturated { value["source_coverage_saturated"] = sourceCoverageSaturated }
        if let displayRenderQuality { value["display_render_quality"] = displayRenderQuality }
        if let maximumDisplayRenderQuality { value["maximum_display_render_quality"] = maximumDisplayRenderQuality }
        if let motionTrackingJSON, let data = motionTrackingJSON.data(using: .utf8),
           let motion = try? JSONSerialization.jsonObject(with: data) {
            value["motion_tracking"] = motion
        }
        return value
    }
}

enum GameWorldEvent: Sendable {
    case lifecycle(GameWorldLifecycleEvent)
    case status(String)
    case presented(GameWorldPresentation)
    // Cheap liveness signal; unlike .presented this never writes a report.
    case gameFrameCompleted(uptime: Double)
    case singleSourcePresented(GameWorldSingleSourcePresentation)
    case gpuTiming(GameWorldGpuTimingSummary)
    case senseUtilityTracking(active: Bool, sequence: UInt64)
    case ended(String)
}

struct GameWorldUniforms {
    var sourceEyeFromClip: simd_float4x4
    var currentEyeFromSourceEye: simd_float4x4
    var currentClipFromEye: simd_float4x4
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32 = 1
    var colorTransfer: UInt32 = 0
    // Keep visible arms/clothing opaque. The former 15–45cm fade produced the
    // recorded first-person Bayer holes; normal ~10cm near clipping protects
    // intersections while this small radial range handles the closest pixels.
    var nearFadeDistances = SIMD2<Float>(0.05, 0.10)
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
    let hud: GameWorldHUD.Snapshot?
    let opticalFrame: GameWorldScopeOptics.Candidate
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
        var optical = SVWorldOpticsFrame()
        optical.abi_version = 1
        optical.byte_size = UInt32(MemoryLayout<SVWorldOpticsFrame>.size)
        guard MemoryLayout<SVWorldOpticsFrame>.size == 112,
              sv_world_frame_optics_v1(native.lease, &optical) == 0,
              optical.abi_version == 1, optical.byte_size == 112,
              optical.lease == native.lease, optical.provenance_epoch != 0,
              optical.game_frame == native.game_frame, optical.request_frame == native.request_frame,
              optical.head_frame == native.head_frame, optical.presentation_time == native.presentation_time,
              optical.source_eye == native.view_index, optical.view_count == native.view_count,
              optical.reserved.0 == 0, optical.reserved.1 == 0,
              optical.reserved.2 == 0, optical.reserved.3 == 0,
              let optics = GameWorldScopeOptics.Optics(flags: optical.flags, magnification: optical.magnification,
                  referenceFOV: optical.reference_fov_degrees, actualFOV: optical.actual_fov_degrees,
                  scopeSequence: optical.scope_sequence) else { return nil }
        let opticalFrame = GameWorldScopeOptics.Candidate(lease: native.lease, epoch: optical.provenance_epoch,
            gameFrame: native.game_frame, requestFrame: native.request_frame, headFrame: native.head_frame,
            cameraSequence: optical.camera_sequence, presentationTime: native.presentation_time,
            sourceEye: eye, viewCount: viewCount, optics: optics)
        guard opticalFrame.qualified else { return nil }
        if optics.scoped {
            let reversed = projection.columns.2.z >= 0
            guard let retained = XRHeadTracking.shared.retainedPose(headFrame: native.head_frame, now: timestamp),
                  retained.requestFrame == native.request_frame, retained.eyes.indices.contains(eye),
                  let physical = retained.eyes[eye].physicalProjectionFromEye,
                  let expected = GameWorldScopeOptics.expectedSourceProjection(retained.eyes[eye].projectionFromEye,
                      optics: optics, depthReversed: true),
                  GameWorldSourcePoseCheck.projectionError(retained: expected, lease: projection,
                      leaseDepthReversed: reversed) <= GameWorldSourcePoseCheck.projectionTolerance,
                  GameWorldScopeOptics.admitSource(projection, depthReversed: reversed, retainedPhysical: physical,
                      guardScale: retained.eyes[eye].sourceGuardBandScale, optics: optics).passed else { return nil }
        }
        self.native = native
        self.color = color
        self.depth = depth
        self.opticalFrame = opticalFrame
        sourceProjection = projection
        gameFromEye = eyeTransform
        gameFromStage = stageTransform
        hud = GameWorldHUD.Snapshot(world: native, device: device)
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

/// One source eye image with its provenance. `.actual` eyes carry a native lease;
/// the probe's eyes carry none and are never acknowledged or reported as game output.
private struct GameWorldSourceEye: @unchecked Sendable {
    let kind: GameWorldSourceKind
    let viewIndex: Int
    var color: any MTLTexture
    let depth: any MTLTexture
    let width: UInt32
    let height: UInt32
    let sourceProjection: simd_float4x4
    let gameFromEye: simd_float4x4
    let gameFromStage: simd_float4x4
    let headFrame: UInt64
    let requestFrame: UInt64
    let gameFrame: UInt64
    let presentationTime: Double
    let depthNear: Float
    let depthFar: Float
    let lease: GameWorldLease?

    init(lease: GameWorldLease) {
        kind = .actual
        viewIndex = Int(lease.native.view_index)
        color = lease.color
        depth = lease.depth
        width = lease.native.width
        height = lease.native.height
        sourceProjection = lease.sourceProjection
        gameFromEye = lease.gameFromEye
        gameFromStage = lease.gameFromStage
        headFrame = lease.native.head_frame
        requestFrame = lease.native.request_frame
        gameFrame = lease.native.game_frame
        presentationTime = lease.native.presentation_time
        depthNear = lease.native.depth_near
        depthFar = lease.native.depth_far
        self.lease = lease
    }

    init(probe pair: GameWorldAnchorProbe.Pair, eye index: Int) {
        let eye = pair.eyes[index]
        kind = pair.sourceKind
        viewIndex = index
        color = eye.color
        depth = eye.depth
        width = UInt32(eye.color.width)
        height = UInt32(eye.color.height)
        sourceProjection = eye.projectionFromEye
        gameFromEye = pair.stageFromHead * eye.headFromEye
        gameFromStage = matrix_identity_float4x4
        headFrame = pair.headFrame
        requestFrame = pair.requestFrame
        gameFrame = pair.renderedCompositorFrame
        presentationTime = pair.timestamp
        // Reverse-Z near/far from the projection, as the game bridge derives them.
        let z = eye.projectionFromEye.columns.2.z, w = eye.projectionFromEye.columns.3.z
        depthNear = z + 1 != 0 ? w / (z + 1) : .infinity
        depthFar = z != 0 ? w / z : .infinity
        lease = nil
    }
}

/// Per-frame path decision counters, reported in the periodic diagnostic log.
private struct GameWorldPathCounters {
    var copyDrawables: UInt64 = 0
    var adaptiveEyes: UInt64 = 0
    var precomputeEyes: UInt64 = 0
    var legacyEyes: UInt64 = 0
    var pairAcquired: UInt64 = 0
    var pairNotReady: UInt64 = 0
    var pairRejected: UInt64 = 0
    var copyFallbacks: [String: UInt64] = [:]
    var adaptiveFallbacks: UInt64 = 0
    mutating func copyFallback(_ reason: String) { copyFallbacks[reason, default: 0] &+= 1 }
}

// BEGIN GameWorldStatusLimiter
/// Status changes can alternate within one render frame. Bound expensive main
/// actor/report updates while preserving the first occurrence of each error.
struct GameWorldStatusLimiter {
    private var lastMessage = ""
    private var lastReported: [String: Double] = [:]
    mutating func shouldReport(_ message: String, at now: Double) -> Bool {
        guard message != lastMessage else { return false }
        lastMessage = message
        if let previous = lastReported[message], now - previous < 1 { return false }
        if lastReported[message] == nil, lastReported.count >= 32,
           let oldest = lastReported.min(by: { $0.value < $1.value })?.key {
            lastReported.removeValue(forKey: oldest)
        }
        lastReported[message] = now
        return true
    }
}
// END GameWorldStatusLimiter

/// Native Metal snapshots are immutable and already GPU-complete when leased.
/// The render thread only calls host mailbox/lease APIs, never guest functions.
final class GameWorldRenderer: @unchecked Sendable {
    private let layer: LayerRenderer
    private let session = ARKitSession()
    // Replaced only during startup if optional motion providers fail. ARKit
    // stops every supplied provider on run failure; a stopped one cannot retry.
    private var tracking = WorldTrackingProvider()
    private let motionTracking = XRMotionTracking(enabled:
        sv_xr_motion_arms_enabled_v1() != 0
        || ProcessInfo.processInfo.arguments.contains("--sv-motion-tracking-preview")
        || ProcessInfo.processInfo.environment["MUGUET_XR_MOTION_AIM"] == "1"
        || ProcessInfo.processInfo.environment["MUGUET_XR_MOTION_ARMS"] == "1")
    private let queue: any MTLCommandQueue
    private let pipeline: any MTLRenderPipelineState
    private let reticle: GameWorldReticle?
    private let hud: GameWorldHUD?
    private let senseUtilities: GameWorldSenseUtilities?
    private let senseUtilityTrackingLock = NSLock()
    private var senseUtilityTrackingLive = false
    private var senseUtilityTrackingSequence: UInt64 = 0
    /// Optional exact corner-depth precompute (MUGUET_COMPOSITOR_CORNER_PRECOMPUTE=1); nil keeps the
    /// single-pass pipeline above. Off by default.
    private let cornerPrecompute: (kernel: any MTLComputePipelineState, pipeline: any MTLRenderPipelineState,
                                   scratch: CornerScratchPool)?
    private var loggedCornerFallback = false
    /// Experimental paths, all nil unless their flag is "1". See docs/REPROJECTION-IMPLEMENTATION-20261011.md.
    private let sourceCopy: GameWorldSourceCopy?
    /// Physical A/B showed a partial improvement. Final ownership protects real
    /// far geometry; QA can disable this, and other rendering paths stay separate.
    private let finiteSkySupport: GameWorldFiniteSkySupport?
    private let finiteSkySupportUnavailable: GameWorldFiniteSkyTelemetry.Outcome?
    private let finiteSkyTelemetry: GameWorldFiniteSkyTelemetry
    /// Opt-in owner-0 gap fill; needs the finite-sky ownership mask.
    private let depthEdgeFill: GameWorldDepthEdgeFill?
    private let imageQuality: GameWorldImageQualityTelemetry
    /// Captured once for this renderer so both eyes, captures and optional
    /// precomputed corners use the same surface reconstruction policy.
    private let smoothSurfaceContinuity = GameWorldSurfaceContinuityPolicy.requested
    private let allowSyntheticAnchorCopy: Bool
    private let adaptiveMesh: GameWorldAdaptiveMesh?
    private let anchorProbe: GameWorldAnchorProbe?
    private let pairedRender: Bool
    /// Diagnostic only: one immutable native eye image reprojected into both
    /// physical eyes. Never a certified pair or a source-anchor copy.
    private let singleSourceStereo = ProcessInfo.processInfo.arguments.contains("--sv-world-single-source-stereo")
    /// Per-lease optics also needs the exact physical/request projection pair.
    /// The existing count/age bounds apply to normal world rendering too.
    private var retainsAnchors: Bool { true }
    private var loggedPairUnavailable = false
    private var pathCounters = GameWorldPathCounters()
    private let countersLock = NSLock()
    private var adaptiveFanTiles: UInt64 = 0
    private var adaptiveFallbackTiles: UInt64 = 0
    private var hudSelectionAttempts: UInt64 = 0
    private var hudSelections: UInt64 = 0
    private var scopedHUDSelectionAttempts: UInt64 = 0
    private var scopedHUDCanvasUnavailable: UInt64 = 0
    private var scopedVirtualBaselineDrawables: UInt64 = 0
    private var lastHUDGameFrame: UInt64 = 0
    private var lastReticleReference: [String: Any] = [:]
    private var sourceCoverage = GameWorldSourceCoverage()
    private var sourceCoverageMagnification: Float = 1
    private let depthState: any MTLDepthStencilState
    private let report: @Sendable (GameWorldEvent) -> Void
    private let gpuTiming = ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_GPU_TIMING"] == "1"
        ? GameWorldGpuTiming() : nil
    /// Probe presentations are timed in their own namespace and only logged; they never reach the runtime report.
    private let probeTiming: GameWorldGpuTiming?
    private let stopLock = NSLock()
    private var stopped = false
    private var active = true
    private let sessionToken = UUID()
    private let inFlight = DispatchSemaphore(value: 3)
    private var statusLimiter = GameWorldStatusLimiter()
    private var frameNumber: UInt64 = 0
    private var nextLifecycleSample: Double = 0
    private var nextRejectedViewSample: Double = 0
    private var lastViewRequestResult: Int32?
    private var lastRejectedViewResult: Int32?
    private var nextViewRecoverySample: Double = 0
    private var loggedFrame = false
    private let debugPrefix = GameWorldDebugCapture.requestedPrefix()
    private let debugMotionPolicy = GameWorldCaptureMotionPolicy(arguments: ProcessInfo.processInfo.arguments)
    private var didDebugCapture = false
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameCompositor")
    private let probeLog = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameCompositorProbe")

    init(layer: LayerRenderer, utilityAtlas: (any MTLTexture)? = nil,
         report: @escaping @Sendable (GameWorldEvent) -> Void) throws {
        self.layer = layer
        if GameWorldDisplayQuality.requested { layer.renderQuality = .init(rawValue: 1) }
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
        reticle = GameWorldReticle(device: layer.device, library: library,
                                   colorFormat: layer.configuration.colorFormat,
                                   depthFormat: layer.configuration.depthFormat)
        let environment = ProcessInfo.processInfo.environment
        let cornerRequested = environment["MUGUET_COMPOSITOR_CORNER_PRECOMPUTE"] == "1"
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
        let copyRequested = environment["MUGUET_COMPOSITOR_SOURCE_ANCHOR_COPY"] == "1"
        let probeRequested = environment["MUGUET_COMPOSITOR_ANCHOR_PROBE"] == "1"
        hud = !copyRequested && !probeRequested
            && !ProcessInfo.processInfo.arguments.contains("--sv-world-single-source-stereo")
            ? GameWorldHUD(device: layer.device, library: library,
                colorFormat: layer.configuration.colorFormat, depthFormat: layer.configuration.depthFormat) : nil
        senseUtilities = !copyRequested && !probeRequested
            && !ProcessInfo.processInfo.arguments.contains("--sv-world-single-source-stereo")
            ? utilityAtlas.flatMap { GameWorldSenseUtilities(device: layer.device, library: library, atlas: $0,
                colorFormat: layer.configuration.colorFormat, depthFormat: layer.configuration.depthFormat,
                trackingFormat: layer.configuration.trackingAreasFormat) } : nil
        // The probe compares mesh against copy, so it needs the copy pipeline even when the
        // game-facing copy flag is off; with the probe on, no game frame reaches either path.
        sourceCopy = copyRequested || probeRequested
            ? GameWorldSourceCopy(device: layer.device, library: library,
                                  colorFormat: layer.configuration.colorFormat,
                                  depthFormat: layer.configuration.depthFormat) : nil
        allowSyntheticAnchorCopy = environment["MUGUET_COMPOSITOR_SOURCE_ANCHOR_COPY_ALLOW_SYNTHETIC"] == "1"
        let adaptiveRequested = RenderingOptimizations.adaptiveMeshEnabled
        let skyFanRequested = RenderingOptimizations.skyFanEnabled
        let skyRequested = GameWorldFiniteSkySupport.requested
        let skyCompatible = !copyRequested && !probeRequested && !adaptiveRequested
        finiteSkySupport = skyRequested && skyCompatible
            ? GameWorldFiniteSkySupport(device: layer.device, library: library,
                colorFormat: layer.configuration.colorFormat, depthFormat: layer.configuration.depthFormat) : nil
        finiteSkySupportUnavailable = !skyRequested ? .qaDisabled
            : !skyCompatible ? .incompatibleConfiguration
            : finiteSkySupport == nil ? .pipelineUnavailable : nil
        finiteSkyTelemetry = GameWorldFiniteSkyTelemetry(requested: skyRequested,
            available: finiteSkySupport != nil, unavailableReason: finiteSkySupportUnavailable)
        let fillRequested = GameWorldDepthEdgeFillPolicy.requested(environment: environment,
            arguments: ProcessInfo.processInfo.arguments, stored: GameWorldDepthEdgeFillPolicy.stored)
        depthEdgeFill = fillRequested && finiteSkySupport != nil
            ? GameWorldDepthEdgeFill(device: layer.device, library: library,
                colorFormat: layer.configuration.colorFormat, depthFormat: layer.configuration.depthFormat) : nil
        imageQuality = GameWorldImageQualityTelemetry(fillRequested: fillRequested, fillAvailable: depthEdgeFill != nil)
        adaptiveMesh = adaptiveRequested ? GameWorldAdaptiveMesh(device: layer.device, library: library,
                                                                 colorFormat: layer.configuration.colorFormat,
                                                                 depthFormat: layer.configuration.depthFormat,
                                                                 skyFanEnabled: skyFanRequested) : nil
        anchorProbe = probeRequested ? GameWorldAnchorProbe(device: layer.device, library: library) : nil
        probeTiming = probeRequested ? GameWorldGpuTiming() : nil
        pairedRender = environment["MUGUET_XR_PAIRED_RENDER"] == "1"
        let depthDescriptor = MTLDepthStencilDescriptor()
        depthDescriptor.depthCompareFunction = .greaterEqual
        depthDescriptor.isDepthWriteEnabled = true
        guard let depthState = layer.device.makeDepthStencilState(descriptor: depthDescriptor) else {
            throw GameWorldError.missingMetal
        }
        self.depthState = depthState
        if GameWorldSourceGuardBand.configuredScale > 1 {
            log.info("Source-only guard band enabled: scale=\(GameWorldSourceGuardBand.configuredScale). Display projection and eye spacing unchanged; fixed-size source loses angular resolution.")
        }
        log.info("Finite sky support: requested=\(skyRequested) available=\(self.finiteSkySupport != nil) reason=\(self.finiteSkySupportUnavailable?.rawValue ?? "ready", privacy: .public); partial physical improvement, no unseen coverage")
        log.info("Depth-edge gap fill: requested=\(fillRequested) available=\(self.depthEdgeFill != nil); owner-0 pixels inside the source image only")
        if cornerRequested {
            log.info("Corner-depth precompute requested: \(precompute == nil ? "unavailable, using the single-pass pipeline" : "enabled", privacy: .public)")
        }
        if copyRequested || probeRequested {
            log.info("Source-anchor fullscreen copy \(copyRequested ? "requested" : "created for the anchor probe only", privacy: .public): \(self.sourceCopy == nil ? "unavailable, mesh paths only" : "enabled (experimental, eligibility-gated)", privacy: .public) allow_synthetic_anchor=\(self.allowSyntheticAnchorCopy)")
        }
        if adaptiveRequested {
            log.info("Adaptive tile mesh requested: \(self.adaptiveMesh == nil ? "unavailable, legacy mesh" : "enabled (experimental), tolerance_ulp=\(self.adaptiveMesh?.toleranceULPs ?? 0)", privacy: .public)")
        }
        if probeRequested {
            log.info("Anchor probe requested: \(self.anchorProbe == nil ? "unavailable" : "enabled; analytic scene replaces game content, mode=\(self.anchorProbe?.mode.rawValue ?? "?"), hold_frames=\(self.anchorProbe?.holdFrames ?? 0)", privacy: .public) copy_pipeline=\(self.sourceCopy != nil)")
            if self.anchorProbe != nil, self.sourceCopy == nil, self.anchorProbe?.mode != .mesh {
                log.error("Anchor probe mode \(self.anchorProbe?.mode.rawValue ?? "?", privacy: .public) needs the copy pipeline, which failed to build; the probe will present the mesh path only.")
            }
        }
        if pairedRender {
            log.info("Paired render requested: pair_layout_592=\(GameWorldPairAcquisition.layoutMatchesDocumentation) pair_size=\(MemoryLayout<SVWorldPair>.size)")
        }
    }

    func start() async throws {
        recordLifecycle("startup_entered")
        try Task.checkCancellation()
        if SimulatorPoseFixture.shared == nil {
            guard WorldTrackingProvider.isSupported else { throw GameWorldError.unsupportedTracking }
            recordLifecycle("tracking_run_requested")
            let providers = await motionTracking.prepare(session: session, world: tracking)
            do {
                try await session.run(providers)
                motionTracking.didStart()
            }
            catch {
                if providers.count > 1 {
                    motionTracking.disableForSession(reason: String(reflecting: error))
                    tracking = WorldTrackingProvider()
                    recordLifecycle("motion_tracking_fallback", detail: "Optional tracking failed; retrying head tracking with a fresh provider.")
                    do { try await session.run([tracking]) }
                    catch {
                        recordLifecycle("tracking_run_failed", detail: String(reflecting: error))
                        throw error
                    }
                } else {
                    recordLifecycle("tracking_run_failed", detail: String(reflecting: error))
                    throw error
                }
            }
            recordLifecycle("tracking_run_returned")
        }
        try Task.checkCancellation()
        let acquired: Int32? = stopLock.withLock {
            stopped ? nil : WorldCameraOwnership.shared.acquire(sessionToken,
                reticleReady: reticle != nil, hudReady: hud != nil)
        }
        guard let result = acquired else { session.stop(); return }
        recordLifecycle("world_camera_requested", detail: "return_code=\(result)")
        guard result == 0 else { throw GameWorldError.cameraEnable(result) }
        reportStatus(anchorProbe == nil ? "Waiting for a qualified native game world frame with its actual depth."
                     : "Anchor probe active: an analytic scene replaces game content. No game frame is being presented.")
        let thread = Thread { [self] in renderLoop() }
        thread.name = "SantosVision native game immersive renderer"
        thread.qualityOfService = .userInteractive
        thread.start()
    }

    func stop() {
        stopLock.lock()
        stopped = true
        stopLock.unlock()
        updateSenseUtilityTracking(false)
        motionTracking.stop()
        if retainsAnchors { XRHeadTracking.shared.invalidateRetainedAnchors(reason: "renderer stopped") }
        WorldCameraOwnership.shared.release(sessionToken)
        session.stop()
    }

    func setActive(_ active: Bool) {
        stopLock.lock()
        if self.active != active { gpuTiming?.reset(); probeTiming?.reset() }
        self.active = active
        stopLock.unlock()
        motionTracking.setActive(active)
        if !active { updateSenseUtilityTracking(false) }
    }

    func setSenseUtilitiesVisible(_ visible: Bool) { senseUtilities?.setVisible(visible) }
    func setSenseUtilitiesPhoneOpen(_ open: Bool) { senseUtilities?.setPhoneOpen(open) }

    private func updateSenseUtilityTracking(_ active: Bool) {
        senseUtilityTrackingLock.lock()
        guard active != senseUtilityTrackingLive else { senseUtilityTrackingLock.unlock(); return }
        senseUtilityTrackingLive = active
        senseUtilityTrackingSequence &+= 1
        let sequence = senseUtilityTrackingSequence
        senseUtilityTrackingLock.unlock()
        report(.senseUtilityTracking(active: active, sequence: sequence))
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
        guard statusLimiter.shouldReport(message, at: ProcessInfo.processInfo.systemUptime) else { return }
        report(.status(message))
        log.info("\(message, privacy: .public)")
    }

    private func recordLifecycle(_ stage: String, detail: String? = nil,
                                 viewCount: Int? = nil, trackedPose: Bool? = nil,
                                 requestBytesHex: String? = nil,
                                 sourceGuardBandScale: Float? = nil,
                                 sourceCoverageSaturated: Bool? = nil) {
        let motionJSON = (try? JSONSerialization.data(withJSONObject: motionTracking.report, options: [.sortedKeys]))
            .flatMap { String(data: $0, encoding: .utf8) }
        report(.lifecycle(GameWorldLifecycleEvent(stage: stage,
            uptime: ProcessInfo.processInfo.systemUptime,
            layerState: String(describing: layer.state),
            trackingState: String(describing: tracking.state),
            rendererActive: isActive, rendererStopped: isStopped,
            ownsWorldCamera: WorldCameraOwnership.shared.isOwner(sessionToken),
            detail: detail, viewCount: viewCount, trackedPose: trackedPose,
            requestBytesHex: requestBytesHex, motionTrackingJSON: motionJSON,
            sourceGuardBandScale: sourceGuardBandScale, sourceCoverageSaturated: sourceCoverageSaturated,
            displayRenderQuality: layer.renderQuality.rawValue, maximumDisplayRenderQuality: layer.configuration.maxRenderQuality.rawValue)))
    }

    private func renderLoop() {
        var endReason = "stop requested"
        var previousLayerState: LayerRenderer.State?
        var previousTrackingState: DataProviderState?
        var previousActive: Bool?
        recordLifecycle("render_loop_started")
        defer {
            updateSenseUtilityTracking(false)
            motionTracking.stop()
            session.stop()
            if retainsAnchors { XRHeadTracking.shared.invalidateRetainedAnchors(reason: "render loop ended") }
            WorldCameraOwnership.shared.release(sessionToken)
            recordLifecycle("render_loop_ended", detail: endReason)
            report(.ended(endReason))
        }
        while !isStopped {
            let layerState = layer.state
            let trackingState = tracking.state
            let active = isActive
            if layerState != previousLayerState || trackingState != previousTrackingState || active != previousActive {
                if layerState != .running || trackingState != .running || !active {
                    updateSenseUtilityTracking(false)
                    motionTracking.invalidate(reason: "World renderer or tracking inactive")
                }
                previousLayerState = layerState
                previousTrackingState = trackingState
                previousActive = active
                recordLifecycle("render_state_changed")
            }
            guard isActive else { Thread.sleep(forTimeInterval: 0.01); continue }
            switch layer.state {
            case .invalidated:
                endReason = "compositor layer invalidated"
                return
            case .paused: Thread.sleep(forTimeInterval: 0.01)
            case .running: autoreleasepool { renderFrame() }
            @unknown default:
                endReason = "unknown compositor layer state"
                return
            }
        }
    }

    private func renderFrame() {
        guard inFlight.wait(timeout: .now() + 0.1) == .success else { return }
        var ownsSlot = true
        defer { if ownsSlot { inFlight.signal() } }
        // Do not enter a compositor submission that cannot be presented.
        guard let commands = queue.makeCommandBuffer() else { return }
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
        commands.label = "SantosVision actual game world compositor frame"
        var leases: [GameWorldLease] = []
        var cornerScratch = CornerScratchLease()
        var adaptiveScratch = AdaptiveScratchLease()
        var probeLease = GameWorldAnchorProbe.Lease()
        var presentation: GameWorldPresentation?
        var singleSourcePresentation: GameWorldSingleSourcePresentation?
        var probePresentation: (kind: GameWorldSourceKind, copy: Bool)?
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
            if builtIn { updateSenseUtilityTracking(stageFromHead != nil) }
            let sampleTime = ProcessInfo.processInfo.systemUptime
            if builtIn, sampleTime >= nextLifecycleSample {
                nextLifecycleSample = sampleTime + 5
                recordLifecycle("frame_sample", viewCount: drawable.views.count,
                                trackedPose: stageFromHead != nil,
                                sourceGuardBandScale: sourceCoverage.scale,
                                sourceCoverageSaturated: sourceCoverage.saturated)
            }
            var sources: [GameWorldSourceEye] = []
            var drawableLeases: [GameWorldLease] = []
            var pairQualified = false
            var acquisitionFailed = false
            var singleSource: GameWorldSourceEye?
            var singleSourceEpoch: UInt64?
            if builtIn, let stageFromHead, (1...2).contains(drawable.views.count) {
                let physicalProjections = drawable.views.indices.map {
                    drawable.computeProjection(convention: .rightUpBack, viewIndex: $0)
                }
                let sourceScale = sourceCoverage.sample(timestamp: timestamp, pose: stageFromHead,
                    projections: physicalProjections, magnification: sourceCoverageMagnification)
                let eyes = drawable.views.indices.compactMap { index -> XRRetainedEyeView? in
                    let physicalProjection = physicalProjections[index]
                    // Retain precisely the projection submitted to the game's
                    // scalar camera bridge. Encoding below still uses the full
                    // compositor projection for display-time reprojection.
                    guard let nativeProjection = GameWorldSourceGuardBand.projection(physicalProjection, scale: sourceScale) else { return nil }
                    return XRRetainedEyeView(headFromEye: drawable.views[index].transform,
                                      projectionFromEye: nativeProjection,
                                      physicalProjectionFromEye: physicalProjection,
                                      sourceGuardBandScale: sourceScale)
                }
                if eyes.count != drawable.views.count {
                    motionTracking.invalidate(reason: "Physical projection exceeds the native camera conversion budget")
                    reportStatus("The headset projection exceeds the native camera conversion budget.")
                    // Even a rejected source request must clear and present
                    // this acquired drawable before endSubmission().
                } else {
                // The request frame names the eye geometry; the head frame names the pose. Both are
                // retained together so a lease can be checked against the exact pair it claims.
                let requestFrame = WorldRequestCounter.shared.next()
                var headFrame: UInt64?
                if retainsAnchors {
                    headFrame = XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: stageFromHead, tracked: true,
                                                              anchor: trackedAnchor, requestFrame: requestFrame, eyes: eyes, retain: true)
                } else {
                    headFrame = XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: stageFromHead, tracked: true,
                                                              anchor: trackedAnchor, requestFrame: requestFrame, eyes: eyes, retain: false)
                }
                if let headFrame {
                    motionTracking.sample(timestamp: timestamp, stageFromHead: stageFromHead, headFrame: headFrame)
                } else {
                    motionTracking.invalidate(reason: "No accepted native head generation")
                }
                let viewRequestAccepted = submitViews(drawable: drawable, timestamp: timestamp,
                                                       requestFrame: requestFrame, eyes: eyes)
                if viewRequestAccepted {
                    WorldCameraOwnership.shared.refreshReady(sessionToken,
                        reticleReady: reticle != nil, hudReady: hud != nil)
                }
                if let anchorProbe {
                    // Probe: no native lease is acquired; the analytic pair stands in for the game.
                    let views = drawable.views.indices.map { index in
                        (headFromEye: eyes[index].headFromEye, projectionFromEye: eyes[index].projectionFromEye,
                         width: Int(drawable.views[index].textureMap.viewport.width),
                         height: Int(drawable.views[index].textureMap.viewport.height))
                    }
                    if let pair = anchorProbe.pair(compositorFrame: frameNumber, headFrame: headFrame, requestFrame: requestFrame,
                                                   timestamp: timestamp, stageFromHead: stageFromHead, anchor: trackedAnchor,
                                                   views: views, commands: commands, lease: &probeLease) {
                        sources = pair.eyes.indices.map { GameWorldSourceEye(probe: pair, eye: $0) }
                        pairQualified = true
                    } else {
                        reportStatus("Anchor probe: waiting for a retained head pose to render the analytic scene.")
                    }
                } else {
                    if pairedRender, !singleSourceStereo, drawable.views.count == 2 {
                        drawableLeases = acquirePair(timestamp: timestamp)
                        pairQualified = drawableLeases.count == 2
                    }
                    if drawableLeases.isEmpty {
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
                    }
                    sources = drawableLeases.map { GameWorldSourceEye(lease: $0) }
                    if singleSourceStereo {
                        let selected = selectSingleSource(drawableLeases, timestamp: timestamp,
                            currentHeadFrame: viewRequestAccepted ? headFrame : nil,
                            targetViewCount: drawable.views.count)
                        for lease in drawableLeases where lease.native.lease != selected?.source.lease?.native.lease {
                            lease.release()
                        }
                        drawableLeases = selected?.source.lease.map { [$0] } ?? []
                        sources = selected.map { [$0.source] } ?? []
                        singleSource = selected?.source
                        singleSourceEpoch = selected?.epoch
                        pairQualified = false
                    }
                    // A rejected view request is the actionable failure. Do not
                    // replace it with generic waiting text later in the same frame.
                    if drawableLeases.isEmpty, !acquisitionFailed, viewRequestAccepted {
                        reportStatus("Waiting for a qualified native game world frame with its actual depth.")
                    } else if !singleSourceStereo, drawableLeases.count < drawable.views.count, !acquisitionFailed, viewRequestAccepted {
                        reportStatus("Waiting for both distinct native game eye views and their matching depth.")
                    }
                }
                }
            } else if builtIn, stageFromHead == nil {
                motionTracking.invalidate(reason: "Head tracking unavailable")
                XRHeadTracking.shared.submit(timestamp: timestamp, stageFromHead: nil, tracked: false)
                reportStatus("Head pose unavailable; native world drawing is suspended until tracking returns.")
            } else if builtIn {
                reportStatus("The compositor supplied an unsupported game view count (\(drawable.views.count)); this bridge supports one or two views.")
            }
            let encoded = encode(drawable: drawable, stageFromHead: stageFromHead, sources: sources, pairQualified: pairQualified,
                                 timestamp: timestamp, commands: commands, cornerScratch: &cornerScratch, adaptiveScratch: &adaptiveScratch,
                                 singleSource: singleSource)
            if builtIn {
                sourceCoverageMagnification = encoded.displayOptics.magnification
                for source in sources where source.kind == .actual {
                    sourceCoverage.observeSourceAge(timestamp - source.presentationTime)
                }
            }
            if builtIn, encoded.views == drawable.views.count, !sources.isEmpty {
                if let first = sources.first, first.kind != .actual {
                    // Probe output: a separate namespace. Never a game presentation, never acknowledged.
                    probePresentation = (first.kind, encoded.copy)
                    // Counted as a presentation only when the command buffer completes successfully (see the handler).
                    reportStatus("Anchor probe active: \(first.kind.rawValue) analytic scene, path=\(encoded.copy ? "fullscreen copy with retained source anchor" : "depth mesh"), mode=\(anchorProbe?.mode.rawValue ?? "?"). No game frame is being presented.")
                } else if !drawableLeases.isEmpty {
                    if let singleSource, let singleSourceEpoch {
                        singleSourcePresentation = GameWorldSingleSourcePresentation(targetViewCount: drawable.views.count,
                            sourceViewIndex: singleSource.viewIndex, gameFrame: singleSource.gameFrame,
                            requestFrame: singleSource.requestFrame, headFrame: singleSource.headFrame,
                            retainedEpoch: singleSourceEpoch)
                    } else {
                        presentation = GameWorldPresentation(viewCount: drawable.views.count,
                            gameFrames: drawableLeases.map { $0.native.game_frame },
                            requestFrames: drawableLeases.map { $0.native.request_frame },
                            headFrames: drawableLeases.map { $0.native.head_frame })
                    }
                    if !didDebugCapture, let debugPrefix, let stageFromHead,
                       let captureSource = singleSource ?? sources.first(where: { $0.viewIndex == 0 }),
                       let lease = captureSource.lease,
                       lease.native.game_frame >= GameWorldDebugCapture.minimumGameFrame,
                       let view = drawable.views.first {
                        let physical = drawable.computeProjection(convention: .rightUpBack, viewIndex: 0)
                        let displayProjection = GameWorldScopeOptics.displayProjection(physical, headFromEye: view.transform,
                            optics: encoded.displayOptics) ?? physical
                        // Record the exact eye-0 world uniforms encode() used.
                        let physicalEyes = drawable.views.map(\.transform)
                        let worldEye = GameWorldScopeOptics.eyeTransforms(physicalEyes, optics: encoded.displayOptics)?.first
                            ?? view.transform
                        let uniforms = worldUniforms(eye: captureSource, stageFromHead: stageFromHead,
                            headFromEye: worldEye,
                            projection: displayProjection)
                        if debugMotionPolicy?.allows(uniforms.currentEyeFromSourceEye) == true {
                        debugCapture = GameWorldDebugCapture(prefix: debugPrefix, device: layer.device, commands: commands,
                            color: encoded.hudIsolated ? (lease.hud?.worldColor ?? lease.color) : lease.color,
                            depth: lease.depth,
                            outputColor: drawable.colorTextures[view.textureMap.textureIndex],
                            outputDepth: drawable.depthTextures[view.textureMap.textureIndex],
                            slice: view.textureMap.sliceIndex, viewport: view.textureMap.viewport, native: lease.native,
                            sourceEyeFromClip: uniforms.sourceEyeFromClip,
                            currentEyeFromSourceEye: uniforms.currentEyeFromSourceEye,
                            currentClipFromEye: uniforms.currentClipFromEye,
                            nearFadeDistances: uniforms.nearFadeDistances, timestamp: timestamp,
                            outputViewIndex: 0, renderingMethod: (singleSource == nil
                                ? (encoded.finiteSkyFirstEye ? "finite_sky_support_experiment_depth_mesh"
                                    : (encoded.copy ? "source_anchor_copy" : "independent_eye_depth_mesh"))
                                : "single_source_depth_stereo") + (encoded.hudIsolated ? "_single_hud_canvas" : ""))
                        didDebugCapture = true
                        if debugCapture == nil { log.error("Live world capture unsupported or exceeded its 96 MiB bound.") }
                        }
                    }
                }
            }
            leases += drawableLeases
            if let senseUtilities {
                senseUtilities.encode(drawable: drawable, commands: commands,
                    tracked: stageFromHead != nil && encoded.views == drawable.views.count && !sources.isEmpty)
            } else {
                GameWorldSenseUtilities.clearTracking(drawable: drawable, commands: commands)
            }
            drawable.encodePresent(commandBuffer: commands)
        }
        let completedLeases = leases
        let completedCornerScratch = cornerScratch
        let completedAdaptiveScratch = adaptiveScratch
        let completedProbeLease = probeLease
        let probe = anchorProbe
        let cornerPool = cornerPrecompute?.scratch
        let adaptivePool = adaptiveMesh?.pool
        let completedPresentation = presentation
        let completedSingleSourcePresentation = singleSourcePresentation
        let completedProbe = probePresentation
        let completedDebugCapture = debugCapture
        let reportThisFrame = !loggedFrame || frameNumber % 120 == 0
        if presentation != nil || probePresentation != nil || singleSourcePresentation != nil { loggedFrame = true }
        frameNumber &+= 1
        if reportThisFrame { logDiagnostics() }
        commands.addCompletedHandler { [inFlight, report, log, probeLog, gpuTiming, probeTiming, weak self] buffer in
            defer {
                inFlight.signal()
                completedLeases.forEach { $0.release() }
                // Only now can a later submission write these scratch resources.
                cornerPool?.release(completedCornerScratch)
                adaptivePool?.release(completedAdaptiveScratch)
                // Returns the probe's reader references and publishes the rendered pair only on success.
                probe?.complete(completedProbeLease, success: buffer.status == .completed)
            }
            if buffer.status == .completed, !completedAdaptiveScratch.sets.isEmpty {
                self?.accumulateAdaptiveCounters(completedAdaptiveScratch)
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
                if acknowledged, buffer.status == .completed {
                    report(.gameFrameCompleted(uptime: ProcessInfo.processInfo.systemUptime))
                }
            }
            if let gpuTiming,
               let summary = gpuTiming.record(completed: buffer.status == .completed, error: buffer.status == .error,
                    qualified: acknowledged && completedPresentation != nil,
                    debugBlit: completedDebugCapture != nil,
                    gpuStart: buffer.status == .completed ? buffer.gpuStartTime : 0,
                    gpuEnd: buffer.status == .completed ? buffer.gpuEndTime : 0) {
                report(.gpuTiming(summary))
            }
            if let completedProbe, buffer.status == .completed { probe?.recordPresentation(copy: completedProbe.copy) }
            if let completedProbe, let probeTiming,
               let summary = probeTiming.record(completed: buffer.status == .completed, error: buffer.status == .error,
                    qualified: true, debugBlit: false,
                    gpuStart: buffer.status == .completed ? buffer.gpuStartTime : 0,
                    gpuEnd: buffer.status == .completed ? buffer.gpuEndTime : 0) {
                var value = summary.reportValue
                value["scope"] = "anchor probe compositor command buffer: analytic source render (when re-rendered), presentation path, encodePresent; not game content"
                value["source_kind"] = completedProbe.kind.rawValue
                value["path"] = completedProbe.copy ? "copy" : "mesh"
                let json = (try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])).map { String(decoding: $0, as: UTF8.self) } ?? "{}"
                probeLog.info("probe_gpu_timing \(json, privacy: .public)")
            }
            if buffer.status == .completed { completedDebugCapture?.write(compositorAcknowledged: acknowledged) }
            if buffer.status == .error {
                report(.status("Immersive game Metal command failed: \(buffer.error?.localizedDescription ?? "unknown GPU error")"))
            } else if buffer.status == .completed, acknowledged, let completedPresentation, reportThisFrame {
                log.info("Actual GTA compositor command completed: views=\(completedPresentation.viewCount) game_frames=\(String(describing: completedPresentation.gameFrames), privacy: .public) request_frames=\(String(describing: completedPresentation.requestFrames), privacy: .public) head_frames=\(String(describing: completedPresentation.headFrames), privacy: .public)")
                report(.presented(completedPresentation))
            } else if buffer.status == .completed, let completedSingleSourcePresentation, reportThisFrame {
                // One source token cannot satisfy native two-distinct-eye acknowledgement.
                // Real GPU completion still informs the visibility watchdog separately.
                let retained = XRHeadTracking.shared.retainedPose(headFrame: completedSingleSourcePresentation.headFrame,
                    now: ProcessInfo.processInfo.systemUptime)
                if retained?.epoch == completedSingleSourcePresentation.retainedEpoch {
                    log.info("Single-source depth stereo diagnostic completed: source_eye=\(completedSingleSourcePresentation.sourceViewIndex) game_frame=\(completedSingleSourcePresentation.gameFrame) target_views=\(completedSingleSourcePresentation.targetViewCount); not two rendered game eyes.")
                    report(.singleSourcePresented(completedSingleSourcePresentation))
                } else {
                    log.debug("Single-source diagnostic completion discarded: retained source epoch expired.")
                }
            }
        }
        ownsSlot = false
        commands.commit()
    }

    private func selectSingleSource(_ leases: [GameWorldLease], timestamp: Double,
                                    currentHeadFrame: UInt64?, targetViewCount: Int)
        -> (source: GameWorldSourceEye, epoch: UInt64)? {
        let currentEpoch = currentHeadFrame.flatMap {
            XRHeadTracking.shared.retainedPose(headFrame: $0, now: timestamp)?.epoch
        }
        let candidates = leases.map { lease -> GameWorldSingleSourceSelection.Candidate in
            let retained = XRHeadTracking.shared.retainedPose(headFrame: lease.native.head_frame, now: timestamp)
            let qualified = retained.map { pose in
                GameWorldSourcePoseCheck.verify(retainedStageFromHead: pose.stageFromHead,
                    retainedEyes: pose.eyes, retainedRequestFrame: pose.requestFrame,
                    eyeIndex: Int(lease.native.view_index), requestFrame: lease.native.request_frame,
                    leaseGameFromStage: lease.gameFromStage, leaseGameFromEye: lease.gameFromEye,
                    leaseProjection: lease.sourceProjection,
                    leaseDepthReversed: lease.sourceProjection.columns.2.z >= 0).passed
            } ?? false
            return .init(lease: lease.native.lease, gameFrame: lease.native.game_frame,
                requestFrame: lease.native.request_frame, headFrame: lease.native.head_frame,
                sourceViewIndex: Int(lease.native.view_index), viewCount: Int(lease.native.view_count),
                presentationTime: lease.native.presentation_time, retainedEpoch: retained?.epoch ?? 0,
                poseQualified: qualified)
        }
        let decision = GameWorldSingleSourceSelection.select(candidates, currentEpoch: currentEpoch,
            timestamp: timestamp, targetViewCount: targetViewCount)
        guard let index = decision.index, let epoch = currentEpoch,
              XRHeadTracking.shared.retainedPose(headFrame: leases[index].native.head_frame, now: timestamp)?.epoch == epoch else {
            reportStatus("Single-source depth stereo diagnostic waiting: \(decision.reason).")
            return nil
        }
        return (GameWorldSourceEye(lease: leases[index]), epoch)
    }

    /// One atomic pair from the certified producer, or nothing. Any rejection releases both leases.
    private func acquirePair(timestamp: Double) -> [GameWorldLease] {
        guard GameWorldPairAcquisition.layoutMatchesDocumentation else {
            if !loggedPairUnavailable {
                loggedPairUnavailable = true
                log.error("Paired render requested but SVWorldPair is \(MemoryLayout<SVWorldPair>.size) bytes, not the documented 592; using independent eye acquisition.")
            }
            return []
        }
        let (code, pair) = GameWorldPairAcquisition.acquire()
        switch code {
        case 0: break
        case 1: pathCounters.pairNotReady &+= 1; return []
        default:
            pathCounters.pairRejected &+= 1
            if !loggedPairUnavailable { loggedPairUnavailable = true; log.error("sv_world_acquire_pair returned \(code); using independent eye acquisition.") }
            return []
        }
        // Every token the producer handed out is released exactly once: raw tokens here until a
        // GameWorldLease owns them, then only through that lease (release() is idempotent).
        func releaseRaw(_ token: UInt64) { if token != 0 { _ = sv_world_release_frame(token) } }
        if let reason = GameWorldPairValidation.reject(pair) {
            pathCounters.pairRejected &+= 1
            pathCounters.copyFallback("pair_\(reason)")
            releaseRaw(pair.eyes.0.lease)
            if pair.eyes.1.lease != pair.eyes.0.lease { releaseRaw(pair.eyes.1.lease) }
            return []
        }
        guard let left = GameWorldLease(native: pair.eyes.0, device: layer.device, eye: 0, viewCount: 2, timestamp: timestamp) else {
            pathCounters.pairRejected &+= 1
            pathCounters.copyFallback("pair_left_lease_checks")
            releaseRaw(pair.eyes.0.lease)
            releaseRaw(pair.eyes.1.lease)
            return []
        }
        guard let right = GameWorldLease(native: pair.eyes.1, device: layer.device, eye: 1, viewCount: 2, timestamp: timestamp) else {
            pathCounters.pairRejected &+= 1
            pathCounters.copyFallback("pair_right_lease_checks")
            left.release()
            releaseRaw(pair.eyes.1.lease)
            return []
        }
        pathCounters.pairAcquired &+= 1
        return [left, right]
    }

    private func submitViews(drawable: LayerRenderer.Drawable, timestamp: Double, requestFrame: UInt64, eyes: [XRRetainedEyeView]) -> Bool {
        var request = SVXRViewRequest()
        request.version = 1
        request.size = UInt32(MemoryLayout<SVXRViewRequest>.size)
        request.frame = requestFrame
        request.presentation_time_seconds = timestamp
        request.view_count = UInt32(drawable.views.count)
        withUnsafeMutableBytes(of: &request.views) { bytes in
            let entries = bytes.bindMemory(to: SVXREyeView.self)
            for index in drawable.views.indices {
                var headFromEye = eyes[index].headFromEye
                var projection = eyes[index].projectionFromEye
                withUnsafeMutableBytes(of: &entries[index].head_from_eye) { destination in
                    withUnsafeBytes(of: &headFromEye) { destination.copyMemory(from: $0) }
                }
                withUnsafeMutableBytes(of: &entries[index].projection_from_eye) { destination in
                    withUnsafeBytes(of: &projection) { destination.copyMemory(from: $0) }
                }
            }
        }
        let result = sv_xr_submit_views(&request)
        if result != 0 {
            let now = ProcessInfo.processInfo.systemUptime
            if lastRejectedViewResult != result || now >= nextRejectedViewSample {
                lastRejectedViewResult = result
                nextRejectedViewSample = now + 5
                // Preserve the exact ABI bytes, including nonfinite Float bit
                // patterns, without putting invalid JSON numbers in the report.
                let bytes = withUnsafeBytes(of: request) { bytes in
                    bytes.map { String(format: "%02x", $0) }.joined()
                }
                recordLifecycle("view_request_rejected", detail: "return_code=\(result)",
                                viewCount: drawable.views.count, requestBytesHex: bytes)
            }
            reportStatus("Native compositor view submission failed (\(result)).")
        } else if let lastViewRequestResult, lastViewRequestResult != 0 {
            let now = ProcessInfo.processInfo.systemUptime
            if now >= nextViewRecoverySample {
                nextViewRecoverySample = now + 5
                recordLifecycle("view_request_recovered")
            }
        }
        lastViewRequestResult = result
        return result == 0
    }

    /// Encodes the corner-depth kernel for one eye on `commands`, ahead of that eye's draw. Returns nil,
    /// leaving the single-pass pipeline in use for this eye, when no bounded scratch texture is free.
    private func encodeCornerDepths(eye: GameWorldSourceEye, uniforms: GameWorldUniforms, commands: any MTLCommandBuffer,
                                    cornerScratch: inout CornerScratchLease) -> (any MTLTexture)? {
        guard let precompute = cornerPrecompute else { return nil }
        guard let corners = precompute.scratch.acquire(device: layer.device, width: Int(eye.width),
                                                       height: Int(eye.height)) else {
            if !loggedCornerFallback {
                loggedCornerFallback = true
                log.error("Corner-depth precompute had no bounded scratch texture for \(eye.width)x\(eye.height); that eye used the single-pass pipeline.")
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
        encoder.setTexture(eye.depth, index: 0)
        encoder.setTexture(corners, index: 1)
        let groupWidth = precompute.kernel.threadExecutionWidth
        let groupHeight = max(1, precompute.kernel.maxTotalThreadsPerThreadgroup / groupWidth)
        encoder.dispatchThreadgroups(MTLSize(width: (corners.width + groupWidth - 1) / groupWidth,
                                             height: (corners.height + groupHeight - 1) / groupHeight, depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: groupWidth, height: groupHeight, depth: 1))
        encoder.endEncoding()
        return corners
    }

    /// The copy path is a drawable-level decision: one anchor labels both views, so every view's
    /// source must come from the same consumed pose and request, verified against the retained record.
    private func copyEligibility(drawable: LayerRenderer.Drawable, sources: [GameWorldSourceEye], pairQualified: Bool,
                                 timestamp: Double) -> (retained: XRRetainedHeadPose, verified: [GameWorldSourcePoseCheck.Result])? {
        guard sourceCopy != nil else { return nil }
        guard sources.allSatisfy({ $0.lease?.opticalFrame.optics.sourceCopyAllowed ?? true }) else {
            pathCounters.copyFallback("scoped_optical_magnification"); return nil
        }
        guard sources.count == drawable.views.count, !sources.isEmpty else { pathCounters.copyFallback("incomplete_sources"); return nil }
        if drawable.views.count == 2, !pairQualified { pathCounters.copyFallback("unpaired_eyes"); return nil }
        guard let first = sources.first,
              sources.allSatisfy({ $0.headFrame == first.headFrame && $0.requestFrame == first.requestFrame }) else {
            pathCounters.copyFallback("mixed_pose_or_request"); return nil
        }
        guard first.headFrame != 0, let retained = XRHeadTracking.shared.retainedPose(headFrame: first.headFrame, now: timestamp) else {
            pathCounters.copyFallback("anchor_not_retained"); return nil
        }
        if retained.isSynthetic, first.kind == .actual, !allowSyntheticAnchorCopy {
            pathCounters.copyFallback("synthetic_anchor_not_allowed"); return nil
        }
        var results: [GameWorldSourcePoseCheck.Result] = []
        for source in sources {
            guard source.viewIndex >= 0, source.viewIndex < drawable.views.count else { pathCounters.copyFallback("view_index"); return nil }
            let reversed = source.sourceProjection.columns.2.z >= 0
            let result = GameWorldSourcePoseCheck.verify(retainedStageFromHead: retained.stageFromHead, retainedEyes: retained.eyes,
                                                         retainedRequestFrame: retained.requestFrame,
                                                         eyeIndex: source.viewIndex, requestFrame: source.requestFrame,
                                                         leaseGameFromStage: source.gameFromStage, leaseGameFromEye: source.gameFromEye,
                                                         leaseProjection: source.sourceProjection, leaseDepthReversed: reversed)
            if !result.passed { pathCounters.copyFallback("pose_check_\(result.reason)"); return nil }
            // The copy reads nearest source depth at the output pixel's ray; that equals the mesh's
            // interpolated plane only on an identical pixel lattice with the identical projection.
            // The ray origin under the labelled source anchor is head_from_eye: the present view transform must
            // equal the one retained with the pose, or a changed eye calibration would be mislabelled.
            let lattice = GameWorldCopyLatticeCheck.verify(sourceWidth: Int(source.width), sourceHeight: Int(source.height),
                                                           viewport: drawable.views[source.viewIndex].textureMap.viewport,
                                                           sourceProjection: source.sourceProjection, sourceDepthReversed: reversed,
                                                           targetProjection: drawable.computeProjection(convention: .rightUpBack, viewIndex: source.viewIndex),
                                                           retainedHeadFromEye: retained.eyes[source.viewIndex].headFromEye,
                                                           currentHeadFromEye: drawable.views[source.viewIndex].transform)
            if !lattice.passed { pathCounters.copyFallback("lattice_\(lattice.reason)"); return nil }
            results.append(result)
        }
        return (retained, results)
    }

    private func encode(drawable: LayerRenderer.Drawable, stageFromHead: simd_float4x4?,
                        sources: [GameWorldSourceEye], pairQualified: Bool, timestamp: Double,
                        commands: any MTLCommandBuffer, cornerScratch: inout CornerScratchLease,
                        adaptiveScratch: inout AdaptiveScratchLease,
                        singleSource: GameWorldSourceEye? = nil) -> (views: Int, copy: Bool, finiteSkyFirstEye: Bool, hudIsolated: Bool,
                            displayOptics: GameWorldScopeOptics.Optics) {
        var clearedTextures = Set<String>()
        var encodedViews = 0
        var finiteSkyFirstEye = false
        // Both eyes use one original-game aim sample, even if the script thread
        // publishes another sample between the two encoding calls.
        let gameplayAim = reticle == nil ? nil : GameWorldReticle.snapshot()
        // One bounded tuple for the whole drawable. Source-generation gaps are
        // explicit: a coherent current ray does not imply the older displayed
        // foreground has already reached that pose.
        var reticleAudit: [String: Any] = ["snapshot_available": gameplayAim != nil,
            "scope": "CPU aim/reference selection; no rendered gun or impact certification"]
        if let snapshot = gameplayAim {
            reticleAudit["aim_view_epoch"] = snapshot.aim.view_epoch
            reticleAudit["aim_flags"] = snapshot.aim.flags
            reticleAudit["aim_age_seconds"] = snapshot.aim.age_seconds
            reticleAudit["common_native_reference"] = snapshot.reference != nil
            if let reference = snapshot.reference {
                reticleAudit["reference_camera_sequence"] = reference.camera
                reticleAudit["reference_request_frame"] = reference.request
                reticleAudit["reference_head_frame"] = reference.head
                reticleAudit["producer_camera_sequence_tag"] = reference.producer
                reticleAudit["scope_sequence"] = reference.scope
                reticleAudit["displayed_sources"] = sources.filter { $0.kind == .actual }.map { source -> [String: Any] in
                    var row: [String: Any] = ["eye": source.viewIndex, "game_frame": source.gameFrame,
                        "head_frame": source.headFrame, "request_frame": source.requestFrame]
                    if let optical = source.lease?.opticalFrame {
                        row["camera_sequence"] = optical.cameraSequence
                        row["matching_view_epoch"] = optical.epoch == snapshot.aim.view_epoch
                        if let lag = GameWorldAimReferencePolicy.displayedSourceLag(
                            producer: reference.producer, source: optical.cameraSequence) {
                            row["camera_generations_behind_producer_tag"] = lag
                        }
                    }
                    return row
                }
            }
        }
        countersLock.lock()
        lastReticleReference = reticleAudit
        countersLock.unlock()
        // Keep the original complete game color unless every selected eye has
        // certified decomposition. Select ONE HUD once, never one per eye.
        let orderedSources = drawable.views.indices.compactMap { eye in
            sources.first(where: { $0.viewIndex == eye })
        }
        let requiresOpticalPair = singleSource == nil && !orderedSources.isEmpty
            && orderedSources.allSatisfy { $0.kind == .actual }
        let opticalIndex = requiresOpticalPair
            ? GameWorldScopeOptics.selectedIndex(orderedSources.map { $0.lease?.opticalFrame }, viewCount: drawable.views.count) : nil
        let opticsQualified = !requiresOpticalPair || opticalIndex != nil
        let displayOptics = opticalIndex.flatMap { orderedSources[$0].lease?.opticalFrame.optics }
            ?? singleSource?.lease?.opticalFrame.optics ?? .identity
        // Non-optical scope families retain the qualified native graphic too.
        // This fallback affects HUD placement only, never optical authority.
        let nativeScope = displayOptics.scoped || sv_xr_native_scope_active() == 1
        let hudIndex = hud != nil && stageFromHead != nil && singleSource == nil && opticsQualified
            ? GameWorldHUDPolicy.selectedIndex(orderedSources.map { $0.lease?.hud?.candidate },
                viewCount: drawable.views.count) : nil
        let hudSnapshot = hudIndex.flatMap { orderedSources[$0].lease?.hud }
        if hud != nil, !sources.isEmpty {
            countersLock.lock()
            hudSelectionAttempts &+= 1
            if let hudSnapshot {
                hudSelections &+= 1
                lastHUDGameFrame = hudSnapshot.native.game_frame
            }
            // A scoped drawable without the single canvas shows each eye's own
            // baked GTA HUD at its asymmetric source center (about 20 degrees
            // outward per eye on the captured calibration): never collimated.
            if nativeScope {
                scopedHUDSelectionAttempts &+= 1
                if hudSnapshot == nil { scopedHUDCanvasUnavailable &+= 1 }
            }
            countersLock.unlock()
        }
        // Scoped world only: virtual eyes at 1/m baseline (natural disparity
        // after magnification). The HUD canvas keeps the physical eye pose.
        let physicalEyes = drawable.views.map(\.transform)
        let worldEyes = GameWorldScopeOptics.eyeTransforms(physicalEyes, optics: displayOptics) ?? physicalEyes
        if displayOptics.magnification != 1, !sources.isEmpty {
            countersLock.lock()
            scopedVirtualBaselineDrawables &+= 1
            countersLock.unlock()
        }
        var displaySources = opticsQualified ? sources : []
        if hudSnapshot != nil {
            for index in displaySources.indices {
                if let isolated = displaySources[index].lease?.hud {
                    displaySources[index].color = isolated.worldColor
                }
            }
        }
        // Copy path decision for the whole drawable. The probe may force the mesh for A/B/A.
        var copyDecision: (retained: XRRetainedHeadPose, verified: [GameWorldSourcePoseCheck.Result])?
        if opticsQualified, singleSource == nil, sourceCopy != nil, stageFromHead != nil, anchorProbe?.useCopyPath(now: timestamp) ?? true {
            copyDecision = copyEligibility(drawable: drawable, sources: sources, pairQualified: pairQualified, timestamp: timestamp)
        }
        if let copyDecision {
            // Label the drawable with the pose the source was rendered at. The system's documented
            // display-time adjustment then owns the correction; our encoder performs no reprojection.
            if let anchor = copyDecision.retained.anchor { drawable.deviceAnchor = anchor }
            pathCounters.copyDrawables &+= 1
        }
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
            var source: (eye: GameWorldSourceEye, uniforms: GameWorldUniforms)?
            let physicalProjection = drawable.computeProjection(convention: .rightUpBack, viewIndex: eye)
            let projection = GameWorldScopeOptics.displayProjection(physicalProjection, headFromEye: view.transform,
                optics: displayOptics) ?? physicalProjection
            let worldEye = worldEyes.indices.contains(eye) ? worldEyes[eye] : view.transform
            if let stageFromHead, let sourceEye = singleSource ?? displaySources.first(where: { $0.viewIndex == eye }) {
                source = (sourceEye, worldUniforms(eye: sourceEye, stageFromHead: stageFromHead, headFromEye: worldEye,
                                                   projection: projection))
            }
            if let source, source.eye.kind == .actual,
               let density = GameWorldSamplingDensity.measure(eye: eye, displayProjection: projection,
                   displayPixels: SIMD2(Int(map.viewport.width), Int(map.viewport.height)),
                   sourceProjection: source.eye.sourceProjection,
                   sourcePixels: SIMD2(Int(source.eye.width), Int(source.eye.height))) {
                imageQuality.record(density: density)
            }
            let skyOutcome: GameWorldFiniteSkyTelemetry.Outcome
            var skyResult: GameWorldFiniteSkySupport.Result?
            if let unavailable = finiteSkySupportUnavailable { skyOutcome = unavailable }
            else if singleSource != nil { skyOutcome = .singleSourceDiagnostic }
            else if copyDecision != nil { skyOutcome = .sourceCopyActive }
            else if let source, source.eye.kind == .actual, let finiteSkySupport {
                skyResult = finiteSkySupport.encode(commands: commands, pass: pass, viewport: map.viewport,
                    projection: projection, far: drawable.depthRange.x, uniforms: source.uniforms,
                    color: source.eye.color, depth: source.eye.depth)
                skyOutcome = skyResult != nil ? .encoded : .encodeUnavailable
            } else { skyOutcome = .noQualifiedActualSource }
            finiteSkyTelemetry.record(view: eye, outcome: skyOutcome, source: source.map {
                .init(eye: $0.eye.viewIndex, game: $0.eye.gameFrame,
                      request: $0.eye.requestFrame, head: $0.eye.headFrame)
            })
            if imageQuality.fillRequested, skyOutcome != .encoded, source?.eye.kind == .actual {
                imageQuality.record(view: eye, outcome: .finiteSkyOwnershipUnavailable)
            }
            if skyOutcome == .encoded, let source {
                encodedViews += 1
                if eye == 0 { finiteSkyFirstEye = true }
                // The fill reconstructs the exact frame the world pass drew.
                var fill: GameWorldDepthEdgeFillUniforms?
                if imageQuality.fillRequested {
                    if depthEdgeFill == nil { imageQuality.record(view: eye, outcome: .pipelineUnavailable) }
                    else if let skyResult {
                        fill = GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection: source.eye.sourceProjection,
                            currentEyeFromSourceEye: source.uniforms.currentEyeFromSourceEye, displayProjection: projection,
                            viewportOrigin: SIMD2(Float(map.viewport.originX), Float(map.viewport.originY)),
                            viewportSize: SIMD2(Float(map.viewport.width), Float(map.viewport.height)),
                            sourceSize: source.uniforms.sourceSize, sourceDepthReversed: source.uniforms.sourceDepthReversed,
                            colorTransfer: source.uniforms.colorTransfer,
                            nearestDistance: source.uniforms.nearFadeDistances.y, skyRadius: skyResult.radius)
                        if fill == nil { imageQuality.record(view: eye, outcome: .invalidGeometry) }
                    }
                }
                // Ownership and depth repair have finished. This new pass has
                // only attachment0, matching the normal reticle pipeline.
                if fill != nil || hudSnapshot != nil || gameplayAim != nil {
                    pass.colorAttachments[0].loadAction = .load
                    pass.depthAttachment.loadAction = .load
                    if let overlay = commands.makeRenderCommandEncoder(descriptor: pass) {
                        overlay.label = "Depth-edge fill, single HUD canvas and gameplay reticle after finite sky support"
                        overlay.setViewport(map.viewport)
                        // Gaps first: HUD and reticle set their own pipeline and depth state.
                        if let fill, let depthEdgeFill, let skyResult {
                            imageQuality.record(view: eye, outcome: depthEdgeFill.encode(overlay, commands: commands,
                                eye: eye, uniforms: fill, ownership: skyResult.ownership, color: source.eye.color,
                                depth: source.eye.depth, telemetry: imageQuality) ? .encoded : .resourceMismatch)
                        }
                        if let hudSnapshot {
                            hud?.encode(overlay, snapshot: hudSnapshot,
                                headFromEye: view.transform, projection: physicalProjection, nativeScope: nativeScope)
                        }
                        if let stageFromHead, let gameplayAim,
                           gameplayAim.aim.view_epoch == source.eye.lease?.opticalFrame.epoch {
                            let gameFromCurrentEye = gameplayAim.gameFromStage(fallback: source.eye.gameFromStage)
                                * worldDoubleMatrix(stageFromHead) * worldDoubleMatrix(worldEye)
                            reticle?.encode(overlay, aim: gameplayAim.aim, gameFromCurrentEye: gameFromCurrentEye,
                                            projection: projection, viewport: map.viewport)
                        }
                        overlay.endEncoding()
                    } else {
                        if fill != nil { imageQuality.record(view: eye, outcome: .encoderUnavailable) }
                        // The world is already encoded; do not acknowledge it
                        // as a complete UI presentation if its overlay failed.
                        // Without HUD/reticle the unfilled world is still complete.
                        if hudSnapshot != nil || gameplayAim != nil {
                            encodedViews -= 1
                            reportStatus("The immersive HUD overlay could not be encoded; waiting for the next complete frame.")
                        }
                    }
                }
                continue
            }
            // Pre-pass work for this eye, encoded on this command buffer before its render pass.
            var corners: (any MTLTexture)?
            var adaptiveSet: AdaptiveScratchSet?
            if copyDecision == nil, let source {
                if let adaptiveMesh {
                    if let set = adaptiveMesh.pool.acquire(device: layer.device, width: Int(source.eye.width), height: Int(source.eye.height)) {
                        // Leased the moment it is acquired, so a failed encode still returns it on completion.
                        adaptiveScratch.sets.append(set)
                        if adaptiveMesh.encodeClassification(commands: commands, set: set, uniforms: source.uniforms, depth: source.eye.depth) {
                            adaptiveSet = set
                        } else {
                            pathCounters.adaptiveFallbacks &+= 1
                        }
                    } else {
                        pathCounters.adaptiveFallbacks &+= 1
                    }
                }
                if adaptiveSet == nil {
                    corners = encodeCornerDepths(eye: source.eye, uniforms: source.uniforms, commands: commands, cornerScratch: &cornerScratch)
                }
            }
            guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { continue }
            encoder.label = "Actual GTA world eye \(eye)"
            encoder.setViewport(map.viewport)
            if let (sourceEye, sourceUniforms) = source {
                var uniforms = sourceUniforms
                if let copyDecision, let sourceCopy {
                    // Output and depth stay source-anchored; only the near fade is judged from the present head.
                    let copyUniforms = GameWorldCopyUniforms(sourceEyeFromClip: uniforms.sourceEyeFromClip,
                                                             sourceClipFromEye: sourceEye.sourceProjection,
                                                             currentEyeFromClip: projection.inverse,
                                                             currentClipFromEye: projection,
                                                             currentEyeFromSourceEye: uniforms.currentEyeFromSourceEye,
                                                             sourceSize: uniforms.sourceSize,
                                                             sourceDepthReversed: uniforms.sourceDepthReversed,
                                                             colorTransfer: uniforms.colorTransfer,
                                                             nearFadeDistances: uniforms.nearFadeDistances)
                    sourceCopy.encode(encoder, uniforms: copyUniforms, depthState: depthState, color: sourceEye.color, depth: sourceEye.depth)
                    if !loggedFrame {
                        let check = copyDecision.verified[min(eye, copyDecision.verified.count - 1)]
                        log.info("Source-anchor copy eye \(eye): kind=\(sourceEye.kind.rawValue, privacy: .public) head_frame=\(sourceEye.headFrame) request_frame=\(sourceEye.requestFrame) anchor=\(copyDecision.retained.isSynthetic ? "synthetic" : "arkit", privacy: .public) source_age_s=\(timestamp - copyDecision.retained.timestamp) source=\(sourceEye.width)x\(sourceEye.height) viewport=\(map.viewport.width)x\(map.viewport.height) pose_check_rotation=\(check.maximumRotationError) translation=\(check.maximumTranslationError) projection=\(check.maximumProjectionError) source_near=\(sourceEye.depthNear) source_far=\(sourceEye.depthFar) target_depth_range=\(drawable.depthRange.y)...\(drawable.depthRange.x)")
                    }
                } else if let adaptiveSet, let adaptiveMesh {
                    adaptiveMesh.encodeDraws(encoder, set: adaptiveSet, uniforms: uniforms, depthState: depthState,
                                             depth: sourceEye.depth, color: sourceEye.color)
                    pathCounters.adaptiveEyes &+= 1
                } else {
                    if let corners, let precompute = cornerPrecompute {
                        encoder.setRenderPipelineState(precompute.pipeline)
                        encoder.setVertexTexture(corners, index: 1)
                        pathCounters.precomputeEyes &+= 1
                    } else {
                        encoder.setRenderPipelineState(pipeline)
                        pathCounters.legacyEyes &+= 1
                    }
                    encoder.setDepthStencilState(depthState)
                    encoder.setCullMode(.none)
                    encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
                    encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
                    encoder.setVertexTexture(sourceEye.depth, index: 0)
                    encoder.setFragmentTexture(sourceEye.color, index: 0)
                    encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4,
                        instanceCount: Int(sourceEye.width) * Int(sourceEye.height))
                }
                encodedViews += 1
                if !loggedFrame {
                    log.info("Actual GTA source eye \(eye): kind=\(sourceEye.kind.rawValue, privacy: .public) size=\(sourceEye.width)x\(sourceEye.height) format=\(sourceEye.color.pixelFormat.rawValue) color_device=\(sourceEye.color.device.registryID) compositor_device=\(self.layer.device.registryID) eye_x=\(view.transform.columns.3.x) projection_x=\(uniforms.currentClipFromEye.columns.2.x) source_near=\(sourceEye.depthNear) source_far=\(sourceEye.depthFar) corner_precompute=\(corners != nil) adaptive=\(adaptiveSet != nil) copy=\(copyDecision != nil)")
                }
            }
            if source?.eye.kind == .actual, let hudSnapshot {
                hud?.encode(encoder, snapshot: hudSnapshot, headFromEye: view.transform, projection: physicalProjection, nativeScope: nativeScope)
            }
            if let source, source.eye.kind == .actual, let stageFromHead, let gameplayAim,
               gameplayAim.aim.view_epoch == source.eye.lease?.opticalFrame.epoch {
                let gameFromCurrentEye = gameplayAim.gameFromStage(fallback: source.eye.gameFromStage)
                    * worldDoubleMatrix(stageFromHead) * worldDoubleMatrix(worldEye)
                reticle?.encode(encoder, aim: gameplayAim.aim, gameFromCurrentEye: gameFromCurrentEye,
                                projection: projection, viewport: map.viewport)
            }
            encoder.endEncoding()
            if let adaptiveSet, let adaptiveMesh { adaptiveMesh.encodeCounterCopy(commands: commands, set: adaptiveSet) }
        }
        return (encodedViews, copyDecision != nil, finiteSkyFirstEye, hudSnapshot != nil, displayOptics)
    }

    /// Synchronized compact snapshot read by the existing lifecycle report.
    /// CPU encode outcomes are deliberately separate from GPU/pixel acceptance.
    func finiteSkySupportStatus() -> [String: Any] { finiteSkyTelemetry.reportValue }

    func imageQualityStatus() -> [String: Any] { imageQuality.reportValue }

    func hudCanvasStatus() -> [String: Any] {
        countersLock.lock()
        defer { countersLock.unlock() }
        return ["pipeline_available": hud != nil, "selection_attempts": hudSelectionAttempts,
            "all_eyes_qualified_selections": hudSelections, "last_selected_game_frame": lastHUDGameFrame,
            "native_scope_selection_attempts": scopedHUDSelectionAttempts,
            "native_scope_canvas_unavailable": scopedHUDCanvasUnavailable,
            "scoped_world_virtual_baseline_drawables": scopedVirtualBaselineDrawables,
            "reticle_reference": lastReticleReference,
            "angular_width_degrees": 60, "distance_metres": 2,
            "stage": "CPU selection only; game presentation GPU completion is reported separately"]
    }

    private func worldUniforms(eye: GameWorldSourceEye, stageFromHead: simd_float4x4,
                               headFromEye: simd_float4x4, projection: simd_float4x4) -> GameWorldUniforms {
        // Subtract large GTA world origins in Double on the CPU, then send the
        // small relative eye matrix to both the shader and its live diagnostic.
        let gameFromStage = worldDoubleMatrix(eye.gameFromStage)
        let stageFromHead = worldDoubleMatrix(stageFromHead)
        let headFromCurrentEye = worldDoubleMatrix(headFromEye)
        let gameFromCurrentEye = gameFromStage * stageFromHead * headFromCurrentEye
        let gameFromSourceEye = worldDoubleMatrix(eye.gameFromEye)
        let relativeEye = gameFromCurrentEye.inverse * gameFromSourceEye
        var uniforms = GameWorldUniforms(sourceEyeFromClip: eye.sourceProjection.inverse,
            currentEyeFromSourceEye: worldFloatMatrix(relativeEye), currentClipFromEye: projection,
            sourceSize: SIMD2<UInt32>(eye.width, eye.height))
        uniforms.reserved.x = smoothSurfaceContinuity ? 1 : 0
        uniforms.sourceDepthReversed = eye.sourceProjection.columns.2.z >= 0 ? 1 : 0
        let sourceSRGB = eye.color.pixelFormat == .rgba8Unorm_srgb || eye.color.pixelFormat == .bgra8Unorm_srgb
        let destinationEncoded = layer.configuration.colorFormat == .bgra8Unorm
            || layer.configuration.colorFormat == .rgba8Unorm
        if sourceSRGB && destinationEncoded { uniforms.colorTransfer = 1 }
        else if !sourceSRGB && !destinationEncoded { uniforms.colorTransfer = 2 }
        return uniforms
    }

    private func accumulateAdaptiveCounters(_ lease: AdaptiveScratchLease) {
        countersLock.lock()
        defer { countersLock.unlock() }
        for set in lease.sets {
            let counters = GameWorldAdaptiveMesh.counters(set)
            adaptiveFanTiles &+= UInt64(counters.fanTiles)
            adaptiveFallbackTiles &+= UInt64(counters.fallbackTiles)
        }
    }

    /// Periodic one-line diagnostic for every experimental path; silent when all flags are off.
    private func logDiagnostics() {
        guard sourceCopy != nil || adaptiveMesh != nil || anchorProbe != nil || pairedRender else { return }
        countersLock.lock()
        let fan = adaptiveFanTiles, fallback = adaptiveFallbackTiles
        countersLock.unlock()
        let counters = pathCounters
        let fallbacks = counters.copyFallbacks.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: ",")
        let anchors = XRHeadTracking.shared.retainedAnchorStatistics
        let adaptivePool = adaptiveMesh?.pool.statistics
        let probe = anchorProbe?.currentStatistics
        log.info("Compositor paths: copy_drawables=\(counters.copyDrawables) adaptive_eyes=\(counters.adaptiveEyes) precompute_eyes=\(counters.precomputeEyes) legacy_eyes=\(counters.legacyEyes) pair_acquired=\(counters.pairAcquired) pair_not_ready=\(counters.pairNotReady) pair_rejected=\(counters.pairRejected) copy_fallbacks=[\(fallbacks, privacy: .public)] adaptive_fallbacks=\(counters.adaptiveFallbacks) adaptive_fan_tiles=\(fan) adaptive_fallback_tiles=\(fallback) adaptive_pool=\(adaptivePool?.sets ?? 0)/\(adaptivePool?.busy ?? 0)/\(adaptivePool?.bytes ?? 0)B anchors_retained=\(anchors.retained) synthetic=\(anchors.retainedSynthetic) hits=\(anchors.hits) miss_unknown=\(anchors.missesUnknown) miss_expired=\(anchors.missesExpired) miss_epoch=\(anchors.missesEpoch) evicted_capacity=\(anchors.evictedByCapacity) evicted_age=\(anchors.evictedByAge) invalidations=\(anchors.invalidations) tracking_loss=\(anchors.trackingLossInvalidations) occupancy=\(anchors.occupancy)/\(anchors.capacity) probe_rendered=\(probe?.pairsRendered ?? 0) probe_issued=\(probe?.rendersIssued ?? 0) probe_gpu_failed=\(probe?.rendersFailedOnGPU ?? 0) probe_pending_deferred=\(probe?.rendersDeferredPending ?? 0) probe_held_invalidated=\(probe?.heldInvalidated ?? 0) probe_reused=\(probe?.pairsReused ?? 0) probe_mesh=\(probe?.meshPresentations ?? 0) probe_copy=\(probe?.copyPresentations ?? 0) probe_failures=\(probe?.renderFailures ?? 0) probe_deferred=\(probe?.rendersDeferredNoFreeSet ?? 0) probe_sets=\(probe?.sets ?? 0)/\(probe?.busySets ?? 0)/\(probe?.bytes ?? 0)B cap=\(probe?.bytesCap ?? 0)B probe_downscaled=\(probe?.downscaledRenders ?? 0) probe_alloc_failures=\(probe?.allocationFailures ?? 0) probe_source=\(probe?.lastSourceWidth ?? 0)x\(probe?.lastSourceHeight ?? 0)")
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
    func isOwner(_ token: UUID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return owner == token
    }
    func acquire(_ token: UUID, reticleReady: Bool, hudReady: Bool) -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        let result = sv_xr_world_camera_set_enabled(1)
        if result == 0 {
            owner = token
            _ = sv_xr_set_reticle_ready(reticleReady ? 1 : 0)
            _ = sv_xr_set_hud_ready(hudReady ? 1 : 0)
        }
        return result
    }
    func release(_ token: UUID) {
        lock.lock()
        defer { lock.unlock() }
        guard owner == token else { return }
        owner = nil
        _ = sv_xr_set_reticle_ready(0)
        _ = sv_xr_set_hud_ready(0)
        _ = sv_xr_world_camera_set_enabled(0)
    }
    func refreshReady(_ token: UUID, reticleReady: Bool, hudReady: Bool) {
        lock.lock()
        defer { lock.unlock() }
        guard owner == token else { return }
        // Tracking loss/recenter advances the native epoch without replacing
        // the Swift renderer. Rebind readiness without resetting the camera.
        _ = sv_xr_set_reticle_ready(reticleReady ? 1 : 0)
        _ = sv_xr_set_hud_ready(hudReady ? 1 : 0)
    }
}
#endif

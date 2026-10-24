#if SV_GAME_RUNTIME
import Foundation
import Metal
import simd

/// Opt-in depth-edge gap fill. Runs after the finite-sky ownership pass and
/// before HUD/reticle, inside the same overlay encoder when one is needed.
/// Only owner-0 pixels can be written; covered pixels, depth of covered
/// pixels, the display projection and the head pose are untouched.
final class GameWorldDepthEdgeFill: @unchecked Sendable {
    private let pipeline: any MTLRenderPipelineState
    private let countingPipeline: any MTLRenderPipelineState
    private let depthState: any MTLDepthStencilState

    init?(device: any MTLDevice, library: any MTLLibrary, colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat) {
        func state(counting: Bool) -> (any MTLRenderPipelineState)? {
            let constants = MTLFunctionConstantValues()
            var value = counting
            constants.setConstantValue(&value, type: .bool, index: 0)
            guard let vertex = library.makeFunction(name: "gameWorldSkyRepairVertex"),
                  let fragment = try? library.makeFunction(name: "gameWorldDepthEdgeFillFragment",
                                                           constantValues: constants) else { return nil }
            let descriptor = MTLRenderPipelineDescriptor()
            descriptor.label = counting ? "Depth-edge gap fill, sampled GPU counters" : "Depth-edge gap fill"
            descriptor.vertexFunction = vertex
            descriptor.fragmentFunction = fragment
            descriptor.colorAttachments[0].pixelFormat = colorFormat
            descriptor.depthAttachmentPixelFormat = depthFormat
            return try? device.makeRenderPipelineState(descriptor: descriptor)
        }
        let depth = MTLDepthStencilDescriptor()
        // Owner-0 depth is still the cleared far value; never pass anything nearer.
        depth.depthCompareFunction = .greaterEqual
        depth.isDepthWriteEnabled = true
        guard let plain = state(counting: false), let counting = state(counting: true),
              let depthState = device.makeDepthStencilState(descriptor: depth) else { return nil }
        pipeline = plain
        countingPipeline = counting
        self.depthState = depthState
    }

    /// Encodes one fullscreen triangle into an encoder whose attachments are
    /// the eye's color/depth targets with load actions. The encoder's later
    /// users set their own pipeline and depth state. False encodes nothing.
    func encode(_ encoder: any MTLRenderCommandEncoder, commands: any MTLCommandBuffer, eye: Int,
                uniforms: GameWorldDepthEdgeFillUniforms, ownership: any MTLTexture,
                color: any MTLTexture, depth: any MTLTexture, telemetry: GameWorldImageQualityTelemetry) -> Bool {
        guard ownership.pixelFormat == .r8Uint, depth.pixelFormat == .r32Float,
              color.width == Int(uniforms.sourceSize.x), color.height == Int(uniforms.sourceSize.y),
              depth.width == color.width, depth.height == color.height else { return false }
        var counters: (any MTLBuffer)?
        if telemetry.shouldSample(view: eye, now: ProcessInfo.processInfo.systemUptime) {
            counters = encoder.device.makeBuffer(length: 4 * MemoryLayout<UInt32>.stride, options: .storageModeShared)
            if let counters { memset(counters.contents(), 0, counters.length) }
        }
        encoder.setRenderPipelineState(counters == nil ? pipeline : countingPipeline)
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        var values = uniforms
        encoder.setFragmentBytes(&values, length: MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride, index: 0)
        if let counters { encoder.setFragmentBuffer(counters, offset: 0, index: 1) }
        encoder.setFragmentTexture(ownership, index: 0)
        encoder.setFragmentTexture(color, index: 1)
        encoder.setFragmentTexture(depth, index: 2)
        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
        if let counters {
            let sample = GameWorldDepthEdgeFillCounterLease(buffer: counters)
            commands.addCompletedHandler { completed in
                telemetry.recordSample(view: eye, counts: sample.counts, completed: completed.status == .completed)
            }
        }
        return true
    }
}

private struct GameWorldDepthEdgeFillCounterLease: @unchecked Sendable {
    let buffer: any MTLBuffer
    var counts: [UInt32] {
        let values = buffer.contents().bindMemory(to: UInt32.self, capacity: 4)
        return (0..<4).map { values[$0] }
    }
}

/// Fill encode outcomes, at most one GPU counter sample per eye per second
/// from successfully completed command buffers, and per-eye sampling density.
final class GameWorldImageQualityTelemetry: @unchecked Sendable {
    enum Outcome: String, CaseIterable, Sendable {
        case encoded
        case notRequested = "not_requested"
        case pipelineUnavailable = "pipeline_unavailable"
        case finiteSkyOwnershipUnavailable = "finite_sky_ownership_unavailable"
        case invalidGeometry = "invalid_geometry"
        case resourceMismatch = "input_resource_mismatch"
        case encoderUnavailable = "encoder_unavailable"
    }

    let fillRequested: Bool
    let fillAvailable: Bool
    private let lock = NSLock()
    private var outcomes = [[UInt64]](repeating: [UInt64](repeating: 0, count: Outcome.allCases.count), count: 2)
    private var nextSample = [Double](repeating: 0, count: 2)
    private var samples = [[String: Any]?](repeating: nil, count: 2)
    private var completedSamples: UInt64 = 0
    private var failedSamples: UInt64 = 0
    private var density = [GameWorldSamplingDensity?](repeating: nil, count: 2)

    init(fillRequested: Bool, fillAvailable: Bool) {
        self.fillRequested = fillRequested
        self.fillAvailable = fillAvailable
    }

    func record(view: Int, outcome: Outcome) {
        guard (0..<2).contains(view), let index = Outcome.allCases.firstIndex(of: outcome) else { return }
        lock.lock()
        defer { lock.unlock() }
        if outcomes[view][index] != .max { outcomes[view][index] += 1 }
    }

    func record(density value: GameWorldSamplingDensity) {
        guard (0..<2).contains(value.eye) else { return }
        lock.lock()
        density[value.eye] = value
        lock.unlock()
    }

    func shouldSample(view: Int, now: Double) -> Bool {
        guard (0..<2).contains(view), now.isFinite else { return false }
        lock.lock()
        defer { lock.unlock() }
        guard now >= nextSample[view] else { return false }
        nextSample[view] = now + 1
        return true
    }

    func recordSample(view: Int, counts: [UInt32], completed: Bool) {
        guard (0..<2).contains(view), counts.count == 4 else { return }
        lock.lock()
        defer { lock.unlock() }
        guard completed else { failedSamples &+= 1; return }
        completedSamples &+= 1
        samples[view] = ["uncovered_pixels": counts[0], "filled_from_finite_background": counts[1],
                         "filled_from_sky": counts[2], "left_uncovered": counts[3],
                         "observed_at_uptime_seconds": ProcessInfo.processInfo.systemUptime]
    }

    var reportValue: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        let views: [[String: Any]] = (0..<2).map { view in
            var counts: [String: UInt64] = [:]
            for (outcome, count) in zip(Outcome.allCases, outcomes[view]) where count > 0 { counts[outcome.rawValue] = count }
            var row: [String: Any] = ["view_index": view, "encode_outcomes": counts]
            if let sample = samples[view] { row["last_gpu_sample"] = sample }
            return row
        }
        return ["schema": 1,
                "depth_edge_fill": ["requested": fillRequested, "available": fillAvailable,
                    "completed_gpu_samples": completedSamples, "failed_gpu_samples": failedSamples, "views": views,
                    "scope": "Counters come from sampled, successfully completed compositor command buffers. A filled pixel copies one adjacent source texel from the same game frame: an estimate of hidden background, not newly rendered geometry, and not visual acceptance."] as [String: Any],
                "sampling_density": density.compactMap { $0?.reportValue },
                "sampling_density_scope": "Straight ahead of each eye, from the actual display projection/viewport and the displayed source lease projection/size. Above 1 display pixel per source texel is enlargement; this is not perceived sharpness."]
    }
}
#endif

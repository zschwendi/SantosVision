#if SV_GAME_RUNTIME
import ARKit
import Foundation
import Metal
import OSLog
import simd

/// MUGUET_COMPOSITOR_ANCHOR_PROBE (default OFF). Renders a frozen analytic scene
/// into two coherent source eye textures from ONE retained head pose, holds that
/// pair for a controlled number of compositor frames so its source age grows,
/// and tells the renderer which presentation path to use. It is a platform
/// probe: it never produces a native lease, is never acknowledged to the game,
/// and is never reported as a game presentation.
///
/// Resource ownership: every texture set is leased to each command buffer that
/// reads it and returned only from that buffer's completion handler. A set is
/// rewritten only when its reader count is zero, whatever the hold length,
/// drawable count or in-flight depth; if no set is free the held pair is reused
/// and the deferral is counted. A freshly rendered pair is `pending` until the
/// command buffer that rendered it completes successfully; only then does it
/// become `held`. A failed or incomplete render bumps its set's generation, so
/// no later frame can reuse those textures, and it is never counted as rendered.
final class GameWorldAnchorProbe: @unchecked Sendable {
    enum Mode: String { case mesh, copy, alternate }

    struct Eye: @unchecked Sendable {
        let color: any MTLTexture
        let depth: any MTLTexture
        let headFromEye: simd_float4x4
        let projectionFromEye: simd_float4x4
    }

    struct Pair: @unchecked Sendable {
        let headFrame: UInt64
        let requestFrame: UInt64
        let timestamp: Double
        let stageFromHead: simd_float4x4
        let anchor: DeviceAnchor?
        let eyes: [Eye]
        let renderedCompositorFrame: UInt64
        let setIndex: Int
        /// Generation of the set when this pair was rendered; a mismatch means the set was rewritten or invalidated.
        let setGeneration: UInt64
        var sourceKind: GameWorldSourceKind { anchor == nil ? .synthetic : .analytic }
    }

    /// Texture-set references one command buffer holds until it completes, plus the
    /// set it rendered into (if any) so completion can publish or invalidate it.
    struct Lease: Sendable {
        var setIndices: [Int] = []
        var writerSetIndex: Int?
        var writerGeneration: UInt64 = 0
    }

    struct Statistics: Sendable {
        /// Renders whose command buffer completed successfully and were published as the held pair.
        var pairsRendered: UInt64 = 0
        /// Renders encoded and awaiting completion (cumulative).
        var rendersIssued: UInt64 = 0
        /// Renders whose command buffer failed; their textures were invalidated, never published.
        var rendersFailedOnGPU: UInt64 = 0
        var pairsReused: UInt64 = 0
        var rendersDeferredNoFreeSet: UInt64 = 0
        var rendersDeferredPending: UInt64 = 0
        var heldInvalidated: UInt64 = 0
        var renderFailures: UInt64 = 0
        var meshPresentations: UInt64 = 0
        var copyPresentations: UInt64 = 0
        var sets: Int = 0
        var busySets: Int = 0
        var bytes: Int = 0
        var bytesCap: Int = 0
        var downscaledRenders: UInt64 = 0
        var allocationFailures: UInt64 = 0
        var lastSourceWidth: Int = 0
        var lastSourceHeight: Int = 0
    }

    private struct Vertex {
        var position: SIMD4<Float>
        var color: SIMD4<Float>
        var uv: SIMD2<Float>
        var pad = SIMD2<Float>.zero
    }

    private struct TextureSet {
        var eyes: [(color: any MTLTexture, depth: any MTLTexture, depthAttachment: any MTLTexture)]
        var width: Int
        var height: Int
        /// Command buffers that have been encoded to read this set and have not completed.
        var readers: Int
        /// Bumped on every write and on every failed or incomplete write; pairs carry the generation they saw.
        var generation: UInt64
        var bytes: Int { eyes.count * width * height * (4 + 4 + 4) }
    }

    static let maximumSets = 4
    static let maximumDimension = 4096
    /// Aggregate cap over every retained texture set (colour + R32 depth + depth attachment, both eyes).
    /// 384 MiB holds two 4K-sized stereo sets; MUGUET_COMPOSITOR_ANCHOR_PROBE_MAX_MIB (16...2048) overrides.
    /// When a set would exceed it the PROBE lowers its own analytic source resolution, never the game's.
    static let defaultMaximumAggregateBytes = 384 << 20
    let maximumAggregateBytes: Int

    let mode: Mode
    let holdFrames: UInt64
    let switchSeconds: Double
    let scale: Float
    private let device: any MTLDevice
    private let pipeline: any MTLRenderPipelineState
    private let depthState: any MTLDepthStencilState
    private let vertices: any MTLBuffer
    private let vertexCount: Int
    private let lock = NSLock()
    private var textureSets: [TextureSet] = []
    private var held: Pair?
    /// Rendered this frame or earlier, awaiting successful completion before it may be held.
    private var pending: Pair?
    private var statistics = Statistics()
    private var firstSelection: Double?
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameCompositorProbe")

    init?(device: any MTLDevice, library: any MTLLibrary) {
        let environment = ProcessInfo.processInfo.environment
        mode = Mode(rawValue: environment["MUGUET_COMPOSITOR_ANCHOR_PROBE_MODE"] ?? "") ?? .alternate
        let hold = UInt64(environment["MUGUET_COMPOSITOR_ANCHOR_PROBE_HOLD_FRAMES"] ?? "") ?? 8
        holdFrames = min(max(hold, 1), 900)
        let seconds = Double(environment["MUGUET_COMPOSITOR_ANCHOR_PROBE_SWITCH_SECONDS"] ?? "") ?? 5
        switchSeconds = seconds.isFinite && seconds > 0 ? min(seconds, 600) : 5
        let requestedScale = Float(environment["MUGUET_COMPOSITOR_ANCHOR_PROBE_SCALE"] ?? "") ?? 1
        scale = requestedScale.isFinite && requestedScale > 0 ? min(requestedScale, 2) : 1
        let capMiB = Int(environment["MUGUET_COMPOSITOR_ANCHOR_PROBE_MAX_MIB"] ?? "") ?? (Self.defaultMaximumAggregateBytes >> 20)
        maximumAggregateBytes = min(max(capMiB, 16), 2048) << 20
        self.device = device
        guard let vertex = library.makeFunction(name: "gameWorldProbeVertex"),
              let fragment = library.makeFunction(name: "gameWorldProbeFragment") else { return nil }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Anchor probe analytic source scene"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = .rgba8Unorm
        descriptor.colorAttachments[1].pixelFormat = .r32Float
        descriptor.depthAttachmentPixelFormat = .depth32Float
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor) else { return nil }
        self.pipeline = pipeline
        let depthDescriptor = MTLDepthStencilDescriptor()
        depthDescriptor.depthCompareFunction = .greaterEqual
        depthDescriptor.isDepthWriteEnabled = true
        guard let depthState = device.makeDepthStencilState(descriptor: depthDescriptor) else { return nil }
        self.depthState = depthState
        let geometry = Self.staticScene()
        vertexCount = geometry.count
        guard let buffer = device.makeBuffer(bytes: geometry, length: geometry.count * MemoryLayout<Vertex>.stride,
                                             options: .storageModeShared) else { return nil }
        buffer.label = "Anchor probe analytic scene"
        vertices = buffer
    }

    /// Frozen stage-space geometry: checkered ground, a wall with a depth edge in
    /// front of a farther wall, and sky left as clear depth. The near object is
    /// placed per pair in front of the head (see `nearObject`).
    private static func staticScene() -> [Vertex] {
        var out: [Vertex] = []
        func quad(_ a: SIMD3<Float>, _ b: SIMD3<Float>, _ c: SIMD3<Float>, _ d: SIMD3<Float>,
                  color: SIMD3<Float>, checker: Float, uvScale: Float = 1) {
            let corners = [(a, SIMD2<Float>(0, 0)), (b, SIMD2<Float>(uvScale, 0)), (c, SIMD2<Float>(uvScale, uvScale)),
                           (a, SIMD2<Float>(0, 0)), (c, SIMD2<Float>(uvScale, uvScale)), (d, SIMD2<Float>(0, uvScale))]
            for (position, uv) in corners {
                out.append(Vertex(position: SIMD4(position, 1), color: SIMD4(color, checker), uv: uv))
            }
        }
        // Ground at 1.5 m below the stage origin, 40 m deep, 1 m checks.
        quad(SIMD3(-20, -1.5, 0.5), SIMD3(20, -1.5, 0.5), SIMD3(20, -1.5, -40), SIMD3(-20, -1.5, -40),
             color: SIMD3(0.55, 0.60, 0.50), checker: 40, uvScale: 1)
        // Near wall segment at 6 m with a hard vertical edge at x = 0.
        quad(SIMD3(-2.5, -1.5, -6), SIMD3(0, -1.5, -6), SIMD3(0, 1.0, -6), SIMD3(-2.5, 1.0, -6),
             color: SIMD3(0.85, 0.35, 0.25), checker: 4)
        // Far wall at 9 m behind the edge.
        quad(SIMD3(-6, -1.5, -9), SIMD3(6, -1.5, -9), SIMD3(6, 2.0, -9), SIMD3(-6, 2.0, -9),
             color: SIMD3(0.25, 0.40, 0.80), checker: 8)
        return out
    }

    /// A small panel 0.30 m in front of the head, spanning the 0.15-0.45 m fade band when leaned into.
    private static func nearObject(stageFromHead: simd_float4x4) -> [Vertex] {
        let forward = -SIMD3(stageFromHead.columns.2.x, stageFromHead.columns.2.y, stageFromHead.columns.2.z)
        let right = SIMD3(stageFromHead.columns.0.x, stageFromHead.columns.0.y, stageFromHead.columns.0.z)
        let up = SIMD3(stageFromHead.columns.1.x, stageFromHead.columns.1.y, stageFromHead.columns.1.z)
        let head = SIMD3(stageFromHead.columns.3.x, stageFromHead.columns.3.y, stageFromHead.columns.3.z)
        let center = head + forward * 0.30 - up * 0.08 + right * 0.12
        let half: Float = 0.05
        let a = center - right * half - up * half, b = center + right * half - up * half
        let c = center + right * half + up * half, d = center - right * half + up * half
        let color = SIMD4<Float>(0.95, 0.85, 0.20, 2)
        return [Vertex(position: SIMD4(a, 1), color: color, uv: SIMD2(0, 0)),
                Vertex(position: SIMD4(b, 1), color: color, uv: SIMD2(1, 0)),
                Vertex(position: SIMD4(c, 1), color: color, uv: SIMD2(1, 1)),
                Vertex(position: SIMD4(a, 1), color: color, uv: SIMD2(0, 0)),
                Vertex(position: SIMD4(c, 1), color: color, uv: SIMD2(1, 1)),
                Vertex(position: SIMD4(d, 1), color: color, uv: SIMD2(0, 1))]
    }

    /// Which presentation path this compositor frame should use for the held pair.
    func useCopyPath(now: Double) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        switch mode {
        case .mesh: return false
        case .copy: return true
        case .alternate:
            let first = firstSelection ?? now
            firstSelection = first
            let phase = Int(((now - first) / switchSeconds).rounded(.down))
            // A/B/A: mesh, copy, mesh, ...
            return phase % 2 == 1
        }
    }

    /// Successful compositor presentations of the probe by path; called from the completion handler
    /// only when the command buffer completed, so failed buffers are never counted as presented.
    func recordPresentation(copy: Bool) {
        lock.lock()
        if copy { statistics.copyPresentations &+= 1 } else { statistics.meshPresentations &+= 1 }
        lock.unlock()
    }

    var currentStatistics: Statistics {
        lock.lock()
        defer { lock.unlock() }
        var copy = statistics
        copy.sets = textureSets.count
        copy.busySets = textureSets.filter { $0.readers > 0 }.count
        copy.bytes = textureSets.reduce(0) { $0 + $1.bytes }
        copy.bytesCap = maximumAggregateBytes
        return copy
    }

    /// Called from the completion handler of the command buffer that read (and possibly wrote)
    /// the sets. Returns reader references, then publishes the pending pair only if this buffer
    /// rendered it and completed successfully; otherwise the writer set is invalidated.
    func complete(_ lease: Lease, success: Bool) {
        guard !lease.setIndices.isEmpty || lease.writerSetIndex != nil else { return }
        lock.lock()
        defer { lock.unlock() }
        for index in lease.setIndices where index >= 0 && index < textureSets.count && textureSets[index].readers > 0 {
            textureSets[index].readers -= 1
        }
        guard let writer = lease.writerSetIndex, writer >= 0, writer < textureSets.count else { return }
        let matchesPending = pending?.setIndex == writer && pending?.setGeneration == lease.writerGeneration
        if success, matchesPending, let pending {
            held = pending
            self.pending = nil
            statistics.pairsRendered &+= 1
        } else {
            if matchesPending { pending = nil }
            if textureSets[writer].generation == lease.writerGeneration { textureSets[writer].generation &+= 1 }
            if !success {
                statistics.rendersFailedOnGPU &+= 1
                log.error("Anchor probe source render failed on the GPU; its textures were invalidated and nothing was published.")
            }
        }
    }

    /// The held pair, or a freshly rendered one when the hold has elapsed and a
    /// free texture set exists. The returned pair's set is leased to `commands`
    /// through `lease`; rendering of a new pair is encoded on `commands` ahead
    /// of the eye passes that read it, so the same buffer writes then reads it.
    func pair(compositorFrame: UInt64, headFrame: UInt64?, requestFrame: UInt64, timestamp: Double,
              stageFromHead: simd_float4x4?, anchor: DeviceAnchor?,
              views: [(headFromEye: simd_float4x4, projectionFromEye: simd_float4x4, width: Int, height: Int)],
              commands: any MTLCommandBuffer, lease: inout Lease) -> Pair? {
        lock.lock()
        defer { lock.unlock() }
        // A held pair whose set was rewritten or invalidated since it was rendered is dropped, never shown.
        if let current = held, current.setIndex >= textureSets.count || textureSets[current.setIndex].generation != current.setGeneration {
            held = nil
            statistics.heldInvalidated &+= 1
        }
        func reuseHeld() -> Pair? {
            guard let held, held.eyes.count == views.count else { return nil }
            statistics.pairsReused &+= 1
            textureSets[held.setIndex].readers += 1
            lease.setIndices.append(held.setIndex)
            return held
        }
        if let held, compositorFrame &- held.renderedCompositorFrame < holdFrames, held.eyes.count == views.count {
            return reuseHeld()
        }
        // One render in flight at a time: until it completes successfully, frames keep the held pair.
        if pending != nil { statistics.rendersDeferredPending &+= 1; return reuseHeld() }
        guard let headFrame, let stageFromHead, !views.isEmpty, views.count <= 2 else { return reuseHeld() }
        var width = min(max(Int((Float(views[0].width) * scale).rounded()), 16), Self.maximumDimension)
        var height = min(max(Int((Float(views[0].height) * scale).rounded()), 16), Self.maximumDimension)
        // Aggregate budget: a new or replacing set must fit beside every set still leased or held.
        // Only the probe's own analytic resolution is lowered to fit; game resolution is untouched.
        var downscaled = false
        while !fitsBudgetLocked(width: width, height: height, eyeCount: views.count, excluding: held?.setIndex), width > 16, height > 16 {
            width = max(16, width / 2)
            height = max(16, height / 2)
            downscaled = true
        }
        guard fitsBudgetLocked(width: width, height: height, eyeCount: views.count, excluding: held?.setIndex) else {
            statistics.allocationFailures &+= 1
            if statistics.allocationFailures == 1 {
                log.error("Anchor probe cannot fit a source pair within its \(self.maximumAggregateBytes >> 20) MiB aggregate cap; reusing the held pair.")
            }
            return reuseHeld()
        }
        if downscaled {
            statistics.downscaledRenders &+= 1
            if width != statistics.lastSourceWidth || height != statistics.lastSourceHeight {
                log.info("Anchor probe lowered its own analytic source to \(width)x\(height) to stay within \(self.maximumAggregateBytes >> 20) MiB; game resolution is unaffected.")
            }
        }
        statistics.lastSourceWidth = width
        statistics.lastSourceHeight = height
        // The held set is never a write candidate, so a failed render can still hand back the held pair intact.
        guard let setIndex = freeSetLocked(width: width, height: height, eyeCount: views.count, excluding: held?.setIndex) else {
            statistics.rendersDeferredNoFreeSet &+= 1
            return reuseHeld()
        }
        // Lease the writer set before encoding anything: this command buffer owns it through
        // completion whether or not every eye pass is encoded. The generation it writes is
        // recorded so completion can publish exactly this render or invalidate it.
        textureSets[setIndex].generation &+= 1
        let generation = textureSets[setIndex].generation
        textureSets[setIndex].readers += 1
        lease.setIndices.append(setIndex)
        lease.writerSetIndex = setIndex
        lease.writerGeneration = generation
        let near = Self.nearObject(stageFromHead: stageFromHead)
        guard let nearBuffer = device.makeBuffer(bytes: near, length: near.count * MemoryLayout<Vertex>.stride,
                                                 options: .storageModeShared) else {
            statistics.renderFailures &+= 1
            textureSets[setIndex].generation &+= 1   // partially prepared: never reusable
            return reuseHeld()
        }
        var eyes: [Eye] = []
        for (index, view) in views.enumerated() {
            let textures = textureSets[setIndex].eyes[index]
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = textures.color
            pass.colorAttachments[0].loadAction = .clear
            pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColor(red: 0.50, green: 0.72, blue: 0.95, alpha: 1) // sky
            pass.colorAttachments[1].texture = textures.depth
            pass.colorAttachments[1].loadAction = .clear
            pass.colorAttachments[1].storeAction = .store
            pass.colorAttachments[1].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 0) // reverse-Z clear depth
            pass.depthAttachment.texture = textures.depthAttachment
            pass.depthAttachment.loadAction = .clear
            pass.depthAttachment.storeAction = .dontCare
            pass.depthAttachment.clearDepth = 0
            guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else {
                statistics.renderFailures &+= 1
                textureSets[setIndex].generation &+= 1   // partially written: never reusable
                return reuseHeld()
            }
            encoder.label = "Anchor probe analytic source eye \(index)"
            let stageFromEye = stageFromHead * view.headFromEye
            var uniforms = view.projectionFromEye * stageFromEye.inverse
            encoder.setRenderPipelineState(pipeline)
            encoder.setDepthStencilState(depthState)
            encoder.setCullMode(.none)
            encoder.setVertexBytes(&uniforms, length: MemoryLayout<simd_float4x4>.stride, index: 1)
            encoder.setVertexBuffer(vertices, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: vertexCount)
            encoder.setVertexBuffer(nearBuffer, offset: 0, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: near.count)
            encoder.endEncoding()
            eyes.append(Eye(color: textures.color, depth: textures.depth, headFromEye: view.headFromEye,
                            projectionFromEye: view.projectionFromEye))
        }
        let pair = Pair(headFrame: headFrame, requestFrame: requestFrame, timestamp: timestamp, stageFromHead: stageFromHead,
                        anchor: anchor, eyes: eyes, renderedCompositorFrame: compositorFrame, setIndex: setIndex, setGeneration: generation)
        // This command buffer renders and then reads the pair; it becomes `held` only when it completes.
        pending = pair
        statistics.rendersIssued &+= 1
        return pair
    }

    /// Whether a set of this shape can be reused, replace an idle non-held set, or be added
    /// without the aggregate of retained bytes exceeding the cap.
    private func fitsBudgetLocked(width: Int, height: Int, eyeCount: Int, excluding: Int?) -> Bool {
        let needed = eyeCount * width * height * 12
        guard needed <= maximumAggregateBytes else { return false }
        func matches(_ set: TextureSet) -> Bool { set.width == width && set.height == height && set.eyes.count == eyeCount }
        if textureSets.indices.contains(where: { $0 != excluding && textureSets[$0].readers == 0 && matches(textureSets[$0]) }) { return true }
        let total = textureSets.reduce(0) { $0 + $1.bytes }
        if let replaceable = textureSets.indices.first(where: { $0 != excluding && $0 != pending?.setIndex && textureSets[$0].readers == 0 && !matches(textureSets[$0]) }) {
            // The idle set is evicted before its replacement is allocated, so the cap is never exceeded transiently.
            return total - textureSets[replaceable].bytes + needed <= maximumAggregateBytes
        }
        return textureSets.count < Self.maximumSets && total + needed <= maximumAggregateBytes
    }

    /// A set with no in-flight readers and the right shape, excluding the held pair's set;
    /// otherwise an idle non-held set of another shape is replaced in place (indices stay
    /// stable for outstanding leases); otherwise a new set within the bound.
    private func freeSetLocked(width: Int, height: Int, eyeCount: Int, excluding: Int?) -> Int? {
        func shapeMatches(_ set: TextureSet) -> Bool { set.width == width && set.height == height && set.eyes.count == eyeCount }
        let protected: Set<Int> = Set([excluding, pending?.setIndex].compactMap { $0 })
        if let index = textureSets.indices.first(where: { !protected.contains($0) && textureSets[$0].readers == 0 && shapeMatches(textureSets[$0]) }) {
            return index
        }
        let replaceable = textureSets.indices.first(where: { !protected.contains($0) && textureSets[$0].readers == 0 && !shapeMatches(textureSets[$0]) })
        guard replaceable != nil || textureSets.count < Self.maximumSets else { return nil }
        if let replaceable {
            // Evict first: drop the idle textures (readers == 0, not held, not pending) so old + new never coexist.
            textureSets[replaceable] = TextureSet(eyes: [], width: 0, height: 0, readers: 0, generation: textureSets[replaceable].generation &+ 1)
        }
        func make(_ format: MTLPixelFormat, _ usage: MTLTextureUsage, _ label: String) -> (any MTLTexture)? {
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
            descriptor.usage = usage
            descriptor.storageMode = .private
            let texture = device.makeTexture(descriptor: descriptor)
            texture?.label = label
            return texture
        }
        var eyes: [(color: any MTLTexture, depth: any MTLTexture, depthAttachment: any MTLTexture)] = []
        for eye in 0..<eyeCount {
            guard let color = make(.rgba8Unorm, [.renderTarget, .shaderRead], "Anchor probe source colour eye \(eye)"),
                  let depth = make(.r32Float, [.renderTarget, .shaderRead], "Anchor probe source depth eye \(eye)"),
                  let attachment = make(.depth32Float, [.renderTarget], "Anchor probe depth attachment eye \(eye)") else {
                statistics.allocationFailures &+= 1
                log.error("Anchor probe texture allocation failed at \(width)x\(height); the held pair is reused.")
                return nil
            }
            eyes.append((color, depth, attachment))
        }
        if let replaceable {
            textureSets[replaceable] = TextureSet(eyes: eyes, width: width, height: height, readers: 0, generation: textureSets[replaceable].generation)
            return replaceable
        }
        textureSets.append(TextureSet(eyes: eyes, width: width, height: height, readers: 0, generation: 0))
        return textureSets.count - 1
    }
}
#endif

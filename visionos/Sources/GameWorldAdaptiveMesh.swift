#if SV_GAME_RUNTIME
import Foundation
import Metal
import simd

// BEGIN AdaptiveScratchPool (the standalone GPU harness compiles this block verbatim)
/// One eye's bounded classification buffers for MUGUET_COMPOSITOR_ADAPTIVE_MESH.
struct AdaptiveScratchSet: @unchecked Sendable {
    let classes: any MTLBuffer
    let fanTiles: any MTLBuffer
    let fallbackTiles: any MTLBuffer
    /// Two 32-byte records: indexed fan arguments at byte 0, fallback arguments at byte 32. Private.
    let draws: any MTLBuffer
    /// Shared copy of `draws` taken at the end of the command for completion-time counters.
    let stats: any MTLBuffer
    let tileCount: SIMD2<UInt32>
    var bytes: Int { classes.length + fanTiles.length + fallbackTiles.length + draws.length + stats.length }
}

/// The scratch sets one command buffer holds until it completes.
struct AdaptiveScratchLease: @unchecked Sendable {
    var sets: [AdaptiveScratchSet] = []
}

/// Bounded pool: at most three in-flight command buffers times two eyes. A set is
/// handed to one command buffer and returns only from that buffer's completion
/// handler. Nothing is keyed by texture pointer and nothing survives a source
/// frame; a size change drops idle sets before allocating.
final class AdaptiveScratchPool: @unchecked Sendable {
    static let tile = 8
    static let maximumSets = 6
    /// 4K is 480x270 = 129,600 tiles: three 4-byte entries per tile plus 128 bytes, about 1.48 MiB.
    static let maximumBytesPerSet = 4 << 20
    /// Exactly the old 32-triangle fan order, with a shared centre and 32 shared perimeter corners.
    static let fanIndices: [UInt16] = {
        var indices = [UInt16]()
        indices.reserveCapacity(96)
        for triangle in 0..<32 {
            indices.append(0)
            indices.append(UInt16(1 + triangle))
            indices.append(UInt16(1 + (triangle + 1) % 32))
        }
        return indices
    }()
    private let lock = NSLock()
    private var entries: [(set: AdaptiveScratchSet, busy: Bool)] = []

    /// Whole and partial tiles covering the source, or nil outside the bound.
    static func tileCount(width: Int, height: Int) -> SIMD2<UInt32>? {
        guard width >= 3, height >= 3, width <= 16_384, height <= 16_384 else { return nil }
        let count = SIMD2<UInt32>(UInt32((width + tile - 1) / tile), UInt32((height + tile - 1) / tile))
        let tiles = Int(count.x) * Int(count.y)
        guard tiles * 12 + 128 <= maximumBytesPerSet else { return nil }
        return count
    }

    func acquire(device: any MTLDevice, width: Int, height: Int) -> AdaptiveScratchSet? {
        guard let count = Self.tileCount(width: width, height: height) else { return nil }
        lock.lock()
        defer { lock.unlock() }
        if let index = entries.firstIndex(where: { !$0.busy && $0.set.tileCount == count }) {
            entries[index].busy = true
            return entries[index].set
        }
        entries.removeAll { !$0.busy }
        guard entries.count < Self.maximumSets else { return nil }
        let tiles = Int(count.x) * Int(count.y)
        guard let classes = device.makeBuffer(length: tiles * 4, options: .storageModePrivate),
              let fan = device.makeBuffer(length: tiles * 4, options: .storageModePrivate),
              let fallback = device.makeBuffer(length: tiles * 4, options: .storageModePrivate),
              let draws = device.makeBuffer(length: 64, options: .storageModePrivate),
              let stats = device.makeBuffer(length: 64, options: .storageModeShared) else { return nil }
        classes.label = "Game world adaptive tile classes"
        fan.label = "Game world adaptive fan tiles"
        fallback.label = "Game world adaptive fallback tiles"
        draws.label = "Game world adaptive indirect draws"
        stats.label = "Game world adaptive draw counters (shared)"
        let set = AdaptiveScratchSet(classes: classes, fanTiles: fan, fallbackTiles: fallback, draws: draws, stats: stats, tileCount: count)
        entries.append((set, true))
        return set
    }

    /// Called from the completion handler of the command buffer that used them.
    func release(_ lease: AdaptiveScratchLease) {
        guard !lease.sets.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        for set in lease.sets {
            if let index = entries.firstIndex(where: { $0.set.draws === set.draws }) { entries[index].busy = false }
        }
    }

    var statistics: (sets: Int, busy: Int, bytes: Int) {
        lock.lock()
        defer { lock.unlock() }
        return (entries.count, entries.filter(\.busy).count, entries.reduce(0) { $0 + $1.set.bytes })
    }
}
// END AdaptiveScratchPool

/// Mirrors GameWorldAdaptiveParams in GameWorld.metal.
struct GameWorldAdaptiveParams {
    var tileCount: SIMD2<UInt32>
    var toleranceULPs: UInt32
    var rejectBorderTiles: UInt32
}

/// Counters read from the shared copy of the indirect arguments after completion.
struct GameWorldAdaptiveCounters: Sendable {
    var fanTiles: UInt32
    var fallbackInstances: UInt32
    var fallbackTiles: UInt32 { fallbackInstances / 64 }
}

/// Classification plus two indirect draws for one eye. The accepted-tile fan and
/// the per-texel fallback share the legacy fragment function, depth state and
/// attachments, so fade, colour transfer and reverse-Z output are unchanged.
final class GameWorldAdaptiveMesh: @unchecked Sendable {
    let reset: any MTLComputePipelineState
    let classify: any MTLComputePipelineState
    let fan: any MTLRenderPipelineState
    let fallback: any MTLRenderPipelineState
    let fanIndexBuffer: any MTLBuffer
    let pool = AdaptiveScratchPool()
    /// Stated initial tolerance for "one plane": default 16 ULP of the largest depth in the
    /// tile and halo, overridable with MUGUET_COMPOSITOR_ADAPTIVE_TOLERANCE_ULP (1...4096).
    let toleranceULPs: UInt32
    let rejectBorderTiles: UInt32 = 1

    init?(device: any MTLDevice, library: any MTLLibrary, colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat) {
        guard let resetFunction = library.makeFunction(name: "gameWorldAdaptiveReset"),
              let classifyFunction = library.makeFunction(name: "gameWorldAdaptiveClassify"),
              let fanVertex = library.makeFunction(name: "gameWorldAdaptiveFanVertex"),
              let fallbackVertex = library.makeFunction(name: "gameWorldAdaptiveFallbackVertex"),
              let fragment = library.makeFunction(name: "gameWorldFragment"),
              let reset = try? device.makeComputePipelineState(function: resetFunction),
              let classify = try? device.makeComputePipelineState(function: classifyFunction) else { return nil }
        guard classify.maxTotalThreadsPerThreadgroup >= 64 else { return nil }
        func pipeline(_ vertex: any MTLFunction, _ label: String) -> (any MTLRenderPipelineState)? {
            let descriptor = MTLRenderPipelineDescriptor()
            descriptor.label = label
            descriptor.vertexFunction = vertex
            descriptor.fragmentFunction = fragment
            descriptor.colorAttachments[0].pixelFormat = colorFormat
            descriptor.depthAttachmentPixelFormat = depthFormat
            return try? device.makeRenderPipelineState(descriptor: descriptor)
        }
        let indices = AdaptiveScratchPool.fanIndices
        guard let fan = pipeline(fanVertex, "Actual native GTA world depth reprojection, adaptive tile fan"),
              let fallback = pipeline(fallbackVertex, "Actual native GTA world depth reprojection, adaptive per-texel fallback"),
              let fanIndexBuffer = device.makeBuffer(bytes: indices, length: indices.count * MemoryLayout<UInt16>.stride,
                                                     options: .storageModeShared) else { return nil }
        self.reset = reset
        self.classify = classify
        self.fan = fan
        self.fallback = fallback
        fanIndexBuffer.label = "Game world adaptive 32-triangle fan indices"
        self.fanIndexBuffer = fanIndexBuffer
        let requested = UInt32(ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_ADAPTIVE_TOLERANCE_ULP"] ?? "") ?? 16
        toleranceULPs = min(max(requested, 1), 4096)
    }

    func params(for set: AdaptiveScratchSet) -> GameWorldAdaptiveParams {
        GameWorldAdaptiveParams(tileCount: set.tileCount, toleranceULPs: toleranceULPs, rejectBorderTiles: rejectBorderTiles)
    }

    /// Reset and classification on `commands`, ahead of the eye's render pass.
    func encodeClassification(commands: any MTLCommandBuffer, set: AdaptiveScratchSet,
                              uniforms: GameWorldUniforms, depth: any MTLTexture) -> Bool {
        guard let encoder = commands.makeComputeCommandEncoder() else { return false }
        var uniforms = uniforms
        var params = params(for: set)
        encoder.label = "Game world adaptive tile classification"
        encoder.setComputePipelineState(reset)
        encoder.setBuffer(set.draws, offset: 0, index: 0)
        encoder.dispatchThreads(MTLSize(width: 1, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 1, height: 1, depth: 1))
        encoder.setComputePipelineState(classify)
        encoder.setBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setBytes(&params, length: MemoryLayout<GameWorldAdaptiveParams>.stride, index: 1)
        encoder.setBuffer(set.fanTiles, offset: 0, index: 2)
        encoder.setBuffer(set.fallbackTiles, offset: 0, index: 3)
        encoder.setBuffer(set.draws, offset: 0, index: 4)
        encoder.setBuffer(set.classes, offset: 0, index: 5)
        encoder.setTexture(depth, index: 0)
        encoder.dispatchThreadgroups(MTLSize(width: Int(set.tileCount.x), height: Int(set.tileCount.y), depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: AdaptiveScratchPool.tile, height: AdaptiveScratchPool.tile, depth: 1))
        encoder.endEncoding()
        return true
    }

    /// The two indirect draws on an already configured render encoder.
    func encodeDraws(_ encoder: any MTLRenderCommandEncoder, set: AdaptiveScratchSet, uniforms: GameWorldUniforms,
                     depthState: any MTLDepthStencilState, depth: any MTLTexture, color: any MTLTexture) {
        var uniforms = uniforms
        var params = params(for: set)
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setVertexBytes(&params, length: MemoryLayout<GameWorldAdaptiveParams>.stride, index: 1)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setVertexTexture(depth, index: 0)
        encoder.setFragmentTexture(color, index: 0)
        encoder.setRenderPipelineState(fan)
        encoder.setVertexBuffer(set.fanTiles, offset: 0, index: 2)
        encoder.drawIndexedPrimitives(type: .triangle, indexType: .uint16, indexBuffer: fanIndexBuffer,
                                      indexBufferOffset: 0, indirectBuffer: set.draws, indirectBufferOffset: 0)
        encoder.setRenderPipelineState(fallback)
        encoder.setVertexBuffer(set.fallbackTiles, offset: 0, index: 2)
        encoder.drawPrimitives(type: .triangleStrip, indirectBuffer: set.draws, indirectBufferOffset: 32)
    }

    /// Copies the final indirect arguments into the shared counter buffer.
    func encodeCounterCopy(commands: any MTLCommandBuffer, set: AdaptiveScratchSet) {
        guard let blit = commands.makeBlitCommandEncoder() else { return }
        blit.label = "Game world adaptive counters"
        blit.copy(from: set.draws, sourceOffset: 0, to: set.stats, destinationOffset: 0, size: 64)
        blit.endEncoding()
    }

    /// Valid only after the command buffer that encoded the copy has completed.
    static func counters(_ set: AdaptiveScratchSet) -> GameWorldAdaptiveCounters {
        let words = set.stats.contents().assumingMemoryBound(to: UInt32.self)
        return GameWorldAdaptiveCounters(fanTiles: words[1], fallbackInstances: words[9])
    }
}
#endif

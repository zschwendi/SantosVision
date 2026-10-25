#if SV_GAME_RUNTIME
import Foundation
import Metal
import simd

/// Finite support partially reduced sky holes in physical build7 A/B. It is
/// enabled normally, with an explicit QA disable, and creates no unseen sky.
/// A depth-tested MRT records the final source owner, then a depth-only pass
/// gives only surviving sky a finite support sphere. RGB and finite-world depth
/// are unchanged, including geometry farther away than the support sphere.
final class GameWorldFiniteSkySupport: @unchecked Sendable {
    static var requested: Bool {
        GameWorldFiniteSkyPolicy.requested(environment: ProcessInfo.processInfo.environment,
                                          arguments: ProcessInfo.processInfo.arguments)
    }

    private let worldPipeline: any MTLRenderPipelineState
    private let repairPipeline: any MTLRenderPipelineState
    private let verticalReconstruction: Bool
    /// The exact existing nine-sample corner fit, evaluated once per geometric
    /// corner rather than once for each of its four source-texel owners.
    /// Diagnostic opt-in: the registered Mac source was slower after warming,
    /// so the original vertex path stays the normal-play default.
    private let corners: (kernel: any MTLComputePipelineState, world: any MTLRenderPipelineState)?
    let cornerPool = CornerScratchPool()
    private let worldDepth: any MTLDepthStencilState
    private let repairDepth: any MTLDepthStencilState
    let pool = GameWorldSkyOwnershipPool()

    // The texture is exclusively leased to this command until its completion;
    // only the synchronized pool is touched from the completion callback.
    private struct CompletionLease: @unchecked Sendable {
        let pool: GameWorldSkyOwnershipPool
        let texture: any MTLTexture
        func release() { pool.release(texture) }
    }

    struct Result {
        let ownership: any MTLTexture
        let radius: Float
        let cornersPrecomputed: Bool
    }

    private struct CornerCompletionLease: @unchecked Sendable {
        let pool: CornerScratchPool
        let lease: CornerScratchLease
        func release() { pool.release(lease) }
    }

    private struct RepairUniforms {
        var eyeFromClip: simd_float4x4
        var clipFromEye: simd_float4x4
        var viewportOrigin: SIMD2<Float>
        var viewportSize: SIMD2<Float>
        var radius: Float
        var padding: Float = 0
        var reserved = SIMD2<Float>.zero
    }

    init?(device: any MTLDevice, library: any MTLLibrary,
          colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat,
          precomputeCorners: Bool = ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_CORNER_PRECOMPUTE"] == "1",
          verticalReconstruction: Bool = false) {
        guard let vertex = library.makeFunction(name: "gameWorldVertex"),
              let ownerFragment = library.makeFunction(name: verticalReconstruction
                ? "gameWorldSkyOwnershipVerticalReconstructionFragment" : "gameWorldSkyOwnershipFragment"),
              let repairVertex = library.makeFunction(name: "gameWorldSkyRepairVertex"),
              let repairFragment = library.makeFunction(name: "gameWorldSkyRepairFragment") else { return nil }
        let world = MTLRenderPipelineDescriptor()
        world.label = "EXPERIMENT finite sky: original color/depth and final source ownership"
        world.vertexFunction = vertex
        world.fragmentFunction = ownerFragment
        world.colorAttachments[0].pixelFormat = colorFormat
        world.colorAttachments[1].pixelFormat = .r8Uint
        world.depthAttachmentPixelFormat = depthFormat
        let repair = MTLRenderPipelineDescriptor()
        repair.label = "EXPERIMENT finite sky: depth-only final-sky repair"
        repair.vertexFunction = repairVertex
        repair.fragmentFunction = repairFragment
        repair.depthAttachmentPixelFormat = depthFormat
        let worldState = MTLDepthStencilDescriptor()
        worldState.depthCompareFunction = .greaterEqual
        worldState.isDepthWriteEnabled = true
        let repairState = MTLDepthStencilDescriptor()
        repairState.depthCompareFunction = .always
        repairState.isDepthWriteEnabled = true
        guard let wp = try? device.makeRenderPipelineState(descriptor: world),
              let rp = try? device.makeRenderPipelineState(descriptor: repair),
              let wd = device.makeDepthStencilState(descriptor: worldState),
              let rd = device.makeDepthStencilState(descriptor: repairState) else { return nil }
        self.verticalReconstruction = verticalReconstruction
        worldPipeline = wp
        var precomputed: (kernel: any MTLComputePipelineState, world: any MTLRenderPipelineState)?
        if precomputeCorners,
           let cornerKernel = library.makeFunction(name: "gameWorldCornerDepths"),
           let cornerVertex = library.makeFunction(name: "gameWorldPrecomputedVertex"),
           let kernel = try? device.makeComputePipelineState(function: cornerKernel) {
            // Fragment, MRT formats and depth ownership remain identical.
            let sharedWorld = world.copy() as! MTLRenderPipelineDescriptor
            sharedWorld.label = "Finite sky: shared corner depths, original final ownership"
            sharedWorld.vertexFunction = cornerVertex
            if let state = try? device.makeRenderPipelineState(descriptor: sharedWorld) {
                precomputed = (kernel, state)
            }
        }
        corners = precomputed
        repairPipeline = rp
        worldDepth = wd
        repairDepth = rd
    }

    /// Encodes both passes before any reticle/UI. A failure returns nil; the
    /// caller can clear and redraw its original pass. No caller descriptor is
    /// changed and every acquired mask is retained until command completion.
    func encode(commands: any MTLCommandBuffer, pass: MTLRenderPassDescriptor,
                viewport: MTLViewport, projection: simd_float4x4, far: Float,
                uniforms: GameWorldUniforms, color: any MTLTexture,
                depth: any MTLTexture) -> Result? {
        guard let target = pass.colorAttachments[0].texture,
              let targetDepth = pass.depthAttachment.texture,
              target.sampleCount == 1, targetDepth.sampleCount == 1,
              target.width == targetDepth.width, target.height == targetDepth.height,
              uniforms.sourceSize.x == color.width, uniforms.sourceSize.y == color.height,
              color.width == depth.width, color.height == depth.height,
              color.textureType == .type2D, depth.textureType == .type2D,
              color.sampleCount == 1, depth.sampleCount == 1,
              color.width > 0, color.height > 0, color.width <= 16_384, color.height <= 16_384,
              color.width * color.height <= (16 << 20),
              viewport.originX.isFinite, viewport.originY.isFinite,
              viewport.width.isFinite, viewport.height.isFinite,
              viewport.originX >= 0, viewport.originY >= 0,
              viewport.width > 0, viewport.height > 0,
              viewport.originX + viewport.width <= Double(target.width),
              viewport.originY + viewport.height <= Double(target.height),
              far.isFinite, far > 1, far <= 100_000,
              (0..<4).allSatisfy({ column in (0..<4).allSatisfy { projection[column][$0].isFinite } }),
              let ownership = pool.acquire(device: target.device, width: target.width, height: target.height) else { return nil }
        let completedOwnership = CompletionLease(pool: pool, texture: ownership)
        commands.addCompletedHandler { _ in completedOwnership.release() }
        // If a bounded corner texture or compute encoder is unavailable, draw
        // the original vertex path. Acquired textures stay exclusively leased
        // until this command completes even when precompute cannot be encoded.
        var cornerDepths: (any MTLTexture)?
        if let corners,
           let texture = cornerPool.acquire(device: target.device, width: color.width, height: color.height) {
            let completion = CornerCompletionLease(pool: cornerPool, lease: CornerScratchLease(textures: [texture]))
            commands.addCompletedHandler { _ in completion.release() }
            if let compute = commands.makeComputeCommandEncoder() {
                compute.label = "Finite sky: once-per-corner depth qualification"
                compute.setComputePipelineState(corners.kernel)
                var values = uniforms
                compute.setBytes(&values, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
                compute.setTexture(depth, index: 0)
                compute.setTexture(texture, index: 1)
                let width = corners.kernel.threadExecutionWidth
                let height = max(1, corners.kernel.maxTotalThreadsPerThreadgroup / width)
                compute.dispatchThreadgroups(MTLSize(width: (texture.width + width - 1) / width,
                    height: (texture.height + height - 1) / height, depth: 1),
                    threadsPerThreadgroup: MTLSize(width: width, height: height, depth: 1))
                compute.endEncoding()
                cornerDepths = texture
            }
        }
        // Copy before attaching the private MRT: fallback and reticle passes
        // must keep their original single-color-attachment pipeline contract.
        let world = pass.copy() as! MTLRenderPassDescriptor
        world.colorAttachments[1].texture = ownership
        world.colorAttachments[1].loadAction = .clear
        world.colorAttachments[1].storeAction = .store
        world.colorAttachments[1].clearColor = MTLClearColorMake(0, 0, 0, 0)
        guard let encoder = commands.makeRenderCommandEncoder(descriptor: world) else { return nil }
        encoder.label = "EXPERIMENT final sky ownership (0 uncovered, 1 sky, 2 finite)"
        encoder.setViewport(viewport)
        if cornerDepths != nil, let corners { encoder.setRenderPipelineState(corners.world) }
        else { encoder.setRenderPipelineState(worldPipeline) }
        encoder.setDepthStencilState(worldDepth)
        encoder.setCullMode(.none)
        var uniforms = uniforms
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setVertexTexture(depth, index: 0)
        if let cornerDepths { encoder.setVertexTexture(cornerDepths, index: 1) }
        encoder.setFragmentTexture(color, index: 0)
        if verticalReconstruction { encoder.setFragmentTexture(depth, index: 1) }
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4,
                               instanceCount: Int(uniforms.sourceSize.x) * Int(uniforms.sourceSize.y))
        encoder.endEncoding()

        let radius = far * 0.8
        guard encodeRepair(commands: commands, depth: targetDepth, slice: pass.depthAttachment.slice,
            ownership: ownership, viewport: viewport, projection: projection, radius: radius) else { return nil }
        return Result(ownership: ownership, radius: radius, cornersPrecomputed: cornerDepths != nil)
    }

    /// A separate ordered pass is essential: a support sphere may be nearer
    /// than legitimate world geometry. Final MRT ownership, rather than support
    /// depth comparison, decides whether this pass is allowed to write.
    func encodeRepair(commands: any MTLCommandBuffer, depth: any MTLTexture, slice: Int,
                      ownership: any MTLTexture, viewport: MTLViewport,
                      projection: simd_float4x4, radius: Float) -> Bool {
        guard ownership.width == depth.width, ownership.height == depth.height,
              ownership.pixelFormat == .r8Uint, radius.isFinite, radius > 0 else { return false }
        let repair = MTLRenderPassDescriptor()
        repair.depthAttachment.texture = depth
        repair.depthAttachment.slice = slice
        repair.depthAttachment.loadAction = .load
        repair.depthAttachment.storeAction = .store
        guard let fix = commands.makeRenderCommandEncoder(descriptor: repair) else { return false }
        fix.label = "EXPERIMENT support depth only where final source owner is sky"
        fix.setViewport(viewport)
        fix.setRenderPipelineState(repairPipeline)
        fix.setDepthStencilState(repairDepth)
        fix.setCullMode(.none)
        var repairUniforms = RepairUniforms(eyeFromClip: projection.inverse, clipFromEye: projection,
            viewportOrigin: SIMD2(Float(viewport.originX), Float(viewport.originY)),
            viewportSize: SIMD2(Float(viewport.width), Float(viewport.height)), radius: radius)
        fix.setFragmentBytes(&repairUniforms, length: MemoryLayout<RepairUniforms>.stride, index: 0)
        fix.setFragmentTexture(ownership, index: 0)
        fix.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
        fix.endEncoding()
        return true
    }
}

/// At most six masks for three in-flight two-eye commands. No history of scene
/// images and no cross-frame ownership reuse. Idle wrong-size masks are dropped.
final class GameWorldSkyOwnershipPool: @unchecked Sendable {
    static let maximumTextures = 6
    static let maximumTextureBytes = 16 << 20
    private let lock = NSLock()
    private var entries: [(texture: any MTLTexture, busy: Bool)] = []

    func acquire(device: any MTLDevice, width: Int, height: Int) -> (any MTLTexture)? {
        guard width > 0, height > 0, width <= 16_384, height <= 16_384,
              width * height <= Self.maximumTextureBytes else { return nil }
        lock.lock()
        defer { lock.unlock() }
        if let index = entries.firstIndex(where: { !$0.busy && $0.texture.width == width && $0.texture.height == height }) {
            entries[index].busy = true
            return entries[index].texture
        }
        entries.removeAll { !$0.busy }
        guard entries.count < Self.maximumTextures else { return nil }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r8Uint, width: width, height: height, mipmapped: false)
        descriptor.usage = [.renderTarget, .shaderRead]
        descriptor.storageMode = .private
        guard let texture = device.makeTexture(descriptor: descriptor) else { return nil }
        texture.label = "EXPERIMENT bounded final source ownership"
        entries.append((texture, true))
        return texture
    }

    func release(_ texture: any MTLTexture) {
        lock.lock()
        defer { lock.unlock() }
        if let index = entries.firstIndex(where: { $0.texture === texture }) { entries[index].busy = false }
    }

    var statistics: (textures: Int, busy: Int, bytes: Int) {
        lock.lock()
        defer { lock.unlock() }
        return (entries.count, entries.filter(\.busy).count, entries.reduce(0) { $0 + $1.texture.width * $1.texture.height })
    }
}
#endif

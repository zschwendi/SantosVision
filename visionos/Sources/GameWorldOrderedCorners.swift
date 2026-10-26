import Foundation

/// Pure admission arithmetic shared by the actual pool and CPU overflow tests.
enum GameWorldOrderedCornersBudget {
    static let maximumSets = 6
    static let maximumBytes = 384 << 20

    struct Layout: Equatable {
        let width: Int
        let height: Int
        let cornerWidth: Int
        let cornerHeight: Int
        let indexCount: Int
        let indexBytes: Int
        let cornerBytes: Int
        let bytes: Int
    }

    static func layout(width: Int, height: Int, maxBufferLength: Int) -> Layout? {
        // The additional corner row/column must fit a 16384-wide 2D texture.
        guard width > 0, height > 0, width < 16_384, height < 16_384,
              maxBufferLength > 0 else { return nil }
        func product(_ lhs: Int, _ rhs: Int) -> Int? {
            let result = lhs.multipliedReportingOverflow(by: rhs)
            return result.overflow ? nil : result.partialValue
        }
        guard let pixels = product(width, height),
              let count = product(pixels, 6), let indexBytes = product(count, 4),
              let corners = product(width + 1, height + 1), let cornerBytes = product(corners, 4),
              let fallbackVertices = product(pixels, 4) else { return nil }
        let vertexCount = corners.addingReportingOverflow(fallbackVertices)
        let total = indexBytes.addingReportingOverflow(cornerBytes)
        guard !vertexCount.overflow, vertexCount.partialValue <= Int(UInt32.max),
              !total.overflow, total.partialValue <= maximumBytes,
              indexBytes <= maxBufferLength else { return nil }
        return Layout(width: width, height: height, cornerWidth: width + 1, cornerHeight: height + 1,
            indexCount: count, indexBytes: indexBytes, cornerBytes: cornerBytes, bytes: total.partialValue)
    }

    static func canAdd(bytes: Int, heldBytes: Int, heldSets: Int) -> Bool {
        guard bytes > 0, heldBytes >= 0, heldSets >= 0, heldSets < maximumSets else { return false }
        let total = heldBytes.addingReportingOverflow(bytes)
        return !total.overflow && total.partialValue <= maximumBytes
    }
}

#if SV_GAME_RUNTIME
import Metal
import simd

/// Optional ordered indexed corner preparation. Resources belong exclusively
/// to the command supplied to prepare; they must not be used by a later command.
final class GameWorldOrderedCorners: @unchecked Sendable {
    struct Resources: @unchecked Sendable {
        let corners: any MTLTexture
        let indices: any MTLBuffer
    }

    struct Statistics: Sendable {
        let sets: Int
        let busy: Int
        let bytes: Int
        let releasedLeases: UInt64
    }

    private final class Pool: @unchecked Sendable {
        private struct Entry {
            let resources: Resources
            let width: Int
            let height: Int
            let bytes: Int
            var checkout: UInt64?
        }
        private let lock = NSLock()
        private var entries: [Entry] = []
        private var nextCheckout: UInt64 = 0
        private var released: UInt64 = 0

        func acquire(device: any MTLDevice, layout: GameWorldOrderedCornersBudget.Layout)
            -> (Resources, CompletionLease)? {
            lock.lock()
            defer { lock.unlock() }
            guard nextCheckout < .max else { return nil }
            nextCheckout += 1
            let checkout = nextCheckout
            if let index = entries.firstIndex(where: { $0.checkout == nil
                && $0.width == layout.width && $0.height == layout.height }) {
                entries[index].checkout = checkout
                return (entries[index].resources, CompletionLease(pool: self, checkout: checkout))
            }
            // Idle sets of another extent are disposable. Busy resources cannot
            // be evicted to make room, even when their source size has changed.
            entries.removeAll { $0.checkout == nil }
            let heldBytes = entries.reduce(0) { $0 + $1.bytes }
            guard GameWorldOrderedCornersBudget.canAdd(bytes: layout.bytes,
                heldBytes: heldBytes, heldSets: entries.count) else { return nil }
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float,
                width: layout.cornerWidth, height: layout.cornerHeight, mipmapped: false)
            descriptor.storageMode = .private
            descriptor.usage = [.shaderRead, .shaderWrite]
            guard let corners = device.makeTexture(descriptor: descriptor),
                  let indices = device.makeBuffer(length: layout.indexBytes, options: .storageModePrivate) else { return nil }
            // Driver allocation sizes may include row/page padding. Keep the
            // budget at least nominal and recheck the actual returned resources.
            let textureBytes = max(layout.cornerBytes, corners.allocatedSize)
            let bufferBytes = max(layout.indexBytes, indices.allocatedSize)
            let actual = textureBytes.addingReportingOverflow(bufferBytes)
            guard !actual.overflow, GameWorldOrderedCornersBudget.canAdd(bytes: actual.partialValue,
                heldBytes: heldBytes, heldSets: entries.count) else { return nil }
            corners.label = "Ordered game world qualified corner depths"
            indices.label = "Ordered game world source-pixel triangle indices"
            let resources = Resources(corners: corners, indices: indices)
            entries.append(Entry(resources: resources, width: layout.width, height: layout.height,
                bytes: actual.partialValue, checkout: checkout))
            return (resources, CompletionLease(pool: self, checkout: checkout))
        }

        func release(checkout: UInt64) {
            lock.lock()
            defer { lock.unlock() }
            guard let index = entries.firstIndex(where: { $0.checkout == checkout }) else { return }
            entries[index].checkout = nil
            if released < .max { released += 1 }
        }

        var statistics: Statistics {
            lock.lock()
            defer { lock.unlock() }
            return Statistics(sets: entries.count, busy: entries.filter { $0.checkout != nil }.count,
                bytes: entries.reduce(0) { $0 + $1.bytes }, releasedLeases: released)
        }
    }

    /// The command's completion block is the owner after the first encoder is
    /// created. A discarded uncommitted command destroys that block and its
    /// lease, returning the set without a pool-to-command retain cycle.
    private final class CompletionLease: @unchecked Sendable {
        private let lock = NSLock()
        private var pool: Pool?
        private let checkout: UInt64
        init(pool: Pool, checkout: UInt64) { self.pool = pool; self.checkout = checkout }
        func release() {
            lock.lock()
            let owned = pool
            pool = nil
            lock.unlock()
            owned?.release(checkout: checkout)
        }
        deinit { release() }
    }

    private let device: any MTLDevice
    private let cornerKernel: any MTLComputePipelineState
    private let indexKernel: any MTLComputePipelineState
    private let pool = Pool()

    #if SV_ORDERED_CORNERS_VALIDATION
    // Test-only refusal at the real encoder boundary, absent from production.
    var validationRejectEncoderOrdinal: Int?
    #endif

    init?(device: any MTLDevice, library: any MTLLibrary) {
        guard library.device.registryID == device.registryID,
              let corners = library.makeFunction(name: "gameWorldCornerDepths"),
              let indices = library.makeFunction(name: "orderedCornerIndices"),
              let cornerKernel = try? device.makeComputePipelineState(function: corners),
              let indexKernel = try? device.makeComputePipelineState(function: indices) else { return nil }
        self.device = device
        self.cornerKernel = cornerKernel
        self.indexKernel = indexKernel
    }

    var statistics: Statistics { pool.statistics }

    func prepare(commands: any MTLCommandBuffer, uniforms: GameWorldUniforms,
                 depth: any MTLTexture) -> Resources? {
        guard commands.commandQueue.device.registryID == device.registryID,
              depth.device.registryID == device.registryID,
              commands.status == .notEnqueued,
              depth.textureType == .type2D, depth.pixelFormat == .r32Float,
              depth.sampleCount == 1, depth.arrayLength == 1, depth.depth == 1,
              depth.usage.contains(.shaderRead), uniforms.sourceDepthReversed <= 1,
              depth.width == Int(uniforms.sourceSize.x), depth.height == Int(uniforms.sourceSize.y),
              let layout = GameWorldOrderedCornersBudget.layout(width: depth.width, height: depth.height,
                  maxBufferLength: device.maxBufferLength),
              let (resources, lease) = pool.acquire(device: device, layout: layout) else { return nil }
        func encoder(_ ordinal: Int) -> (any MTLComputeCommandEncoder)? {
            #if SV_ORDERED_CORNERS_VALIDATION
            if validationRejectEncoderOrdinal == ordinal { return nil }
            #endif
            return commands.makeComputeCommandEncoder()
        }
        guard let corners = encoder(1) else { return nil }
        // From here even a later encoder failure can leave work referencing the
        // set in this command. Return it only on completion or command disposal.
        commands.addCompletedHandler { [lease] _ in lease.release() }
        var values = uniforms
        corners.label = "Ordered corners: shared depth qualification"
        corners.setComputePipelineState(cornerKernel)
        corners.setBytes(&values, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        corners.setTexture(depth, index: 0)
        corners.setTexture(resources.corners, index: 1)
        dispatch(corners, kernel: cornerKernel, width: layout.cornerWidth, height: layout.cornerHeight)
        corners.endEncoding()
        guard let indices = encoder(2) else { return nil }
        indices.label = "Ordered corners: original source-pixel triangle sequence"
        indices.setComputePipelineState(indexKernel)
        indices.setBytes(&values, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        indices.setBuffer(resources.indices, offset: 0, index: 1)
        indices.setTexture(resources.corners, index: 0)
        dispatch(indices, kernel: indexKernel, width: layout.width, height: layout.height)
        indices.endEncoding()
        return resources
    }

    private func dispatch(_ encoder: any MTLComputeCommandEncoder, kernel: any MTLComputePipelineState,
                          width: Int, height: Int) {
        let x = min(16, kernel.threadExecutionWidth)
        let y = max(1, min(16, kernel.maxTotalThreadsPerThreadgroup / x))
        encoder.dispatchThreads(MTLSize(width: width, height: height, depth: 1),
            threadsPerThreadgroup: MTLSize(width: x, height: y, depth: 1))
    }
}
#endif

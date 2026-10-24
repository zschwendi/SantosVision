import Foundation
import simd

enum XRMotionPosePolicy {
    static let maximumSourceAge = 0.150
    static let maximumFutureSource = 0.005
    static let maximumFuturePresentation = 0.050

    static func fresh(source: Double, target: Double) -> Bool {
        source.isFinite && target.isFinite && source > 0 && target > 0
            && target - source <= maximumSourceAge && source - target <= maximumFutureSource
    }

    /// Not querying an AR_MT_UNSAFE provider while it is being updated is not
    /// evidence of tracking loss. The new head still has no unqueried pose.
    static func lostSources(previous: UInt32, current: UInt32, queried: UInt32) -> UInt32 {
        previous & queried & 3 & ~current
    }

    static func activeBareHandLost(wasTracked: Bool, left: Bool, lastFlags: UInt32) -> Bool {
        wasTracked && lastFlags & (left ? (1 << 8) : (1 << 9)) != 0
    }

    static func rigid(_ matrix: simd_float4x4) -> Bool {
        let c = [matrix.columns.0, matrix.columns.1, matrix.columns.2, matrix.columns.3]
        guard c.allSatisfy({ $0.x.isFinite && $0.y.isFinite && $0.z.isFinite && $0.w.isFinite }),
              abs(c[0].w) < 0.001, abs(c[1].w) < 0.001, abs(c[2].w) < 0.001,
              abs(c[3].w - 1) < 0.001 else { return false }
        let x = SIMD3(c[0].x, c[0].y, c[0].z)
        let y = SIMD3(c[1].x, c[1].y, c[1].z)
        let z = SIMD3(c[2].x, c[2].y, c[2].z)
        return abs(simd_length_squared(x) - 1) < 0.02
            && abs(simd_length_squared(y) - 1) < 0.02
            && abs(simd_length_squared(z) - 1) < 0.02
            && abs(simd_dot(x, y)) < 0.02 && abs(simd_dot(x, z)) < 0.02 && abs(simd_dot(y, z)) < 0.02
            && simd_dot(simd_cross(x, y), z) > 0.98
    }

    /// Cancel the common world-reference ancestor before composing into the
    /// renderer's session origin. Never mistake referenceFromLocation for a
    /// stage transform or guess the accessory's barrel from its base anchor.
    static func stageLocation(stageFromAnchor: simd_float4x4,
                              referenceFromAnchor: simd_float4x4,
                              referenceFromLocation: simd_float4x4) -> simd_float4x4? {
        guard rigid(stageFromAnchor), rigid(referenceFromAnchor), rigid(referenceFromLocation) else { return nil }
        let result = stageFromAnchor * simd_inverse(referenceFromAnchor) * referenceFromLocation
        return rigid(result) ? result : nil
    }
}

struct XRMotionPacket: Sendable {
    var matrices: [simd_float4x4]
    var times: [Double]
    var ids: [UInt64]
    var flags: UInt32
}

/// The SDK marks latestAnchors and predictAnchor AR_MT_UNSAFE. A render sample
/// gets a nonblocking exclusive read lease. An async update reserves the writer
/// first, waits for the existing read to finish, and excludes reads until its
/// completion handler has returned. No lock is held across an await.
final class XRMotionProviderGate: @unchecked Sendable {
    private let lock = NSLock()
    private var reading = false
    private var updating = false
    private var waiting: CheckedContinuation<Bool, Never>?

    func beginRead() -> Bool {
        lock.withLock {
            guard !reading, !updating else { return false }
            reading = true
            return true
        }
    }

    func endRead() {
        let continuation = lock.withLock { () -> CheckedContinuation<Bool, Never>? in
            reading = false
            let previous = waiting
            waiting = nil
            return previous
        }
        continuation?.resume(returning: true)
    }

    func beginUpdate() async -> Bool {
        await withCheckedContinuation { continuation in
            let immediate = lock.withLock { () -> Bool? in
                guard !updating else { return false }
                updating = true
                if reading { waiting = continuation; return nil }
                return true
            }
            if let immediate { continuation.resume(returning: immediate) }
        }
    }

    func endUpdate() { lock.withLock { updating = false } }
    var updatePending: Bool { lock.withLock { updating } }
}

/// A sampling operation captures each hand's revocation generation before
/// querying ARKit. A later source loss cannot be resurrected by that query.
struct XRMotionSourceLease: Sendable {
    let epoch: UInt64
    let generations: SIMD2<UInt64>
}

/// One active immersive owner. Old renderer cleanup cannot clear a newer
/// owner's snapshots. Every immutable packet names its exact accepted head.
final class XRMotionTransport: @unchecked Sendable {
    static let shared = XRMotionTransport()
    private let lock = NSLock()
    private var owner: UUID?
    private var epoch: UInt64 = 0
    private var sequence: UInt64 = 0
    private var packets: [XRMotionPacket] = []
    private var sourceGenerations = SIMD2<UInt64>.zero
    private var revokedThroughSequence = SIMD2<UInt64>.zero
    static let capacity = 64

    @discardableResult
    func claim(_ newOwner: UUID) -> UInt64 {
        lock.withLock {
            owner = newOwner
            epoch &+= 1
            if epoch == 0 { epoch = 1 }
            packets.removeAll(keepingCapacity: true)
            sourceGenerations = .zero
            revokedThroughSequence = .zero
            return epoch
        }
    }

    @discardableResult
    func invalidate(_ requestingOwner: UUID) -> UInt64? {
        lock.withLock {
            guard owner == requestingOwner else { return nil }
            epoch &+= 1
            if epoch == 0 { epoch = 1 }
            packets.removeAll(keepingCapacity: true)
            sourceGenerations = .zero
            revokedThroughSequence = .zero
            return epoch
        }
    }

    func currentEpoch(_ requestingOwner: UUID) -> UInt64? {
        lock.withLock { owner == requestingOwner ? epoch : nil }
    }

    func samplingLease(_ requestingOwner: UUID) -> XRMotionSourceLease? {
        lock.withLock { owner == requestingOwner ? XRMotionSourceLease(epoch: epoch, generations: sourceGenerations) : nil }
    }

    /// Losing one independent controller is not a session-origin change. Keep
    /// healthy exact-head samples immutable and revoke only the lost source.
    @discardableResult
    func invalidateSources(_ flags: UInt32, owner requestingOwner: UUID) -> Bool {
        lock.withLock {
            guard owner == requestingOwner else { return false }
            for hand in 0..<2 where flags & (1 << hand) != 0 {
                sourceGenerations[hand] &+= 1
                revokedThroughSequence[hand] = sequence
            }
            return true
        }
    }

    @discardableResult
    func publish(_ packet: XRMotionPacket, owner requestingOwner: UUID,
                 lease expected: XRMotionSourceLease) -> UInt32? {
        lock.withLock {
            guard owner == requestingOwner, epoch == expected.epoch, packet.matrices.count == 5,
                  packet.times.count == 3, packet.ids.count == 4, packet.ids[2] != 0,
                  !packets.contains(where: { $0.ids[2] == packet.ids[2] }) else { return nil }
            var stored = packet
            for hand in 0..<2 where expected.generations[hand] != sourceGenerations[hand] {
                Self.clearSource(hand, packet: &stored)
            }
            sequence &+= 1
            stored.ids = [1, epoch, packet.ids[2], sequence]
            packets.removeAll { stored.times[0] - $0.times[0] > XRMotionPosePolicy.maximumSourceAge }
            packets.append(stored)
            if packets.count > Self.capacity { packets.removeFirst(packets.count - Self.capacity) }
            return stored.flags
        }
    }

    private static func clearSource(_ hand: Int, packet: inout XRMotionPacket) {
        packet.flags &= ~((1 << hand) | (1 << (hand + 8)))
        packet.matrices[1 + hand * 2] = simd_float4x4()
        packet.matrices[2 + hand * 2] = simd_float4x4()
    }

    func read(expectedHeadFrame: UInt64, now: Double) -> XRMotionPacket? {
        lock.withLock {
            guard var packet = expectedHeadFrame == 0 ? packets.last : packets.last(where: { $0.ids[2] == expectedHeadFrame }),
                  now.isFinite, now > 0,
                  now - packet.times[0] <= XRMotionPosePolicy.maximumSourceAge,
                  packet.times[0] - now <= XRMotionPosePolicy.maximumFuturePresentation,
                  XRMotionPosePolicy.rigid(packet.matrices[0]) else { return nil }
            for (bit, sourceBit, grip, aim, time) in [(UInt32(1), UInt32(1 << 8), 1, 2, 1), (UInt32(2), UInt32(1 << 9), 3, 4, 2)] {
                let bare = packet.flags & sourceBit != 0
                let hand = time - 1
                let valid = packet.flags & bit != 0
                    && packet.ids[3] > revokedThroughSequence[hand]
                    && XRMotionPosePolicy.fresh(source: packet.times[time], target: packet.times[0])
                    && XRMotionPosePolicy.fresh(source: packet.times[time], target: now)
                    && XRMotionPosePolicy.rigid(packet.matrices[grip])
                    && (bare || XRMotionPosePolicy.rigid(packet.matrices[aim]))
                if !valid {
                    packet.flags &= ~(bit | sourceBit)
                    packet.matrices[grip] = simd_float4x4()
                    packet.matrices[aim] = simd_float4x4()
                }
            }
            packet.flags &= 0x303
            return packet
        }
    }
}

/// ABI v1. Nonzero expectedHeadFrame performs exact lookup; 0 is diagnostics
/// only. Matrices: stageFromHead,leftGrip,leftAim,rightGrip,rightAim. Times:
/// target presentation time, original left/right source-anchor times. IDs:
/// version, lifecycle/session epoch, accepted native head frame, sequence.
@_cdecl("sv_xr_motion_read_v1")
func svXRMotionReadV1(_ expectedHeadFrame: UInt64, _ floats: UnsafeMutablePointer<Float>?, _ floatCount: UInt32,
                     _ times: UnsafeMutablePointer<Double>?, _ timeCount: UInt32,
                     _ ids: UnsafeMutablePointer<UInt64>?, _ idCount: UInt32) -> UInt32 {
    guard floatCount == 80, timeCount == 3, idCount == 4, let floats, let times, let ids else { return 0 }
    for index in 0..<80 { floats[index] = 0 }
    for index in 0..<3 { times[index] = 0 }
    for index in 0..<4 { ids[index] = 0 }
    guard let packet = XRMotionTransport.shared.read(expectedHeadFrame: expectedHeadFrame,
                                                     now: ProcessInfo.processInfo.systemUptime) else { return 0 }
    for matrixIndex in 0..<5 {
        let matrix = packet.matrices[matrixIndex]
        for column in 0..<4 { for row in 0..<4 {
            floats[matrixIndex * 16 + column * 4 + row] = matrix[column][row]
        } }
    }
    for index in 0..<3 { times[index] = packet.times[index] }
    for index in 0..<4 { ids[index] = packet.ids[index] }
    return packet.flags
}

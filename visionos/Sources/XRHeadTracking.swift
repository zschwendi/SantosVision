import ARKit
import Foundation
import OSLog
import simd

// BEGIN XRRetainedEyeView (the standalone harness compiles this block verbatim)
/// The exact per-eye geometry the compositor asked the game to render for one
/// submitted head pose. Retained only so a later lease can be verified against
/// the pose it claims (`head_frame`), never used to fabricate an anchor.
struct XRRetainedEyeView: Sendable {
    let headFromEye: simd_float4x4
    let projectionFromEye: simd_float4x4
    let physicalProjectionFromEye: simd_float4x4?
    let sourceGuardBandScale: Float
    init(headFromEye: simd_float4x4, projectionFromEye: simd_float4x4,
         physicalProjectionFromEye: simd_float4x4? = nil, sourceGuardBandScale: Float = 1) {
        self.headFromEye = headFromEye
        self.projectionFromEye = projectionFromEye
        self.physicalProjectionFromEye = physicalProjectionFromEye
        self.sourceGuardBandScale = sourceGuardBandScale
    }
}
// END XRRetainedEyeView

/// One successfully submitted native head pose and, when it came from ARKit,
/// the actual `DeviceAnchor` value that ARKit returned for it. Synthetic
/// simulator poses are retained with `anchor == nil` and are never presented
/// as physical anchors.
struct XRRetainedHeadPose: @unchecked Sendable {
    let headFrame: UInt64
    /// The eye-geometry request submitted in the same compositor frame as this
    /// pose. `head_frame` names the pose the game consumed, not its request
    /// geometry; a lease whose `request_frame` differs must not be trusted to
    /// have used these eye transforms.
    let requestFrame: UInt64
    let timestamp: Double
    let stageFromHead: simd_float4x4
    let anchor: DeviceAnchor?
    let eyes: [XRRetainedEyeView]
    let epoch: UInt64
    var isSynthetic: Bool { anchor == nil }
}

/// Counters for the retained-anchor cache. Reported, never asserted.
struct XRRetainedAnchorStatistics: Sendable {
    var retained: UInt64 = 0
    var retainedSynthetic: UInt64 = 0
    var evictedByCapacity: UInt64 = 0
    var evictedByAge: UInt64 = 0
    var invalidations: UInt64 = 0
    var trackingLossInvalidations: UInt64 = 0
    var rejectedInputs: UInt64 = 0
    var hits: UInt64 = 0
    var missesUnknown: UInt64 = 0
    var missesExpired: UInt64 = 0
    var missesEpoch: UInt64 = 0
    var occupancy: Int = 0
    var capacity: Int = 0
    var maximumAgeSeconds: Double = 0
}

/// One counter for the process preserves ordering when immersion is reopened.
/// The native receiver stores a snapshot; camera updates run on the game thread.
final class XRHeadTracking: @unchecked Sendable {
    static let shared = XRHeadTracking()
    private let lock = NSLock()
    private var frame: UInt64 = 0
    private var lastError: Int32 = 0
    private var allowsTrackedPoses = true
    private var lastSubmittedTracked = false
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "HeadPose")

    // Retained-anchor cache (MUGUET_COMPOSITOR_SOURCE_ANCHOR_COPY / _ANCHOR_PROBE).
    // Bounded by count and age; both bounds are configuration, not measured truths.
    // Every invalidation (recenter, suspend, renderer stop) bumps the epoch so a
    // retained pose from an earlier tracking reference can never match a lease.
    private var retained: [XRRetainedHeadPose] = []
    private var retainedEpoch: UInt64 = 1
    private var statistics = XRRetainedAnchorStatistics()
    private let retainCapacity: Int
    private let retainMaximumAge: Double

    private init() {
        let environment = ProcessInfo.processInfo.environment
        let capacity = Int(environment["MUGUET_XR_ANCHOR_RETAIN_FRAMES"] ?? "") ?? 64
        let age = Double(environment["MUGUET_XR_ANCHOR_RETAIN_SECONDS"] ?? "") ?? 2
        retainCapacity = min(max(capacity, 1), 1024)
        retainMaximumAge = age.isFinite && age > 0 ? min(age, 30) : 2
        statistics.capacity = retainCapacity
        statistics.maximumAgeSeconds = retainMaximumAge
    }

    /// Legacy entry point; behaviour unchanged. Nothing is retained.
    func submit(timestamp: Double, stageFromHead: simd_float4x4?, tracked: Bool) {
        _ = submit(timestamp: timestamp, stageFromHead: stageFromHead, tracked: tracked,
                   anchor: nil, requestFrame: 0, eyes: [], retain: false)
    }

    /// Submits the pose exactly as before and, when `retain` is set, keeps the
    /// pose and the actual ARKit anchor under the native head frame it was given.
    /// Returns that head frame, or nil when nothing was published. An untracked
    /// submission after a tracked one is a tracking loss: the native side resets
    /// its reference, so every retained anchor is dropped with it.
    @discardableResult
    func submit(timestamp: Double, stageFromHead: simd_float4x4?, tracked: Bool,
                anchor: DeviceAnchor?, requestFrame: UInt64, eyes: [XRRetainedEyeView], retain: Bool) -> UInt64? {
        #if SV_GAME_RUNTIME
        lock.lock()
        defer { lock.unlock() }
        guard !tracked || allowsTrackedPoses else { return nil }
        if !tracked, lastSubmittedTracked {
            invalidateRetainedLocked(reason: "tracking lost")
            statistics.trackingLossInvalidations &+= 1
        }
        lastSubmittedTracked = tracked
        let published = publishLocked(timestamp: timestamp, stageFromHead: stageFromHead, tracked: tracked)
        guard let published else { return nil }
        if retain, tracked, let stageFromHead {
            let finite = timestamp.isFinite && Self.isFinite(stageFromHead)
                && eyes.allSatisfy { Self.isFinite($0.headFromEye) && Self.isFinite($0.projectionFromEye)
                    && ($0.physicalProjectionFromEye.map(Self.isFinite) ?? true)
                    && $0.sourceGuardBandScale.isFinite && $0.sourceGuardBandScale >= 1 }
            if finite, anchor == nil || anchor?.isTracked == true {
                retainLocked(XRRetainedHeadPose(headFrame: published, requestFrame: requestFrame, timestamp: timestamp,
                                                stageFromHead: stageFromHead, anchor: anchor, eyes: eyes,
                                                epoch: retainedEpoch), now: timestamp)
            } else {
                statistics.rejectedInputs &+= 1
            }
        }
        return published
        #else
        return nil
        #endif
    }

    private static func isFinite(_ matrix: simd_float4x4) -> Bool {
        [matrix.columns.0, matrix.columns.1, matrix.columns.2, matrix.columns.3].allSatisfy {
            $0.x.isFinite && $0.y.isFinite && $0.z.isFinite && $0.w.isFinite
        }
    }

    func resume() {
        lock.lock()
        allowsTrackedPoses = true
        lock.unlock()
    }

    func suspend() {
        lock.lock()
        defer { lock.unlock() }
        allowsTrackedPoses = false
        invalidateRetainedLocked(reason: "suspend")
        #if SV_GAME_RUNTIME
        publishLocked(timestamp: ProcessInfo.processInfo.systemUptime, stageFromHead: nil, tracked: false)
        #endif
    }

    /// Drops every retained anchor and starts a new retention epoch. Called on
    /// recenter, suspend and renderer stop; a lease from before the call can no
    /// longer resolve to an anchor.
    func invalidateRetainedAnchors(reason: String) {
        lock.lock()
        defer { lock.unlock() }
        invalidateRetainedLocked(reason: reason)
    }

    /// The pose the game actually consumed for `headFrame`, if it is still
    /// retained, from the current epoch, and younger than the age bound.
    func retainedPose(headFrame: UInt64, now: Double) -> XRRetainedHeadPose? {
        lock.lock()
        defer { lock.unlock() }
        guard now.isFinite else { statistics.rejectedInputs &+= 1; return nil }
        guard let pose = retained.first(where: { $0.headFrame == headFrame }) else {
            statistics.missesUnknown &+= 1
            return nil
        }
        guard pose.epoch == retainedEpoch else { statistics.missesEpoch &+= 1; return nil }
        guard now - pose.timestamp <= retainMaximumAge else { statistics.missesExpired &+= 1; return nil }
        statistics.hits &+= 1
        return pose
    }

    var retainedAnchorStatistics: XRRetainedAnchorStatistics {
        lock.lock()
        defer { lock.unlock() }
        var copy = statistics
        copy.occupancy = retained.count
        return copy
    }

    private func retainLocked(_ pose: XRRetainedHeadPose, now: Double) {
        let before = retained.count
        retained.removeAll { now - $0.timestamp > retainMaximumAge }
        statistics.evictedByAge &+= UInt64(before - retained.count)
        while retained.count >= retainCapacity {
            retained.removeFirst()
            statistics.evictedByCapacity &+= 1
        }
        retained.append(pose)
        statistics.retained &+= 1
        if pose.isSynthetic { statistics.retainedSynthetic &+= 1 }
    }

    private func invalidateRetainedLocked(reason: String) {
        retainedEpoch &+= 1
        guard !retained.isEmpty else { return }
        retained.removeAll()
        statistics.invalidations &+= 1
        log.info("Retained head anchors invalidated: \(reason, privacy: .public)")
    }

    #if SV_GAME_RUNTIME
    /// Returns the head frame assigned to this pose, or nil when the native side rejected it.
    @discardableResult
    private func publishLocked(timestamp: Double, stageFromHead: simd_float4x4?, tracked: Bool) -> UInt64? {
        frame &+= 1
        var pose = SVXRHeadPose()
        pose.version = 1
        pose.size = UInt32(MemoryLayout<SVXRHeadPose>.size)
        pose.frame = frame
        pose.presentation_time_seconds = timestamp
        pose.tracked = tracked ? 1 : 0
        pose.reserved = 0
        var matrix = stageFromHead ?? matrix_identity_float4x4
        withUnsafeMutableBytes(of: &pose.stage_from_head) { destination in
            withUnsafeBytes(of: &matrix) { destination.copyMemory(from: $0) }
        }
        let result = sv_xr_submit_pose(&pose)
        if result != 0, result != lastError {
            log.error("Native headset pose submission failed: \(result)")
        }
        lastError = result
        return result == 0 ? frame : nil
    }
    #endif

    func recenter() {
        #if SV_GAME_RUNTIME
        lock.lock()
        defer { lock.unlock() }
        invalidateRetainedLocked(reason: "recenter")
        sv_xr_recenter()
        #endif
    }
}

import Foundation
import OSLog
import simd

/// One counter for the process preserves ordering when immersion is reopened.
/// The native receiver stores a snapshot; camera updates run on the game thread.
final class XRHeadTracking: @unchecked Sendable {
    static let shared = XRHeadTracking()
    private let lock = NSLock()
    private var frame: UInt64 = 0
    private var lastError: Int32 = 0
    private var allowsTrackedPoses = true
    private let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "HeadPose")

    func submit(timestamp: Double, stageFromHead: simd_float4x4?, tracked: Bool) {
        #if SV_GAME_RUNTIME
        lock.lock()
        defer { lock.unlock() }
        guard !tracked || allowsTrackedPoses else { return }
        publishLocked(timestamp: timestamp, stageFromHead: stageFromHead, tracked: tracked)
        #endif
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
        #if SV_GAME_RUNTIME
        publishLocked(timestamp: ProcessInfo.processInfo.systemUptime, stageFromHead: nil, tracked: false)
        #endif
    }

    #if SV_GAME_RUNTIME
    private func publishLocked(timestamp: Double, stageFromHead: simd_float4x4?, tracked: Bool) {
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
    }
    #endif

    func recenter() {
        #if SV_GAME_RUNTIME
        lock.lock()
        defer { lock.unlock() }
        sv_xr_recenter()
        #endif
    }
}

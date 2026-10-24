import ARKit
import Foundation
import Spatial
import simd

/// Optional acquisition only. The renderer supplies its existing session and
/// accepted head generation; this object never owns another ARKitSession and
/// never mutates gameplay, fires a weapon or treats a wrist as a gun barrel.
final class XRMotionTracking: @unchecked Sendable {
    let enabled: Bool
    private let owner = UUID()
    private let lock = NSLock()
    private let accessoryGate = XRMotionProviderGate()
    private let sampleLock = NSLock()
    private var configuration = UUID()
    private var prepared = false
    private var started = false
    private var active = true
    private var session: ARKitSession?
    private var accessoryProvider: AccessoryTrackingProvider?
    private var handProvider: HandTrackingProvider?
    private var accessoryIDs = Set<UUID>()
    private var providerAccessoryIDs = Set<UUID>()
    private var accessoryChiralities: [UUID: Accessory.Chirality] = [:]
    private var accessoryUpdateInProgress = false
    private var accessoryUpdateQueued = false
    private var trackedAccessoryIDs = Set<UUID>()
    private var originalHands: [HandAnchor.Chirality: HandAnchor] = [:]
    private var observer: NSObjectProtocol?
    private var tasks: [Task<Void, Never>] = []
    private var status = "Motion tracking preview disabled"
    private var permissionStatus: [String: String] = [:]
    private var requiresReentry = false
    private var lastFlags: UInt32 = 0
    private var lastHeadFrame: UInt64 = 0
    private var headTracked = false
    private var sourceSamples: [String: [String: Any]] = [:]
    private var firstSourcePose: [String: (grip: simd_float4x4, aim: simd_float4x4)] = [:]

    init(enabled: Bool) { self.enabled = enabled }

    /// Call once before the renderer's session.run. Optional denied providers
    /// are omitted, leaving normal world tracking available. If run itself
    /// throws, all supplied providers stop: the caller must create a fresh
    /// WorldTrackingProvider for its world-only retry, never reuse the stopped one.
    @MainActor
    func prepare(session: ARKitSession, world: WorldTrackingProvider) async -> [any DataProvider] {
        guard enabled else { return [world] }
        stop()
        let token = UUID()
        lock.withLock {
            configuration = token
            prepared = true
            active = true
            sourceSamples.removeAll()
            firstSourcePose.removeAll()
            self.session = session
            status = "Preparing supported motion providers"
        }
        XRMotionTransport.shared.claim(owner)
        let accessories = SpatialGamepadInput.shared.trackingAccessories
        let wantsAccessory = AccessoryTrackingProvider.isSupported && !accessories.isEmpty
        let wantsHands = HandTrackingProvider.isSupported
        var requested: [ARKitSession.AuthorizationType] = []
        if wantsAccessory { requested += AccessoryTrackingProvider.requiredAuthorizations }
        if wantsHands { requested += HandTrackingProvider.requiredAuthorizations }
        requested = Array(Set(requested))
        let authorization = requested.isEmpty ? [:] : await session.requestAuthorization(for: requested)
        guard !Task.isCancelled, lock.withLock({ configuration == token && prepared }) else { return [world] }
        func allowed(_ required: [ARKitSession.AuthorizationType]) -> Bool {
            required.allSatisfy { authorization[$0] == .allowed }
        }
        let accessory = wantsAccessory && allowed(AccessoryTrackingProvider.requiredAuthorizations)
            ? AccessoryTrackingProvider(accessories: accessories) : nil
        let hands = wantsHands && allowed(HandTrackingProvider.requiredAuthorizations) ? HandTrackingProvider() : nil
        let installed = lock.withLock { () -> Bool in
            guard configuration == token, prepared else { return false }
            accessoryProvider = accessory
            handProvider = hands
            accessoryIDs = accessory == nil ? [] : Set(accessories.map(\.id))
            providerAccessoryIDs = accessoryIDs
            accessoryChiralities = accessory == nil ? [:] : Dictionary(uniqueKeysWithValues: accessories.map { ($0.id, $0.inherentChirality) })
            permissionStatus = Dictionary(uniqueKeysWithValues: authorization.map { (String(describing: $0.key), String(describing: $0.value)) })
            status = accessory == nil && hands == nil ? "No authorized motion providers; normal world tracking retained"
                : "Motion acquisition prepared; gameplay motion aiming remains separate"
            return true
        }
        guard installed else { return [world] }
        let observation = NotificationCenter.default.addObserver(forName: SpatialGamepadInput.accessoriesDidChange,
                                                                 object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in await self?.refreshAccessories() }
        }
        let observationInstalled = lock.withLock { () -> Bool in
            guard configuration == token, prepared else { return false }
            observer = observation
            return true
        }
        guard observationInstalled else {
            NotificationCenter.default.removeObserver(observation)
            return [world]
        }
        var providers: [any DataProvider] = [world]
        if let accessory { providers.append(accessory) }
        if let hands { providers.append(hands) }
        return providers
    }

    /// Called only after the caller's combined session.run succeeded.
    func didStart() {
        let state = lock.withLock { () -> (UUID, ARKitSession?, AccessoryTrackingProvider?, HandTrackingProvider?) in
            guard enabled, prepared, !started else { return (configuration, nil, nil, nil) }
            started = true
            return (configuration, session, accessoryProvider, handProvider)
        }
        guard let session = state.1 else { return }
        var newTasks: [Task<Void, Never>] = []
        newTasks.append(Task { [weak self, session] in
            for await event in session.events {
                guard !Task.isCancelled else { return }
                self?.sessionEvent(event, token: state.0)
            }
        })
        if let provider = state.2 {
            newTasks.append(Task { [weak self, provider] in
                for await update in provider.anchorUpdates {
                    guard !Task.isCancelled else { return }
                    self?.accessoryUpdate(update, token: state.0)
                }
            })
        }
        if let provider = state.3 {
            newTasks.append(Task { [weak self, provider] in
                for await update in provider.anchorUpdates {
                    guard !Task.isCancelled else { return }
                    self?.handUpdate(update, token: state.0)
                }
            })
        }
        let keep = lock.withLock { () -> Bool in
            guard configuration == state.0, started else { return false }
            tasks += newTasks
            return true
        }
        if !keep { newTasks.forEach { $0.cancel() } }
    }

    func disableForSession(reason: String) {
        stop()
        lock.withLock { status = "Motion providers disabled for this session: \(reason.prefix(240))" }
    }

    func setActive(_ value: Bool) {
        let changed = lock.withLock { () -> Bool in
            guard active != value else { return false }
            active = value
            originalHands.removeAll()
            trackedAccessoryIDs.removeAll()
            lastFlags = 0
            headTracked = false
            firstSourcePose.removeAll()
            return true
        }
        if changed { XRMotionTransport.shared.invalidate(owner) }
    }

    func invalidate(reason: String) {
        lock.withLock {
            originalHands.removeAll()
            trackedAccessoryIDs.removeAll()
            lastFlags = 0
            headTracked = false
            firstSourcePose.removeAll()
            status = String(reason.prefix(240))
        }
        XRMotionTransport.shared.invalidate(owner)
    }

    func stop() {
        let previous = lock.withLock { () -> ([Task<Void, Never>], NSObjectProtocol?) in
            let previous = (tasks, observer)
            tasks = []
            observer = nil
            configuration = UUID()
            prepared = false
            started = false
            accessoryProvider = nil
            handProvider = nil
            accessoryIDs.removeAll()
            providerAccessoryIDs.removeAll()
            accessoryChiralities.removeAll()
            accessoryUpdateInProgress = false
            accessoryUpdateQueued = false
            trackedAccessoryIDs.removeAll()
            originalHands.removeAll()
            session = nil
            requiresReentry = false
            lastFlags = 0
            headTracked = false
            status = enabled ? "Motion acquisition stopped" : "Motion tracking preview disabled"
            return previous
        }
        previous.0.forEach { $0.cancel() }
        if let observer = previous.1 { NotificationCenter.default.removeObserver(observer) }
        XRMotionTransport.shared.invalidate(owner)
    }

    @MainActor
    private func refreshAccessories() async {
        let current = SpatialGamepadInput.shared.trackingAccessories
        let ids = Set(current.map(\.id))
        if lock.withLock({ enabled && prepared && started && ids != accessoryIDs }) {
            XRMotionTransport.shared.invalidate(owner)
        }
        let state = lock.withLock { () -> (UUID, AccessoryTrackingProvider?)? in
            guard enabled, prepared, started, accessoryUpdateInProgress || ids != providerAccessoryIDs else { return nil }
            // Removed devices become unusable immediately, before any await.
            accessoryIDs.formIntersection(ids)
            accessoryChiralities = accessoryChiralities.filter { ids.contains($0.key) }
            trackedAccessoryIDs.removeAll()
            lastFlags = 0
            if accessoryUpdateInProgress { accessoryUpdateQueued = true; return nil }
            accessoryUpdateInProgress = true
            return (configuration, accessoryProvider)
        }
        guard let state else { return }
        defer {
            let queued = lock.withLock { () -> Bool in
                guard configuration == state.0 else { return false }
                accessoryUpdateInProgress = false
                let queued = accessoryUpdateQueued
                accessoryUpdateQueued = false
                return queued
            }
            if queued { Task { @MainActor [weak self] in await self?.refreshAccessories() } }
        }
        guard let provider = state.1 else {
            lock.withLock { requiresReentry = !ids.isEmpty; status = "Re-enter immersion after pairing to start accessory tracking" }
            return
        }
        guard #available(visionOS 27.0, *) else {
            lock.withLock { requiresReentry = !ids.isEmpty; status = "visionOS 26 accessory changes require re-entering immersion" }
            return
        }
        guard await accessoryGate.beginUpdate() else { return }
        defer { accessoryGate.endUpdate() }
        guard !Task.isCancelled, lock.withLock({ configuration == state.0 && started }) else { return }
        do {
            try await provider.updateAccessories(current)
            lock.withLock {
                guard configuration == state.0, started else { return }
                providerAccessoryIDs = ids
                if !accessoryUpdateQueued {
                    accessoryIDs = ids
                    accessoryChiralities = Dictionary(uniqueKeysWithValues: current.map { ($0.id, $0.inherentChirality) })
                }
                requiresReentry = false
                status = "Accessory tracking list updated; waiting for fresh full poses"
            }
        } catch {
            lock.withLock {
                guard configuration == state.0 else { return }
                requiresReentry = true
                status = "Accessory update failed; re-enter immersion: \(error.localizedDescription.prefix(180))"
            }
        }
    }

    private func sessionEvent(_ event: ARKitSession.Event, token: UUID) {
        let reason: String? = lock.withLock {
            guard configuration == token, started else { return nil }
            switch event {
            case .authorizationChanged(let type, let authorization):
                permissionStatus[String(describing: type)] = String(describing: authorization)
                return authorization == .allowed ? nil : "Motion authorization changed: \(type) \(authorization)"
            case .dataProviderStateChanged(_, let state, let error):
                return state == .running ? nil : "Tracking provider \(state): \(error?.localizedDescription ?? "no error")"
            @unknown default: return "Unknown tracking session event"
            }
        }
        if let reason { invalidate(reason: reason) }
    }

    private func accessoryUpdate(_ update: AnchorUpdate<AccessoryAnchor>, token: UUID) {
        let lost = lock.withLock { () -> Bool in
            guard configuration == token, started, active, accessoryIDs.contains(update.anchor.accessory.id) else { return false }
            let id = update.anchor.accessory.id
            let full = update.event != .removed && update.anchor.isTracked && update.anchor.trackingState == .positionOrientationTracked
            if full { trackedAccessoryIDs.insert(id); return false }
            return trackedAccessoryIDs.remove(id) != nil
        }
        if lost {
            let hand = update.anchor.accessory.inherentChirality
            invalidateSources(hand == .left ? 1 : hand == .right ? 2 : 0,
                              reason: "Accessory lost full position/orientation tracking")
        }
    }

    private func handUpdate(_ update: AnchorUpdate<HandAnchor>, token: UUID) {
        let lost = lock.withLock { () -> Bool in
            guard configuration == token, started, active else { return false }
            let hand = update.anchor.chirality
            let wasTracked = originalHands[hand] != nil
            if update.event == .removed || !update.anchor.isTracked {
                originalHands.removeValue(forKey: hand)
                return XRMotionPosePolicy.activeBareHandLost(wasTracked: wasTracked, left: hand == .left, lastFlags: lastFlags)
            }
            originalHands[hand] = update.anchor
            return false
        }
        if lost { invalidateSources(update.anchor.chirality == .left ? 1 : 2, reason: "Bare hand tracking lost") }
    }

    /// Source occlusion is independent per hand. Transport generation fences
    /// also reject an ARKit read already in progress when this loss arrives.
    private func invalidateSources(_ flags: UInt32, reason: String) {
        guard flags & 3 != 0 else { return }
        lock.withLock {
            lastFlags &= ~(flags | (flags << 8))
            if flags & 1 != 0 { originalHands.removeValue(forKey: .left) }
            if flags & 2 != 0 { originalHands.removeValue(forKey: .right) }
            status = String(reason.prefix(240))
        }
        XRMotionTransport.shared.invalidateSources(flags, owner: owner)
    }

    /// Bounded evidence that actual named controller poses change, rather than
    /// treating connection/validity bits or native hook counts as movement.
    private func recordSource(left: Bool, grip: simd_float4x4, aim: simd_float4x4,
                              originalTime: Double, targetTime: Double, headFrame: UInt64, sourceEpoch: UInt64, configuration expected: UUID) {
        lock.withLock {
            guard configuration == expected, prepared, started, active else { return }
            let key = left ? "left" : "right"
            let continuing = firstSourcePose[key] != nil
            let first = firstSourcePose[key] ?? (grip, aim)
            firstSourcePose[key] = first
            let positionDelta = simd_length(grip.columns.3 - first.grip.columns.3)
            let firstAim = simd_normalize(simd_quatf(first.aim)), latestAim = simd_normalize(simd_quatf(aim))
            let dot = min(1, max(0, abs(simd_dot(firstAim.vector, latestAim.vector))))
            let rotationDegrees = 2 * acos(dot) * 180 / Float.pi
            let previous = continuing ? sourceSamples[key] ?? [:] : [:]
            sourceSamples[key] = ["accepted_samples": (previous["accepted_samples"] as? UInt64 ?? 0) + 1,
                "last_head_frame": headFrame, "transport_epoch": sourceEpoch, "original_source_time_seconds": originalTime,
                "target_presentation_time_seconds": targetTime,
                "source_age_at_target_ms": (targetTime - originalTime) * 1000,
                "stage_grip_position_m": [grip.columns.3.x, grip.columns.3.y, grip.columns.3.z],
                "stage_aim_negative_z": [-aim.columns.2.x, -aim.columns.2.y, -aim.columns.2.z],
                "maximum_grip_displacement_from_first_m": max(previous["maximum_grip_displacement_from_first_m"] as? Float ?? 0, positionDelta),
                "maximum_aim_rotation_from_first_degrees": max(previous["maximum_aim_rotation_from_first_degrees"] as? Float ?? 0, rotationDegrees)]
        }
    }

    /// Call immediately after XRHeadTracking.submit accepts this same head
    /// frame, using that drawable's presentation time and session-origin head.
    func sample(timestamp: Double, stageFromHead: simd_float4x4?, headFrame: UInt64) {
        // Hand prediction is likewise sampled on one render call at a time;
        // never block a second compositor caller behind an ARKit query.
        guard sampleLock.try() else { return }
        defer { sampleLock.unlock() }
        let state = lock.withLock { () -> (UUID, AccessoryTrackingProvider?, HandTrackingProvider?, Set<UUID>, [HandAnchor.Chirality: HandAnchor], Set<Accessory.Chirality>)? in
            guard enabled, prepared, started, active else { return nil }
            return (configuration, accessoryProvider, handProvider, accessoryIDs, originalHands, Set(accessoryChiralities.values))
        }
        guard let state, let sourceLease = XRMotionTransport.shared.samplingLease(owner) else { return }
        guard headFrame != 0, timestamp.isFinite, let stageFromHead, XRMotionPosePolicy.rigid(stageFromHead) else {
            let hadPose = lock.withLock { headTracked }
            if hadPose { invalidate(reason: "Head tracking unavailable") }
            return
        }
        lock.withLock { headTracked = true }
        var matrices = [stageFromHead] + Array(repeating: simd_float4x4(), count: 4)
        var times = [timestamp, 0, 0]
        var flags: UInt32 = 0
        var queriedSources: UInt32 = 0
        let accessoryHands = state.5
        let accessoryRead = accessoryGate.beginRead()
        defer { if accessoryRead { accessoryGate.endRead() } }
        if accessoryRead, let provider = state.1, provider.state == .running {
            if accessoryHands.contains(.left) { queriedSources |= 1 }
            if accessoryHands.contains(.right) { queriedSources |= 2 }
            for original in provider.latestAnchors where state.3.contains(original.accessory.id) {
                let hand = original.accessory.inherentChirality
                guard hand == .left || hand == .right else { continue }
                guard original.isTracked, original.trackingState == .positionOrientationTracked,
                      XRMotionPosePolicy.fresh(source: original.timestamp, target: timestamp),
                      original.accessory.locations.contains(.grip), original.accessory.locations.contains(.aim),
                      let predicted = provider.predictAnchor(for: original, at: timestamp), predicted.isTracked,
                      predicted.trackingState == .positionOrientationTracked else { continue }
                let anchorSpace = predicted.coordinateSpace(correction: .none)
                let referenceFromAnchor = anchorSpace.ancestorFromSpaceTransformFloat().matrix
                let referenceFromGrip = predicted.coordinateSpace(for: .grip, correction: .none).ancestorFromSpaceTransformFloat().matrix
                let referenceFromAim = predicted.coordinateSpace(for: .aim, correction: .none).ancestorFromSpaceTransformFloat().matrix
                guard let grip = XRMotionPosePolicy.stageLocation(stageFromAnchor: predicted.originFromAnchorTransform,
                                                                 referenceFromAnchor: referenceFromAnchor, referenceFromLocation: referenceFromGrip),
                      let aim = XRMotionPosePolicy.stageLocation(stageFromAnchor: predicted.originFromAnchorTransform,
                                                                referenceFromAnchor: referenceFromAnchor, referenceFromLocation: referenceFromAim) else { continue }
                let left = hand == .left, index = left ? 1 : 3
                matrices[index] = grip
                matrices[index + 1] = aim
                times[left ? 1 : 2] = original.timestamp
                flags |= left ? 1 : 2
            }
        }
        if let provider = state.2, provider.state == .running {
            if !accessoryHands.contains(.left) { queriedSources |= 1 }
            if !accessoryHands.contains(.right) { queriedSources |= 2 }
            let predicted = provider.handAnchors(at: timestamp)
            for (hand, anchor) in [(HandAnchor.Chirality.left, predicted.leftHand), (.right, predicted.rightHand)] {
                let left = hand == .left
                guard !accessoryHands.contains(left ? .left : .right),
                      let original = state.4[hand], original.isTracked,
                      XRMotionPosePolicy.fresh(source: original.timestamp, target: timestamp),
                      let anchor, anchor.id == original.id, anchor.isTracked,
                      let skeleton = anchor.handSkeleton else { continue }
                let wrist = skeleton.joint(.wrist)
                guard wrist.isTracked else { continue }
                let stageFromWrist = anchor.originFromAnchorTransform * wrist.anchorFromJointTransform
                guard XRMotionPosePolicy.rigid(stageFromWrist) else { continue }
                matrices[left ? 1 : 3] = stageFromWrist
                // Aim stays zero: a wrist frame is not a calibrated barrel.
                times[left ? 1 : 2] = original.timestamp
                flags |= left ? (1 | (1 << 8)) : (2 | (1 << 9))
            }
        }
        let stillActive = lock.withLock { configuration == state.0 && started && active }
        guard stillActive else { return }
        let lostSources = lock.withLock {
            XRMotionPosePolicy.lostSources(previous: lastFlags, current: flags, queried: queriedSources)
        }
        if lostSources != 0 {
            invalidateSources(lostSources, reason: "Motion source lost fresh full tracking; independent healthy source retained")
        }
        let packet = XRMotionPacket(matrices: matrices, times: times, ids: [1, sourceLease.epoch, headFrame, 0], flags: flags)
        if let publishedFlags = XRMotionTransport.shared.publish(packet, owner: owner, lease: sourceLease) {
            lock.withLock {
                lastFlags = publishedFlags
                lastHeadFrame = headFrame
                if publishedFlags & 3 != 0 { status = "Fresh motion packet published; flags=\(publishedFlags & 0x303)" }
            }
            for left in [true, false] {
                let index = left ? 1 : 3, bit: UInt32 = left ? 1 : 2
                if publishedFlags & bit != 0 && publishedFlags & (bit << 8) == 0 {
                    recordSource(left: left, grip: matrices[index], aim: matrices[index + 1],
                                 originalTime: times[left ? 1 : 2], targetTime: timestamp, headFrame: headFrame,
                                 sourceEpoch: sourceLease.epoch, configuration: state.0)
                }
            }
        }
    }

    var report: [String: Any] {
        lock.withLock {
            ["preview_enabled": enabled, "prepared": prepared, "started": started, "active": active,
             "status": status, "permissions": permissionStatus, "requires_immersive_reentry": requiresReentry,
             "accessory_provider": accessoryProvider.map { String(describing: $0.state) } ?? "absent",
             "hand_provider": handProvider.map { String(describing: $0.state) } ?? "absent",
             "configured_accessories": providerAccessoryIDs.count, "currently_eligible_accessories": accessoryIDs.count,
             "last_pose_flags": lastFlags,
             "last_native_head_frame": lastHeadFrame, "max_original_source_age_seconds": XRMotionPosePolicy.maximumSourceAge,
             "named_accessory_source_samples": sourceSamples,
             "named_accessory_source_samples_scope": "historical; extents reset at head/session-origin invalidation, retained on exit; timestamps do not imply current validity",
             "source_loss_policy": "per-hand revocation fences; head/session loss revokes whole epoch",
             "location_transform": "originFromAnchor * inverse(referenceFromAnchor_none) * referenceFromNamedLocation_none",
             "bare_hand_source": "tracked skeleton wrist; aim matrix zero; not gun barrel",
             "gameplay_mutation": false]
        }
    }
}

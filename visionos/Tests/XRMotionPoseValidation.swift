import Foundation
import simd

@main
enum XRMotionPoseValidation {
    static func translated(_ x: Float, _ y: Float = 0, _ z: Float = 0) -> simd_float4x4 {
        var matrix = matrix_identity_float4x4
        matrix.columns.3 = SIMD4(x, y, z, 1)
        return matrix
    }

    static func packet(_ frame: UInt64, _ time: Double, flags: UInt32 = 3) -> XRMotionPacket {
        XRMotionPacket(matrices: [translated(1), translated(-0.3), translated(-0.3, 0, -0.2),
                                  translated(0.3), translated(0.3, 0, -0.2)],
                       times: [time, time - 0.01, time - 0.02], ids: [1, 1, frame, 0], flags: flags)
    }

    static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        guard value() else { fatalError(message) }
    }

    static func main() async {
        var tests = 0
        func test(_ name: String, _ body: () -> Void) { body(); tests += 1; print("PASS \(name)") }
        test("freshness uses original source time and rejects stale/future/nonfinite") {
            check(XRMotionPosePolicy.fresh(source: 10, target: 10.1), "fresh source rejected")
            check(!XRMotionPosePolicy.fresh(source: 10, target: 10.151), "stale source accepted")
            check(!XRMotionPosePolicy.fresh(source: 10.01, target: 10), "future source accepted")
            check(!XRMotionPosePolicy.fresh(source: .nan, target: 10), "NaN source accepted")
            check(!XRMotionPosePolicy.fresh(source: 0, target: 10), "missing source accepted")
        }
        test("rigid poses reject mirror, scale, perspective and nonfinite") {
            check(XRMotionPosePolicy.rigid(translated(3, 2, 1)), "rigid translation rejected")
            var bad = translated(0); bad.columns.0.x = -1
            check(!XRMotionPosePolicy.rigid(bad), "mirror accepted")
            bad = translated(0); bad.columns.1.y = 2
            check(!XRMotionPosePolicy.rigid(bad), "scale accepted")
            bad = translated(0); bad.columns.2.w = 0.2
            check(!XRMotionPosePolicy.rigid(bad), "perspective accepted")
            bad = translated(0); bad.columns.3.x = .infinity
            check(!XRMotionPosePolicy.rigid(bad), "infinite translation accepted")
        }
        test("common world reference cancels before named location enters stage") {
            let rotation = simd_float4x4(simd_quatf(angle: .pi / 2, axis: SIMD3(0, 1, 0)))
            let reference = translated(100, 20, -30) * rotation
            let stage = translated(1, 2, 3)
            let anchorFromLocation = translated(0.2, 0.1, -0.3)
            let result = XRMotionPosePolicy.stageLocation(stageFromAnchor: stage,
                referenceFromAnchor: reference, referenceFromLocation: reference * anchorFromLocation)!
            let expected = stage * anchorFromLocation
            check(simd_length(result.columns.3 - expected.columns.3) < 0.0001, "reference offset leaked into stage")
            check(XRMotionPosePolicy.stageLocation(stageFromAnchor: stage,
                referenceFromAnchor: simd_float4x4(), referenceFromLocation: reference) == nil, "singular reference accepted")
        }
        test("exact head lookup cannot substitute newer snapshot or restamp source") {
            let transport = XRMotionTransport(), owner = UUID(), epoch = transport.claim(owner)
            check(transport.publish(packet(11, 100), owner: owner, epoch: epoch), "publish11")
            check(transport.publish(packet(12, 100.01), owner: owner, epoch: epoch), "publish12")
            let exact = transport.read(expectedHeadFrame: 11, now: 100.02)!
            check(exact.ids[2] == 11 && exact.times[0] == 100 && exact.times[1] == 99.99, "exact head/source changed")
            check(transport.read(expectedHeadFrame: 99, now: 100.02) == nil, "unknown head substituted latest")
            check(transport.read(expectedHeadFrame: 0, now: 100.02)!.ids[2] == 12, "latest diagnostic incorrect")
            check(!transport.publish(packet(11, 100.02), owner: owner, epoch: epoch), "immutable exact head replaced")
        }
        test("partial stale source clears only its validity and matrices") {
            let transport = XRMotionTransport(), owner = UUID(), epoch = transport.claim(owner)
            var value = packet(1, 10); value.times[1] = 9
            transport.publish(value, owner: owner, epoch: epoch)
            let result = transport.read(expectedHeadFrame: 1, now: 10)!
            check(result.flags == 2, "wrong hand remained valid")
            check(result.matrices[1] == simd_float4x4() && result.matrices[2] == simd_float4x4(), "stale hand matrices retained")
            check(result.matrices[3] == value.matrices[3], "fresh opposite hand discarded")
        }
        test("bare wrist never becomes a fabricated gun barrel") {
            let transport = XRMotionTransport(), owner = UUID(), epoch = transport.claim(owner)
            var value = packet(1, 10, flags: 1 | 256); value.matrices[2] = simd_float4x4()
            transport.publish(value, owner: owner, epoch: epoch)
            let result = transport.read(expectedHeadFrame: 1, now: 10)!
            check(result.flags == 257 && result.matrices[2] == simd_float4x4(), "bare source was fabricated or lost identity")
        }
        test("normal hand occlusion cannot invalidate an active Sense source") {
            check(!XRMotionPosePolicy.activeBareHandLost(wasTracked: true, left: true, lastFlags: 3), "raw hand occlusion invalidated controller tracking")
            check(XRMotionPosePolicy.activeBareHandLost(wasTracked: true, left: true, lastFlags: 257), "active bare hand loss retained pose")
            check(!XRMotionPosePolicy.activeBareHandLost(wasTracked: true, left: false, lastFlags: 257), "opposite raw hand invalidated active source")
            check(!XRMotionPosePolicy.activeBareHandLost(wasTracked: false, left: true, lastFlags: 257), "unseen hand generated loss")
        }
        test("loss clears entire ring and epoch rejects in-flight old publisher") {
            let transport = XRMotionTransport(), owner = UUID(), epoch = transport.claim(owner)
            transport.publish(packet(1, 10), owner: owner, epoch: epoch)
            let nextEpoch = transport.invalidate(owner)!
            check(nextEpoch != epoch, "epoch did not advance")
            check(transport.read(expectedHeadFrame: 1, now: 10) == nil, "loss retained old head pose")
            check(!transport.publish(packet(2, 10), owner: owner, epoch: epoch), "old in-flight pose published after loss")
        }
        test("old renderer cleanup cannot invalidate new owner") {
            let transport = XRMotionTransport(), oldOwner = UUID(), newOwner = UUID()
            let oldEpoch = transport.claim(oldOwner), epoch = transport.claim(newOwner)
            transport.publish(packet(1, 10), owner: newOwner, epoch: epoch)
            check(transport.invalidate(oldOwner) == nil, "old owner invalidated new renderer")
            check(!transport.publish(packet(2, 10), owner: oldOwner, epoch: oldEpoch), "old owner published")
            check(transport.read(expectedHeadFrame: 1, now: 10) != nil, "new owner lost valid pose")
        }
        test("ring is bounded and expires original source rather than read time") {
            let transport = XRMotionTransport(), owner = UUID(), epoch = transport.claim(owner)
            for frame in 1...80 { transport.publish(packet(UInt64(frame), 10), owner: owner, epoch: epoch) }
            check(transport.read(expectedHeadFrame: 1, now: 10) == nil, "capacity did not evict oldest")
            check(transport.read(expectedHeadFrame: 17, now: 10) != nil, "capacity64 incorrectly evicted live snapshot")
            check(transport.read(expectedHeadFrame: 80, now: 10.151) == nil, "stale pose restamped on read")
        }
        test("C ABI exact lookup, column order and invalid-size rejection") {
            let owner = UUID(), epoch = XRMotionTransport.shared.claim(owner)
            let now = ProcessInfo.processInfo.systemUptime
            XRMotionTransport.shared.publish(packet(987, now), owner: owner, epoch: epoch)
            var floats = [Float](repeating: -7, count: 80), times = [Double](repeating: -7, count: 3), ids = [UInt64](repeating: 7, count: 4)
            check(svXRMotionReadV1(987, &floats, 80, &times, 3, &ids, 4) == 3, "ABI valid exact read failed")
            check(ids[2] == 987 && ids[1] == epoch && floats[12] == 1, "ABI layout/transposition wrong")
            check(svXRMotionReadV1(988, &floats, 80, &times, 3, &ids, 4) == 0 && floats.allSatisfy { $0 == 0 }, "ABI substituted or retained unknown head")
            floats[0] = 55
            check(svXRMotionReadV1(987, &floats, 79, &times, 3, &ids, 4) == 0 && floats[0] == 55, "invalid size wrote output")
            XRMotionTransport.shared.invalidate(owner)
        }
        let gate = XRMotionProviderGate()
        check(gate.beginRead(), "first reader rejected")
        check(!gate.beginRead(), "concurrent AR_MT_UNSAFE reader admitted")
        let writer = Task { await gate.beginUpdate() }
        for _ in 0..<10_000 {
            if gate.updatePending { break }
            await Task.yield()
        }
        check(gate.updatePending, "writer failed to reserve barrier")
        check(!gate.beginRead(), "reader entered after update reservation")
        gate.endRead()
        let acquired = await writer.value
        check(acquired, "writer failed after reader released")
        check(!gate.beginRead(), "reader entered during async provider update")
        let competingWriter = await gate.beginUpdate()
        check(!competingWriter, "concurrent async update admitted")
        gate.endUpdate()
        check(gate.beginRead(), "reader did not resume after completed update")
        gate.endRead()
        tests += 1
        print("PASS explicit reader/update barrier excludes all AR_MT_UNSAFE overlap")
        print("PASS \(tests) motion pose policy/transport tests; no ARKit/device/runtime initialized")
    }
}

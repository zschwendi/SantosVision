import Foundation
import simd

@main
struct DepthRangeValidation {
    static func main() throws {
        var checks = 0
        func check(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        for unknown: Float in [.nan, .infinity, 0, -1, 20_001] {
            check(GameWorldDepthRange.selectedFar(nativeFar: unknown) == 1000, "unknown original far retains finite fallback")
        }
        check(GameWorldDepthRange.selectedFar(nativeFar: 10_000) == 10_000, "preserve observed10km native world")
        check(GameWorldDepthRange.selectedFar(nativeFar: 20_000) == 20_000, "finite upper native limit")
        let packet = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        check(packet.count == 288, "registered physical eye packet")
        for eye in 0..<2 {
            let v = (0..<16).map { i in Float(bitPattern: packet.withUnsafeBytes {
                $0.loadUnaligned(fromByteOffset: 96 + eye * 128 + i * 4, as: UInt32.self)
            }) }
            let captured = simd_float4x4(columns: (SIMD4(v[0],v[1],v[2],v[3]),SIMD4(v[4],v[5],v[6],v[7]),
                SIMD4(v[8],v[9],v[10],v[11]),SIMD4(v[12],v[13],v[14],v[15])))
            // Captured SDK calibrated X/Y/W with mathematically regenerated finite
            // depth rows. These are not falsely labeled new SDK10km captures.
            for near: Float in [0.05, 0.1, 0.15] { for far: Float in [1000, 10_000, 20_000] {
                var physical = captured
                physical[2][2] = near / (far - near)
                physical[3][2] = near * far / (far - near)
                check(GameWorldProjection.nativePinhole(physical) != nil, "extended finite calibrated source admits unchanged ray budget")
                for scale: Float in [1.3, 2, 3] { for fov: Float in [45, 15, 5] {
                    let optic = GameWorldScopeOptics.Optics(flags: 1,
                        magnification: GameWorldScopeOptics.magnification(referenceFOV:45, actualFOV:fov)!,
                        referenceFOV:45, actualFOV:fov, scopeSequence:1)!
                    let request = GameWorldSourceGuardBand.projection(physical, scale:scale)!
                    for reversed in [true, false] {
                        let source = GameWorldScopeOptics.expectedSourceProjection(request, optics:optic, depthReversed:reversed)!
                        let admission = GameWorldScopeOptics.admitSource(source, depthReversed:reversed,
                            retainedPhysical:physical, guardScale:scale, optics:optic)
                        check(admission.passed, "both depth conventions retain exact expanded lease-to-physical optical admission")
                        let display = GameWorldScopeOptics.projection(physical, optics:optic)!
                        for c in 0..<4 { check(display[c][2].bitPattern == physical[c][2].bitPattern,
                            "zoom preserves extended physical depth row") }
                    }
                } }
                let a = physical * SIMD4<Float>(0,0,-far * 0.9,1)
                check(a.z / a.w > 0, "distant world inside native far survives display clipping")
                var old = physical; old[2][2] = near / (1000-near); old[3][2] = near*1000/(1000-near)
                if far > 1000 {
                    let clipped = old * SIMD4<Float>(0,0,-far * 0.9,1)
                    check(clipped.z < 0, "old1km range demonstrably clips native distant world")
                }
            } }
            var infinite = captured; infinite[2][2] = 0
            check(GameWorldProjection.nativePinhole(infinite) == nil, "infinite far remains unsupported")
        }
        print("PASS: \(checks) finite depth policy, calibrated source/scope admission and distant clipping checks. Captured eye calibration with regenerated depth rows; no new SDK/device capture.")
    }
}

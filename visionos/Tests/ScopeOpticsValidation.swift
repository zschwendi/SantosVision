import Foundation
import simd

@MainActor
@main
struct ScopeOpticsValidation {
    static var checks = 0
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message)
        checks += 1
    }
    static func bits(_ matrix: simd_float4x4) -> [UInt32] {
        (0..<4).flatMap { c in (0..<4).map { matrix[c][$0].bitPattern } }
    }
    static func d(_ matrix: simd_float4x4) -> simd_double4x4 {
        .init(columns: (SIMD4<Double>(matrix[0]), SIMD4<Double>(matrix[1]),
                        SIMD4<Double>(matrix[2]), SIMD4<Double>(matrix[3])))
    }
    static func pinhole(asymmetry: Float = 0) -> simd_float4x4 {
        .init(columns: (SIMD4(1.1, 0, 0, 0), SIMD4(0, 1.2, 0, 0),
                        SIMD4(asymmetry, -0.025, 0.1 / 999.9, -1), SIMD4(0, 0, 100 / 999.9, 0)))
    }
    static func optics(_ fov: Float, sequence: UInt64 = 7) -> GameWorldScopeOptics.Optics {
        let m = GameWorldScopeOptics.magnification(referenceFOV: 45, actualFOV: fov)!
        return .init(flags: 1, magnification: m, referenceFOV: 45, actualFOV: fov, scopeSequence: sequence)!
    }
    static func candidate(_ eye: Int, camera: UInt64 = 9, epoch: UInt64 = 2,
                          frame: UInt64 = 10, optic: GameWorldScopeOptics.Optics = optics(15)) -> GameWorldScopeOptics.Candidate {
        .init(lease: UInt64(eye + 1), epoch: epoch, gameFrame: frame, requestFrame: 5,
              headFrame: 6, cameraSequence: camera, presentationTime: 7,
              sourceEye: eye, viewCount: 2, optics: optic)
    }
    static func main() throws {
        let identity = GameWorldScopeOptics.Optics(flags: 0, magnification: 1,
            referenceFOV: 0, actualFOV: 0, scopeSequence: 0)!
        check(identity.sourceCopyAllowed, "identity may proceed to existing copy checks")
        check(!optics(15).sourceCopyAllowed, "magnified source cannot claim source-anchor copy")
        check(optics(45).magnification == 1, "same native aim reference FOV is exactly identity")
        check(abs(optics(5).magnification - 9.48705) < 1e-5, "native minimum FOV uses tangent ratio, not FOV quotient")
        for bad in [Float.nan, .infinity, 0, 0.99, 131] {
            check(GameWorldScopeOptics.magnification(referenceFOV: 45, actualFOV: bad) == nil, "invalid FOV rejected")
        }
        check(GameWorldScopeOptics.magnification(referenceFOV: 45, actualFOV: 60) == nil, "unsupported zoom out rejected")
        check(GameWorldScopeOptics.magnification(referenceFOV: 45, actualFOV: 1) == nil, "outside qualified native optical range rejected")
        check(GameWorldScopeOptics.Optics(flags: 2, magnification: 1, referenceFOV: 45, actualFOV: 45, scopeSequence: 7) == nil, "unknown flags rejected")
        check(GameWorldScopeOptics.Optics(flags: 1, magnification: 3, referenceFOV: 45, actualFOV: 15, scopeSequence: 7) == nil, "invented magnification rejected")
        check(GameWorldScopeOptics.Optics(flags: 1, magnification: 1, referenceFOV: 45, actualFOV: 45, scopeSequence: 0) == nil, "scope without identity rejected")
        check(GameWorldScopeOptics.Optics(flags: 0, magnification: 1, referenceFOV: 45, actualFOV: 45, scopeSequence: 0) == nil, "ordinary metadata cannot masquerade as qualified scope")

        for asymmetry: Float in [-0.085, 0.075] {
            let physical = pinhole(asymmetry: asymmetry)
            check(bits(GameWorldScopeOptics.projection(physical, optics: identity)!) == bits(physical), "identity projection is byte exact")
            for fov: Float in [45, 30, 15, 10, 7.5, 5] {
                let optic = optics(fov)
                let display = GameWorldScopeOptics.projection(physical, optics: optic)!
                let forward = physical * SIMD4<Float>(0, 0, -1, 0)
                let scopedForward = display * SIMD4<Float>(0, 0, -1, 0)
                check(forward == scopedForward, "principal point must not acquire per-eye offset")
                for c in 0..<4 {
                    check(display[c][2].bitPattern == physical[c][2].bitPattern, "depth row preserved")
                    check(display[c][3].bitPattern == physical[c][3].bitPattern, "homogeneous row preserved")
                }
                var request = physical
                for c in 0..<4 { request[c][0] /= 1.3; request[c][1] /= 1.3 }
                for reversed in [true, false] {
                    let source = GameWorldScopeOptics.expectedSourceProjection(request, optics: optic, depthReversed: reversed)!
                    check(source[2][0].bitPattern == request[2][0].bitPattern, "native source offset is byte exact")
                    check(source[2][1].bitPattern == request[2][1].bitPattern, "native vertical offset is byte exact")
                    check(source[0][0] == request[0][0] * optic.magnification, "native sparse X operation reproduced")
                    let admission = GameWorldScopeOptics.admitSource(source, depthReversed: reversed,
                        retainedPhysical: physical, guardScale: 1.3, optics: optic)
                    check(admission.passed, "canonical source stays within unchanged physical budget")
                    for point in [SIMD4<Double>(0.01, 0, -0.5, 1), SIMD4(0.2, -0.1, -10, 1), SIMD4(1, 0.5, -40, 1)] {
                        let sourceClip = d(source) * point
                        let recovered = d(source).inverse * sourceClip
                        let coupled = d(display) * recovered
                        let plain = d(physical) * recovered
                        let direct = d(physical) * point
                        let center = Double(forward.x / forward.w)
                        check(simd_length(recovered - point) < 1e-8, "own source inverse reconstructs true point")
                        check(abs(coupled.x / coupled.w - center - Double(optic.magnification) * (direct.x / direct.w - center)) < 2e-6, "coupled source/display scale produces correct optical zoom")
                        check(abs(plain.x / plain.w - direct.x / direct.w) < 1e-8, "source-only FOV change cancels, catching original defect")
                        check(abs(coupled.z / coupled.w - direct.z / direct.w) < 1e-10, "magnification does not invent depth")
                    }
                }
            }
        }
        check(GameWorldScopeOptics.selectedIndex([candidate(0, camera: 9, optic: optics(15)), candidate(1, camera: 10, optic: optics(10))], viewCount: 2) == 1, "one latest same-scope scale for both targets")
        check(GameWorldScopeOptics.selectedIndex([candidate(0, camera: 10, optic: optics(10)), candidate(1, camera: 9, optic: optics(15))], viewCount: 2) == 0, "choice follows source generation, not eye side")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), candidate(1, optic: optics(15, sequence: 8))], viewCount: 2) == nil, "scope transition never mixes eyes")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), candidate(1, epoch: 3)], viewCount: 2) == nil, "epoch transition rejected")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), candidate(1, optic: identity)], viewCount: 2) == nil, "scoped and ordinary eyes not mixed")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), nil], viewCount: 2) == nil, "missing optical lease rejected")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), candidate(0)], viewCount: 2) == nil, "duplicate eye rejected")
        check(GameWorldScopeOptics.selectedIndex([candidate(0), candidate(1, optic: optics(10))], viewCount: 2) == nil, "same camera generation cannot disagree about optics")

        var bad = pinhole(); bad[0][0] = .nan
        check(GameWorldScopeOptics.projection(bad, optics: identity) == nil, "nonfinite projection rejected before identity")
        bad = pinhole(); bad[2][2] = 0
        check(GameWorldScopeOptics.projection(bad, optics: optics(15)) == nil, "infinite far unsupported")
        bad = pinhole(); bad[0][0] = -1
        check(GameWorldScopeOptics.projection(bad, optics: optics(15)) == nil, "negative focal rejected")
        let physical = pinhole(), optic = optics(15)
        var request = physical
        for c in 0..<4 { request[c][0] /= 1.3; request[c][1] /= 1.3 }
        var source = GameWorldScopeOptics.expectedSourceProjection(request, optics: optic, depthReversed: true)!
        source[1][0] = 0.01
        check(!GameWorldScopeOptics.admitSource(source, depthReversed: true, retainedPhysical: physical, guardScale: 1.3, optics: optic).passed, "wrong source geometry does not widen existing error tolerance")
        check(!GameWorldScopeOptics.admitSource(physical, depthReversed: true, retainedPhysical: physical, guardScale: 0.9, optics: optic).passed, "invalid guard rejected")

        if CommandLine.arguments.count > 1 {
            let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
            check(data.count == 288, "captured physical packet size")
            for eye in 0..<2 {
                let vals = (0..<16).map { index in Float(bitPattern: data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 96 + eye * 128 + index * 4, as: UInt32.self) }) }
                let raw = simd_float4x4(columns: (SIMD4(vals[0], vals[1], vals[2], vals[3]), SIMD4(vals[4], vals[5], vals[6], vals[7]), SIMD4(vals[8], vals[9], vals[10], vals[11]), SIMD4(vals[12], vals[13], vals[14], vals[15])))
                for guardScale: Float in [1.3, 1.75, 2, 3] {
                var requested = GameWorldProjection.nativePinhole(raw)!.matrix
                for c in 0..<4 { requested[c][0] /= guardScale; requested[c][1] /= guardScale }
                for fov: Float in [45, 30, 15, 10, 7.5, 5] {
                    let o = optics(fov)
                    for reversed in [true, false] {
                        let actual = GameWorldScopeOptics.expectedSourceProjection(requested, optics: o, depthReversed: reversed)!
                        let admission = GameWorldScopeOptics.admitSource(actual, depthReversed: reversed, retainedPhysical: raw, guardScale: guardScale, optics: o)
                        check(admission.passed, "actual physical AVP calibration retains unchanged admission budget")
                        let display = GameWorldScopeOptics.projection(raw, optics: o)!
                        for c in 0..<4 { check(display[c][2].bitPattern == raw[c][2].bitPattern && display[c][3].bitPattern == raw[c][3].bitPattern, "actual physical calibrated Z/W unchanged") }
                        let normalized = d(actual)
                        let inverse = normalized.inverse
                        for x: Double in [-0.75, 0, 0.75] { for y: Double in [-0.75, 0, 0.75] { for z: Double in [0, 0.5, 1] {
                            let clip = SIMD4<Double>(x, y, z, 1)
                            let point = inverse * clip
                            check(simd_length(normalized * point - clip) < 1e-8, "calibrated scoped depth project/unproject roundtrip")
                        } } }
                    }
                }
            }
            }
        }
        print("PASS: \(checks) scope optics assertions; pure source/display projection, immutable binocular selection and physical-calibration admission. No game/GPU/headset execution.")
    }
}

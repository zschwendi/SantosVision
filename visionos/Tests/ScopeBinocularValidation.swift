// Scoped binocular registration with the captured physical eye calibration.
// CPU evaluation of the complete production chain for one world point:
// native scoped source (guard band + own-axis optics) -> world reprojection
// uniforms -> magnified display projection -> perceived physical direction,
// compared with the production collimated native graticule. This is geometric
// evidence only: no GPU, game, headset or same-update stereo claim.
import Foundation
import simd

@MainActor
@main
struct ScopeBinocularValidation {
    static var checks = 0
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message)
        checks += 1
    }
    static func bits(_ m: simd_float4x4) -> [UInt32] { (0..<4).flatMap { c in (0..<4).map { m[c][$0].bitPattern } } }
    static func d(_ m: simd_float4x4) -> simd_double4x4 {
        .init(columns: (SIMD4<Double>(m[0]), SIMD4<Double>(m[1]), SIMD4<Double>(m[2]), SIMD4<Double>(m[3])))
    }
    static func f(_ m: simd_double4x4) -> simd_float4x4 {
        .init(columns: (SIMD4<Float>(m[0]), SIMD4<Float>(m[1]), SIMD4<Float>(m[2]), SIMD4<Float>(m[3])))
    }
    static func optics(_ fov: Float) -> GameWorldScopeOptics.Optics {
        if fov == 45 { return .identity }
        let m = GameWorldScopeOptics.magnification(referenceFOV: 45, actualFOV: fov)!
        return .init(flags: 1, magnification: m, referenceFOV: 45, actualFOV: fov, scopeSequence: 3)!
    }
    /// Perceived head-space direction of a display NDC through the PHYSICAL
    /// display calibration (what the panel pixel means to that eye).
    static func perceived(_ ndc: SIMD2<Float>, physical: simd_float4x4, eye: simd_float4x4) -> SIMD3<Double> {
        let p = d(physical).inverse * SIMD4<Double>(Double(ndc.x), Double(ndc.y), 0.5, 1)
        var rotation = d(eye); rotation[3] = SIMD4(0, 0, 0, 1)
        let v = rotation * SIMD4(p.x / p.w, p.y / p.w, p.z / p.w, 0)
        return simd_normalize(SIMD3(v.x, v.y, v.z))
    }
    /// Yaw/pitch in degrees about head -Z.
    static func angles(_ v: SIMD3<Double>) -> SIMD2<Double> {
        SIMD2(atan2(v.x, -v.z), atan2(v.y, -v.z)) * 180 / .pi
    }
    struct Chain { let ndc: SIMD2<Float>; let relative: simd_float4x4 }
    /// The renderer's per-eye chain for one world point (stage == game == head
    /// origin; static head). sourceEye/displayEye are head-from-eye.
    static func chain(world: SIMD3<Double>, sourceEye: simd_float4x4, source: simd_float4x4,
                      displayEye: simd_float4x4, display: simd_float4x4) -> Chain? {
        // Native source: the lease's depth/color at this point's source NDC.
        let eyePoint = d(sourceEye).inverse * SIMD4(world, 1)
        let clip = d(source) * eyePoint
        guard clip.w > 0 else { return nil }
        let sndc = SIMD3(clip.x, clip.y, clip.z) / clip.w
        guard abs(sndc.x) <= 1, abs(sndc.y) <= 1, sndc.z > 0, sndc.z < 1 else { return nil }
        // GameWorldRenderer.worldUniforms with gameFromStage = stageFromHead = I.
        let relative = f(d(displayEye).inverse * d(sourceEye))
        // gameWorldCornerVertex: unproject with the source inverse, then reproject.
        var point = source.inverse * SIMD4<Float>(Float(sndc.x), Float(sndc.y), Float(sndc.z), 1)
        point /= point.w
        let current = display * (relative * point)
        guard current.w > 0 else { return nil }
        return Chain(ndc: SIMD2(current.x, current.y) / current.w, relative: relative)
    }

    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass captured 288-byte physical request and Rust baseline fixture")
        let packet = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        precondition(packet.count == 288)
        func matrix(_ offset: Int) -> simd_float4x4 {
            let v = (0..<16).map { index in Float(bitPattern: packet.withUnsafeBytes {
                $0.loadUnaligned(fromByteOffset: offset + index * 4, as: UInt32.self) }) }
            return .init(columns: (SIMD4(v[0], v[1], v[2], v[3]), SIMD4(v[4], v[5], v[6], v[7]),
                SIMD4(v[8], v[9], v[10], v[11]), SIMD4(v[12], v[13], v[14], v[15])))
        }
        let eyes = [matrix(32), matrix(160)], physical = [matrix(96), matrix(224)]
        let ipd = simd_distance(SIMD3<Double>(Double(eyes[0][3].x), Double(eyes[0][3].y), Double(eyes[0][3].z)),
                                SIMD3<Double>(Double(eyes[1][3].x), Double(eyes[1][3].y), Double(eyes[1][3].z)))
        check(abs(ipd - 0.06798) < 0.0001, "registered captured calibration")
        let mid = SIMD3<Double>((0..<3).map { (Double(eyes[0][3][$0]) + Double(eyes[1][3][$0])) / 2 })

        // Cross-language parity: the native consumed-request baseline equals Swift bit for bit.
        let fixture = try String(contentsOfFile: CommandLine.arguments[2], encoding: .utf8)
        var nativeEyes: [UInt32: (m: UInt32, eyes: [simd_float4x4])] = [:]
        for line in fixture.split(separator: "\n") {
            let words = line.split(separator: " ").map { UInt32($0)! }
            precondition(words.count == 19)
            let v = words[3...].map { Float(bitPattern: $0) }
            let mat = simd_float4x4(columns: (SIMD4(v[0], v[1], v[2], v[3]), SIMD4(v[4], v[5], v[6], v[7]),
                SIMD4(v[8], v[9], v[10], v[11]), SIMD4(v[12], v[13], v[14], v[15])))
            nativeEyes[words[0], default: (words[1], [])].eyes.append(mat)
            check(nativeEyes[words[0]]!.m == words[1] && Int(words[2]) == nativeEyes[words[0]]!.eyes.count - 1, "fixture order")
        }
        check(nativeEyes.count == 4, "four native fixture magnifications")
        for (fovBits, native) in nativeEyes {
            let o = optics(Float(bitPattern: fovBits))
            check(o.magnification.bitPattern == native.m, "native and Swift qualified magnification bits agree")
            let swift = GameWorldScopeOptics.eyeTransforms(eyes, optics: o)!
            for eye in 0..<2 { check(bits(swift[eye]) == bits(native.eyes[eye]), "native ScopedEyeBaseline and Swift eyeTransforms are bit-identical") }
        }

        // Identity optics: byte-exact existing behavior.
        for eye in 0..<2 {
            check(bits(GameWorldScopeOptics.displayProjection(physical[eye], headFromEye: eyes[eye], optics: .identity)!) == bits(physical[eye]),
                  "unscoped display projection is byte exact")
            check(bits(GameWorldScopeOptics.eyeTransforms(eyes, optics: .identity)![eye]) == bits(eyes[eye]), "unscoped eyes byte exact")
        }
        check(bits(GameWorldScopeOptics.eyeTransforms([eyes[0]], optics: optics(5))![0]) == bits(eyes[0]), "one-view drawable keeps its eye")
        var bad = eyes; bad[1][3].x = .nan
        check(GameWorldScopeOptics.eyeTransforms(bad, optics: optics(5)) == nil, "nonfinite eye rejected")

        // Production collimated native graticule center (GameWorldHUDPolicy).
        let half = GameWorldHUDPolicy.halfSize(width: 1920, height: 1080, angularWidth: 60, distance: 2)!
        let layout = GameWorldHUDPolicy.placement(halfSize: half, nativeScope: true)
        let graticule = (0..<2).map { eye -> SIMD2<Double> in
            let clip = GameWorldHUDPolicy.clipFromHead(headFromEye: eyes[eye], projection: physical[eye],
                placementY: layout.y, nativeScope: true) * SIMD4<Float>(0, 0, -2, 1)
            return angles(perceived(SIMD2(clip.x, clip.y) / clip.w, physical: physical[eye], eye: eyes[eye]))
        }
        check(simd_length(graticule[0] - graticule[1]) < 0.001, "collimated graticule common to both eyes")

        let guardScale: Float = 1.3
        var lines: [String] = []
        var worstNew = 0.0, worstOldHorizontal = 0.0, worstOldVertical = 0.0
        for fov: Float in [45, 15, 7.5, 5] {
            let o = optics(fov), m = Double(o.magnification)
            let newEyes = GameWorldScopeOptics.eyeTransforms(eyes, optics: o)!
            let sources = (0..<2).map { eye -> simd_float4x4 in
                let request = GameWorldSourceGuardBand.projection(physical[eye], scale: guardScale)!
                return GameWorldScopeOptics.expectedSourceProjection(request, optics: o, depthReversed: true)!
            }
            let oldDisplay = (0..<2).map { GameWorldScopeOptics.projection(physical[$0], optics: o)! }
            let newDisplay = (0..<2).map { GameWorldScopeOptics.displayProjection(physical[$0], headFromEye: eyes[$0], optics: o)! }
            for eye in 0..<2 {
                // Z/W rows unchanged: native depth direction/range is untouched.
                for c in 0..<4 { check(newDisplay[eye][c][2].bitPattern == physical[eye][c][2].bitPattern
                    && newDisplay[eye][c][3].bitPattern == physical[eye][c][3].bitPattern, "display Z/W unchanged") }
                // Every display corner remains inside the guard-banded scoped source.
                for x: Float in [-1, 1] { for y: Float in [-1, 1] {
                    let ray = d(newDisplay[eye]).inverse * SIMD4<Double>(Double(x), Double(y), 0.5, 1)
                    let worldPoint = d(newEyes[eye]) * SIMD4(ray.x / ray.w * 50, ray.y / ray.w * 50, ray.z / ray.w * 50, 1)
                    let s = d(sources[eye]) * (d(newEyes[eye]).inverse * worldPoint)
                    check(abs(s.x / s.w) < 1 && abs(s.y / s.w) < 1, "scoped display corner lies inside guard-banded source")
                } }
            }
            // The captured lens reaches 1km; 900m approximates the collimated limit.
            for distance in [10.0, 20, 50, 100, 300, 900] {
                for offset in [SIMD2<Double>(0, 0), SIMD2(0.6, 0), SIMD2(-0.6, 0.4), SIMD2(0, -0.8)] {
                    // True angular offset from the head axis, inside the magnified field.
                    let tangent = offset * .pi / 180
                    guard m * simd_length(tangent) < 0.12 else { continue }
                    let world = mid + SIMD3(tangent.x, tangent.y, -1) * distance
                    var natural: [SIMD2<Double>] = [], before: [SIMD2<Double>] = [], after: [SIMD2<Double>] = []
                    for eye in 0..<2 {
                        let unmagnified = GameWorldScopeOptics.expectedSourceProjection(
                            GameWorldSourceGuardBand.projection(physical[eye], scale: guardScale)!, optics: .identity, depthReversed: true)!
                        guard let n = chain(world: world, sourceEye: eyes[eye], source: unmagnified, displayEye: eyes[eye], display: physical[eye]),
                              let b = chain(world: world, sourceEye: eyes[eye], source: sources[eye], displayEye: eyes[eye], display: oldDisplay[eye]),
                              let a = chain(world: world, sourceEye: newEyes[eye], source: sources[eye], displayEye: newEyes[eye], display: newDisplay[eye])
                        else { preconditionFailure("test point must remain inside both source and display") }
                        // Source and display share the scoped virtual eye: reprojection is rigid identity.
                        check((0..<4).allSatisfy { c in (0..<4).allSatisfy { abs(a.relative[c][$0] - (c == $0 ? 1 : 0)) < 1e-6 } },
                              "scoped source/display eye agree")
                        natural.append(angles(perceived(n.ndc, physical: physical[eye], eye: eyes[eye])))
                        before.append(angles(perceived(b.ndc, physical: physical[eye], eye: eyes[eye])))
                        after.append(angles(perceived(a.ndc, physical: physical[eye], eye: eyes[eye])))
                    }
                    // Disparity relative to the common collimated graticule.
                    let g = graticule[0] - graticule[1]
                    let naturalDisparity = natural[0] - natural[1] - g
                    let oldDisparity = before[0] - before[1] - g
                    let newDisparity = after[0] - after[1] - g
                    let newError = simd_length(newDisparity - naturalDisparity)
                    worstNew = max(worstNew, newError)
                    worstOldHorizontal = max(worstOldHorizontal, abs(oldDisparity.x - naturalDisparity.x))
                    worstOldVertical = max(worstOldVertical, abs(oldDisparity.y - naturalDisparity.y))
                    check(newError < 0.02, "scoped world keeps natural binocular disparity against the graticule")
                    // The scope still magnifies: average perceived offset from the graticule is m-times in tangent.
                    let center = (graticule[0] + graticule[1]) / 2
                    let shown = (after[0] + after[1]) / 2 - center
                    let expected = SIMD2(atan(m * tangent.x), atan(m * tangent.y)) * 180 / .pi
                    if distance >= 900 { check(simd_length(shown - expected) < 0.02, "scoped view retains native magnification about the graticule") }
                    if offset == .zero && distance >= 900 {
                        for eye in 0..<2 { check(simd_length(after[eye] - graticule[eye]) < 0.01, "aim axis stays under the graticule at every zoom") }
                    }
                    if offset == .zero {
                        lines.append(String(format: "fov %4.1f m %.4f D %7.0fm natural %.3f/%.3f deg | before %.3f/%.3f | after %.3f/%.3f",
                            fov, m, distance, naturalDisparity.x, naturalDisparity.y, oldDisparity.x, oldDisparity.y, newDisparity.x, newDisparity.y))
                    }
                }
            }
        }
        // The previous display path reproduces the reported scale of doubling.
        check(worstOldHorizontal > 1.5, "old full-baseline magnified world exceeded 1.5 degrees against the graticule")
        check(worstOldVertical > 0.3, "old own-axis magnification added >0.3 degree vertical misregistration")
        for line in lines { print(line) }
        print(String(format: "PASS: %d scoped binocular assertions with captured IPD %.5fm. Worst old excess vs natural: horizontal %.3f deg, vertical %.3f deg; worst new excess %.4f deg. CPU geometry only; no GPU, game or headset acceptance.",
            checks, ipd, worstOldHorizontal, worstOldVertical, worstNew))
    }
}

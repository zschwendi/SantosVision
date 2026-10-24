import Foundation
import simd

#if HUD_ACTUAL_UNIFORM_LAYOUT
// This validation is concatenated after the unmodified production HUD source.
// Same-file access checks its actual private uniform type, not a copied mirror.
extension GameWorldHUD {
    static func validationUniformLayout() -> [Int] {
        [MemoryLayout<Uniforms>.size, MemoryLayout<Uniforms>.stride, MemoryLayout<Uniforms>.alignment,
         MemoryLayout<Uniforms>.offset(of: \.clipFromHead)!, MemoryLayout<Uniforms>.offset(of: \.halfSize)!,
         MemoryLayout<Uniforms>.offset(of: \.distance)!, MemoryLayout<Uniforms>.offset(of: \.decodeForLinearTarget)!]
    }
}
#endif

@main
struct HUDPolicyValidation {
    static func main() throws {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        typealias Candidate = GameWorldHUDPolicy.Candidate
        let left = Candidate(lease: 71, epoch: 4, gameFrame: 100, sourceEye: 0)
        let right = Candidate(lease: 72, epoch: 4, gameFrame: 101, sourceEye: 1)
        require(GameWorldHUDPolicy.selectedIndex([left, right], viewCount: 2) == 1,
                "The latest logical HUD must be selected once for both physical eyes")
        let newerLeft = Candidate(lease: 73, epoch: 4, gameFrame: 102, sourceEye: 0)
        require(GameWorldHUDPolicy.selectedIndex([newerLeft, right], viewCount: 2) == 0,
                "The selected canvas must follow freshness, not always choose one eye")
        require(GameWorldHUDPolicy.selectedIndex([left], viewCount: 1) == 0, "Qualified mono source is valid")
        let fallbacks: [([Candidate?], Int)] = [
            ([], 0), ([], 2), ([left], 2), ([left, right], 1), ([left, right, nil], 3),
            ([nil, right], 2), ([left, nil], 2),
            ([Candidate(lease: 0, epoch: 4, gameFrame: 100, sourceEye: 0), right], 2),
            ([left, Candidate(lease: 0, epoch: 4, gameFrame: 101, sourceEye: 1)], 2),
            ([Candidate(lease: 71, epoch: 0, gameFrame: 100, sourceEye: 0), right], 2),
            ([left, Candidate(lease: 72, epoch: 5, gameFrame: 101, sourceEye: 1)], 2),
            ([right, left], 2),
            ([left, Candidate(lease: 72, epoch: 4, gameFrame: 101, sourceEye: 0)], 2)
        ]
        for (values, count) in fallbacks {
            require(GameWorldHUDPolicy.selectedIndex(values, viewCount: count) == nil,
                    "Partial/mismatched decomposition must fall back to both original full game colors")
        }
        // Model the exact consumer decision: if either eye is unqualified, neither
        // eye can swap its full color for a decomposed world-only texture.
        let fullColors = ["left original", "right original"]
        let worldColors = ["left decomposed", "right decomposed"]
        func colors(_ candidates: [Candidate?]) -> [String] {
            GameWorldHUDPolicy.selectedIndex(candidates, viewCount: 2) == nil ? fullColors : worldColors
        }
        require(colors([left, nil]) == fullColors && colors([nil, right]) == fullColors,
                "Fallback cannot leave one physical eye missing the HUD")
        require(colors([left, right]) == worldColors, "Qualified pair uses both world-only images")

        let half = GameWorldHUDPolicy.halfSize(width: 1920, height: 1080, angularWidth: 60, distance: 2)!
        let safeHalf = half * GameWorldHUDPolicy.immersiveScale
        let safeY = half.y * GameWorldHUDPolicy.immersiveVerticalFraction
        let scope = GameWorldHUDPolicy.placement(halfSize: half, nativeScope: true)
        let ordinary = GameWorldHUDPolicy.placement(halfSize: half, nativeScope: false)
        require(scope.halfSize == half && scope.y == 0,
                "Native scope crosshair stays centered and uses its original canvas extent")
        require(ordinary.halfSize == safeHalf && ordinary.y == safeY,
                "Ordinary HUD retains the lower inset safe area")
        require(-safeHalf.x > -half.x && safeHalf.x < half.x,
                "HUD safe area moves minimap right and money/ammo left, never moves minimap farther left")
        require(safeY + safeHalf.y < half.y && safeY - safeHalf.y < -half.y,
                "Both top-right HUD and minimap move lower")
        require(abs(half.x - 2 * tan(Float.pi / 6)) < 1e-6, "Configured HUD horizontal angle is 60 degrees")
        require(abs(half.y / half.x - 9.0 / 16.0) < 1e-6, "HUD preserves actual source aspect ratio")
        let scaled = GameWorldHUDPolicy.halfSize(width: 3840, height: 2160, angularWidth: 60, distance: 4)!
        require(simd_length(scaled - half * 2) < 1e-6, "Angular safe area must not depend on texture resolution or distance")
        let invalid: [(Int, Int, Float, Float)] = [
            (0, 1080, 60, 2), (1, 1080, 60, 2), (1920, 1, 60, 2),
            (16385, 1080, 60, 2), (1920, 16385, 60, 2),
            (1920, 1080, .nan, 2), (1920, 1080, .infinity, 2),
            (1920, 1080, 19.9, 2), (1920, 1080, 90.1, 2),
            (1920, 1080, 60, .nan), (1920, 1080, 60, .infinity),
            (1920, 1080, 60, 0.499), (1920, 1080, 60, 10.001)
        ]
        for (width, height, angle, distance) in invalid {
            require(GameWorldHUDPolicy.halfSize(width: width, height: height, angularWidth: angle, distance: distance) == nil,
                    "Invalid or unbounded finite HUD geometry must reject")
        }
        for angle: Float in [20, 60, 90] {
            for distance: Float in [0.5, 2, 10] {
                require(GameWorldHUDPolicy.halfSize(width: 16384, height: 16384, angularWidth: angle, distance: distance) != nil,
                        "Documented finite policy boundaries are inclusive")
            }
        }

        // Inspect the actual captured physical eye matrices, including XROS
        // calibration residuals. No symmetric-projection replacement is used.
        guard CommandLine.arguments.count == 2 else {
            fatalError("Pass the captured 288-byte two-eye physical view request")
        }
        let input = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        require(input.count == 288, "Physical view request ABI must match captured fixture")
        func matrix(at offset: Int) -> simd_float4x4 {
            let values = (0..<16).map { index in
                Float(bitPattern: input.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset + index * 4, as: UInt32.self) })
            }
            return simd_float4x4(columns: (SIMD4(values[0], values[1], values[2], values[3]),
                SIMD4(values[4], values[5], values[6], values[7]), SIMD4(values[8], values[9], values[10], values[11]),
                SIMD4(values[12], values[13], values[14], values[15])))
        }
        let heads = [matrix(at: 32), matrix(at: 160)]
        let projections = [matrix(at: 96), matrix(at: 224)]
        let measuredIPD = abs(heads[1][3].x - heads[0][3].x)
        require((0.05...0.085).contains(measuredIPD), "Fixture contains real separated physical eyes")

        // Scope graticules are angular, unlike the ordinary finite HUD. Check
        // the actual captured calibrated eye matrices, not equal NDC positions
        // (which would be wrong for asymmetric physical projections).
        var scopeRecovered = [[SIMD3<Float>]](repeating: [], count: 2)
        var oldScopeRecovered = [[SIMD3<Float>]](repeating: [], count: 2)
        for eye in 0..<2 {
            let ordinaryClip = GameWorldHUDPolicy.clipFromHead(headFromEye: heads[eye],
                projection: projections[eye], placementY: safeY, nativeScope: false)
            var ordinaryPlacement = matrix_identity_float4x4
            ordinaryPlacement[3].y = safeY
            let originalOrdinaryClip = projections[eye] * heads[eye].inverse * ordinaryPlacement
            require(ordinaryClip == originalOrdinaryClip, "Ordinary HUD matrix retains exact existing finite placement")
            let scopeClip = GameWorldHUDPolicy.clipFromHead(headFromEye: heads[eye],
                projection: projections[eye], placementY: 0, nativeScope: true)
            var rotation = heads[eye]
            rotation[3] = SIMD4(0, 0, 0, 1)
            for x: Float in [-0.8, 0, 0.8] { for y: Float in [-0.8, 0, 0.8] {
                let point = SIMD4<Float>(x * half.x, y * half.y, -2, 1)
                let scoped = scopeClip * point
                let recoveredEye = projections[eye].inverse * scoped
                let recoveredHead = rotation * SIMD4<Float>(recoveredEye.x, recoveredEye.y, recoveredEye.z, 0)
                let direction = simd_normalize(SIMD3(recoveredHead.x, recoveredHead.y, recoveredHead.z))
                let expected = simd_normalize(SIMD3(point.x, point.y, point.z))
                require(simd_length(direction - expected) < 2e-6,
                        "Every native scope pixel represents the same head-space angular direction in either calibrated eye")
                scopeRecovered[eye].append(direction)
                let oldClip = projections[eye] * heads[eye].inverse * point
                let oldEye = projections[eye].inverse * oldClip
                let oldHead = rotation * SIMD4<Float>(oldEye.x, oldEye.y, oldEye.z, 0)
                oldScopeRecovered[eye].append(simd_normalize(SIMD3(oldHead.x, oldHead.y, oldHead.z)))
            } }
            for distance: Float in [0.5, 2, 10] {
                var moved = heads[eye]
                moved[3] = SIMD4(eye == 0 ? -distance : distance, distance * 0.1, -distance * 0.2, 1)
                require(GameWorldHUDPolicy.clipFromHead(headFromEye: moved, projection: projections[eye],
                    placementY: 0, nativeScope: true) == scopeClip,
                    "Scope collimation is independent of eye-origin translations without replacing calibrated rotation")
            }
        }
        for index in 0..<9 {
            require(simd_length(scopeRecovered[0][index] - scopeRecovered[1][index]) < 3e-6,
                    "Scope graticule has no artificial finite-plane binocular disparity")
        }
        require(simd_length(oldScopeRecovered[0][4] - oldScopeRecovered[1][4]) > 0.02,
                "Regression fixture exposes the old 2 m scope convergence, rather than silently passing the previous implementation")
        let positions: [SIMD4<Float>] = [-1.0, 0, 1].flatMap { x in
            [-1.0, 0, 1].map { y in SIMD4(Float(x) * safeHalf.x, safeY + Float(y) * safeHalf.y, -2, 1) }
        }
        var maxHorizontal: Float = 0, maxVertical: Float = 0
        for eye in 0..<2 {
            for point in positions {
                let clip = projections[eye] * heads[eye].inverse * point
                let ndc = clip / clip.w
                require(clip.w.isFinite && clip.w > 0, "Finite HUD remains in front of each physical eye")
                require(abs(ndc.x) < 1 && abs(ndc.y) < 1 && (0...1).contains(ndc.z),
                        "The complete 60-degree HUD safe area is inside the real physical frustum")
                maxHorizontal = max(maxHorizontal, abs(ndc.x)); maxVertical = max(maxVertical, abs(ndc.y))
                let reconstructed = heads[eye] * projections[eye].inverse * clip
                require(simd_length(reconstructed - point) < 3e-5,
                        "Both eye projections reconstruct the identical finite head-space canvas")
                for yaw: Float in [-3.0, -1.0, 0, 1.0, 3.0] {
                    let q = simd_quatf(angle: yaw, axis: SIMD3(0, 1, 0))
                    var stageFromHead = simd_float4x4(q)
                    stageFromHead[3] = SIMD4(4, 1.8, -7, 1)
                    let stageFromEye = stageFromHead * heads[eye]
                    let independentClip = projections[eye] * stageFromEye.inverse * (stageFromHead * point)
                    let independentNDC = independentClip / independentClip.w
                    require(simd_length(independentNDC - ndc) < 2e-5,
                            "Current head transforms cancel exactly; this canvas never uses a stale source head pose")
                }
            }
        }
        // Independently check convergence sign and finite-IPD scaling, rather
        // than requiring equal screen X coordinates in asymmetric eye frusta.
        let focal: Float = 0.721
        let canonical = simd_float4x4(columns: (SIMD4(focal, 0, 0, 0), SIMD4(0, 0.9, 0, 0),
            SIMD4(0, 0, 0.0001, -1), SIMD4(0, 0, 0.1, 0)))
        for ipd: Float in [0.05, 0.064, 0.075, 0.085] {
            var l = matrix_identity_float4x4, r = matrix_identity_float4x4
            l[3].x = -ipd / 2; r[3].x = ipd / 2
            let center = SIMD4<Float>(0, 0, -2, 1)
            let lc = canonical * l.inverse * center, rc = canonical * r.inverse * center
            require(abs((lc.x / lc.w - rc.x / rc.w) - focal * ipd / 2) < 1e-6,
                    "Finite canvas disparity has correct eye sign and IPD/distance scaling")
        }

        #if HUD_BRIDGE_ABI_TEST
        require(MemoryLayout<SVWorldHudFrame>.size == 104 && MemoryLayout<SVWorldHudFrame>.stride == 104,
                "Actual imported C HUD ABI is 104 bytes")
        require(MemoryLayout<SVWorldHudFrame>.offset(of: \.lease) == 8
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.provenance_epoch) == 16
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.color_texture) == 48
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.world_color_texture) == 56
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.presentation_time) == 64
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.source_eye) == 80
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.angular_width_degrees) == 88
            && MemoryLayout<SVWorldHudFrame>.offset(of: \.distance_metres) == 92,
                "Actual imported C fields retain their documented native offsets")
        #endif
        #if HUD_ACTUAL_UNIFORM_LAYOUT
        require(GameWorldHUD.validationUniformLayout() == [80, 80, 16, 0, 64, 72, 76],
                "Actual private production Swift uniforms match the Metal 80-byte field layout")
        #endif
        print("PASS HUD policy/projection: \(checks) checks; actual physical IPD \(measuredIPD)m; max HUD |NDC|=\(maxHorizontal),\(maxVertical)")
    }
}

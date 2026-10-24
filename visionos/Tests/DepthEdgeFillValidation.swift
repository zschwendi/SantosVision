// Run scripts/test-depth-edge-fill.sh. CPU only: production uniform/policy
// Swift plus DepthEdgeFillMirror.swift, the CPU mirror of the Metal walk.
// GPU parity (scripts/test-depth-edge-fill-metal.sh) and physical
// visibility are separate root acceptance steps.
import Foundation
import simd

var checks = 0
func check(_ condition: @autoclosure () -> Bool, _ message: @autoclosure () -> String) {
    if !condition() { fatalError("FAIL: " + message()) }
    checks += 1
}

// ---- Analytic scenes with exact ground truth ----
func perspective(_ a: Float, _ b: Float, _ cx: Float, _ cy: Float, near: Float, far: Float, reversed: Bool) -> simd_float4x4 {
    // right-up-back eye space; reversed maps near->1, far->0.
    let (zz, zw): (Float, Float) = reversed ? (near / (far - near), far * near / (far - near))
                                            : (-far / (far - near), -far * near / (far - near))
    return simd_float4x4(columns: (SIMD4(a, 0, 0, 0), SIMD4(0, b, 0, 0), SIMD4(cx, cy, zz, -1), SIMD4(0, 0, zw, 0)))
}
// Registered AVP left-eye physical projection (candidate7 normal capture).
let physical = perspective(0.7319810390472412, 0.9124256372451782, -0.2679583728313446, -0.08748899400234222,
                           near: 0.1, far: 1000, reversed: true)
func guarded(_ p: simd_float4x4, _ scale: Float) -> simd_float4x4 {
    var m = p
    for c in 0..<4 { m[c][0] /= scale; m[c][1] /= scale }
    return m
}

enum Surface: Equatable { case foreground, background, sky }
struct Scene {
    var foregroundZ: Float = -1.2
    var foregroundX: ClosedRange<Float> = -0.30...0.05
    var foregroundY: ClosedRange<Float> = -0.40...0.30
    var backgroundZ: Float = -6
    var backgroundTop: Float = 3          // above this the world is sky
    func backgroundColor(_ p: SIMD3<Float>) -> SIMD3<Float> {
        SIMD3(0.5 + 0.4 * sin(p.x * 1.3), 0.5 + 0.4 * cos(p.y * 1.7), 0.3)
    }
    static let foregroundColor = SIMD3<Float>(1, 0, 1)
    static let skyColor = SIMD3<Float>(0.2, 0.6, 0.9)
    /// First surface along origin + t * direction (source/world coordinates).
    func trace(_ origin: SIMD3<Float>, _ direction: SIMD3<Float>) -> (Surface, SIMD3<Float>) {
        if direction.z < 0 {
            let tf = (foregroundZ - origin.z) / direction.z
            let pf = origin + tf * direction
            if tf > 0, foregroundX.contains(pf.x), foregroundY.contains(pf.y) { return (.foreground, pf) }
            let tb = (backgroundZ - origin.z) / direction.z
            let pb = origin + tb * direction
            if tb > 0, pb.y <= backgroundTop { return (.background, pb) }
        }
        return (.sky, origin + 1e6 * direction)
    }
    func color(_ surface: Surface, _ p: SIMD3<Float>) -> SIMD3<Float> {
        switch surface {
        case .foreground: Self.foregroundColor
        case .background: backgroundColor(p)
        case .sky: Self.skyColor
        }
    }
}

struct SourceImage {
    let width: Int, height: Int
    var depth: [Float]
    var color: [SIMD3<Float>]
    var surface: [Surface]
    func depthAt(_ t: SIMD2<UInt32>) -> Float { depth[Int(t.y) * width + Int(t.x)] }
}
func renderSource(_ scene: Scene, projection: simd_float4x4, width: Int, height: Int, reversed: Bool) -> SourceImage {
    let inverse = projection.inverse
    var image = SourceImage(width: width, height: height, depth: [], color: [], surface: [])
    for y in 0..<height { for x in 0..<width {
        let ndc = SIMD2((Float(x) + 0.5) / Float(width) * 2 - 1, 1 - (Float(y) + 0.5) / Float(height) * 2)
        let e = inverse * SIMD4(ndc.x, ndc.y, 0.5, 1)
        let direction = simd_normalize(SIMD3(e.x, e.y, e.z) / e.w)
        let (surface, p) = scene.trace(.zero, direction)
        var z: Float = reversed ? 0 : 1
        if surface != .sky { let c = projection * SIMD4(p, 1); z = c.z / c.w }
        image.depth.append(z); image.color.append(scene.color(surface, p)); image.surface.append(surface)
    }}
    return image
}

struct Outcome {
    var holesInside = 0, holesFilled = 0, holesWrongForeground = 0, holesOutsideFilled = 0
    var maximumColorError: Float = 0, maximumDepthDistanceError: Float = 0, colorErrorBound: Float = 0
    var visibleChecked = 0, visibleAgree = 0
    var skyFills = 0, maximumSkyRadiusError: Float = 0
    var holesOtherSurface = 0, holesOtherSurfaceBeyondOneTexel = 0, holesSubTexelForeground = 0
}
/// Every display pixel: ground truth from the current eye, owner-0 emulated as
/// "visible surface absent from the source image" (occluded or off-image).
func run(_ scene: Scene, current sourceFromCurrent: simd_float4x4, sourceProjection: simd_float4x4,
         reversed: Bool, sourceSize: SIMD2<Int>, display: SIMD2<Int>, steps: UInt32 = GameWorldDepthEdgeFillPolicy.maximumSteps,
         displayProjection: simd_float4x4 = physical) -> Outcome {
    let image = renderSource(scene, projection: sourceProjection, width: sourceSize.x, height: sourceSize.y, reversed: reversed)
    var u = GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection: sourceProjection,
        currentEyeFromSourceEye: sourceFromCurrent.inverse, displayProjection: displayProjection,
        viewportOrigin: .zero, viewportSize: SIMD2(Float(display.x), Float(display.y)),
        sourceSize: SIMD2(UInt32(sourceSize.x), UInt32(sourceSize.y)), sourceDepthReversed: reversed ? 1 : 0,
        colorTransfer: 0, nearestDistance: 0.10, skyRadius: 800)!
    u.maximumSteps = steps
    let displayInverse = displayProjection.inverse
    let origin = SIMD3(sourceFromCurrent.columns.3.x, sourceFromCurrent.columns.3.y, sourceFromCurrent.columns.3.z)
    var out = Outcome()
    // Adjacent background differs from the hidden point by at most the hidden
    // width plus two source texels at the background, times the colour slope.
    let hidden = simd_length(origin) * (scene.backgroundZ / scene.foregroundZ - 1)
    let texelAtBackground = -scene.backgroundZ * 2 / (sourceProjection[0][0] * Float(sourceSize.x))
    out.colorErrorBound = Float(sqrt(0.52 * 0.52 + 0.68 * 0.68)) * (hidden + 2 * texelAtBackground) + 1e-4
    func near(_ texel: SIMD2<UInt32>, _ surface: Surface) -> Bool {
        for dy in -1...1 { for dx in -1...1 {
            let x = Int(texel.x) + dx, y = Int(texel.y) + dy
            if x >= 0, y >= 0, x < image.width, y < image.height, image.surface[y * image.width + x] == surface { return true }
        }}
        return false
    }
    for y in 0..<display.y { for x in 0..<display.x {
        let ndc = SIMD2((Float(x) + 0.5) / Float(display.x) * 2 - 1, 1 - (Float(y) + 0.5) / Float(display.y) * 2)
        let e = displayInverse * SIMD4(ndc.x, ndc.y, 0.5, 1)
        let local = simd_normalize(SIMD3(e.x, e.y, e.z) / e.w)
        let world4 = sourceFromCurrent * SIMD4(local, 0)
        let direction = simd_normalize(SIMD3(world4.x, world4.y, world4.z))
        let (surface, p) = scene.trace(origin, direction)
        // Is the true surface visible in the source image?
        let clip = sourceProjection * SIMD4(surface == .sky ? direction : p, surface == .sky ? 0 : 1)
        let inside = clip.w > 0 && abs(clip.x) <= clip.w && abs(clip.y) <= clip.w
        var seen = inside
        if inside, surface != .sky { seen = scene.trace(.zero, simd_normalize(p)).0 == surface }
        if inside, surface == .sky { seen = scene.trace(.zero, direction).0 == .sky }
        let result = fill(pixel: SIMD2(x, y), u, image.depthAt)
        if !seen {
            // Margin of one source texel from the image edge keeps the off-image class unambiguous.
            let margin = 2 / Float(min(sourceSize.x, sourceSize.y))
            let deep = inside && abs(clip.x) <= clip.w * (1 - margin) && abs(clip.y) <= clip.w * (1 - margin)
            if !inside, abs(clip.x) > clip.w * (1 + margin) || abs(clip.y) > clip.w * (1 + margin) || clip.w <= 0 {
                if result != nil { out.holesOutsideFilled += 1 }
            }
            guard deep else { continue }
            out.holesInside += 1
            guard let result else { continue }
            out.holesFilled += 1
            let index = Int(result.texel.y) * image.width + Int(result.texel.x)
            let filledSurface = image.surface[index]
            // The source cannot place an edge inside one texel: gaps whose true
            // point is within a texel of the occluder may resolve to either side.
            let truthClip = sourceProjection * SIMD4(surface == .sky ? direction : p, surface == .sky ? 0 : 1)
            let truthTexel = SIMD2<UInt32>(UInt32(max(0, min(Float(sourceSize.x - 1), (truthClip.x / truthClip.w * 0.5 + 0.5) * Float(sourceSize.x)))),
                                           UInt32(max(0, min(Float(sourceSize.y - 1), (0.5 - truthClip.y / truthClip.w * 0.5) * Float(sourceSize.y)))))
            if filledSurface == .foreground, surface != .foreground {
                if near(truthTexel, .foreground) { out.holesSubTexelForeground += 1 } else { out.holesWrongForeground += 1 }
            }
            if result.sky {
                out.skyFills += 1
                let c = displayInverse * SIMD4(ndc.x, ndc.y, result.depth, 1)
                out.maximumSkyRadiusError = max(out.maximumSkyRadiusError, abs(simd_length(SIMD3(c.x, c.y, c.z) / c.w) - 800))
            }
            // A gap thinner than one source texel at a background/sky edge can
            // only resolve to the texel across that edge.
            guard filledSurface == surface else {
                out.holesOtherSurface += 1
                if !near(result.texel, surface) { out.holesOtherSurfaceBeyondOneTexel += 1 }
                continue
            }
            out.maximumColorError = max(out.maximumColorError, simd_length(image.color[index] - scene.color(surface, p)))
            if surface == .background {
                let c = displayInverse * SIMD4(ndc.x, ndc.y, result.depth, 1)
                let distance = simd_length(SIMD3(c.x, c.y, c.z) / c.w)
                out.maximumDepthDistanceError = max(out.maximumDepthDistanceError, abs(distance - simd_distance(p, origin)) / simd_distance(p, origin))
            }
        } else if surface != .sky {
            // The production pass never runs here (the mesh owns these pixels),
            // but the same walk must recover the visible source surface.
            out.visibleChecked += 1
            if let result, near(result.texel, surface) { out.visibleAgree += 1 }
        }
    }}
    return out
}

func pose(_ translation: SIMD3<Float>, yawDegrees: Float, pitchDegrees: Float = 0) -> simd_float4x4 {
    var m = simd_float4x4(simd_quatf(angle: yawDegrees * .pi / 180, axis: SIMD3(0, 1, 0))
                          * simd_quatf(angle: pitchDegrees * .pi / 180, axis: SIMD3(1, 0, 0)))
    m.columns.3 = SIMD4(translation, 1)
    return m
}

// ABI must match GameWorld.metal (static_asserts in scripts/test-depth-edge-fill.sh).
check(MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride == 240, "fill uniform stride")
check(MemoryLayout<GameWorldDepthEdgeFillUniforms>.offset(of: \.viewportOrigin) == 192, "viewport offset")
check(MemoryLayout<GameWorldDepthEdgeFillUniforms>.offset(of: \.sourceSize) == 208, "source size offset")
check(MemoryLayout<GameWorldDepthEdgeFillUniforms>.offset(of: \.maximumInverseDistance) == 224, "distance offset")
check(MemoryLayout<GameWorldDepthEdgeFillUniforms>.offset(of: \.maximumSteps) == 232, "steps offset")

// Launch policy: off unless stored/selected; explicit QA values win.
let policy = GameWorldDepthEdgeFillPolicy.self
check(!policy.requested(environment: [:], arguments: [], stored: false), "default must stay off")
check(policy.requested(environment: [:], arguments: [], stored: true), "stored launcher choice")
check(policy.requested(environment: [:], arguments: ["--sv-world-depth-edge-fill"], stored: false), "QA enable argument")
check(!policy.requested(environment: ["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL": "0"], arguments: ["--sv-world-depth-edge-fill"], stored: true), "QA env disable")
check(policy.requested(environment: ["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL": "1"], arguments: [], stored: false), "QA env enable")
check(!policy.requested(environment: ["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL": "1"], arguments: ["--sv-world-disable-depth-edge-fill"], stored: true), "disable argument always wins")
check(policy.uniforms(sourceProjection: physical, currentEyeFromSourceEye: matrix_identity_float4x4, displayProjection: physical,
    viewportOrigin: .zero, viewportSize: SIMD2(0, 10), sourceSize: SIMD2(4, 4), sourceDepthReversed: 1, colorTransfer: 0,
    nearestDistance: 0.1, skyRadius: 800) == nil, "invalid viewport rejected")
check(policy.uniforms(sourceProjection: physical, currentEyeFromSourceEye: simd_float4x4(diagonal: SIMD4<Float>(repeating: 0)), displayProjection: physical,
    viewportOrigin: .zero, viewportSize: SIMD2(10, 10), sourceSize: SIMD2(4, 4), sourceDepthReversed: 1, colorTransfer: 0,
    nearestDistance: 0.1, skyRadius: 800) == nil, "singular relative eye rejected")

var results: [String: Any] = [:]
func record(_ name: String, _ o: Outcome) {
    results[name] = ["holes_inside_source": o.holesInside, "holes_filled": o.holesFilled,
        "holes_filled_with_foreground": o.holesWrongForeground,
        "sub_texel_edge_holes_filled_with_foreground": o.holesSubTexelForeground, "off_image_holes_filled": o.holesOutsideFilled,
        "max_color_error": o.maximumColorError, "color_error_bound": o.colorErrorBound,
        "max_relative_background_distance_error": o.maximumDepthDistanceError,
        "sky_fills": o.skyFills, "max_sky_radius_error_m": o.maximumSkyRadiusError,
        "holes_filled_across_sub_texel_edge": o.holesOtherSurface,
        "holes_filled_beyond_one_texel_of_truth": o.holesOtherSurfaceBeyondOneTexel,
        "visible_pixels_checked": o.visibleChecked, "visible_pixels_walk_agrees": o.visibleAgree]
}
let source = guarded(physical, 1.3)
let sourceSize = SIMD2(320, 180), display = SIMD2(300, 285)
let scene = Scene()

// 1. Head translation beside a near occluder: the registered failure class.
// The 7mm/2.9-degree pose is the registered capture's relative eye, at half
// the device viewport and half the 1920x1080 source so its narrow gaps resolve.
for (name, translation, yaw, sizes) in [
        ("translate_right_3cm", SIMD3<Float>(0.03, 0.01, 0), Float(2), (sourceSize, display)),
        ("translate_left_3cm", SIMD3<Float>(-0.03, -0.01, 0.01), Float(-2), (sourceSize, display)),
        ("translate_7mm_like_capture", SIMD3<Float>(0.007, 0.0013, -0.0011), Float(2.9), (SIMD2(960, 540), SIMD2(944, 896)))] {
    let o = run(scene, current: pose(translation, yawDegrees: yaw), sourceProjection: source, reversed: true,
                sourceSize: sizes.0, display: sizes.1)
    record(name, o)
    check(o.holesInside > 20, "\(name): fixture must expose disocclusion")
    check(o.holesFilled == o.holesInside, "\(name): every in-image gap must find adjacent background")
    check(o.holesWrongForeground == 0, "\(name): gap filled with the occluder's colour")
    check(o.holesOutsideFilled == 0, "\(name): off-image border must stay untouched")
    check(o.holesOtherSurfaceBeyondOneTexel == 0, "\(name): fill crossed more than one source texel from the true surface")
    // One source texel of background gradient, plus the hidden width itself.
    check(o.maximumColorError <= o.colorErrorBound, "\(name): fill colour is not the adjacent background (\(o.maximumColorError))")
    check(o.maximumDepthDistanceError < 2e-3, "\(name): extended background depth is wrong (\(o.maximumDepthDistanceError))")
    check(Double(o.visibleAgree) >= 0.995 * Double(o.visibleChecked), "\(name): walk disagrees with visible surfaces")
}

// 2. Sky behind the occluder: sky texel and the finite-sky support sphere.
var skyScene = Scene()
skyScene.backgroundTop = -0.2
let skyOutcome = run(skyScene, current: pose(SIMD3(0.04, 0, 0), yawDegrees: 1), sourceProjection: source,
                     reversed: true, sourceSize: sourceSize, display: display)
record("sky_behind_occluder", skyOutcome)
check(skyOutcome.skyFills > 10 && skyOutcome.holesWrongForeground == 0, "sky gaps must use sky texels")
check(skyOutcome.maximumSkyRadiusError < 1, "sky fill depth must be the 800m support sphere")

// 3. Standard (non-reversed) depth convention.
let standard = guarded(perspective(0.7319810390472412, 0.9124256372451782, -0.2679583728313446, -0.08748899400234222,
                                   near: 0.1, far: 1000, reversed: false), 1.3)
let standardOutcome = run(scene, current: pose(SIMD3(0.03, 0.01, 0), yawDegrees: 2), sourceProjection: standard,
                          reversed: false, sourceSize: sourceSize, display: display,
                          displayProjection: perspective(0.7319810390472412, 0.9124256372451782, -0.2679583728313446,
                                                         -0.08748899400234222, near: 0.1, far: 1000, reversed: false))
record("standard_depth_convention", standardOutcome)
check(standardOutcome.holesFilled == standardOutcome.holesInside && standardOutcome.holesInside > 20
      && standardOutcome.holesWrongForeground == 0, "standard depth convention")

// 4. Gaps wider than the bounded walk stay untouched rather than guessed.
let wide = run(scene, current: pose(SIMD3(0.6, 0, 0), yawDegrees: 0), sourceProjection: source, reversed: true,
               sourceSize: sourceSize, display: display, steps: 4)
record("walk_bound_4_texels_60cm", wide)
check(wide.holesFilled < wide.holesInside && wide.holesWrongForeground == 0, "bounded walk must leave wide gaps untouched")

// 5. Turned past the source frustum: off-image directions are never filled.
let turned = run(scene, current: pose(.zero, yawDegrees: 120), sourceProjection: source, reversed: true,
                 sourceSize: sourceSize, display: display)
record("turned_120_degrees", turned)
check(turned.holesFilled == 0 && turned.holesOutsideFilled == 0, "no source evidence behind the source camera")

// 6. Pure rotation: a crack pixel takes its own texel and exact depth.
let pure = run(scene, current: pose(.zero, yawDegrees: 4, pitchDegrees: 2), sourceProjection: source, reversed: true,
               sourceSize: sourceSize, display: display)
record("pure_rotation", pure)
check(pure.holesInside == 0 && Double(pure.visibleAgree) >= 0.995 * Double(pure.visibleChecked), "rotation-only walk")

// Production density measurement on the registered AVP left-eye projection and
// its actual 1888x1792 viewport. Source and display share the frustum, so the
// enlargement is scale x (display pixels / source pixels) on each axis.
var densityRows: [[String: Any]] = []
for (width, height) in [(1920, 1080), (2560, 1440), (3840, 2160)] {
    for scale: Float in [1.0, 1.3, 1.844, 2.175, 3.0] {
        let d = GameWorldSamplingDensity.measure(eye: 0, displayProjection: physical, displayPixels: SIMD2(1888, 1792),
            sourceProjection: guarded(physical, scale), sourcePixels: SIMD2(width, height))!
        let expected = SIMD2(Double(scale) * 1888 / Double(width), Double(scale) * 1792 / Double(height))
        check(simd_length(d.displayPixelsPerSourceTexel - expected) < 1e-4, "density ratio \(width)x\(height) at \(scale)")
        densityRows.append(["source": [width, height], "guard_scale": scale,
            "display_center_px_per_degree": [d.displayPixelsPerDegree.x, d.displayPixelsPerDegree.y],
            "source_center_px_per_degree": [d.sourcePixelsPerDegree.x, d.sourcePixelsPerDegree.y],
            "display_pixels_per_source_texel": [d.displayPixelsPerSourceTexel.x, d.displayPixelsPerSourceTexel.y]])
    }
}
check(GameWorldSamplingDensity.measure(eye: 0, displayProjection: physical, displayPixels: SIMD2(0, 10),
    sourceProjection: physical, sourcePixels: SIMD2(10, 10)) == nil, "empty viewport has no density")
results["sampling_density_registered_avp_left_eye_1888x1792"] = densityRows

// Optional: replay the registered candidate7 third-person capture's actual
// uncovered output pixels (exact black RGB with cleared depth 0).
if CommandLine.arguments.count >= 3 {
    let prefix = CommandLine.arguments[1], destination = URL(fileURLWithPath: CommandLine.arguments[2])
    let metadata = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: prefix + "-report.json"))) as! [String: Any]
    func matrix(_ key: String) -> simd_float4x4 {
        let v = (metadata[key] as! [NSNumber]).map(\.floatValue)
        return simd_float4x4(columns: (SIMD4(v[0], v[1], v[2], v[3]), SIMD4(v[4], v[5], v[6], v[7]),
                                       SIMD4(v[8], v[9], v[10], v[11]), SIMD4(v[12], v[13], v[14], v[15])))
    }
    func ppm(_ path: String) throws -> (Int, Int, [UInt8]) {
        let data = [UInt8](try Data(contentsOf: URL(fileURLWithPath: path)))
        var at = 0
        func token() -> String {
            while data[at] <= 32 { at += 1 }
            let start = at
            while data[at] > 32 { at += 1 }
            return String(bytes: data[start..<at], encoding: .ascii)!
        }
        precondition(token() == "P6")
        let w = Int(token())!, h = Int(token())!
        precondition(token() == "255")
        at += 1
        return (w, h, Array(data[at..<(at + w * h * 3)]))
    }
    func floats(_ path: String, _ count: Int) throws -> [Float] {
        let bytes = try Data(contentsOf: URL(fileURLWithPath: path))
        precondition(bytes.count == count * 4)
        return bytes.withUnsafeBytes { raw in (0..<count).map {
            Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 4, as: UInt32.self))) } }
    }
    let (sw, sh, sourceRGB) = try ppm(prefix + "-source.ppm")
    let sourceDepth = try floats(prefix + "-source-depth.f32", sw * sh)
    let (ow, oh, outputRGB) = try ppm(prefix + "-output.ppm")
    let outputDepth = try floats(prefix + "-output-depth.f32", ow * oh)
    let viewport = (metadata["viewport"] as! [NSNumber]).map(\.floatValue)
    let u = GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection: matrix("source_projection_from_eye"),
        currentEyeFromSourceEye: matrix("current_eye_from_source_eye"), displayProjection: matrix("current_clip_from_eye"),
        viewportOrigin: SIMD2(viewport[0], viewport[1]), viewportSize: SIMD2(viewport[2], viewport[3]),
        sourceSize: SIMD2(UInt32(sw), UInt32(sh)), sourceDepthReversed: 1, colorTransfer: 0,
        nearestDistance: 0.10, skyRadius: 800)!
    let depthAt: (SIMD2<UInt32>) -> Float = { sourceDepth[Int($0.y) * sw + Int($0.x)] }
    var filled = outputRGB
    var uncovered = 0, finiteFills = 0, skyFills = 0, left = 0
    var roi = (uncovered: 0, filled: 0)
    // The audited third-person body region (body-speckle-audit-20261020).
    let body = (x: 1020..<1320, y: 680..<1490)
    for y in 0..<oh { for x in 0..<ow {
        let i = y * ow + x
        guard outputDepth[i] == 0, outputRGB[i * 3] == 0, outputRGB[i * 3 + 1] == 0, outputRGB[i * 3 + 2] == 0 else { continue }
        uncovered += 1
        let inBody = body.x.contains(x) && body.y.contains(y)
        if inBody { roi.uncovered += 1 }
        guard let result = fill(pixel: SIMD2(x, y), u, depthAt) else { left += 1; continue }
        if result.sky { skyFills += 1 } else { finiteFills += 1 }
        if inBody { roi.filled += 1 }
        let s = (Int(result.texel.y) * sw + Int(result.texel.x)) * 3
        for c in 0..<3 { filled[i * 3 + c] = sourceRGB[s + c] }
    }}
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    var header = Data("P6\n\(ow) \(oh)\n255\n".utf8)
    header.append(contentsOf: filled)
    try header.write(to: destination.appendingPathComponent("normal7-output-depth-edge-filled.ppm"))
    results["registered_candidate7_normal_capture"] = ["uncovered_black_clear_pixels": uncovered,
        "filled_from_finite_background": finiteFills, "filled_from_sky": skyFills, "left_uncovered": left,
        "audited_body_roi_uncovered": roi.uncovered, "audited_body_roi_filled": roi.filled,
        "scope": "CPU mirror applied to the device's actual compositor output; covered pixels are copied unchanged. Not GPU execution, display composition or visual acceptance."]
    check(roi.uncovered == 1557, "registered body ROI must still contain the audited 1557 black gaps")
}

let summary = try JSONSerialization.data(withJSONObject: ["passed": true, "checks": checks, "cases": results],
                                         options: [.prettyPrinted, .sortedKeys])
if CommandLine.arguments.count >= 3 {
    try summary.write(to: URL(fileURLWithPath: CommandLine.arguments[2]).appendingPathComponent("cpu-validation.json"))
}
print(String(data: summary, encoding: .utf8)!)
print("PASS: \(checks) CPU depth-edge fill checks (mirror of the Metal walk; no GPU execution).")

// Standalone actual-GPU harness for the experimental reprojection paths.
//
//   harness selfcheck                                            CPU only: pure helper tests, no Metal device
//   harness parity <candidate.metal> <baseline.metal> <out-dir>  original vs legacy-OFF vs adaptive vs copy, with masks and a plane oracle
//   harness timing <candidate.metal> <baseline.metal> <out-dir>  paired isolated GPU command timings, raw samples retained
//
// "original" is the frozen pre-change shader (compositor-performance-v1/baseline/GameWorld.metal).
// The legacy-OFF path of the candidate must be bit-identical to it. The adaptive and copy
// paths are judged under stated tolerances with explicit written-pixel masks (clear alpha 0,
// shader writes alpha 1) and an independent analytic plane oracle; black is never a mask.
// The AdaptiveScratchPool and pure source checks are the production blocks, compiled verbatim
// (build-harness.sh extracts them). Timings are synthetic command-buffer GPU durations on this
// machine's GPU: not game frame rates, not the visionOS compositor, not a headset.
import CryptoKit
import Foundation
import Metal
import simd

// ---- Mirrors of the production uniform structures ----

struct Uniforms {
    var sourceEyeFromClip: simd_float4x4
    var currentEyeFromSourceEye: simd_float4x4
    var currentClipFromEye: simd_float4x4
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32
    var colorTransfer: UInt32 = 0
    var nearFadeDistances = SIMD2<Float>(0.15, 0.45)
    var reserved = SIMD2<Float>.zero
}

struct AdaptiveParams {
    var tileCount: SIMD2<UInt32>
    var toleranceULPs: UInt32
    var rejectBorderTiles: UInt32
}

struct CopyUniforms {
    var sourceEyeFromClip: simd_float4x4
    var sourceClipFromEye: simd_float4x4
    var currentEyeFromClip: simd_float4x4
    var currentClipFromEye: simd_float4x4
    var currentEyeFromSourceEye: simd_float4x4
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32
    var colorTransfer: UInt32
    var nearFadeDistances: SIMD2<Float>
    var reserved = SIMD2<Float>.zero
}

let nearPlane: Float = 0.05
let farPlane: Float = 1000

/// `aspect` scales the x tangent so a non-square image has square pixels (aspect = height / width).
func projection(_ reversed: Bool, near n: Float = nearPlane, far f: Float = farPlane, aspect: Float = 1) -> simd_float4x4 {
    let a = reversed ? n / (f - n) : f / (n - f)
    let b = reversed ? n * f / (f - n) : n * f / (n - f)
    return simd_float4x4(columns: (SIMD4(aspect, 0, 0, 0), SIMD4(0, 1, 0, 0), SIMD4(0.13, -0.07, a, -1), SIMD4(0, 0, b, 0)))
}

// ---- Predeclared gates (edit here, never through the environment) ----
/// Candidate versus baseline depth on fixtures without an analytic oracle (fronto-parallel content).
let candidateVsBaselineULPGate = 4
/// Per-pixel metric depth error budget against the Double oracle, baseline and candidates alike.
/// 0.1 mm at 2.5 m is a disparity change of about 1e-6 rad, two orders below stereo acuity.
let metricDepthBudgetMicrons = 100.0
/// Rasterizer model: vertex positions snap to 2^-8 pixel; interpolated depth at a pixel centre can
/// therefore deviate by up to (screen depth gradient per pixel) / 256 under camera motion, and by
/// nothing at identity where every vertex lands on an integer pixel corner.
let snappingSubpixelBits = 8.0
/// Depth beyond this multiple of the snapping bound plus eight ULP is unexplained by rounding.
let unexplainedDepthMultiple = 4.0
/// Minimum share of the footprint (outside the half-texel band) that the oracle must have judged.
let minimumEligibleFraction = 0.5
/// Colour against an independent bilinear clamp-to-edge sample of the source (the mesh and copy
/// sample with a linear filter): exact bytes when every weight is 0 or 1 (scale 1), otherwise one
/// 8-bit LSB for the UNORM quantization of a blend of quantized texels and 8-bit filter weights.
let bilinearColorToleranceLSB = 1
/// Candidate-relative verdict: a candidate is not worse than the baseline when its metric error is
/// within the absolute budget or within this factor of the baseline's own error on the same fixture.
let candidateRelativeMetricFactor = 1.1

/// Eye motion: yaw about Y, pitch about X, roll about Z (degrees), then translation.
func motion(yaw: Float = 0, pitch: Float = 0, roll: Float = 0, x: Float = 0, y: Float = 0, z: Float = 0) -> simd_float4x4 {
    func radians(_ degrees: Float) -> Float { degrees * .pi / 180 }
    let (cy, sy) = (cos(radians(yaw)), sin(radians(yaw)))
    let (cp, sp) = (cos(radians(pitch)), sin(radians(pitch)))
    let (cr, sr) = (cos(radians(roll)), sin(radians(roll)))
    let yawMatrix = simd_float4x4(columns: (SIMD4(cy, 0, -sy, 0), SIMD4(0, 1, 0, 0), SIMD4(sy, 0, cy, 0), SIMD4(0, 0, 0, 1)))
    let pitchMatrix = simd_float4x4(columns: (SIMD4(1, 0, 0, 0), SIMD4(0, cp, sp, 0), SIMD4(0, -sp, cp, 0), SIMD4(0, 0, 0, 1)))
    let rollMatrix = simd_float4x4(columns: (SIMD4(cr, sr, 0, 0), SIMD4(-sr, cr, 0, 0), SIMD4(0, 0, 1, 0), SIMD4(0, 0, 0, 1)))
    var result = rollMatrix * pitchMatrix * yawMatrix
    result.columns.3 = SIMD4(x, y, z, 1)
    return result
}

func sha256(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
func sha256(_ bytes: [UInt8]) -> String { sha256(Data(bytes)) }
func sha256(_ floats: [Float]) -> String { floats.withUnsafeBytes { sha256(Data($0)) } }

struct Random {
    var state: UInt64
    mutating func next() -> Float {
        state = state &* 6364136223846793005 &+ 1442695040888963407
        return Float(state >> 40) / Float(1 << 24)
    }
}

// ---- Analytic depth fields ----

/// A plane in source eye space: z = -(distance + ax * x + ay * y). NDC depth of such a
/// plane is affine over the image, which is exactly what the shader's corner test qualifies.
struct Plane {
    var distance: Float
    var ax: Float = 0
    var ay: Float = 0

    /// Eye-space hit of the ray through source NDC (nx, ny), or nil if the ray misses.
    func hit(ndc: SIMD2<Double>, eyeFromClip: simd_double4x4) -> SIMD3<Double>? {
        let far = eyeFromClip * SIMD4<Double>(ndc.x, ndc.y, 0.5, 1)
        guard far.w != 0 else { return nil }
        let point = SIMD3(far.x, far.y, far.z) / far.w
        // Direction from the eye through the point; the plane is z = -(d + ax x + ay y).
        let direction = point / -point.z   // scaled so z = -1
        let denominator = 1 - Double(ax) * direction.x - Double(ay) * direction.y
        guard abs(denominator) > 1e-9 else { return nil }
        let t = Double(distance) / denominator
        guard t > 0 else { return nil }
        return direction * t
    }
}

enum Field {
    case plane(Plane)
    case step(Plane, Plane)                 // left half / right half
    case stepAt(Plane, Plane, Int)          // step at a given column
    case island(Plane, Plane, Int, Int)     // plane with one texel at another plane
    case sky
    case skyStep(Plane)                     // foreground left, exact clear right
    case skyIslands(Plane)                  // plane with isolated exact-clear texels
    case invalidIslands(Plane)              // plane with isolated NaN, infinity and out-of-range texels
    case noise(UInt64)
    case affine(Float, Float, Float)        // NDC depth = a + b*x + c*y directly
}

struct Fixture {
    var name: String
    var field: Field
    var width = 64
    var height = 64
    var reversed = true
    var transform = matrix_identity_float4x4
    var scale = 2
    var mayBeEmpty = false
    /// Copy fixture whose geometry stays source-anchored while the near fade follows a moved head:
    /// judged only by the analytic fade oracle, never against the reprojecting mesh.
    var copyFadeOracleOnly = false
    /// Square pixels for a non-square image (x tangent scaled by height / width).
    var squarePixels = false
    /// Predeclared number of accepted (fan) tiles for the adaptive path; nil means the fixture is not
    /// parity-gated on acceptance (timing scenes). Every parity fixture declares it.
    var expectedFanTiles: Int? = nil
    var aspect: Float { squarePixels ? Float(height) / Float(width) : 1 }
    func projectionMatrix(_ reversedZ: Bool) -> simd_float4x4 { projection(reversedZ, aspect: aspect) }
    var currentProjection: simd_float4x4 { projection(true, aspect: aspect) }
    /// The plane the oracle checks, when the field is a single plane.
    var oraclePlane: Plane? {
        if case .plane(let plane) = field { return plane }
        return nil
    }
    var copyEligible: Bool { scale == 1 && (transform == matrix_identity_float4x4 || copyFadeOracleOnly) }
    /// Tiles the classification can see (whole and partial); border and partial tiles always fall back.
    var tileCount: Int { Int((width + 7) / 8) * Int((height + 7) / 8) }
}

func ndcDepth(eyePoint: SIMD3<Double>, clipFromEye: simd_double4x4) -> Double? {
    let clip = clipFromEye * SIMD4<Double>(eyePoint, 1)
    guard clip.w > 0 else { return nil }
    return clip.z / clip.w
}

func doubleMatrix(_ m: simd_float4x4) -> simd_double4x4 {
    func c(_ v: SIMD4<Float>) -> SIMD4<Double> { SIMD4(Double(v.x), Double(v.y), Double(v.z), Double(v.w)) }
    return simd_double4x4(columns: (c(m.columns.0), c(m.columns.1), c(m.columns.2), c(m.columns.3)))
}

func planeDepth(_ plane: Plane, x: Int, y: Int, width: Int, height: Int, reversed: Bool, aspect: Float = 1) -> Float {
    let proj = doubleMatrix(projection(reversed, aspect: aspect))
    let ndc = SIMD2(Double(x) + 0.5, Double(y) + 0.5) / SIMD2(Double(width), Double(height))
    let ndcClip = SIMD2(ndc.x * 2 - 1, 1 - ndc.y * 2)
    guard let hit = plane.hit(ndc: ndcClip, eyeFromClip: proj.inverse), let depth = ndcDepth(eyePoint: hit, clipFromEye: proj) else {
        return reversed ? 0 : 1
    }
    return Float(min(max(depth, 0), 1))
}

func depths(_ fixture: Fixture) -> [Float] {
    let (w, h) = (fixture.width, fixture.height)
    let clear: Float = fixture.reversed ? 0 : 1
    var out = [Float](repeating: clear, count: w * h)
    var random = Random(state: 1)
    if case .noise(let seed) = fixture.field { random = Random(state: seed) }
    for y in 0..<h {
        for x in 0..<w {
            let value: Float
            switch fixture.field {
            case .plane(let plane): value = planeDepth(plane, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .step(let left, let right): value = planeDepth(x < w / 2 ? left : right, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .stepAt(let left, let right, let column): value = planeDepth(x < column ? left : right, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .island(let plane, let other, let ix, let iy): value = planeDepth(x == ix && y == iy ? other : plane, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .sky: value = clear
            case .skyStep(let plane): value = x < w / 2 ? planeDepth(plane, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect) : clear
            case .skyIslands(let plane): value = (x * 7 + y * 3) % 11 == 0 ? clear : planeDepth(plane, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .invalidIslands(let plane):
                let k = (x * 5 + y * 9) % 13
                value = k == 0 ? .nan : k == 1 ? .infinity : k == 2 ? -0.25 : k == 3 ? 1.5
                    : planeDepth(plane, x: x, y: y, width: w, height: h, reversed: fixture.reversed, aspect: fixture.aspect)
            case .noise: value = 0.05 + 0.9 * random.next()
            case .affine(let a, let b, let c):
                value = a + b * (Float(x) + 0.5) / Float(w) + c * (Float(y) + 0.5) / Float(h)
            }
            out[y * w + x] = value
        }
    }
    return out
}

/// Non-black colours everywhere, so colour differences are never hidden by a black clear.
func colors(_ fixture: Fixture) -> [UInt8] {
    let (w, h) = (fixture.width, fixture.height)
    var out = [UInt8](repeating: 255, count: w * h * 4)
    for y in 0..<h {
        for x in 0..<w {
            let i = (y * w + x) * 4
            out[i] = UInt8(40 + (x * 211) % 200)
            out[i + 1] = UInt8(40 + (y * 173) % 200)
            out[i + 2] = UInt8(60 + ((x + y) * 97) % 180)
            out[i + 3] = 255
        }
    }
    return out
}

let fixtures: [Fixture] = [
    // 64x64 sources have 8x8 tiles of which the 6x6 interior can be accepted (border tiles always fall back).
    Fixture(name: "plane_identity_reverse", field: .plane(Plane(distance: 3)), expectedFanTiles: 36),
    Fixture(name: "plane_identity_forward", field: .plane(Plane(distance: 3)), reversed: false, expectedFanTiles: 36),
    Fixture(name: "slope_identity", field: .plane(Plane(distance: 2.5, ax: 0.35, ay: -0.2)), expectedFanTiles: 36),
    Fixture(name: "plane_yaw", field: .plane(Plane(distance: 3)), transform: motion(yaw: 5), expectedFanTiles: 36),
    Fixture(name: "plane_lateral", field: .plane(Plane(distance: 3)), transform: motion(x: 0.08), expectedFanTiles: 36),
    Fixture(name: "plane_lateral_forward", field: .plane(Plane(distance: 3)), reversed: false, transform: motion(roll: 3, x: -0.06), expectedFanTiles: 36),
    // Inside the fade band and crossing the near plane: every tile must fall back.
    Fixture(name: "plane_near_crossing", field: .plane(Plane(distance: 0.12)), transform: motion(z: -0.08), mayBeEmpty: true, expectedFanTiles: 0),
    // One texel off the plane inside tile (3,3): exactly that tile falls back.
    Fixture(name: "island_interior", field: .island(Plane(distance: 3), Plane(distance: 1.5), 28, 28), expectedFanTiles: 35),
    // Step at the boundary between tile columns 3 and 4: both columns fall back (36 - 2 * 6).
    Fixture(name: "shallow_step_tile_boundary", field: .stepAt(Plane(distance: 3), Plane(distance: 3.0006), 32), expectedFanTiles: 24),
    // Step inside tile column 4 only: that column falls back (36 - 6).
    Fixture(name: "step_inside_tile", field: .stepAt(Plane(distance: 3), Plane(distance: 6), 36), expectedFanTiles: 30),
    // Foreground on the left half: interior columns 1 and 2 qualify; column 3's halo touches sky.
    Fixture(name: "sky_foreground", field: .skyStep(Plane(distance: 2)), expectedFanTiles: 12),
    Fixture(name: "sky_islands", field: .skyIslands(Plane(distance: 2.5)), expectedFanTiles: 0),
    Fixture(name: "sky_only_rotated", field: .sky, transform: motion(yaw: 4, roll: 3), expectedFanTiles: 0),
    Fixture(name: "invalid_islands", field: .invalidIslands(Plane(distance: 2.5)), expectedFanTiles: 0),
    // Non-square image with the shared unit-tangent projection (as in runs v1 and v2 before the square variant): 5x3 tiles, interior 3x1.
    Fixture(name: "borders_37x23_slope", field: .plane(Plane(distance: 2.5, ax: 0.2, ay: 0.1)), width: 37, height: 23, transform: motion(roll: 4), expectedFanTiles: 3),
    // The same image with square pixels (x tangent scaled by 23/37): a wider field of view and a per-pixel
    // depth span near 30 mm, so the 1/256 px snapping model predicts about 0.12 mm for ANY path.
    Fixture(name: "borders_37x23_slope_square", field: .plane(Plane(distance: 2.5, ax: 0.2, ay: 0.1)), width: 37, height: 23, transform: motion(roll: 4), squarePixels: true, expectedFanTiles: 3),
    Fixture(name: "small_3x3_plane", field: .plane(Plane(distance: 3)), width: 3, height: 3, scale: 16, expectedFanTiles: 0),
    Fixture(name: "fade_transition", field: .plane(Plane(distance: 0.30)), expectedFanTiles: 0),
    Fixture(name: "fade_visible", field: .plane(Plane(distance: 0.5)), expectedFanTiles: 36),
    Fixture(name: "noise", field: .noise(7), expectedFanTiles: 0),
    // Affine NDC fields sit inside the fade band at these values: all fallback.
    Fixture(name: "affine_reverse", field: .affine(0.35, 0.2, -0.1), expectedFanTiles: 0),
    Fixture(name: "affine_forward", field: .affine(0.6, -0.15, 0.1), reversed: false, expectedFanTiles: 0),
    // Copy-eligible: identical lattice, identical projection up to the depth-row convention.
    Fixture(name: "copy_plane_reverse", field: .plane(Plane(distance: 3)), scale: 1, expectedFanTiles: 36),
    Fixture(name: "copy_plane_forward", field: .plane(Plane(distance: 3)), reversed: false, scale: 1, expectedFanTiles: 36),
    Fixture(name: "copy_slope", field: .plane(Plane(distance: 2.5, ax: 0.35, ay: -0.2)), scale: 1, expectedFanTiles: 36),
    Fixture(name: "copy_step_sky_invalid", field: .invalidIslands(Plane(distance: 2.5)), scale: 1, expectedFanTiles: 0),
    Fixture(name: "copy_noise", field: .noise(11), scale: 1, expectedFanTiles: 0),
    Fixture(name: "copy_fade", field: .plane(Plane(distance: 0.30)), scale: 1, expectedFanTiles: 0),
    Fixture(name: "copy_sky_foreground", field: .skyStep(Plane(distance: 2)), scale: 1, expectedFanTiles: 12),
    // Translated head: output stays source-anchored, the fade band moves with the head.
    Fixture(name: "copy_fade_translated", field: .plane(Plane(distance: 0.30)), transform: motion(x: 0.06, z: 0.12), scale: 1, copyFadeOracleOnly: true, expectedFanTiles: 0),
    Fixture(name: "copy_fade_translated_away", field: .plane(Plane(distance: 0.42)), transform: motion(z: -0.2), scale: 1, copyFadeOracleOnly: true, expectedFanTiles: 36),
]

// ---- Metal rig ----

struct HarnessError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

enum Variant: String, CaseIterable { case original, legacy, adaptive, copy }

struct Source {
    let color: any MTLTexture
    let depth: any MTLTexture
    var uniforms: Uniforms
    var width: Int { depth.width }
    var height: Int { depth.height }
}

struct Target {
    let color: any MTLTexture
    let depth: any MTLTexture
}

struct Image {
    var color: [UInt8]
    var depth: [Float]
    var mask: [Bool]   // alpha == 255: the shader wrote this pixel
    var width: Int
    var height: Int
}

final class Rig {
    let device: any MTLDevice
    let queue: any MTLCommandQueue
    let depthState: any MTLDepthStencilState
    let originalLibrary: any MTLLibrary
    let candidateLibrary: any MTLLibrary
    let reset: any MTLComputePipelineState
    let classify: any MTLComputePipelineState
    let fanIndexBuffer: any MTLBuffer
    let pool = AdaptiveScratchPool()
    var toleranceULPs: UInt32 = 16
    private var pipelines = [String: any MTLRenderPipelineState]()
    let sources: [String: String]

    init(candidatePath: String, baselinePath: String) throws {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { throw HarnessError("no Metal device") }
        self.device = device
        self.queue = queue
        let depthDescriptor = MTLDepthStencilDescriptor()
        depthDescriptor.depthCompareFunction = .greaterEqual
        depthDescriptor.isDepthWriteEnabled = true
        guard let depthState = device.makeDepthStencilState(descriptor: depthDescriptor) else { throw HarnessError("no depth state") }
        self.depthState = depthState
        let candidate = try Data(contentsOf: URL(fileURLWithPath: candidatePath))
        let baseline = try Data(contentsOf: URL(fileURLWithPath: baselinePath))
        sources = ["candidate_sha256": sha256(candidate), "baseline_sha256": sha256(baseline)]
        originalLibrary = try device.makeLibrary(source: String(decoding: baseline, as: UTF8.self), options: nil)
        candidateLibrary = try device.makeLibrary(source: String(decoding: candidate, as: UTF8.self), options: nil)
        guard let resetFunction = candidateLibrary.makeFunction(name: "gameWorldAdaptiveReset"),
              let classifyFunction = candidateLibrary.makeFunction(name: "gameWorldAdaptiveClassify") else { throw HarnessError("candidate lacks adaptive kernels") }
        reset = try device.makeComputePipelineState(function: resetFunction)
        classify = try device.makeComputePipelineState(function: classifyFunction)
        let indices = AdaptiveScratchPool.fanIndices
        guard let fanIndexBuffer = device.makeBuffer(bytes: indices, length: indices.count * MemoryLayout<UInt16>.stride,
                                                     options: .storageModeShared) else { throw HarnessError("no adaptive fan index buffer") }
        self.fanIndexBuffer = fanIndexBuffer
        if let value = ProcessInfo.processInfo.environment["HARNESS_TOLERANCE_ULP"], let parsed = UInt32(value) { toleranceULPs = parsed }
    }

    func pipeline(_ key: String, library: any MTLLibrary, vertex: String, fragment: String, format: MTLPixelFormat) throws -> any MTLRenderPipelineState {
        let cacheKey = "\(key):\(format.rawValue)"
        if let existing = pipelines[cacheKey] { return existing }
        let descriptor = MTLRenderPipelineDescriptor()
        guard let v = library.makeFunction(name: vertex), let f = library.makeFunction(name: fragment) else { throw HarnessError("missing \(vertex)/\(fragment)") }
        descriptor.vertexFunction = v
        descriptor.fragmentFunction = f
        descriptor.colorAttachments[0].pixelFormat = format
        descriptor.depthAttachmentPixelFormat = .depth32Float
        let state = try device.makeRenderPipelineState(descriptor: descriptor)
        pipelines[cacheKey] = state
        return state
    }

    func texture(_ format: MTLPixelFormat, _ usage: MTLTextureUsage, width: Int, height: Int, storage: MTLStorageMode = .shared) throws -> any MTLTexture {
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
        descriptor.usage = usage
        descriptor.storageMode = storage
        guard let texture = device.makeTexture(descriptor: descriptor) else { throw HarnessError("texture \(width)x\(height) \(format.rawValue)") }
        return texture
    }

    func source(depths: [Float], colors: [UInt8], width: Int, height: Int, uniforms: Uniforms) throws -> Source {
        let color = try texture(.rgba8Unorm, .shaderRead, width: width, height: height)
        let depth = try texture(.r32Float, .shaderRead, width: width, height: height)
        let region = MTLRegionMake2D(0, 0, width, height)
        colors.withUnsafeBytes { color.replace(region: region, mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
        depths.withUnsafeBytes { depth.replace(region: region, mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
        return Source(color: color, depth: depth, uniforms: uniforms)
    }

    func target(width: Int, height: Int) throws -> Target {
        Target(color: try texture(.rgba8Unorm, .renderTarget, width: width, height: height),
               depth: try texture(.depth32Float, .renderTarget, width: width, height: height))
    }

    /// Same pass set-up as GameWorldRenderer.encode, except the clear alpha is 0 so written pixels are explicit.
    func renderPass(_ target: Target) -> MTLRenderPassDescriptor {
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = target.color
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
        pass.depthAttachment.texture = target.depth
        pass.depthAttachment.loadAction = .clear
        pass.depthAttachment.storeAction = .store
        pass.depthAttachment.clearDepth = 0
        return pass
    }

    func encodeLegacy(_ commands: any MTLCommandBuffer, original: Bool, source: Source, target: Target) throws {
        guard let encoder = commands.makeRenderCommandEncoder(descriptor: renderPass(target)) else { throw HarnessError("no render encoder") }
        var uniforms = source.uniforms
        let library = original ? originalLibrary : candidateLibrary
        encoder.setRenderPipelineState(try pipeline(original ? "original" : "legacy", library: library, vertex: "gameWorldVertex",
                                                    fragment: "gameWorldFragment", format: target.color.pixelFormat))
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setVertexTexture(source.depth, index: 0)
        encoder.setFragmentTexture(source.color, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4, instanceCount: source.width * source.height)
        encoder.endEncoding()
    }

    /// Classification, two indirect draws and the counter copy, as GameWorldAdaptiveMesh encodes them.
    func encodeAdaptive(_ commands: any MTLCommandBuffer, source: Source, target: Target, set: AdaptiveScratchSet) throws {
        guard let compute = commands.makeComputeCommandEncoder() else { throw HarnessError("no compute encoder") }
        var uniforms = source.uniforms
        var params = AdaptiveParams(tileCount: set.tileCount, toleranceULPs: toleranceULPs, rejectBorderTiles: 1)
        compute.setComputePipelineState(reset)
        compute.setBuffer(set.draws, offset: 0, index: 0)
        compute.dispatchThreads(MTLSize(width: 1, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 1, height: 1, depth: 1))
        compute.setComputePipelineState(classify)
        compute.setBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        compute.setBytes(&params, length: MemoryLayout<AdaptiveParams>.stride, index: 1)
        compute.setBuffer(set.fanTiles, offset: 0, index: 2)
        compute.setBuffer(set.fallbackTiles, offset: 0, index: 3)
        compute.setBuffer(set.draws, offset: 0, index: 4)
        compute.setBuffer(set.classes, offset: 0, index: 5)
        compute.setTexture(source.depth, index: 0)
        compute.dispatchThreadgroups(MTLSize(width: Int(set.tileCount.x), height: Int(set.tileCount.y), depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: AdaptiveScratchPool.tile, height: AdaptiveScratchPool.tile, depth: 1))
        compute.endEncoding()
        guard let encoder = commands.makeRenderCommandEncoder(descriptor: renderPass(target)) else { throw HarnessError("no render encoder") }
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setVertexBytes(&params, length: MemoryLayout<AdaptiveParams>.stride, index: 1)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
        encoder.setVertexTexture(source.depth, index: 0)
        encoder.setFragmentTexture(source.color, index: 0)
        encoder.setRenderPipelineState(try pipeline("fan", library: candidateLibrary, vertex: "gameWorldAdaptiveFanVertex",
                                                    fragment: "gameWorldFragment", format: target.color.pixelFormat))
        encoder.setVertexBuffer(set.fanTiles, offset: 0, index: 2)
        encoder.drawIndexedPrimitives(type: .triangle, indexType: .uint16, indexBuffer: fanIndexBuffer,
                                      indexBufferOffset: 0, indirectBuffer: set.draws, indirectBufferOffset: 0)
        encoder.setRenderPipelineState(try pipeline("fallback", library: candidateLibrary, vertex: "gameWorldAdaptiveFallbackVertex",
                                                    fragment: "gameWorldFragment", format: target.color.pixelFormat))
        encoder.setVertexBuffer(set.fallbackTiles, offset: 0, index: 2)
        encoder.drawPrimitives(type: .triangleStrip, indirectBuffer: set.draws, indirectBufferOffset: 32)
        encoder.endEncoding()
        guard let blit = commands.makeBlitCommandEncoder() else { throw HarnessError("no blit encoder") }
        blit.copy(from: set.draws, sourceOffset: 0, to: set.stats, destinationOffset: 0, size: 64)
        blit.endEncoding()
    }

    func encodeCopy(_ commands: any MTLCommandBuffer, source: Source, target: Target) throws {
        guard let encoder = commands.makeRenderCommandEncoder(descriptor: renderPass(target)) else { throw HarnessError("no render encoder") }
        let u = source.uniforms
        var copy = CopyUniforms(sourceEyeFromClip: u.sourceEyeFromClip, sourceClipFromEye: u.sourceEyeFromClip.inverse,
                                currentEyeFromClip: u.currentClipFromEye.inverse, currentClipFromEye: u.currentClipFromEye,
                                currentEyeFromSourceEye: u.currentEyeFromSourceEye, sourceSize: u.sourceSize, sourceDepthReversed: u.sourceDepthReversed, colorTransfer: u.colorTransfer,
                                nearFadeDistances: u.nearFadeDistances)
        encoder.setRenderPipelineState(try pipeline("copy", library: candidateLibrary, vertex: "gameWorldCopyVertex",
                                                    fragment: "gameWorldSourceCopyFragment", format: target.color.pixelFormat))
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        encoder.setFragmentBytes(&copy, length: MemoryLayout<CopyUniforms>.stride, index: 0)
        encoder.setFragmentTexture(source.color, index: 0)
        encoder.setFragmentTexture(source.depth, index: 1)
        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
        encoder.endEncoding()
    }

    func commandBuffer() throws -> any MTLCommandBuffer {
        guard let commands = queue.makeCommandBuffer() else { throw HarnessError("no command buffer") }
        return commands
    }

    func finish(_ commands: any MTLCommandBuffer) throws -> Double {
        commands.commit()
        commands.waitUntilCompleted()
        if commands.status != .completed { throw HarnessError("command buffer failed: \(String(describing: commands.error))") }
        return (commands.gpuEndTime - commands.gpuStartTime) * 1000
    }

    /// One complete, waited render. Returns GPU milliseconds and, for adaptive, the tile counters.
    struct RenderResult {
        var ms: Double
        var fan: UInt32?
        var fallback: UInt32?
        /// Per-tile class from the classification kernel (1 = accepted fan, 0 = fallback, >1 reject flags << 1).
        var classes: [UInt32]?
    }

    func render(_ variant: Variant, source: Source, target: Target, readClasses: Bool = false) throws -> RenderResult {
        let commands = try commandBuffer()
        var lease = AdaptiveScratchLease()
        var classesBuffer: (any MTLBuffer)?
        switch variant {
        case .original: try encodeLegacy(commands, original: true, source: source, target: target)
        case .legacy: try encodeLegacy(commands, original: false, source: source, target: target)
        case .copy: try encodeCopy(commands, source: source, target: target)
        case .adaptive:
            guard let set = pool.acquire(device: device, width: source.width, height: source.height) else { throw HarnessError("pool refused \(source.width)x\(source.height)") }
            lease.sets.append(set)
            try encodeAdaptive(commands, source: source, target: target, set: set)
            if readClasses, let shared = device.makeBuffer(length: set.classes.length, options: .storageModeShared),
               let blit = commands.makeBlitCommandEncoder() {
                blit.copy(from: set.classes, sourceOffset: 0, to: shared, destinationOffset: 0, size: set.classes.length)
                blit.endEncoding()
                classesBuffer = shared
            }
        }
        let ms = try finish(commands)
        var result = RenderResult(ms: ms)
        if let set = lease.sets.first {
            let words = set.stats.contents().assumingMemoryBound(to: UInt32.self)
            result.fan = words[1]
            result.fallback = words[9] / 64
            if let classesBuffer {
                let count = classesBuffer.length / 4
                result.classes = Array(UnsafeBufferPointer(start: classesBuffer.contents().assumingMemoryBound(to: UInt32.self), count: count))
            }
        }
        pool.release(lease)
        return result
    }

    func image(_ target: Target) -> Image {
        let (w, h) = (target.color.width, target.color.height)
        var bytes = [UInt8](repeating: 0, count: w * h * 4)
        bytes.withUnsafeMutableBytes { target.color.getBytes($0.baseAddress!, bytesPerRow: w * 4, from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0) }
        var values = [Float](repeating: 0, count: w * h)
        values.withUnsafeMutableBytes { target.depth.getBytes($0.baseAddress!, bytesPerRow: w * 4, from: MTLRegionMake2D(0, 0, w, h), mipmapLevel: 0) }
        var mask = [Bool](repeating: false, count: w * h)
        for i in 0..<(w * h) { mask[i] = bytes[i * 4 + 3] == 255 }
        return Image(color: bytes, depth: values, mask: mask, width: w, height: h)
    }
}

func fixtureSource(_ rig: Rig, _ fixture: Fixture) throws -> Source {
    let uniforms = Uniforms(sourceEyeFromClip: fixture.projectionMatrix(fixture.reversed).inverse, currentEyeFromSourceEye: fixture.transform,
                            currentClipFromEye: fixture.currentProjection, sourceSize: SIMD2(UInt32(fixture.width), UInt32(fixture.height)),
                            sourceDepthReversed: fixture.reversed ? 1 : 0)
    return try rig.source(depths: depths(fixture), colors: colors(fixture), width: fixture.width, height: fixture.height, uniforms: uniforms)
}

// ---- Comparison ----

func ulpDistance(_ a: Float, _ b: Float) -> Int {
    guard a.isFinite, b.isFinite else { return a.bitPattern == b.bitPattern ? 0 : Int.max }
    if a == b { return 0 }
    if (a < 0) != (b < 0) { return Int.max }
    return abs(Int(Int32(bitPattern: a.bitPattern)) - Int(Int32(bitPattern: b.bitPattern)))
}

/// Source texels that the FIXTURE defines as discontinuous: step columns, islands, clear sky
/// and invalid samples (plus one texel of dilation). Planes and affine fields have none; noise
/// is discontinuous everywhere. Never derived from rendered output, so smooth slopes, the Bayer
/// fade and coarse/fine seams are never mistaken for edges.
func knownDiscontinuityTexels(_ fixture: Fixture, depths field: [Float]) -> [Bool] {
    let (w, h) = (fixture.width, fixture.height)
    var flagged = [Bool](repeating: false, count: w * h)
    let clear: Float = fixture.reversed ? 0 : 1
    func flagInvalidAndClear() {
        for i in 0..<(w * h) where !field[i].isFinite || field[i] < 0 || field[i] > 1 || field[i] == clear { flagged[i] = true }
    }
    switch fixture.field {
    case .plane, .affine: break
    case .step: for y in 0..<h { flagged[y * w + w / 2] = true; if w / 2 > 0 { flagged[y * w + w / 2 - 1] = true } }
    case .stepAt(_, _, let column): for y in 0..<h where column < w { flagged[y * w + column] = true; if column > 0 { flagged[y * w + column - 1] = true } }
    case .island(_, _, let ix, let iy): if ix < w, iy < h { flagged[iy * w + ix] = true }
    case .sky, .skyStep, .skyIslands, .invalidIslands: flagInvalidAndClear()
    case .noise: for i in 0..<(w * h) { flagged[i] = true }
    }
    var dilated = flagged
    for y in 0..<h { for x in 0..<w where flagged[y * w + x] {
        for dy in -1...1 { for dx in -1...1 {
            let nx = x + dx, ny = y + dy
            if nx >= 0, ny >= 0, nx < w, ny < h { dilated[ny * w + nx] = true }
        } }
    } }
    return dilated
}

/// Source-field discontinuities for the timing scenes, judged on the fixture DATA (not rendered
/// output): clear or invalid texels, or 4-neighbours whose NDC depth differs by more than
/// 1e-3 relative. A smooth plane changes by far less than that per texel.
func fieldDiscontinuityTexels(depths field: [Float], width w: Int, height h: Int, reversed: Bool) -> [Bool] {
    let clear: Float = reversed ? 0 : 1
    var flagged = [Bool](repeating: false, count: w * h)
    for y in 0..<h {
        for x in 0..<w {
            let i = y * w + x, z = field[i]
            if !z.isFinite || z < 0 || z > 1 || z == clear { flagged[i] = true; continue }
            for (dx, dy) in [(1, 0), (0, 1)] {
                let nx = x + dx, ny = y + dy
                guard nx < w, ny < h else { continue }
                let n = field[ny * w + nx]
                if !n.isFinite || abs(n - z) > max(1e-4, 1e-3 * abs(z)) { flagged[i] = true; flagged[ny * w + nx] = true }
            }
        }
    }
    return flagged
}

/// Output-space edge mask: fixture-known discontinuities mapped through the identity lattice
/// (moved-camera fixtures carry no discontinuities by construction), plus a one-pixel halo
/// around the REFERENCE coverage border. Coarse/fine seams are not edges.
func knownEdgeMask(reference: Image, sourceDiscontinuities: [Bool], sourceWidth: Int, sourceHeight: Int, scale: Int, identityLattice: Bool) -> [Bool] {
    let (w, h) = (reference.width, reference.height)
    var edge = [Bool](repeating: false, count: w * h)
    for y in 0..<h {
        for x in 0..<w {
            let i = y * w + x
            if identityLattice {
                let sx = min(x / scale, sourceWidth - 1), sy = min(y / scale, sourceHeight - 1)
                if sourceDiscontinuities[sy * sourceWidth + sx] { edge[i] = true; continue }
            }
            var border = false
            for dy in -1...1 where !border { for dx in -1...1 where !border {
                let nx = x + dx, ny = y + dy
                if nx < 0 || ny < 0 || nx >= w || ny >= h { if reference.mask[i] { border = true }; continue }
                if reference.mask[i] != reference.mask[ny * w + nx] { border = true }
            } }
            edge[i] = border
        }
    }
    return edge
}

struct Comparison {
    var bitIdentical: Bool
    var colorBytesDifferent = 0
    var maximumColorDifference = 0
    var maximumColorDifferenceAwayFromEdges = 0
    var depthValuesDifferent = 0
    var maximumDepthULP = 0
    var maximumDepthULPAwayFromEdges = 0
    var coverageOnlyInCandidate = 0   // novel coverage ("bridges")
    var coverageOnlyInReference = 0   // cracks
    var coverageOnlyInCandidateAwayFromEdges = 0
    var coverageOnlyInReferenceAwayFromEdges = 0
    var comparedPixels = 0
    var json: [String: Any] {
        ["bit_identical": bitIdentical, "color_bytes_different": colorBytesDifferent,
         "maximum_color_difference_8bit": maximumColorDifference, "maximum_color_difference_8bit_away_from_edges": maximumColorDifferenceAwayFromEdges,
         "depth_values_different": depthValuesDifferent, "maximum_depth_ulp": maximumDepthULP == Int.max ? "sign_or_nan_mismatch" : maximumDepthULP as Any,
         "maximum_depth_ulp_away_from_edges": maximumDepthULPAwayFromEdges == Int.max ? "sign_or_nan_mismatch" : maximumDepthULPAwayFromEdges as Any,
         "coverage_only_in_candidate": coverageOnlyInCandidate, "coverage_only_in_reference": coverageOnlyInReference,
         "coverage_only_in_candidate_away_from_edges": coverageOnlyInCandidateAwayFromEdges,
         "coverage_only_in_reference_away_from_edges": coverageOnlyInReferenceAwayFromEdges, "compared_pixels": comparedPixels]
    }
}

func compare(reference: Image, candidate: Image, edges: [Bool]) -> Comparison {
    var c = Comparison(bitIdentical: reference.color == candidate.color && reference.depth.map(\.bitPattern) == candidate.depth.map(\.bitPattern))
    for i in 0..<reference.depth.count {
        let onEdge = edges[i]
        for k in 0..<4 where reference.color[i * 4 + k] != candidate.color[i * 4 + k] {
            c.colorBytesDifferent += 1
            let d = abs(Int(reference.color[i * 4 + k]) - Int(candidate.color[i * 4 + k]))
            c.maximumColorDifference = max(c.maximumColorDifference, d)
            if !onEdge { c.maximumColorDifferenceAwayFromEdges = max(c.maximumColorDifferenceAwayFromEdges, d) }
        }
        if reference.mask[i] != candidate.mask[i] {
            if candidate.mask[i] { c.coverageOnlyInCandidate += 1; if !onEdge { c.coverageOnlyInCandidateAwayFromEdges += 1 } }
            else { c.coverageOnlyInReference += 1; if !onEdge { c.coverageOnlyInReferenceAwayFromEdges += 1 } }
            continue
        }
        guard reference.mask[i] else { continue }
        c.comparedPixels += 1
        if reference.depth[i].bitPattern != candidate.depth[i].bitPattern {
            c.depthValuesDifferent += 1
            let u = ulpDistance(reference.depth[i], candidate.depth[i])
            c.maximumDepthULP = max(c.maximumDepthULP, u)
            if !onEdge { c.maximumDepthULPAwayFromEdges = max(c.maximumDepthULPAwayFromEdges, u) }
        }
    }
    return c
}

/// Independent per-pixel oracle for single-plane fixtures, in Double. For every output pixel it
/// predicts whether the pixel must be written (ray hits the plane inside the source footprint, is
/// inside the clip depth range, and survives the Bayer near fade judged from `fadeTransform`), the
/// NDC and metric depth of a written pixel, and, on an identity lattice, the exact source colour.
/// Pixels in the half-texel footprint band, within 1e-4 of a Bayer threshold, or on fixture-known
/// edges are excluded and counted; nothing else is skipped. Also reports the screen-space depth
/// gradient and the rasterizer snapping bound derived from it.
struct OracleReport {
    var eligible = 0, checked = 0, missing = 0, unexpected = 0, ties = 0, skippedBand = 0, excludedEdges = 0, footprintArea = 0
    /// Written pixels whose depth is NaN, infinite or outside [0, 1]: never tolerated.
    var invalidWrittenDepth = 0
    var colorChecked = 0, colorMismatches = 0, colorMaximumDifferenceLSB = 0
    var colorMode = "none"
    var maximumNDC = 0.0, maximumMetricMicrons = 0.0, maximumULP = 0
    var maximumGradientPerPixel = 0.0
    /// Pixels whose depth error exceeds the per-pixel bound: multiple x local gradient / 2^bits (only when the
    /// camera moved) plus eight ULP of the expected depth itself (local, not a global floor).
    var pixelsBeyondLocalBound = 0
    var maximumExcessOverLocalBoundNDC = 0.0
    var predictedSnappingNDC: Double { maximumGradientPerPixel / pow(2, snappingSubpixelBits) }
    var json: [String: Any] {
        ["eligible_pixels": eligible, "checked_written_pixels": checked, "missing_coverage": missing, "unexpected_coverage": unexpected,
         "invalid_written_depth": invalidWrittenDepth,
         "threshold_ties_skipped": ties, "skipped_footprint_band": skippedBand, "excluded_edge_pixels": excludedEdges, "footprint_area": footprintArea,
         "color_mode": colorMode, "color_checked_pixels": colorChecked, "color_mismatches_beyond_tolerance": colorMismatches,
         "color_maximum_difference_8bit": colorMaximumDifferenceLSB,
         "maximum_depth_abs_ndc_vs_oracle": maximumNDC, "maximum_metric_depth_error_microns": maximumMetricMicrons,
         "maximum_depth_ulp_vs_oracle": maximumULP, "maximum_screen_depth_gradient_per_pixel": maximumGradientPerPixel,
         "predicted_snapping_bound_ndc": predictedSnappingNDC, "pixels_beyond_local_depth_bound": pixelsBeyondLocalBound,
         "maximum_excess_over_local_bound_ndc": maximumExcessOverLocalBoundNDC,
         "note": "Double ray/plane intersection; coverage, fade (Bayer), clip, depth and colour judged per pixel; depth bound is local (gradient-based snapping term only under motion, plus 8 ULP of the expected depth); colour is exact source bytes at scale 1, else an independent bilinear clamp-to-edge sample with UNORM quantization within 1 LSB; only the footprint band, Bayer ties and fixture-known edges are excluded"]
    }
}

/// Independent bilinear clamp-to-edge sample of the RGBA8 source at normalized `uv`, in Double,
/// quantized to UNORM bytes as the RGBA8 target stores it. Mirrors `filter::linear, address::clamp_to_edge`.
func bilinearSourceColor(sourceColors: [UInt8], width: Int, height: Int, uv: SIMD2<Double>) -> (Int, Int, Int) {
    let tx = min(max(uv.x * Double(width) - 0.5, 0), Double(width - 1))
    let ty = min(max(uv.y * Double(height) - 0.5, 0), Double(height - 1))
    let x0 = Int(floor(tx)), y0 = Int(floor(ty))
    let x1 = min(x0 + 1, width - 1), y1 = min(y0 + 1, height - 1)
    let fx = tx - Double(x0), fy = ty - Double(y0)
    func channel(_ k: Int) -> Int {
        func at(_ x: Int, _ y: Int) -> Double { Double(sourceColors[(y * width + x) * 4 + k]) / 255 }
        let value = (1 - fx) * (1 - fy) * at(x0, y0) + fx * (1 - fy) * at(x1, y0) + (1 - fx) * fy * at(x0, y1) + fx * fy * at(x1, y1)
        return Int((value * 255).rounded())
    }
    return (channel(0), channel(1), channel(2))
}

func metricDepth(ndc: Double, projection: simd_double4x4) -> Double {
    // Reverse-Z target: z = -a - b / ze, so ze = -b / (z + a); depth is -ze.
    let a = projection.columns.2.z, b = projection.columns.3.z
    return b / (ndc + a)
}

func analyticOracle(_ fixture: Fixture, image: Image, sourceColors: [UInt8], geometryTransform: simd_float4x4,
                    fadeTransform: simd_float4x4, edges: [Bool], judgeColorAgainstSource: Bool) -> OracleReport? {
    guard let plane = fixture.oraclePlane else { return nil }
    let snappingApplies = geometryTransform != matrix_identity_float4x4
    let sourceProjection = doubleMatrix(fixture.projectionMatrix(fixture.reversed))
    let currentProjection = doubleMatrix(fixture.currentProjection)
    let currentFromSource = doubleMatrix(geometryTransform)
    let sourceFromCurrent = currentFromSource.inverse
    let fadeFromSource = doubleMatrix(fadeTransform)
    let bayer: [Double] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5]
    let (w, h) = (image.width, image.height)
    var report = OracleReport()
    func expectation(_ px: Int, _ py: Int) -> (depth: Double?, inside: Bool, band: Bool, hitSource: SIMD3<Double>?) {
        let ndc = SIMD2((Double(px) + 0.5) / Double(w) * 2 - 1, 1 - (Double(py) + 0.5) / Double(h) * 2)
        let far = currentProjection.inverse * SIMD4<Double>(ndc.x, ndc.y, 0.5, 1)
        let pc = SIMD3(far.x, far.y, far.z) / far.w
        let o4 = sourceFromCurrent * SIMD4<Double>(0, 0, 0, 1), p4 = sourceFromCurrent * SIMD4<Double>(pc, 1)
        let o = SIMD3(o4.x, o4.y, o4.z) / o4.w, d = SIMD3(p4.x, p4.y, p4.z) / p4.w - o
        let n = SIMD3(Double(plane.ax), Double(plane.ay), 1.0)
        let denominator = simd_dot(n, d)
        guard abs(denominator) > 1e-12 else { return (nil, false, false, nil) }
        let t = -(simd_dot(n, o) + Double(plane.distance)) / denominator
        guard t > 0 else { return (nil, false, false, nil) }
        let hit = o + d * t
        let clip = sourceProjection * SIMD4<Double>(hit, 1)
        guard clip.w > 0 else { return (nil, false, false, nil) }
        let sndc = SIMD2(clip.x, clip.y) / clip.w
        let band = SIMD2(1.0 / Double(fixture.width), 1.0 / Double(fixture.height))
        let inside = abs(sndc.x) < 1 - band.x && abs(sndc.y) < 1 - band.y
        let inBand = !inside && abs(sndc.x) <= 1 + band.x && abs(sndc.y) <= 1 + band.y
        if inBand { return (nil, false, true, hit) }
        guard inside else { return (nil, false, false, hit) }
        let hc4 = currentFromSource * SIMD4<Double>(hit, 1)
        let hc = SIMD3(hc4.x, hc4.y, hc4.z) / hc4.w
        guard let depth = ndcDepth(eyePoint: hc, clipFromEye: currentProjection), depth >= 0, depth <= 1 else { return (nil, true, false, hit) }
        return (depth, true, false, hit)
    }
    for y in 0..<h {
        for x in 0..<w {
            let i = y * w + x
            let e = expectation(x, y)
            if e.band { report.skippedBand += 1; continue }
            if e.inside { report.footprintArea += 1 }
            if edges[i] { report.excludedEdges += 1; continue }
            var expectedWritten = false
            if let depth = e.depth, let hit = e.hitSource {
                let f4 = fadeFromSource * SIMD4<Double>(hit, 1)
                let distance = simd_length(SIMD3(f4.x, f4.y, f4.z) / f4.w)
                let t = min(max((distance - 0.15) / (0.45 - 0.15), 0), 1)
                let visibility = t * t * (3 - 2 * t)
                let threshold = (bayer[(y & 3) * 4 + (x & 3)] + 0.5) / 16
                if abs(visibility - threshold) < 1e-4 { report.ties += 1; continue }
                expectedWritten = visibility > threshold
                if expectedWritten {
                    for (nx, ny) in [(x + 1, y), (x, y + 1)] where nx < w && ny < h {
                        if let neighbour = expectation(nx, ny).depth { report.maximumGradientPerPixel = max(report.maximumGradientPerPixel, abs(neighbour - depth)) }
                    }
                }
            }
            report.eligible += 1
            if expectedWritten != image.mask[i] {
                if image.mask[i] { report.unexpected += 1 } else { report.missing += 1 }
                continue
            }
            guard expectedWritten, let depth = e.depth else { continue }
            report.checked += 1
            let actualFloat = image.depth[i]
            let actual = Double(actualFloat)
            guard actualFloat.isFinite, actual >= 0, actual <= 1 else { report.invalidWrittenDepth += 1; continue }
            let error = abs(actual - depth)
            report.maximumNDC = max(report.maximumNDC, error)
            report.maximumULP = max(report.maximumULP, ulpDistance(actualFloat, Float(depth)))
            let metricActual = metricDepth(ndc: actual, projection: currentProjection), metricExpected = metricDepth(ndc: depth, projection: currentProjection)
            guard metricActual.isFinite, metricExpected.isFinite else { report.invalidWrittenDepth += 1; continue }
            report.maximumMetricMicrons = max(report.maximumMetricMicrons, abs(metricActual - metricExpected) * 1e6)
            // Local depth bound: snapping term from this pixel's own gradient (motion only) plus 8 ULP of its own expected depth.
            var localGradient = 0.0
            for (nx, ny) in [(x + 1, y), (x, y + 1), (x - 1, y), (x, y - 1)] where nx >= 0 && ny >= 0 && nx < w && ny < h {
                if let neighbour = expectation(nx, ny).depth { localGradient = max(localGradient, abs(neighbour - depth)) }
            }
            let snappingTerm = snappingApplies ? unexplainedDepthMultiple * localGradient / pow(2, snappingSubpixelBits) : 0
            let localBound = snappingTerm + 8 * Double(Float(depth).ulp)
            if error > localBound {
                report.pixelsBeyondLocalBound += 1
                report.maximumExcessOverLocalBoundNDC = max(report.maximumExcessOverLocalBoundNDC, error - localBound)
            }
            if judgeColorAgainstSource {
                report.colorChecked += 1
                let uv = SIMD2((Double(x) + 0.5) / Double(w), (Double(y) + 0.5) / Double(h))
                let expectedColor: (Int, Int, Int)
                if fixture.scale == 1 {
                    report.colorMode = "exact_source_texel"
                    let si = (min(y, fixture.height - 1) * fixture.width + min(x, fixture.width - 1)) * 4
                    expectedColor = (Int(sourceColors[si]), Int(sourceColors[si + 1]), Int(sourceColors[si + 2]))
                } else {
                    report.colorMode = "bilinear_clamp_to_edge_unorm"
                    expectedColor = bilinearSourceColor(sourceColors: sourceColors, width: fixture.width, height: fixture.height, uv: uv)
                }
                let difference = max(abs(Int(image.color[i * 4]) - expectedColor.0), abs(Int(image.color[i * 4 + 1]) - expectedColor.1), abs(Int(image.color[i * 4 + 2]) - expectedColor.2))
                report.colorMaximumDifferenceLSB = max(report.colorMaximumDifferenceLSB, difference)
                if difference > (fixture.scale == 1 ? 0 : bilinearColorToleranceLSB) { report.colorMismatches += 1 }
            }
        }
    }
    return report
}

/// Output pixels inside accepted (fan) tiles and within one pixel of a fan/non-fan tile boundary,
/// on an identity lattice. Seams are judged, never excluded.
func fanAndSeamMasks(classes: [UInt32], fixture: Fixture, width w: Int, height h: Int) -> (fan: [Bool], seam: [Bool]) {
    let tilesX = (fixture.width + 7) / 8
    func tileClass(_ px: Int, _ py: Int) -> UInt32? {
        let sx = px / fixture.scale, sy = py / fixture.scale
        guard px >= 0, py >= 0, sx < fixture.width, sy < fixture.height else { return nil }
        let index = (sy / 8) * tilesX + (sx / 8)
        return index < classes.count ? classes[index] : nil
    }
    var fan = [Bool](repeating: false, count: w * h), seam = fan
    for y in 0..<h {
        for x in 0..<w {
            let here = tileClass(x, y) == 1
            fan[y * w + x] = here
            var boundary = false
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] where !boundary {
                let other = tileClass(x + dx, y + dy)
                if other == nil { boundary = here } else if (other == 1) != here { boundary = true }
            }
            seam[y * w + x] = boundary
        }
    }
    return (fan, seam)
}

/// Candidate versus reference restricted to a pixel set.
func compareOn(_ pixels: [Bool], reference: Image, candidate: Image) -> [String: Any] {
    var count = 0, maskDifferent = 0, maxColor = 0, maxULP = 0
    for i in 0..<pixels.count where pixels[i] {
        count += 1
        if reference.mask[i] != candidate.mask[i] { maskDifferent += 1; continue }
        for k in 0..<3 { maxColor = max(maxColor, abs(Int(reference.color[i * 4 + k]) - Int(candidate.color[i * 4 + k]))) }
        if reference.mask[i] { maxULP = max(maxULP, ulpDistance(reference.depth[i], candidate.depth[i])) }
    }
    return ["pixels": count, "mask_different": maskDifferent, "maximum_color_difference_8bit": maxColor,
            "maximum_depth_ulp": maxULP == Int.max ? "sign_or_nan_mismatch" : maxULP as Any]
}

// ---- Parity ----

func liveShaderCheck(candidatePath: String) throws -> [String: Any] {
    // The candidate must be the live production shader, so a stale build cannot pass for the source tree.
    let candidate = try Data(contentsOf: URL(fileURLWithPath: candidatePath))
    let livePath = "visionos/Sources/GameWorld.metal"
    guard let live = try? Data(contentsOf: URL(fileURLWithPath: livePath)) else {
        return ["live_shader_path": livePath, "live_shader_readable": false, "candidate_matches_live": false]
    }
    return ["live_shader_path": livePath, "live_shader_readable": true, "live_shader_sha256": sha256(live),
            "candidate_sha256": sha256(candidate), "candidate_matches_live": sha256(live) == sha256(candidate)]
}

func runParity(candidatePath: String, baselinePath: String, outDir: String) throws -> [String: Any] {
    let rig = try Rig(candidatePath: candidatePath, baselinePath: baselinePath)
    try FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)
    var failures = [String]()
    var baselineFailures = [String]()
    var candidateAbsoluteFailures = [String]()
    var candidateRelativeFailures = [String]()
    let live = try liveShaderCheck(candidatePath: candidatePath)
    if live["candidate_matches_live"] as? Bool != true { failures.append("stale_candidate_shader"); candidateAbsoluteFailures.append("stale_candidate_shader"); candidateRelativeFailures.append("stale_candidate_shader") }
    var results = [[String: Any]]()
    for fixture in fixtures {
        let source = try fixtureSource(rig, fixture)
        let sourceColors = colors(fixture)
        let (w, h) = (fixture.width * fixture.scale, fixture.height * fixture.scale)
        var images = [Variant: Image]()
        var counters = [String: Any]()
        var classes: [UInt32]?
        for variant in Variant.allCases where variant != .copy || fixture.copyEligible {
            let target = try rig.target(width: w, height: h)
            let result = try rig.render(variant, source: source, target: target, readClasses: variant == .adaptive)
            images[variant] = rig.image(target)
            if variant == .adaptive {
                classes = result.classes
                counters = ["fan_tiles": result.fan ?? 0, "fallback_tiles": result.fallback ?? 0, "tile_count": fixture.tileCount,
                            "expected_fan_tiles": fixture.expectedFanTiles ?? -1]
            }
        }
        let original = images[.original]!
        let identityLattice = fixture.transform == matrix_identity_float4x4
        let discontinuities = knownDiscontinuityTexels(fixture, depths: depths(fixture))
        let edges = knownEdgeMask(reference: original, sourceDiscontinuities: discontinuities, sourceWidth: fixture.width,
                                  sourceHeight: fixture.height, scale: fixture.scale, identityLattice: identityLattice)
        let discontinuityEdges = knownEdgeMask(reference: Image(color: [], depth: [], mask: [Bool](repeating: true, count: w * h), width: w, height: h),
                                               sourceDiscontinuities: discontinuities, sourceWidth: fixture.width, sourceHeight: fixture.height,
                                               scale: fixture.scale, identityLattice: identityLattice)
        let covered = original.mask.filter { $0 }.count
        let coverageRequired = identityLattice && !fixture.mayBeEmpty
        let hasOracle = fixture.oraclePlane != nil
        let allFallbackExpected = fixture.expectedFanTiles == 0
        // Three verdicts: the inherited baseline's own accuracy, the candidates against absolute budgets, and
        // the candidates relative to the baseline. A fixture passes only when baseline and candidates both pass.
        var baselineProblems = [String]()
        var candidateProblems = [String]()
        var relativeProblems = [String]()
        var comparisons = [String: Any]()
        var oracles = [String: Any]()
        var fanSeam = [String: Any]()

        // 1. Legacy OFF: exact.
        let legacy = compare(reference: original, candidate: images[.legacy]!, edges: edges)
        comparisons["legacy"] = legacy.json
        if !legacy.bitIdentical { candidateProblems.append("legacy OFF not bit-identical to original") }
        if coverageRequired, covered == 0 { baselineProblems.append("original drew nothing") }

        // 2. Predeclared acceptance and mandatory class readback: the fan/seam gates can never disappear.
        let fan = Int(counters["fan_tiles"] as? UInt32 ?? 0), fallback = Int(counters["fallback_tiles"] as? UInt32 ?? 0)
        if let expected = fixture.expectedFanTiles {
            if fan != expected { candidateProblems.append("adaptive: accepted \(fan) tiles, predeclared \(expected)") }
            if fan + fallback != fixture.tileCount { candidateProblems.append("adaptive: fan + fallback \(fan + fallback) differs from tile count \(fixture.tileCount)") }
        } else { candidateProblems.append("fixture lacks a predeclared fan-tile expectation") }
        if let classes {
            if classes.count != fixture.tileCount { candidateProblems.append("adaptive: class readback has \(classes.count) entries, expected \(fixture.tileCount)") }
            let accepted = classes.filter { $0 == 1 }.count
            if accepted != fan { candidateProblems.append("adaptive: class readback shows \(accepted) accepted tiles, counters show \(fan)") }
        } else { candidateProblems.append("adaptive: tile class readback unavailable") }

        // Legacy and classification evidence are prerequisites for both verdicts.
        relativeProblems += candidateProblems

        // 3. Oracle for every variant on plane fixtures: the original's result is the baseline verdict.
        var baselineMetric = 0.0
        if hasOracle {
            func judge(_ variant: Variant, _ image: Image) -> (report: OracleReport, problems: [String])? {
                let geometry = variant == .copy ? matrix_identity_float4x4 : fixture.transform
                let judgeColor = variant == .copy || identityLattice
                guard let report = analyticOracle(fixture, image: image, sourceColors: sourceColors, geometryTransform: geometry,
                                                  fadeTransform: fixture.transform, edges: discontinuityEdges, judgeColorAgainstSource: judgeColor) else { return nil }
                let name = variant.rawValue
                var problems = [String]()
                if report.invalidWrittenDepth > 0 { problems.append("\(name): \(report.invalidWrittenDepth) written pixels with NaN, infinite or out-of-range depth") }
                if report.missing > 0 { problems.append("\(name): missing coverage at \(report.missing) predicted pixels") }
                if report.unexpected > 0 { problems.append("\(name): unexpected coverage at \(report.unexpected) pixels") }
                if report.pixelsBeyondLocalBound > 0 { problems.append("\(name): \(report.pixelsBeyondLocalBound) pixels exceed the local depth bound by up to \(report.maximumExcessOverLocalBoundNDC) NDC") }
                if judgeColor, report.colorMismatches > 0 { problems.append("\(name): \(report.colorMismatches) colour mismatches beyond \(fixture.scale == 1 ? 0 : bilinearColorToleranceLSB) LSB against the \(report.colorMode) oracle") }
                let judgeable = w * h - report.skippedBand
                if judgeable > 0, Double(report.eligible) < minimumEligibleFraction * Double(judgeable) { problems.append("\(name): oracle judged only \(report.eligible) of \(judgeable) pixels outside the footprint band") }
                if coverageRequired, report.checked == 0 { problems.append("\(name): oracle checked no written pixel") }
                return (report, problems)
            }
            if let judged = judge(.original, original) {
                oracles["original"] = judged.report.json
                baselineMetric = judged.report.maximumMetricMicrons
                baselineProblems += judged.problems
                if judged.report.maximumMetricMicrons > metricDepthBudgetMicrons { baselineProblems.append("original: metric depth error \(judged.report.maximumMetricMicrons) um exceeds \(metricDepthBudgetMicrons) um (inherited baseline accuracy)") }
            }
            for (variant, image) in images where variant != .original {
                guard let judged = judge(variant, image) else { continue }
                oracles[variant.rawValue] = judged.report.json
                candidateProblems += judged.problems
                relativeProblems += judged.problems
                let metric = judged.report.maximumMetricMicrons
                if metric > metricDepthBudgetMicrons { candidateProblems.append("\(variant.rawValue): metric depth error \(metric) um exceeds \(metricDepthBudgetMicrons) um") }
                if metric > max(metricDepthBudgetMicrons, candidateRelativeMetricFactor * baselineMetric) { relativeProblems.append("\(variant.rawValue): metric depth error \(metric) um exceeds both the budget and \(candidateRelativeMetricFactor)x the baseline's \(baselineMetric) um") }
            }
        }

        // 4. Candidates against the baseline: exact masks at identity, exact all-fallback, bounded colour and depth.
        for variant in [Variant.adaptive, .copy] {
            guard let image = images[variant] else { continue }
            let name = variant.rawValue
            let c = compare(reference: original, candidate: image, edges: edges)
            comparisons[name] = c.json
            if fixture.copyFadeOracleOnly, variant == .copy { continue } // judged by the oracle only: geometry is source-anchored by design
            var problems = [String]()
            if identityLattice, c.coverageOnlyInCandidate + c.coverageOnlyInReference > 0 { problems.append("\(name): written-pixel mask differs at identity") }
            if c.coverageOnlyInCandidateAwayFromEdges > 0 { problems.append("\(name): novel coverage away from edges") }
            if c.coverageOnlyInReferenceAwayFromEdges > 0 { problems.append("\(name): coverage cracks away from edges") }
            if c.maximumColorDifferenceAwayFromEdges > 1 { problems.append("\(name): colour > 1 LSB away from edges") }
            if identityLattice, hasOracle, c.maximumColorDifference > 0 { problems.append("\(name): colour differs from original at identity") }
            if allFallbackExpected, variant == .adaptive, !c.bitIdentical { problems.append("adaptive: not bit-identical where every tile falls back") }
            if !hasOracle, c.maximumDepthULPAwayFromEdges > candidateVsBaselineULPGate { problems.append("\(name): depth > \(candidateVsBaselineULPGate) ULP vs baseline away from edges") }
            if hasOracle, coverageRequired, c.comparedPixels * 2 < covered { problems.append("\(name): compared only \(c.comparedPixels) of \(covered) covered pixels") }
            candidateProblems += problems
            relativeProblems += problems
        }

        // 5. Fan and seam pixels (identity lattice): judged directly, never excluded.
        if identityLattice, let classes, classes.count == fixture.tileCount, let adaptive = images[.adaptive] {
            let masks = fanAndSeamMasks(classes: classes, fixture: fixture, width: w, height: h)
            let fanPixels = masks.fan.filter { $0 }.count
            let fanCheck = compareOn(masks.fan, reference: original, candidate: adaptive)
            let seamCheck = compareOn(masks.seam, reference: original, candidate: adaptive)
            fanSeam = ["fan_pixels": fanPixels, "seam_pixels": masks.seam.filter { $0 }.count, "fan": fanCheck, "seam": seamCheck]
            var problems = [String]()
            if (fixture.expectedFanTiles ?? 0) > 0, fanPixels == 0 { problems.append("adaptive: accepted tiles cover no output pixel") }
            if fanCheck["mask_different"] as? Int != 0 { problems.append("adaptive: coverage differs inside fan tiles") }
            if seamCheck["mask_different"] as? Int != 0 { problems.append("adaptive: coverage differs on fan/fallback seams") }
            if (fanCheck["maximum_color_difference_8bit"] as? Int ?? 0) > (hasOracle ? 0 : 1) { problems.append("adaptive: colour differs inside fan tiles") }
            if (seamCheck["maximum_color_difference_8bit"] as? Int ?? 0) > (hasOracle ? 0 : 1) { problems.append("adaptive: colour differs on seams") }
            if !hasOracle, (seamCheck["maximum_depth_ulp"] as? Int ?? Int.max) > candidateVsBaselineULPGate { problems.append("adaptive: seam depth > \(candidateVsBaselineULPGate) ULP") }
            candidateProblems += problems
            relativeProblems += problems
        } else if identityLattice {
            candidateProblems.append("adaptive: fan/seam gates could not run (class readback missing)")
            relativeProblems.append("adaptive: fan/seam gates could not run (class readback missing)")
        }

        let problems = baselineProblems + candidateProblems
        let baselinePass = baselineProblems.isEmpty
        let candidateAbsolutePass = candidateProblems.isEmpty
        let candidateRelativePass = relativeProblems.isEmpty
        if !baselinePass { baselineFailures.append(fixture.name) }
        if !candidateAbsolutePass { candidateAbsoluteFailures.append(fixture.name) }
        if !candidateRelativePass { candidateRelativeFailures.append(fixture.name) }
        if !problems.isEmpty { failures.append(fixture.name) }
        var hashes = [String: String]()
        for (variant, image) in images {
            hashes["\(variant.rawValue)_color"] = sha256(image.color)
            hashes["\(variant.rawValue)_depth"] = sha256(image.depth)
            hashes["\(variant.rawValue)_mask"] = sha256(image.mask.map { $0 ? UInt8(1) : 0 })
        }
        results.append(["case": fixture.name, "source_size": [fixture.width, fixture.height], "output_size": [w, h],
                        "source_depth_reversed": fixture.reversed, "square_pixels": fixture.squarePixels, "copy_eligible": fixture.copyEligible,
                        "copy_fade_oracle_only": fixture.copyFadeOracleOnly, "identity_lattice": identityLattice,
                        "covered_pixels_original": covered, "coverage_required": coverageRequired,
                        "known_discontinuity_texels": discontinuities.filter { $0 }.count,
                        "comparisons_vs_original": comparisons, "adaptive_counters": counters, "oracle": oracles, "fan_seam": fanSeam,
                        "image_sha256": hashes, "problems": problems, "baseline_problems": baselineProblems,
                        "candidate_absolute_problems": candidateProblems, "candidate_relative_problems": relativeProblems,
                        "baseline_pass": baselinePass, "candidate_absolute_pass": candidateAbsolutePass,
                        "candidate_relative_pass": candidateRelativePass, "pass": baselinePass && candidateAbsolutePass])
    }
    return ["mode": "parity", "schema": 2, "device": rig.device.name, "sources": rig.sources, "live_shader": live,
            "uniform_stride": MemoryLayout<Uniforms>.stride, "adaptive_tolerance_ulp": rig.toleranceULPs,
            "gates": ["legacy_off": "bit-identical colour, depth and mask versus the frozen original",
                      "acceptance": "adaptive fan tiles equal the predeclared count per fixture; fan + fallback equal the tile count",
                      "oracle": "every variant on plane fixtures: zero missing and zero unexpected coverage (fade and clip predicted per pixel), no NaN/Inf/out-of-range written depth, metric depth error <= \(metricDepthBudgetMicrons) um, colour exact against source texels at scale 1 or within \(bilinearColorToleranceLSB) LSB of an independent bilinear clamp-to-edge UNORM sample otherwise (identity lattices and the copy), >= \(Int(minimumEligibleFraction * 100))% of pixels outside the footprint band judged; per-pixel depth bound = 8 ULP of the expected depth plus, under motion only, \(unexplainedDepthMultiple)x local gradient / 2^\(Int(snappingSubpixelBits))",
                      "verdicts": "baseline_pass (original against the oracle), candidate_absolute_pass (candidates against absolute budgets), candidate_relative_pass (metric within max(budget, \(candidateRelativeMetricFactor)x baseline)); a fixture and the run pass only when baseline and candidates pass",
                      "candidates_vs_baseline": "exact masks at identity, exact colour at identity on plane fixtures, <= 1 LSB away from edges otherwise, zero novel coverage or cracks away from edges, bit-identical where every tile falls back, <= \(candidateVsBaselineULPGate) depth ULP without an oracle (diagnostic with one)",
                      "fan_seam": "fan and seam pixels judged directly on identity lattices: identical coverage, colour as above, seam depth <= \(candidateVsBaselineULPGate) ULP without an oracle",
                      "edges": "fixture-known discontinuities dilated one texel plus the reference coverage border; never rendered-depth deltas; seams are not edges",
                      "overrides": "none: every gate is a source constant"],
            "fixtures": results, "fixture_count": results.count, "failures": failures,
            "baseline_failures": baselineFailures, "candidate_absolute_failures": candidateAbsoluteFailures, "candidate_relative_failures": candidateRelativeFailures,
            "verdict": failures.isEmpty ? "all fixtures pass for baseline and candidates"
                : !baselineFailures.isEmpty && candidateRelativeFailures.isEmpty ? "NOT a pass: inherited baseline accuracy failures on \(baselineFailures); candidates are within 10% of the baseline metric error, with common regression guards passing on every fixture"
                : "NOT a pass: baseline failures \(baselineFailures); candidate failures \(candidateAbsoluteFailures)",
            "pass": failures.isEmpty,
            "limits": "macOS Metal device only. Synthetic fixtures. No visionOS compositor, foveation, rasterization-rate map or headset. The copy path is judged for conversion only; the system's display-time pose adjustment is not exercised here. No sub-pixel raster precision is guaranteed by any document; the 2^-8 pixel model is a hypothesis the fixtures test."]
}

// ---- Timing ----

func streetDepths(width w: Int, height h: Int, reversed: Bool) -> [Float] {
    var out = [Float](repeating: reversed ? 0 : 1, count: w * h)
    let ground = Plane(distance: 8, ax: 0, ay: 1.6)
    for y in 0..<h {
        for x in 0..<w {
            var plane: Plane? = y > h * 55 / 100 ? ground : nil
            let column = (x * 9) / w
            let buildingTop = h * (25 + (column * 7) % 20) / 100
            if y > buildingTop, y <= h * 55 / 100 { plane = Plane(distance: Float(12 + (column * 5) % 23)) }
            out[y * w + x] = plane.map { planeDepth($0, x: x, y: y, width: w, height: h, reversed: reversed) } ?? (reversed ? 0 : 1)
        }
    }
    return out
}

func sceneDepths(_ scene: String, width w: Int, height h: Int) -> [Float] {
    switch scene {
    case "street": return streetDepths(width: w, height: h, reversed: true)
    case "plane": return depths(Fixture(name: scene, field: .plane(Plane(distance: 4, ax: 0.1, ay: 0.3)), width: w, height: h))
    case "sky": return [Float](repeating: 0, count: w * h)
    default: return depths(Fixture(name: scene, field: .noise(5), width: w, height: h))
    }
}

func summary(_ samples: [Double]) -> [String: Any] {
    let sorted = samples.sorted()
    guard !sorted.isEmpty else { return [:] }
    return ["samples": sorted.count, "median_ms": sorted[sorted.count / 2], "mean_ms": sorted.reduce(0, +) / Double(sorted.count),
            "minimum_ms": sorted.first!, "maximum_ms": sorted.last!, "p95_ms": sorted[min(sorted.count - 1, Int(Double(sorted.count) * 0.95))]]
}

func runTiming(candidatePath: String, baselinePath: String, outDir: String) throws -> [String: Any] {
    let rig = try Rig(candidatePath: candidatePath, baselinePath: baselinePath)
    try FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)
    let live = try liveShaderCheck(candidatePath: candidatePath)
    let pairs = Int(ProcessInfo.processInfo.environment["HARNESS_PAIRS"] ?? "") ?? 20
    let warmups = Int(ProcessInfo.processInfo.environment["HARNESS_WARMUPS"] ?? "") ?? 4
    let scenes = (ProcessInfo.processInfo.environment["HARNESS_SCENES"] ?? "street,plane,sky,noise").split(separator: ",").map(String.init)
    let sizes: [(Int, Int)] = [(960, 540), (3840, 2160)]
    let (ow, oh) = (3840, 2160)
    var runs = [[String: Any]]()
    var failures = [String]()
    for (sw, sh) in sizes {
        for scene in scenes {
            let fixture = Fixture(name: scene, field: .plane(Plane(distance: 4)), width: sw, height: sh)
            let uniforms = Uniforms(sourceEyeFromClip: projection(true).inverse, currentEyeFromSourceEye: matrix_identity_float4x4,
                                    currentClipFromEye: projection(true), sourceSize: SIMD2(UInt32(sw), UInt32(sh)), sourceDepthReversed: 1)
            let depthField = sceneDepths(scene, width: sw, height: sh)
            let source = try rig.source(depths: depthField, colors: colors(fixture), width: sw, height: sh, uniforms: uniforms)
            let target = try rig.target(width: ow, height: oh)
            var variants: [Variant] = [.original, .legacy, .adaptive]
            if sw == ow, sh == oh { variants.append(.copy) }
            // Image check before timing: candidates against the original under the parity gates.
            var reference: Image?
            var imageChecks = [String: Any]()
            for variant in variants {
                _ = try rig.render(variant, source: source, target: target)
                let image = rig.image(target)
                if variant == .original { reference = image; continue }
                guard let reference else { continue }
                let discontinuities = fieldDiscontinuityTexels(depths: depthField, width: sw, height: sh, reversed: true)
                let identity = sw == ow && sh == oh
                let edges = knownEdgeMask(reference: reference, sourceDiscontinuities: discontinuities, sourceWidth: sw, sourceHeight: sh,
                                          scale: ow / sw, identityLattice: identity)
                let c = compare(reference: reference, candidate: image, edges: edges)
                var check = c.json
                var ok = variant == .legacy ? c.bitIdentical
                    : c.maximumColorDifferenceAwayFromEdges <= 1 && c.coverageOnlyInCandidateAwayFromEdges == 0 && c.coverageOnlyInReferenceAwayFromEdges == 0
                if variant != .legacy {
                    if identity, c.coverageOnlyInCandidate + c.coverageOnlyInReference > 0 { ok = false }
                    if scene == "plane" {
                        // Sloping plane: judge against the Double oracle under the metric budget, as in parity.
                        let planeFixture = Fixture(name: scene, field: .plane(Plane(distance: 4, ax: 0.1, ay: 0.3)), width: sw, height: sh, scale: ow / sw)
                        if let report = analyticOracle(planeFixture, image: image, sourceColors: colors(planeFixture), geometryTransform: matrix_identity_float4x4,
                                                       fadeTransform: matrix_identity_float4x4, edges: [Bool](repeating: false, count: ow * oh), judgeColorAgainstSource: identity) {
                            check["oracle"] = report.json
                            if report.missing > 0 || report.unexpected > 0 || report.invalidWrittenDepth > 0 || report.maximumMetricMicrons > metricDepthBudgetMicrons || report.colorMismatches > 0 || report.pixelsBeyondLocalBound > 0 { ok = false }
                        }
                    } else if c.maximumDepthULPAwayFromEdges > candidateVsBaselineULPGate { ok = false }
                }
                imageChecks[variant.rawValue] = check
                if !ok { failures.append("\(scene) \(sw)x\(sh) \(variant.rawValue) image") }
            }
            for _ in 0..<warmups { for variant in variants { _ = try rig.render(variant, source: source, target: target) } }
            var samples = [Variant: [Double]]()
            var fanCounts = [UInt32](), fallbackCounts = [UInt32]()
            for iteration in 0..<pairs {
                let order = iteration % 2 == 0 ? variants : variants.reversed()
                for variant in order {
                    let result = try rig.render(variant, source: source, target: target)
                    samples[variant, default: []].append(result.ms)
                    if variant == .adaptive { fanCounts.append(result.fan ?? 0); fallbackCounts.append(result.fallback ?? 0) }
                }
            }
            var gpu = [String: Any]()
            var raw = [String: Any]()
            for (variant, values) in samples { gpu[variant.rawValue] = summary(values); raw[variant.rawValue] = values }
            runs.append(["scene": scene, "source_size": [sw, sh], "output_size": [ow, oh], "paired_iterations": pairs, "warmup_iterations": warmups,
                         "variants": variants.map(\.rawValue), "gpu_ms": gpu, "raw_gpu_ms": raw, "image_checks": imageChecks,
                         "adaptive_fan_tiles": fanCounts, "adaptive_fallback_tiles": fallbackCounts,
                         "tile_count": AdaptiveScratchPool.tileCount(width: sw, height: sh).map { [Int($0.x), Int($0.y)] } ?? [],
                         "requested_legacy_vertices": sw * sh * 4, "requested_legacy_triangles": sw * sh * 2,
                         "source_depth_sha256": sha256(depthField)])
        }
    }
    if live["candidate_matches_live"] as? Bool != true { failures.append("stale_candidate_shader") }
    return ["mode": "timing", "schema": 2, "device": rig.device.name, "sources": rig.sources, "live_shader": live, "adaptive_tolerance_ulp": rig.toleranceULPs,
            "measure": "MTLCommandBuffer gpuEndTime - gpuStartTime of one waited command buffer at a time; adaptive includes its classification and counter copy; raw per-sample durations retained",
            "runs": runs, "failures": failures, "pass": failures.isEmpty,
            "limits": "Synthetic depth fields on this macOS GPU. Not game frame rate, not the visionOS compositor, not a headset."]
}

// ---- Self-check (CPU only) ----

func selfCheck() -> [String: Any] {
    var problems = [String]()
    func expect(_ condition: Bool, _ message: String) { if !condition { problems.append(message) } }
    // Fan perimeter mapping mirrors gameWorldFanPerimeter.
    func perimeter(_ s: Int) -> (Int, Int) { s < 8 ? (s, 0) : s < 16 ? (8, s - 8) : s < 24 ? (24 - s, 8) : (0, 32 - s) }
    expect(perimeter(0) == (0, 0) && perimeter(8) == (8, 0) && perimeter(16) == (8, 8) && perimeter(24) == (0, 8) && perimeter(31) == (0, 1), "fan perimeter mapping")
    var visited = Set<String>()
    for s in 0..<32 { let p = perimeter(s); visited.insert("\(p.0),\(p.1)") }
    expect(visited.count == 32, "fan perimeter covers 32 distinct corners")
    // Pool bounds.
    expect(AdaptiveScratchPool.tileCount(width: 3840, height: 2160) == SIMD2<UInt32>(480, 270), "4K tile count")
    expect(AdaptiveScratchPool.tileCount(width: 960, height: 540) == SIMD2<UInt32>(120, 68), "Low tile count includes the partial row")
    expect(AdaptiveScratchPool.tileCount(width: 2, height: 2) == nil, "too small rejected")
    expect(AdaptiveScratchPool.tileCount(width: 16_384, height: 16_384) == nil, "metadata bound rejected")
    // Pose check: identity pose and request agreement pass; mismatches fail for the right reason.
    let stageFromHead = motion(yaw: 12, x: 0.3, y: 1.6)
    let headFromEye = motion(x: -0.032)
    let eyes = [XRRetainedEyeView(headFromEye: headFromEye, projectionFromEye: projection(true)),
                XRRetainedEyeView(headFromEye: motion(x: 0.032), projectionFromEye: projection(true))]
    let gameFromStage = motion(yaw: -40, x: 120, y: 5, z: -300)
    let gameFromEye = gameFromStage * stageFromHead * headFromEye
    let ok = GameWorldSourcePoseCheck.verify(retainedStageFromHead: stageFromHead, retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 0, requestFrame: 77,
                                             leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(true), leaseDepthReversed: true)
    expect(ok.passed, "pose check passes on consistent inputs (\(ok.reason) r=\(ok.maximumRotationError) t=\(ok.maximumTranslationError) p=\(ok.maximumProjectionError))")
    let forward = GameWorldSourcePoseCheck.verify(retainedStageFromHead: stageFromHead, retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 0, requestFrame: 77,
                                                  leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(false), leaseDepthReversed: false)
    expect(forward.passed, "pose check accepts the forward-Z depth-row flip (\(forward.reason) p=\(forward.maximumProjectionError))")
    let wrongRequest = GameWorldSourcePoseCheck.verify(retainedStageFromHead: stageFromHead, retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 0, requestFrame: 78,
                                                       leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(true), leaseDepthReversed: true)
    expect(!wrongRequest.passed && wrongRequest.reason == "request_frame_mismatch", "pose check rejects a different request frame")
    let wrongPose = GameWorldSourcePoseCheck.verify(retainedStageFromHead: motion(yaw: 13, x: 0.3, y: 1.6), retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 0, requestFrame: 77,
                                                    leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(true), leaseDepthReversed: true)
    expect(!wrongPose.passed && wrongPose.reason == "rotation_mismatch", "pose check rejects a different head pose")
    let wrongEye = GameWorldSourcePoseCheck.verify(retainedStageFromHead: stageFromHead, retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 1, requestFrame: 77,
                                                   leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(true), leaseDepthReversed: true)
    expect(!wrongEye.passed && wrongEye.reason == "translation_mismatch", "pose check rejects the other eye's transform")
    expect(!GameWorldSourcePoseCheck.verify(retainedStageFromHead: stageFromHead, retainedEyes: eyes, retainedRequestFrame: 77, eyeIndex: 2, requestFrame: 77,
                                            leaseGameFromStage: gameFromStage, leaseGameFromEye: gameFromEye, leaseProjection: projection(true), leaseDepthReversed: true).passed, "pose check rejects an unretained eye index")
    // Lattice and eye-transform check.
    let viewport = MTLViewport(originX: 0, originY: 0, width: 1920, height: 1824, znear: 0, zfar: 1)
    let eye = motion(x: -0.032)
    func lattice(_ w: Int, _ h: Int, _ vp: MTLViewport, _ src: simd_float4x4, _ reversed: Bool, _ dst: simd_float4x4, current: simd_float4x4 = motion(x: -0.032)) -> GameWorldCopyLatticeCheck.Result {
        GameWorldCopyLatticeCheck.verify(sourceWidth: w, sourceHeight: h, viewport: vp, sourceProjection: src, sourceDepthReversed: reversed,
                                         targetProjection: dst, retainedHeadFromEye: eye, currentHeadFromEye: current)
    }
    expect(lattice(1920, 1824, viewport, projection(true), true, projection(true)).passed, "lattice passes identical")
    expect(lattice(1920, 1824, viewport, projection(false), false, projection(true)).passed, "lattice accepts forward-Z source with the flipped depth row")
    expect(lattice(3840, 2160, viewport, projection(true), true, projection(true)).reason == "extent_mismatch", "lattice rejects a different extent")
    expect(lattice(1920, 1824, MTLViewport(originX: 0.5, originY: 0, width: 1920, height: 1824, znear: 0, zfar: 1), projection(true), true, projection(true)).reason == "viewport_origin_fractional", "lattice rejects a fractional origin")
    expect(lattice(1920, 1824, viewport, projection(true, near: 0.1), true, projection(true)).reason == "projection_mismatch", "lattice rejects a different depth range")
    expect(lattice(1920, 1824, viewport, projection(true), true, projection(true), current: motion(x: -0.033)).reason == "eye_transform_mismatch", "lattice rejects a 1 mm eye translation change (IPD)")
    expect(lattice(1920, 1824, viewport, projection(true), true, projection(true), current: motion(yaw: 0.01, x: -0.032)).reason == "eye_transform_mismatch", "lattice rejects a 0.01 degree eye rotation change")
    expect(lattice(1920, 1824, viewport, projection(true), true, projection(true), current: motion(x: -0.032 + 5e-5)).passed, "lattice tolerates 0.05 mm eye jitter")
    // Pair validation on the imported header type.
    expect(MemoryLayout<SVWorldPair>.size == 592 && MemoryLayout<SVWorldFrame>.size == 288, "SVWorldPair is 592 bytes and SVWorldFrame 288")
    func frame(_ view: UInt32, lease: UInt64, head: UInt64 = 5, request: UInt64 = 9) -> SVWorldFrame {
        var f = SVWorldFrame()
        f.abi_version = 1; f.byte_size = UInt32(MemoryLayout<SVWorldFrame>.size); f.lease = lease; f.game_frame = 100
        f.request_frame = request; f.head_frame = head; f.presentation_time = 12.5; f.view_index = view; f.view_count = 2
        f.width = 3840; f.height = 2160; f.depth_convention = 0; f.reserved = 0
        f.game_from_stage = (1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)
        return f
    }
    var pair = SVWorldPair()
    pair.abi_version = 1; pair.byte_size = UInt32(MemoryLayout<SVWorldPair>.size); pair.scene_epoch = 42
    pair.eyes = (frame(0, lease: 1), frame(1, lease: 2))
    expect(GameWorldPairValidation.reject(pair) == nil, "valid pair accepted (\(GameWorldPairValidation.reject(pair) ?? "nil"))")
    var zeroEpoch = pair; zeroEpoch.scene_epoch = 0
    expect(GameWorldPairValidation.reject(zeroEpoch) == "pair_scene_epoch_zero", "zero scene epoch rejected")
    var shared = pair; shared.eyes.1.lease = 1
    expect(GameWorldPairValidation.reject(shared) == "pair_shared_lease", "shared lease rejected")
    var mixedHead = pair; mixedHead.eyes.1.head_frame = 6
    expect(GameWorldPairValidation.reject(mixedHead) == "pair_head_frame", "mixed head frames rejected")
    var mixedRequest = pair; mixedRequest.eyes.1.request_frame = 10
    expect(GameWorldPairValidation.reject(mixedRequest) == "pair_request_frame", "mixed request frames rejected")
    var swapped = pair; swapped.eyes = (frame(1, lease: 1), frame(0, lease: 2))
    expect(GameWorldPairValidation.reject(swapped) == "eye0_view_identity", "swapped eye indices rejected")
    // Fixture table sanity: every fixture generates depths of the right count, and moved-camera fixtures carry no
    // fixture-known discontinuities (their edges come only from the coverage border), except sky which draws nothing.
    for fixture in fixtures {
        let field = depths(fixture)
        expect(field.count == fixture.width * fixture.height, "depth count \(fixture.name)")
        if fixture.transform != matrix_identity_float4x4, !fixture.copyFadeOracleOnly {
            let known = knownDiscontinuityTexels(fixture, depths: field)
            if case .sky = fixture.field {} else { expect(!known.contains(true), "moved fixture \(fixture.name) has fixture-known discontinuities") }
        }
    }
    expect(knownDiscontinuityTexels(Fixture(name: "t", field: .stepAt(Plane(distance: 3), Plane(distance: 6), 36)), depths: []).filter { $0 }.count == 64 * 4, "step discontinuity width is four texels after dilation")
    expect(!knownDiscontinuityTexels(Fixture(name: "t", field: .plane(Plane(distance: 3, ax: 0.35))), depths: []).contains(true), "a sloping plane has no discontinuities")
    expect(fixtures.allSatisfy { $0.expectedFanTiles != nil }, "every parity fixture predeclares its fan-tile count")
    expect(fixtures.allSatisfy { ($0.expectedFanTiles ?? 0) <= $0.tileCount }, "predeclared fan tiles never exceed the tile count")
    let proj = doubleMatrix(projection(true))
    expect(abs(metricDepth(ndc: ndcDepth(eyePoint: SIMD3(0, 0, -2.5), clipFromEye: proj)!, projection: proj) - 2.5) < 1e-9, "metric depth recovery")
    let border = fixtures.first { $0.name == "borders_37x23_slope_square" }!
    expect(abs(border.projectionMatrix(true).columns.0.x - 23.0 / 37.0) < 1e-6, "square-pixel aspect on the square border fixture")
    expect(fixtures.first { $0.name == "borders_37x23_slope" }!.projectionMatrix(true).columns.0.x == 1, "original border fixture keeps the unit-tangent projection")
    // Bilinear clamp-to-edge oracle: a 2x2 source sampled at texel centres returns the texel, at the midpoint the mean, and clamps at edges.
    let twoByTwo: [UInt8] = [0, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 255, 255, 255, 255]
    expect(bilinearSourceColor(sourceColors: twoByTwo, width: 2, height: 2, uv: SIMD2(0.25, 0.25)) == (0, 0, 0), "bilinear at a texel centre is that texel")
    expect(bilinearSourceColor(sourceColors: twoByTwo, width: 2, height: 2, uv: SIMD2(0.5, 0.25)) == (128, 0, 0), "bilinear midpoint blends two texels with UNORM rounding")
    expect(bilinearSourceColor(sourceColors: twoByTwo, width: 2, height: 2, uv: SIMD2(0.0, 0.0)) == (0, 0, 0), "bilinear clamps to the edge texel")
    expect(bilinearSourceColor(sourceColors: twoByTwo, width: 2, height: 2, uv: SIMD2(0.5, 0.5)) == (128, 128, 64), "bilinear centre averages four texels")
    // Scale-2 identity output pixel 0 samples 0.25/0.75 of texels 0 and 0 (clamped) in x: equals texel 0.
    let scaleTwo = bilinearSourceColor(sourceColors: twoByTwo, width: 2, height: 2, uv: SIMD2(0.5 / 4, 0.5 / 4))
    expect(scaleTwo == (0, 0, 0), "scale-2 corner pixel is the clamped corner texel")
    expect(fixtures.contains { $0.name == "borders_37x23_slope" && !$0.squarePixels } && fixtures.contains { $0.name == "borders_37x23_slope_square" && $0.squarePixels }, "both border fixtures present")
    let tiny = Fixture(name: "t", field: .plane(Plane(distance: 3)), width: 16, height: 16, scale: 2)
    let masks = fanAndSeamMasks(classes: [1, 0, 0, 0], fixture: tiny, width: 32, height: 32)
    expect(masks.fan.filter { $0 }.count == 256, "fan mask covers the accepted tile")
    expect(masks.seam.filter { $0 }.count > 0 && masks.seam[0] && !masks.seam[5 * 32 + 5], "seam mask marks the tile boundary only")
    return ["mode": "selfcheck", "gpu_used": false, "fixtures": fixtures.count, "copy_eligible_fixtures": fixtures.filter(\.copyEligible).count,
            "uniform_stride": MemoryLayout<Uniforms>.stride, "copy_uniform_stride": MemoryLayout<CopyUniforms>.stride,
            "adaptive_params_stride": MemoryLayout<AdaptiveParams>.stride, "pool_maximum_sets": AdaptiveScratchPool.maximumSets,
            "pool_maximum_bytes_per_set": AdaptiveScratchPool.maximumBytesPerSet, "problems": problems, "pass": problems.isEmpty]
}

// ---- Entry ----

func emit(_ value: [String: Any], to path: String?) {
    let data = (try? JSONSerialization.data(withJSONObject: value, options: [.prettyPrinted, .sortedKeys])) ?? Data("{}".utf8)
    if let path { try? data.write(to: URL(fileURLWithPath: path)) }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([10]))
}

let arguments = CommandLine.arguments
let mode = arguments.count > 1 ? arguments[1] : ""
let outputPath: String? = arguments.count == 5 && (mode == "parity" || mode == "timing") ? arguments[4] + "/\(mode).json" : nil
do {
    switch mode {
    case "selfcheck":
        let result = selfCheck()
        emit(result, to: nil)
        exit(result["pass"] as? Bool == true ? 0 : 1)
    case "parity" where arguments.count == 5:
        let result = try runParity(candidatePath: arguments[2], baselinePath: arguments[3], outDir: arguments[4])
        emit(result, to: outputPath)
        exit(result["pass"] as? Bool == true ? 0 : 1)
    case "timing" where arguments.count == 5:
        let result = try runTiming(candidatePath: arguments[2], baselinePath: arguments[3], outDir: arguments[4])
        emit(result, to: outputPath)
        exit(result["pass"] as? Bool == true ? 0 : 1)
    default:
        FileHandle.standardError.write(Data("usage: harness selfcheck | parity <candidate.metal> <baseline.metal> <out-dir> | timing <candidate.metal> <baseline.metal> <out-dir>\n".utf8))
        exit(2)
    }
} catch {
    // Fail closed: an aborted run still leaves a JSON that says it did not pass.
    emit(["mode": mode, "pass": false, "aborted": true, "error": "\(error)"], to: outputPath)
    FileHandle.standardError.write(Data("harness error: \(error)\n".utf8))
    exit(3)
}

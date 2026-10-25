// Synthetic offscreen Mac Metal. No captured media, simulator or headset.
import Foundation
import Metal
import simd
import Darwin

func require(_ value: @autoclosure () -> Bool, _ message: String) {
    if !value() { fatalError(message) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let output = URL(fileURLWithPath: CommandLine.arguments[2])
let baselineURL = URL(fileURLWithPath: CommandLine.arguments[3])
let timingMode = CommandLine.arguments.contains("--representative-timing")
let targetColorFormat: MTLPixelFormat = timingMode ? .rgba8Unorm_srgb : .rgba8Unorm
let device = MTLCreateSystemDefaultDevice()!
let queue = device.makeCommandQueue()!
let candidateText = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
let baselineText = try String(contentsOf: baselineURL, encoding: .utf8)
let options = MTLCompileOptions()
let candidateLibrary = try device.makeLibrary(source: candidateText, options: options)
let baselineLibrary = try device.makeLibrary(source: baselineText, options: options)
require(MemoryLayout<GameWorldUniforms>.stride == 224, "World uniforms ABI changed")

func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int, _ usage: MTLTextureUsage) -> any MTLTexture {
    let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
    d.storageMode = .shared
    d.usage = usage
    return device.makeTexture(descriptor: d)!
}
func pipeline(_ library: any MTLLibrary, precomputed: Bool) throws -> any MTLRenderPipelineState {
    let d = MTLRenderPipelineDescriptor()
    d.vertexFunction = library.makeFunction(name: precomputed ? "gameWorldPrecomputedVertex" : "gameWorldVertex")!
    d.fragmentFunction = library.makeFunction(name: "gameWorldSkyOwnershipFragment")!
    d.colorAttachments[0].pixelFormat = targetColorFormat
    d.colorAttachments[1].pixelFormat = .r8Uint
    d.depthAttachmentPixelFormat = .depth32Float
    return try device.makeRenderPipelineState(descriptor: d)
}
let oldPipeline = try pipeline(baselineLibrary, precomputed: false)
let newPipeline = try pipeline(candidateLibrary, precomputed: false)
let precomputedPipeline = try pipeline(candidateLibrary, precomputed: true)
let cornerKernel = try device.makeComputePipelineState(function: candidateLibrary.makeFunction(name: "gameWorldCornerDepths")!)
let depthDescriptor = MTLDepthStencilDescriptor()
depthDescriptor.depthCompareFunction = .greaterEqual
depthDescriptor.isDepthWriteEnabled = true
let depthState = device.makeDepthStencilState(descriptor: depthDescriptor)!

let sw = timingMode ? 2560 : 384, sh = timingMode ? 1440 : 320
let focal = timingMode ? SIMD2<Float>(0.5341740847, 0.665856123) : SIMD2<Float>(0.8, 1.0)
let near: Float = 0.05, far: Float = 10000
func projection(reversed: Bool, display: Bool = false) -> simd_float4x4 {
    let f = timingMode && display ? SIMD2<Float>(0.7320510745, 0.9125128984) : focal
    let a = reversed ? near / (far - near) : -far / (far - near)
    let b = reversed ? near * far / (far - near) : -near * far / (far - near)
    return simd_float4x4(columns: (SIMD4(f.x, 0, 0, 0), SIMD4(0, f.y, 0, 0),
        SIMD4(0, 0, a, -1), SIMD4(0, 0, b, 0)))
}
func depthValue(_ distance: Float, reversed: Bool) -> Float {
    let p = projection(reversed: reversed) * SIMD4<Float>(0, 0, -distance, 1)
    return p.z / p.w
}
func linearDepth(_ value: Float, projection: simd_float4x4) -> Float {
    let p = projection.inverse * SIMD4<Float>(0, 0, value, 1)
    return -p.z / p.w
}
func sourceRay(x: Float, y: Float, width: Int, height: Int) -> SIMD3<Float> {
    SIMD3((2 * x / Float(width) - 1) / focal.x, (1 - 2 * y / Float(height)) / focal.y, -1)
}
func smoothHit(_ kind: String, origin: SIMD3<Float>, direction: SIMD3<Float>) -> Float? {
    let center = SIMD3<Float>(0, 0, -1)
    let r: Float = 0.35
    let o = origin - center
    let a: Float, b: Float, c: Float
    if kind == "cylinder" {
        a = direction.x * direction.x + direction.z * direction.z
        b = o.x * direction.x + o.z * direction.z
        c = o.x * o.x + o.z * o.z - r * r
    } else {
        a = simd_dot(direction, direction)
        b = simd_dot(o, direction)
        c = simd_dot(o, o) - r * r
    }
    let d = b * b - a * c
    guard d > 0 else { return nil }
    let t = (-b - sqrt(d)) / a
    return t > 0 ? t : nil
}
struct Fixture {
    let name: String
    let color: any MTLTexture
    let depth: any MTLTexture
    let rgba: [UInt8]
    let smooth: Bool
}
func fixture(_ name: String, reversed: Bool) -> Fixture {
    var rgba = [UInt8](repeating: 255, count: sw * sh * 4)
    var depths = [Float](repeating: depthValue(3, reversed: reversed), count: sw * sh)
    let smooth = name == "sphere" || name == "cylinder"
    for y in 0..<sh { for x in 0..<sw {
        let i = y * sw + x
        let ray = sourceRay(x: Float(x) + 0.5, y: Float(y) + 0.5, width: sw, height: sh)
        var front = false
        if smooth, let t = smoothHit(name, origin: .zero, direction: ray) {
            depths[i] = depthValue(t, reversed: reversed)
            front = true
        } else if name == "depth_step" {
            front = x < sw / 2
            depths[i] = depthValue(front ? 0.2 : 8, reversed: reversed)
        } else if name == "shallow_step" {
            front = x < sw / 2
            depths[i] = depthValue(front ? 1 : 1.005, reversed: reversed)
        } else if name == "slender_pole" {
            front = x == sw / 2 || x == sw / 2 + 1
            depths[i] = depthValue(front ? 0.7 : 3, reversed: reversed)
        } else if name == "sky_invalid" {
            front = x < sw / 2
            depths[i] = front ? depthValue(1, reversed: reversed) : (reversed ? 0 : 1)
            if x >= 40 && x < 44 && y >= 40 && y < 100 { depths[i] = .nan }
        } else if name == "affine_plane" {
            // NDC affine surface, intentionally not constant metric depth.
            depths[i] = depthValue(2, reversed: reversed) + Float(x) * 0.000001 + Float(y) * 0.0000005
        }
        // Include true black/dark source surfaces; ownership never derives from RGB.
        let checker = ((x / 6) + (y / 6)) & 1
        let c: SIMD3<UInt8> = front
            ? (checker == 0 ? SIMD3(0, 0, 0) : SIMD3(18, 34, 55))
            : SIMD3(UInt8(70 + x * 80 / sw), UInt8(95 + y * 60 / sh), 75)
        for channel in 0..<3 { rgba[i * 4 + channel] = c[channel] }
    }}
    let color = texture(.rgba8Unorm, sw, sh, .shaderRead)
    let depth = texture(.r32Float, sw, sh, .shaderRead)
    rgba.withUnsafeBytes { color.replace(region: MTLRegionMake2D(0, 0, sw, sh), mipmapLevel: 0,
        withBytes: $0.baseAddress!, bytesPerRow: sw * 4) }
    depths.withUnsafeBytes { depth.replace(region: MTLRegionMake2D(0, 0, sw, sh), mipmapLevel: 0,
        withBytes: $0.baseAddress!, bytesPerRow: sw * 4) }
    return Fixture(name: name, color: color, depth: depth, rgba: rgba, smooth: smooth)
}
struct Rendered {
    let rgba: [UInt8]
    let depth: [Float]
    let owner: [UInt8]
    let milliseconds: Double
}
func render(_ fixture: Fixture, reversed: Bool, shift: SIMD3<Float>, yaw: Float,
            width: Int, height: Int, enabled: Bool, old: Bool = false, precomputed: Bool = false) -> Rendered {
    let color = texture(targetColorFormat, width, height, [.renderTarget, .shaderRead])
    let depth = texture(.depth32Float, width, height, [.renderTarget, .shaderRead])
    let owner = texture(.r8Uint, width, height, [.renderTarget, .shaderRead])
    var sourceFromCurrent = simd_float4x4(simd_quatf(angle: yaw, axis: SIMD3(0, 1, 0)))
    sourceFromCurrent.columns.3 = SIMD4(shift, 1)
    var uniforms = GameWorldUniforms(sourceEyeFromClip: projection(reversed: reversed).inverse,
        currentEyeFromSourceEye: sourceFromCurrent.inverse, currentClipFromEye: projection(reversed: true, display: true),
        sourceSize: SIMD2(UInt32(sw), UInt32(sh)))
    uniforms.sourceDepthReversed = reversed ? 1 : 0
    uniforms.colorTransfer = timingMode ? 2 : 0
    uniforms.reserved.x = enabled ? 1 : 0
    let commands = queue.makeCommandBuffer()!
    var corners: (any MTLTexture)?
    if precomputed {
        let t = texture(.r32Float, sw + 1, sh + 1, [.shaderRead, .shaderWrite])
        let encoder = commands.makeComputeCommandEncoder()!
        encoder.setComputePipelineState(cornerKernel)
        encoder.setBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setTexture(fixture.depth, index: 0)
        encoder.setTexture(t, index: 1)
        encoder.dispatchThreads(MTLSize(width: sw + 1, height: sh + 1, depth: 1),
            threadsPerThreadgroup: MTLSize(width: 16, height: 16, depth: 1))
        encoder.endEncoding()
        corners = t
    }
    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = color
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1)
    pass.colorAttachments[1].texture = owner
    pass.colorAttachments[1].loadAction = .clear
    pass.colorAttachments[1].storeAction = .store
    pass.depthAttachment.texture = depth
    pass.depthAttachment.loadAction = .clear
    pass.depthAttachment.storeAction = .store
    pass.depthAttachment.clearDepth = 0
    let encoder = commands.makeRenderCommandEncoder(descriptor: pass)!
    encoder.setViewport(MTLViewport(originX: 0, originY: 0, width: Double(width), height: Double(height), znear: 0, zfar: 1))
    encoder.setRenderPipelineState(old ? oldPipeline : (precomputed ? precomputedPipeline : newPipeline))
    encoder.setDepthStencilState(depthState)
    encoder.setCullMode(.none)
    encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
    encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
    encoder.setVertexTexture(fixture.depth, index: 0)
    if let corners { encoder.setVertexTexture(corners, index: 1) }
    encoder.setFragmentTexture(fixture.color, index: 0)
    encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4, instanceCount: sw * sh)
    encoder.endEncoding()
    commands.commit()
    commands.waitUntilCompleted()
    require(commands.status == .completed, "Actual Metal command failed: \(String(describing: commands.error))")
    var rgb = [UInt8](repeating: 0, count: width * height * 4)
    var z = [Float](repeating: 0, count: width * height)
    var owners = [UInt8](repeating: 0, count: width * height)
    rgb.withUnsafeMutableBytes { color.getBytes($0.baseAddress!, bytesPerRow: width * 4, from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
    z.withUnsafeMutableBytes { depth.getBytes($0.baseAddress!, bytesPerRow: width * 4, from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
    owners.withUnsafeMutableBytes { owner.getBytes($0.baseAddress!, bytesPerRow: width, from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
    return Rendered(rgba: rgb, depth: z, owner: owners, milliseconds: (commands.gpuEndTime - commands.gpuStartTime) * 1000)
}
func identical(_ a: Rendered, _ b: Rendered) -> Bool {
    a.rgba == b.rgba && a.owner == b.owner && zip(a.depth, b.depth).allSatisfy { $0.bitPattern == $1.bitPattern }
}
func ppm(_ name: String, rgba: [UInt8], width: Int, height: Int, crop: Bool = false) throws {
    let cw = crop ? 160 : width, ch = crop ? 160 : height
    let ox = crop ? (width - cw) / 2 : 0, oy = crop ? (height - ch) / 2 : 0
    var data = Data("P6\n\(cw) \(ch)\n255\n".utf8)
    for y in oy..<(oy + ch) { for x in ox..<(ox + cw) {
        let i = (y * width + x) * 4
        data.append(contentsOf: rgba[i..<(i + 3)])
    }}
    try data.write(to: output.appendingPathComponent(name + ".ppm"))
}

var cases = [[String: Any]]()
if timingMode {
    func median(_ a: [Double]) -> Double { let s = a.sorted(); return (s[(s.count - 1) / 2] + s[s.count / 2]) * 0.5 }
    for reversed in [false, true] {
        for name in ["sphere", "cylinder"] {
            let f = fixture(name, reversed: reversed)
            let shift = SIMD3<Float>(0.03, -0.01, 0)
            let yaw: Float = 0.025
            let width = 1888, height = 1792
            let old = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false, old: true)
            let disabled = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false)
            require(identical(old, disabled), "Representative geometry changed baseline with reserved.x=0")
            // Four warm paired iterations, then twelve alternating old/new pairs.
            for i in 0..<4 {
                _ = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: i % 2 == 0, old: i % 2 != 0)
                _ = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: i % 2 != 0, old: i % 2 == 0)
            }
            var originalTimes = [Double](), candidateTimes = [Double]()
            for i in 0..<12 {
                let a: Rendered, b: Rendered
                if i % 2 == 0 {
                    a = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false, old: true)
                    b = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: true)
                } else {
                    b = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: true)
                    a = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false, old: true)
                }
                originalTimes.append(a.milliseconds); candidateTimes.append(b.milliseconds)
            }
            cases.append(["fixture": name, "source_depth_reversed": reversed,
                "disabled_color_depth_owner_bit_identical": true,
                "old_gpu_ms": originalTimes, "candidate_gpu_ms": candidateTimes,
                "old_median_ms": median(originalTimes), "candidate_median_ms": median(candidateTimes),
                "median_paired_delta_ms": median(zip(originalTimes, candidateTimes).map { $1 - $0 })])
        }
    }
    let result: [String: Any] = ["passed": true, "device": device.name,
        "scope": "Warmed paired offscreen Mac Metal synthetic scenes; excludes native guest rendering, finite-sky repair/overlays, and system composition; not headset performance",
        "source_size": [sw, sh], "display_size": [1888, 1792], "source_focal": [focal.x, focal.y],
        "centered_display_focal": [0.7320510745, 0.9125128984], "near_far_m": [near, far],
        "color_transfer": 2, "output_format": "rgba8Unorm_srgb", "warm_pairs_per_case": 4,
        "measured_pairs_per_case": 12, "cases": cases]
    try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
        .write(to: output.appendingPathComponent("timing-result.json"))
    for c in cases { print("TIMING \(c["fixture"]!) reverse=\(c["source_depth_reversed"]!) old=\(c["old_median_ms"]!) candidate=\(c["candidate_median_ms"]!) paired_delta=\(c["median_paired_delta_ms"]!) ms") }
    exit(0)
}
var smoothOldHoles = 0, smoothNewHoles = 0
let poses: [(String, SIMD3<Float>, Float)] = [
    ("left", SIMD3(-0.03, 0.01, 0), 0),
    ("right_yaw", SIMD3(0.03, -0.01, 0), 0.025),
    ("rotation", .zero, 0.04)]
for reversed in [false, true] {
    for name in ["sphere", "cylinder", "affine_plane", "depth_step", "shallow_step", "slender_pole", "sky_invalid"] {
        let f = fixture(name, reversed: reversed)
        let identityOld = render(f, reversed: reversed, shift: .zero, yaw: 0, width: sw, height: sh, enabled: false, old: true)
        let identityNew = render(f, reversed: reversed, shift: .zero, yaw: 0, width: sw, height: sh, enabled: true)
        let invalid = name == "sky_invalid"
        require(identityNew.rgba == identityOld.rgba && identityNew.owner == identityOld.owner,
                "\(name): candidate changed same-eye source color or ownership")
        if !invalid {
            require(identityOld.rgba == f.rgba, "\(name): old same-eye source-color identity failed")
            require(identityNew.rgba == f.rgba, "\(name): candidate same-eye source-color identity failed")
        }
        for (pose, shift, yaw) in poses {
            let width = sw * 2, height = sh * 2
            let old = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false, old: true)
            let disabled = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: false)
            require(identical(old, disabled), "\(name): reserved.x=0 changed original color/depth/ownership")
            let candidate = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: true)
            let precomputed = render(f, reversed: reversed, shift: shift, yaw: yaw, width: width, height: height, enabled: true, precomputed: true)
            require(identical(candidate, precomputed), "\(name): precomputed corner output differs")
            var result: [String: Any] = ["fixture": name, "source_depth_reversed": reversed, "pose": pose,
                "disabled_color_depth_owner_bit_identical": true, "precomputed_color_depth_owner_bit_identical": true,
                "old_gpu_ms": old.milliseconds, "candidate_gpu_ms": candidate.milliseconds]
            if f.smooth {
                let rotation = simd_float3x3(simd_quatf(angle: yaw, axis: SIMD3(0, 1, 0)))
                var interior = 0, oldHoles = 0, newHoles = 0, newCovered = 0, falseBridges = 0
                var oldDepthError: Double = 0, newDepthError: Double = 0, oldDepthN = 0, newDepthN = 0
                var maximumNewErrorFootprints: Float = 0
                var allNewCoverage = 0, fullFrameFalseBridges = 0, silhouetteFalseBridges = 0
                for y in 0..<height { for x in 0..<width {
                    let i = y * width + x
                    let ray = sourceRay(x: Float(x) + 0.5, y: Float(y) + 0.5, width: width, height: height)
                    let sourceDirection = rotation * ray
                    let hit = smoothHit(name, origin: shift, direction: sourceDirection)
                    let truthDistance = hit ?? ((-3 - shift.z) / sourceDirection.z)
                    let truthPoint = shift + sourceDirection * truthDistance
                    let truthFootprint = (-truthPoint.z) * min(2 / (Float(sw) * focal.x), 2 / (Float(sh) * focal.y))
                    if candidate.owner[i] == 2 {
                        let newError = abs(linearDepth(candidate.depth[i], projection: projection(reversed: true)) - truthDistance)
                        let oldError = old.owner[i] == 2
                            ? abs(linearDepth(old.depth[i], projection: projection(reversed: true)) - truthDistance) : 0
                        if old.owner[i] == 0 { allNewCoverage += 1 }
                        // Entire frame, including both silhouettes and newly exposed background.
                        // Count only a new candidate error beyond one footprint and the baseline error.
                        if newError > truthFootprint && newError > oldError + truthFootprint {
                            fullFrameFalseBridges += 1
                            if hit == nil { silhouetteFalseBridges += 1 }
                        }
                    }
                    guard let t = hit else { continue }
                    let point = shift + sourceDirection * t
                    let sourceClip = projection(reversed: reversed) * SIMD4(point, 1)
                    let sourceNDC = SIMD2(sourceClip.x, sourceClip.y) / sourceClip.w
                    // A four-source-pixel guard excludes the source-image edge
                    // and unavailable newly exposed strips from interior holes.
                    if abs(sourceNDC.x) >= 1 - 8 / Float(sw) || abs(sourceNDC.y) >= 1 - 8 / Float(sh) { continue }
                    let normal = name == "cylinder" ? simd_normalize(SIMD3(point.x, 0, point.z + 1))
                        : simd_normalize(point - SIMD3(0, 0, -1))
                    // Visible well inside both source and target silhouettes: not newly exposed background.
                    if simd_dot(normal, simd_normalize(-point)) < 0.65 || simd_dot(normal, simd_normalize(-sourceDirection)) < 0.65 { continue }
                    interior += 1
                    if old.owner[i] == 0 { oldHoles += 1 }
                    if candidate.owner[i] == 0 { newHoles += 1 }
                    if old.owner[i] == 0 && candidate.owner[i] != 0 { newCovered += 1 }
                    let footprint = (-point.z) * min(2 / (Float(sw) * focal.x), 2 / (Float(sh) * focal.y))
                    if old.owner[i] == 2 {
                        oldDepthError += Double(abs(linearDepth(old.depth[i], projection: projection(reversed: true)) - t))
                        oldDepthN += 1
                    }
                    if candidate.owner[i] == 2 {
                        let error = abs(linearDepth(candidate.depth[i], projection: projection(reversed: true)) - t)
                        newDepthError += Double(error); newDepthN += 1
                        maximumNewErrorFootprints = max(maximumNewErrorFootprints, error / footprint)
                        if old.owner[i] == 0 && error > footprint { falseBridges += 1 }
                    }
                }}
                require(newHoles <= oldHoles, "\(name)/\(pose): candidate creates more smooth interior holes")
                require(falseBridges == 0, "\(name)/\(pose): new coverage exceeds one-source-footprint depth error")
                require(fullFrameFalseBridges == 0, "\(name)/\(pose): new full-frame/silhouette false bridge")
                smoothOldHoles += oldHoles; smoothNewHoles += newHoles
                result.merge(["interior_pixels": interior, "old_interior_holes": oldHoles, "candidate_interior_holes": newHoles,
                    "newly_covered_interior_pixels": newCovered, "false_bridges_over_one_footprint": falseBridges,
                    "newly_covered_full_frame_pixels": allNewCoverage,
                    "new_full_frame_false_bridges_over_baseline_plus_one_footprint": fullFrameFalseBridges,
                    "new_background_silhouette_false_bridges": silhouetteFalseBridges,
                    "old_mean_linear_depth_error_m": oldDepthError / Double(max(1, oldDepthN)),
                    "candidate_mean_linear_depth_error_m": newDepthError / Double(max(1, newDepthN)),
                    "candidate_maximum_depth_error_source_footprints": maximumNewErrorFootprints]) { _, b in b }
                if !reversed && pose == "left" {
                    try ppm(name + "-source", rgba: f.rgba, width: sw, height: sh)
                    try ppm(name + "-old", rgba: old.rgba, width: width, height: height)
                    try ppm(name + "-candidate", rgba: candidate.rgba, width: width, height: height)
                    try ppm(name + "-old-crop", rgba: old.rgba, width: width, height: height, crop: true)
                    try ppm(name + "-candidate-crop", rgba: candidate.rgba, width: width, height: height, crop: true)
                }
            } else {
                require(identical(old, candidate), "\(name)/\(pose): step/slender/sky/affine fixture changed or bridged")
                result["candidate_color_depth_owner_bit_identical"] = true
                result["changed_or_false_bridge_pixels"] = 0
            }
            cases.append(result)
        }
    }
}
require(smoothOldHoles > 0, "Fixture must expose the baseline coverage defect")
require(smoothNewHoles < smoothOldHoles, "No measured smooth-surface coverage improvement")
let report: [String: Any] = ["passed": true, "scope": "Synthetic actual offscreen Mac Metal; not physical game pixels or headset acceptance",
    "device": device.name, "source_size": [sw, sh], "source_focal": [focal.x, focal.y],
    "near_far_m": [near, far], "old_smooth_interior_holes": smoothOldHoles,
    "candidate_smooth_interior_holes": smoothNewHoles, "cases": cases]
try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
    .write(to: output.appendingPathComponent("result.json"))
print("PASS actual Mac Metal \(cases.count) fixture/pose cases: smooth interior holes \(smoothOldHoles) -> \(smoothNewHoles); no step/slender/sky/affine changes; default path and precomputed path exact.")

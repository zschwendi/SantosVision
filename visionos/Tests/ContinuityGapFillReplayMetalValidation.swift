// Four-way exact registered candidate7 replay; original test is unchanged.
// Offscreen Mac Metal only, not headset performance or current-game acceptance.
import Foundation
import Metal
import simd

func require(_ condition: @autoclosure () -> Bool, _ message: @autoclosure () -> String) {
    if !condition() { fatalError("FAIL: " + message()) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = URL(fileURLWithPath: CommandLine.arguments[2])
let prefix = root.appendingPathComponent(
    "target/verification/device-session-20261020/candidate7/world-normal7-a6e1eec5a70a4ec084e39db09a8fcde5").path
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
    require(token() == "P6", "registered P6 source")
    let w = Int(token())!, h = Int(token())!
    require(token() == "255", "8-bit source")
    at += 1
    return (w, h, Array(data[at..<(at + w * h * 3)]))
}
let (sw, sh, sourceRGB) = try ppm(prefix + "-source.ppm")
let sourceDepth: [Float] = try {
    let bytes = try Data(contentsOf: URL(fileURLWithPath: prefix + "-source-depth.f32"))
    require(bytes.count == sw * sh * 4, "registered source depth extent")
    return bytes.withUnsafeBytes { raw in (0..<(sw * sh)).map {
        Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 4, as: UInt32.self))) } }
}()

let device = MTLCreateSystemDefaultDevice()!
let queue = device.makeCommandQueue()!
let library = try device.makeLibrary(source: String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"),
                                                    encoding: .utf8), options: nil)
let oldLibrary = try device.makeLibrary(source: String(contentsOfFile: CommandLine.arguments[3], encoding: .utf8), options: nil)
let oldSky = GameWorldFiniteSkySupport(device: device, library: oldLibrary, colorFormat: .rgba8Unorm,
    depthFormat: .depth32Float, precomputeCorners: false)!
let sky = GameWorldFiniteSkySupport(device: device, library: library, colorFormat: .rgba8Unorm,
                                    depthFormat: .depth32Float, precomputeCorners: false)!
let gapFill = GameWorldDepthEdgeFill(device: device, library: library, colorFormat: .rgba8Unorm, depthFormat: .depth32Float)!
require(MemoryLayout<GameWorldUniforms>.stride == 224, "production world uniform ABI")
require(MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride == 240, "production fill uniform ABI")

func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int, _ usage: MTLTextureUsage) -> any MTLTexture {
    let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
    descriptor.storageMode = .shared
    descriptor.usage = usage
    return device.makeTexture(descriptor: descriptor)!
}
let sourceColor = texture(.rgba8Unorm, sw, sh, .shaderRead)
let sourceDepthTexture = texture(.r32Float, sw, sh, .shaderRead)
var rgba = [UInt8](repeating: 255, count: sw * sh * 4)
for i in 0..<(sw * sh) { for c in 0..<3 { rgba[i * 4 + c] = sourceRGB[i * 3 + c] } }
rgba.withUnsafeBytes { sourceColor.replace(region: MTLRegionMake2D(0, 0, sw, sh), mipmapLevel: 0,
                                           withBytes: $0.baseAddress!, bytesPerRow: sw * 4) }
sourceDepth.withUnsafeBytes { sourceDepthTexture.replace(region: MTLRegionMake2D(0, 0, sw, sh), mipmapLevel: 0,
                                                          withBytes: $0.baseAddress!, bytesPerRow: sw * 4) }

let width = 1888, height = 1792
let viewport = MTLViewport(originX: 0, originY: 0, width: Double(width), height: Double(height), znear: 0, zfar: 1)
// Production defaults, including the current 5-10cm near fade.
let worldBase = GameWorldUniforms(sourceEyeFromClip: matrix("source_eye_from_clip"),
    currentEyeFromSourceEye: matrix("current_eye_from_source_eye"), currentClipFromEye: matrix("current_clip_from_eye"),
    sourceSize: SIMD2(UInt32(sw), UInt32(sh)))

struct Rendered { var color: [UInt8]; var depth: [Float]; var owner: [UInt8]; var gpuMilliseconds: Double }
func render(smooth: Bool, fill: Bool, telemetry: GameWorldImageQualityTelemetry, old: Bool = false) -> Rendered {
    var world = worldBase
    world.reserved.x = smooth ? 1 : 0
    let target = texture(.rgba8Unorm, width, height, [.renderTarget, .shaderRead])
    let ztarget = texture(.depth32Float, width, height, [.renderTarget, .shaderRead])
    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = target
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1)
    pass.depthAttachment.texture = ztarget
    pass.depthAttachment.loadAction = .clear
    pass.depthAttachment.storeAction = .store
    pass.depthAttachment.clearDepth = 0
    let commands = queue.makeCommandBuffer()!
    let result = (old ? oldSky : sky).encode(commands: commands, pass: pass, viewport: viewport, projection: world.currentClipFromEye,
                            far: 1000, uniforms: world, color: sourceColor, depth: sourceDepthTexture)!
    if fill {
        // The renderer's overlay pass: same attachments with load actions.
        pass.colorAttachments[0].loadAction = .load
        pass.depthAttachment.loadAction = .load
        let encoder = commands.makeRenderCommandEncoder(descriptor: pass)!
        encoder.setViewport(viewport)
        let uniforms = GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection: matrix("source_projection_from_eye"),
            currentEyeFromSourceEye: world.currentEyeFromSourceEye, displayProjection: world.currentClipFromEye,
            viewportOrigin: .zero, viewportSize: SIMD2(Float(width), Float(height)), sourceSize: world.sourceSize,
            sourceDepthReversed: world.sourceDepthReversed, colorTransfer: world.colorTransfer,
            nearestDistance: world.nearFadeDistances.y, skyRadius: result.radius)!
        require(gapFill.encode(encoder, commands: commands, eye: 0, uniforms: uniforms, ownership: result.ownership,
                               color: sourceColor, depth: sourceDepthTexture, telemetry: telemetry), "fill encode")
        encoder.endEncoding()
    }
    let rowBytes = (width + 255) & ~255
    let ownerBuffer = device.makeBuffer(length: rowBytes * height, options: .storageModeShared)!
    let blit = commands.makeBlitCommandEncoder()!
    blit.copy(from: result.ownership, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
              sourceSize: MTLSize(width: width, height: height, depth: 1), to: ownerBuffer, destinationOffset: 0,
              destinationBytesPerRow: rowBytes, destinationBytesPerImage: rowBytes * height)
    blit.endEncoding()
    commands.commit()
    commands.waitUntilCompleted()
    require(commands.status == .completed, "Metal execution failed: \(String(describing: commands.error))")
    var color = [UInt8](repeating: 0, count: width * height * 4)
    color.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: width * 4,
        from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
    var depth = [Float](repeating: 0, count: width * height)
    depth.withUnsafeMutableBytes { ztarget.getBytes($0.baseAddress!, bytesPerRow: width * 4,
        from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
    let bytes = ownerBuffer.contents().bindMemory(to: UInt8.self, capacity: rowBytes * height)
    var owner = [UInt8](repeating: 0, count: width * height)
    for y in 0..<height { for x in 0..<width { owner[y * width + x] = bytes[y * rowBytes + x] } }
    return Rendered(color: color, depth: depth, owner: owner,
                    gpuMilliseconds: (commands.gpuEndTime - commands.gpuStartTime) * 1000)
}

func same(_ a: Rendered, _ b: Rendered) -> Bool {
    a.color == b.color && a.owner == b.owner && zip(a.depth, b.depth).allSatisfy { $0.bitPattern == $1.bitPattern }
}
func changedColor(_ a: Rendered, _ b: Rendered, _ i: Int) -> Bool {
    let offset = i * 4
    return a.color[offset] != b.color[offset] || a.color[offset + 1] != b.color[offset + 1]
        || a.color[offset + 2] != b.color[offset + 2] || a.color[offset + 3] != b.color[offset + 3]
}
func changedDepth(_ a: Rendered, _ b: Rendered, _ i: Int) -> Bool { a.depth[i].bitPattern != b.depth[i].bitPattern }
let old = render(smooth: false, fill: false, telemetry: GameWorldImageQualityTelemetry(fillRequested: false, fillAvailable: false), old: true)
let baseline = render(smooth: false, fill: false, telemetry: GameWorldImageQualityTelemetry(fillRequested: false, fillAvailable: false))
require(same(old, baseline), "reserved.x=0 differs from frozen original game-source replay")
let smooth = render(smooth: true, fill: false, telemetry: GameWorldImageQualityTelemetry(fillRequested: false, fillAvailable: false))
let fillOnlyTelemetry = GameWorldImageQualityTelemetry(fillRequested: true, fillAvailable: true)
let fillOnly = render(smooth: false, fill: true, telemetry: fillOnlyTelemetry)
let bothTelemetry = GameWorldImageQualityTelemetry(fillRequested: true, fillAvailable: true)
let both = render(smooth: true, fill: true, telemetry: bothTelemetry)

// Source support at each output ray's infinite end, exactly the support test
// used by the fill. This identifies outside-image border without inferring
// missing pixels from RGB; actual gaps are owner==0 in the producer's MRT.
let currentEyeFromClip = worldBase.currentClipFromEye.inverse
let sourceClipFromCurrentEye = matrix("source_projection_from_eye") * worldBase.currentEyeFromSourceEye.inverse
var outsideSource = [Bool](repeating: false, count: width * height)
for y in 0..<height { for x in 0..<width {
    let uv = SIMD2((Float(x) + 0.5) / Float(width), (Float(y) + 0.5) / Float(height))
    let p = currentEyeFromClip * SIMD4(uv.x * 2 - 1, 1 - uv.y * 2, 0.5, 1)
    let ray = simd_normalize(SIMD3(p.x, p.y, p.z) / p.w)
    let h = sourceClipFromCurrentEye * SIMD4(ray, 0)
    outsideSource[y * width + x] = h.w <= 1e-6 || abs(h.x) > h.w || abs(h.y) > h.w
}}
func metrics(_ name: String, _ r: Rendered, unfilled: Rendered? = nil) -> [String: Any] {
    var ownerZero = 0, interior = 0, border = 0, residualInterior = 0, residualBorder = 0
    var fillChanged = 0, coveredChanged = 0, borderChanged = 0
    var bodyHoles = 0, bodyResidual = 0
    for y in 0..<height { for x in 0..<width {
        let i = y * width + x
        let changed = unfilled.map { changedColor($0, r, i) || changedDepth($0, r, i) } ?? false
        if r.owner[i] == 0 {
            ownerZero += 1
            if outsideSource[i] { border += 1; if !changed { residualBorder += 1 } }
            else { interior += 1; if !changed { residualInterior += 1 } }
            if (1020..<1320).contains(x) && (680..<1490).contains(y) {
                bodyHoles += 1
                if !changed { bodyResidual += 1 }
            }
            if changed { fillChanged += 1 }
            if outsideSource[i] && changed { borderChanged += 1 }
        } else if changed { coveredChanged += 1 }
    }}
    if let unfilled {
        require(unfilled.owner == r.owner, "\(name): fill changed producer ownership")
        require(coveredChanged == 0, "\(name): fill changed covered pixels")
        require(borderChanged == 0, "\(name): fill changed unsupported border")
    }
    return ["name": name, "owner_zero_pixels": ownerZero, "interior_owner_zero_pixels": interior,
        "outside_source_owner_zero_pixels": border, "residual_interior_holes": residualInterior,
        "residual_outside_source_holes": residualBorder, "filled_owner_zero_pixels": fillChanged,
        "covered_pixels_changed_by_fill": coveredChanged, "unsupported_border_changed_by_fill": borderChanged,
        "body_roi_owner_zero": bodyHoles, "body_roi_residual_holes": bodyResidual,
        "single_command_gpu_ms": r.gpuMilliseconds]
}
var changedRGB = 0, changedZ = 0, changedOwner = 0, newlyCovered = 0, newlyUncovered = 0
var newInteriorCoverage = 0, lostInteriorCoverage = 0
var maximumChangedDepthDifference: Float = 0
var maximumBothCoveredDepthDifference: Float = 0, maximumClearToCoveredDepthDifference: Float = 0
var maximumBothCoveredChannelDifference = 0, bothCoveredDepthChanges = 0
for i in 0..<(width * height) {
    if changedColor(baseline, smooth, i) { changedRGB += 1 }
    if changedDepth(baseline, smooth, i) {
        changedZ += 1
        maximumChangedDepthDifference = max(maximumChangedDepthDifference, abs(baseline.depth[i] - smooth.depth[i]))
        if baseline.owner[i] != 0 && smooth.owner[i] != 0 {
            bothCoveredDepthChanges += 1
            maximumBothCoveredDepthDifference = max(maximumBothCoveredDepthDifference, abs(baseline.depth[i] - smooth.depth[i]))
        }
    }
    if baseline.owner[i] != smooth.owner[i] { changedOwner += 1 }
    if baseline.owner[i] == 0 && smooth.owner[i] != 0 {
        newlyCovered += 1
        maximumClearToCoveredDepthDifference = max(maximumClearToCoveredDepthDifference, abs(baseline.depth[i] - smooth.depth[i]))
        if !outsideSource[i] { newInteriorCoverage += 1 }
    }
    if baseline.owner[i] != 0 && smooth.owner[i] != 0 {
        for channel in 0..<3 {
            maximumBothCoveredChannelDifference = max(maximumBothCoveredChannelDifference,
                abs(Int(baseline.color[i * 4 + channel]) - Int(smooth.color[i * 4 + channel])))
        }
    }
    if baseline.owner[i] != 0 && smooth.owner[i] == 0 {
        newlyUncovered += 1
        if !outsideSource[i] { lostInteriorCoverage += 1 }
    }
}
let allMetrics = [metrics("baseline", baseline), metrics("smooth_only", smooth),
    metrics("fill_only", fillOnly, unfilled: baseline), metrics("smooth_and_fill", both, unfilled: smooth)]

func save(_ name: String, _ r: Rendered) throws {
    var image = Data("P6\n\(width) \(height)\n255\n".utf8)
    for i in 0..<(width * height) { image.append(contentsOf: r.color[(i * 4)..<(i * 4 + 3)]) }
    try image.write(to: destination.appendingPathComponent(name + ".ppm"))
    var crop = Data("P6\n300 810\n255\n".utf8)
    for y in 680..<1490 { for x in 1020..<1320 {
        let i = (y * width + x) * 4
        crop.append(contentsOf: r.color[i..<(i + 3)])
    }}
    try crop.write(to: destination.appendingPathComponent(name + "-body-crop.ppm"))
    try Data(r.owner).write(to: destination.appendingPathComponent(name + "-owner.u8"))
    try r.depth.withUnsafeBytes { try Data($0).write(to: destination.appendingPathComponent(name + "-depth.f32")) }
}
for (name, value) in [("baseline", baseline), ("smooth-only", smooth), ("fill-only", fillOnly), ("smooth-and-fill", both)] { try save(name, value) }
let result: [String: Any] = ["passed": true, "device": device.name, "registered_prefix": prefix,
    "scope": "Actual offscreen Mac Metal replay of registered historical candidate7 third-person source/pose; not current first-person evidence, headset performance or visual acceptance",
    "source_size": [sw, sh], "output_size": [width, height], "frozen_original_baseline_exact": true,
    "geometry_comparison": ["color_changed_pixels": changedRGB, "depth_changed_pixels": changedZ,
        "ownership_changed_pixels": changedOwner, "newly_covered_pixels": newlyCovered,
        "newly_uncovered_pixels": newlyUncovered, "new_interior_coverage": newInteriorCoverage,
        "lost_interior_coverage": lostInteriorCoverage, "max_changed_reverse_depth_difference": maximumChangedDepthDifference,
        "both_covered_depth_changed_pixels": bothCoveredDepthChanges,
        "max_both_covered_reverse_depth_difference": maximumBothCoveredDepthDifference,
        "max_clear_to_covered_reverse_depth_difference": maximumClearToCoveredDepthDifference,
        "max_both_covered_color_channel_difference_8bit": maximumBothCoveredChannelDifference],
    "variants": allMetrics, "fill_only_telemetry": fillOnlyTelemetry.reportValue,
    "smooth_and_fill_telemetry": bothTelemetry.reportValue,
    "limits": ["Real scene ground-truth geometry behind occluders is unavailable; newly covered pixels are not automatically correct.",
        "Continuity can change covered color and depth; gap fill must preserve covered pixels separately for each geometry.",
        "Owner mask remains producer ownership after fill; residual holes use actual fill color/depth changes, including true black source.",
        "Four single-command GPU timings are bookkeeping, not a warmed performance comparison."]]
try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
    .write(to: destination.appendingPathComponent("replay-result.json"))
for m in allMetrics { print("REPLAY \(m["name"]!): interior owner0=\(m["interior_owner_zero_pixels"]!), residual=\(m["residual_interior_holes"]!), filled=\(m["filled_owner_zero_pixels"]!), border changed=\(m["unsupported_border_changed_by_fill"]!)") }
print("PASS fixed-geometry fill covered pixels and unsupported border exact; frozen original baseline exact.")

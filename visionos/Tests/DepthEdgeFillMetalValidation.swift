// Run scripts/test-depth-edge-fill-metal.sh (root GPU step). Real offscreen
// Mac Metal of the production finite-sky ownership pass, then the production
// depth-edge fill, on the registered candidate7 third-person capture. Not
// Vision Pro composition, headset performance or visual acceptance.
import Foundation
import Metal
import simd

func require(_ condition: @autoclosure () -> Bool, _ message: @autoclosure () -> String) {
    if !condition() { fatalError("FAIL: " + message()) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = URL(fileURLWithPath: CommandLine.arguments[2])
let prefix = ProcessInfo.processInfo.environment["SV_REGISTERED_CAPTURE_PREFIX"] ?? root.appendingPathComponent(
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
var world = GameWorldUniforms(sourceEyeFromClip: matrix("source_eye_from_clip"),
    currentEyeFromSourceEye: matrix("current_eye_from_source_eye"), currentClipFromEye: matrix("current_clip_from_eye"),
    sourceSize: SIMD2(UInt32(sw), UInt32(sh)))
// Raw capture RGB is encoded sRGB even though this fixture's texture is UNORM.
world.colorTransfer = 2

// Independent Double reference: finished sRGB bytes -> linear Display P3 ->
// linear UNORM storage. The fill uses a real texel read, without interpolation.
func expectedP3Bytes(_ sourceOffset: Int) -> [UInt8] {
    func decode(_ channel: UInt8) -> Double {
        let value = Double(channel) / 255
        return value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
    }
    let r = decode(sourceRGB[sourceOffset]), g = decode(sourceRGB[sourceOffset + 1]), b = decode(sourceRGB[sourceOffset + 2])
    let p3 = [0.822461969 * r + 0.177538031 * g,
              0.033194199 * r + 0.966805801 * g,
              0.017082631 * r + 0.072397441 * g + 0.910519929 * b]
    return p3.map { UInt8(min(255, max(0, ($0 * 255).rounded()))) }
}

struct Rendered { var color: [UInt8]; var depth: [Float]; var owner: [UInt8]; var gpuMilliseconds: Double }
func render(fill: Bool, telemetry: GameWorldImageQualityTelemetry) -> Rendered {
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
    let result = sky.encode(commands: commands, pass: pass, viewport: viewport, projection: world.currentClipFromEye,
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

let baselineTelemetry = GameWorldImageQualityTelemetry(fillRequested: false, fillAvailable: false)
let fillTelemetry = GameWorldImageQualityTelemetry(fillRequested: true, fillAvailable: true)
let baseline = render(fill: false, telemetry: baselineTelemetry)
let filled = render(fill: true, telemetry: fillTelemetry)
require(baseline.owner == filled.owner, "fill changed ownership")

let mirrorUniforms = GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection: matrix("source_projection_from_eye"),
    currentEyeFromSourceEye: world.currentEyeFromSourceEye, displayProjection: world.currentClipFromEye,
    viewportOrigin: .zero, viewportSize: SIMD2(Float(width), Float(height)), sourceSize: world.sourceSize,
    sourceDepthReversed: world.sourceDepthReversed, colorTransfer: world.colorTransfer,
    nearestDistance: world.nearFadeDistances.y, skyRadius: 800)!
let depthAt: (SIMD2<UInt32>) -> Float = { sourceDepth[Int($0.y) * sw + Int($0.x)] }
var uncovered = 0, gpuFilled = 0, mirrorFilled = 0, agreement = 0, mismatches = 0
var bodyUncovered = 0, bodyFilled = 0
var maximumDepthDifference: Float = 0
for y in 0..<height { for x in 0..<width {
    let i = y * width + x
    let changed = baseline.color[(i * 4)..<(i * 4 + 4)] != filled.color[(i * 4)..<(i * 4 + 4)]
        || baseline.depth[i].bitPattern != filled.depth[i].bitPattern
    guard filled.owner[i] == 0 else {
        require(!changed, "covered pixel \(x),\(y) changed")
        continue
    }
    uncovered += 1
    require(baseline.depth[i] == 0, "uncovered pixel was not cleared")
    let inBody = (1020..<1320).contains(x) && (680..<1490).contains(y)
    if inBody { bodyUncovered += 1 }
    if changed { gpuFilled += 1; if inBody { bodyFilled += 1 } }
    let mirror = fill(pixel: SIMD2(x, y), mirrorUniforms, depthAt)
    if mirror != nil { mirrorFilled += 1 }
    if let mirror, changed {
        let s = (Int(mirror.texel.y) * sw + Int(mirror.texel.x)) * 3
        let expected = expectedP3Bytes(s)
        let same = (0..<3).allSatisfy { abs(Int(filled.color[i * 4 + $0]) - Int(expected[$0])) <= 1 }
        if same { agreement += 1 } else { mismatches += 1 }
        maximumDepthDifference = max(maximumDepthDifference, abs(filled.depth[i] - mirror.depth))
    } else if (mirror != nil) != changed {
        mismatches += 1
    }
}}
// Fast-math GPU arithmetic may flip a walk sample across a texel boundary.
require(gpuFilled > 1000 && Double(mismatches) <= 0.005 * Double(max(1, mirrorFilled)),
        "GPU fill disagrees with the CPU mirror: \(mismatches) of \(mirrorFilled)")
let report = fillTelemetry.reportValue
let views = (report["depth_edge_fill"] as! [String: Any])["views"] as! [[String: Any]]
let sample = views[0]["last_gpu_sample"] as! [String: Any]
let counted = (sample["uncovered_pixels"] as! UInt32, sample["filled_from_finite_background"] as! UInt32,
               sample["filled_from_sky"] as! UInt32, sample["left_uncovered"] as! UInt32)
require(Int(counted.0) == uncovered, "GPU counter disagrees with owner-0 readback")
require(Int(counted.1 + counted.2) == gpuFilled && counted.1 + counted.2 + counted.3 == counted.0,
        "GPU fill counters disagree with readback")

// Warmed alternating Mac timing of the whole command, with and without the fill.
var withFill = [Double](), without = [Double]()
for run in 0..<8 {
    for useFill in (run % 2 == 0 ? [false, true] : [true, false]) {
        let sampleRun = render(fill: useFill, telemetry: GameWorldImageQualityTelemetry(fillRequested: useFill, fillAvailable: useFill))
        if run >= 2 { if useFill { withFill.append(sampleRun.gpuMilliseconds) } else { without.append(sampleRun.gpuMilliseconds) } }
    }
}
func median(_ values: [Double]) -> Double {
    let sorted = values.sorted()
    return (sorted[(sorted.count - 1) / 2] + sorted[sorted.count / 2]) * 0.5
}
try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
var image = Data("P6\n\(width) \(height)\n255\n".utf8)
for i in 0..<(width * height) { image.append(contentsOf: filled.color[(i * 4)..<(i * 4 + 3)]) }
try image.write(to: destination.appendingPathComponent("normal7-gpu-depth-edge-filled.ppm"))
let result: [String: Any] = ["passed": true, "device": device.name,
    "scope": "Actual offscreen Mac Metal of production finite-sky ownership plus production depth-edge fill on the registered candidate7 capture; not Vision Pro composition, headset performance or visual acceptance.",
    "uncovered_pixels": uncovered, "gpu_filled": gpuFilled, "cpu_mirror_filled": mirrorFilled,
    "gpu_cpu_same_texel_colour": agreement, "gpu_cpu_mismatches": mismatches,
    "color_reference": "Independent Double sRGB decode and D65 sRGB-to-P3 matrix, linear UNORM target, maximum one-byte rounding tolerance",
    "output_ppm_note": "Raw linear Display P3 bytes without ICC tag; use numeric comparison, not an assumed sRGB preview",
    "max_gpu_cpu_depth_difference": maximumDepthDifference,
    "audited_body_roi_uncovered": bodyUncovered, "audited_body_roi_filled": bodyFilled,
    "gpu_counter_sample": ["uncovered": counted.0, "finite": counted.1, "sky": counted.2, "left": counted.3],
    "covered_pixels_bit_identical": true,
    "warmed_mac_command_gpu_ms": ["without_fill_median": median(without), "with_fill_median": median(withFill),
                                  "without_fill": without, "with_fill": withFill]]
try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
    .write(to: destination.appendingPathComponent("metal-result.json"))
print("PASS Mac Metal depth-edge fill parity: \(gpuFilled) of \(uncovered) uncovered pixels filled; covered pixels bit-identical")

// Run scripts/test-finite-sky-support.sh. Real offscreen Mac Metal execution;
// this is not a simulator, headset composition test, or physical acceptance.
import Foundation
import Metal
import simd

func require(_ condition: @autoclosure () -> Bool, _ message: String) {
    if !condition() { fatalError(message) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = URL(fileURLWithPath: CommandLine.arguments[2])
let capture = root.appendingPathComponent("target/verification/device-session-20261020/world-20261020-c07bd06c17a34cb9b238adde8e6209c5-report.json")
let metadata = try JSONSerialization.jsonObject(with: Data(contentsOf: capture)) as! [String: Any]
let device = MTLCreateSystemDefaultDevice()!
let queue = device.makeCommandQueue()!
let source = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
let library = try device.makeLibrary(source: source, options: nil)
let experiment = GameWorldFiniteSkySupport(device: device, library: library, colorFormat: .rgba8Unorm, depthFormat: .depth32Float)!
let baselineDescriptor = MTLRenderPipelineDescriptor()
baselineDescriptor.vertexFunction = library.makeFunction(name: "gameWorldVertex")!
baselineDescriptor.fragmentFunction = library.makeFunction(name: "gameWorldFragment")!
baselineDescriptor.colorAttachments[0].pixelFormat = .rgba8Unorm
baselineDescriptor.depthAttachmentPixelFormat = .depth32Float
let baseline = try device.makeRenderPipelineState(descriptor: baselineDescriptor)
let depthDescriptor = MTLDepthStencilDescriptor()
depthDescriptor.depthCompareFunction = .greaterEqual
depthDescriptor.isDepthWriteEnabled = true
let depthState = device.makeDepthStencilState(descriptor: depthDescriptor)!
require(MemoryLayout<GameWorldUniforms>.stride == 224, "Production uniform ABI changed")

func matrix(_ key: String) -> simd_float4x4 {
    let values = (metadata[key] as! [NSNumber]).map(\.floatValue)
    return simd_float4x4(columns: (SIMD4(values[0], values[1], values[2], values[3]),
        SIMD4(values[4], values[5], values[6], values[7]), SIMD4(values[8], values[9], values[10], values[11]),
        SIMD4(values[12], values[13], values[14], values[15])))
}
func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int, _ usage: MTLTextureUsage) -> any MTLTexture {
    let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
    descriptor.storageMode = .shared
    descriptor.usage = usage
    return device.makeTexture(descriptor: descriptor)!
}
func rgba(_ url: URL) throws -> (Int, Int, [UInt8]) {
    let data = [UInt8](try Data(contentsOf: url))
    var at = 0
    func token() -> String {
        while data[at] <= 32 { at += 1 }
        let start = at
        while data[at] > 32 { at += 1 }
        return String(bytes: data[start..<at], encoding: .ascii)!
    }
    require(token() == "P6", "Only registered P6 source captures are accepted")
    let width = Int(token())!, height = Int(token())!
    require(token() == "255", "Unexpected PPM channel range")
    at += 1
    require(data.count - at == width * height * 3, "Unexpected PPM payload size")
    var result = [UInt8](repeating: 255, count: width * height * 4)
    for index in 0..<(width * height) { for channel in 0..<3 { result[index * 4 + channel] = data[at + index * 3 + channel] } }
    return (width, height, result)
}
func floats(_ url: URL, count: Int) throws -> [Float] {
    let bytes = try Data(contentsOf: url)
    require(bytes.count == count * 4, "Registered Float32 source has wrong extent")
    return bytes.withUnsafeBytes { raw in (0..<count).map { Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 4, as: UInt32.self))) } }
}
func readColor(_ texture: any MTLTexture) -> [UInt8] {
    var bytes = [UInt8](repeating: 0, count: texture.width * texture.height * 4)
    bytes.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: texture.width * 4,
        from: MTLRegionMake2D(0, 0, texture.width, texture.height), mipmapLevel: 0) }
    return bytes
}
func readDepth(_ texture: any MTLTexture) -> [Float] {
    var bytes = [Float](repeating: 0, count: texture.width * texture.height)
    bytes.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: texture.width * 4,
        from: MTLRegionMake2D(0, 0, texture.width, texture.height), mipmapLevel: 0) }
    return bytes
}
struct Rendered {
    let color: [UInt8]
    let depth: [Float]
    let owner: [UInt8]
}
func render(uniforms: GameWorldUniforms, color: any MTLTexture, depth: any MTLTexture,
            targetSize: SIMD2<Int>, experimental: Bool) -> Rendered {
    let target = texture(.rgba8Unorm, targetSize.x, targetSize.y, [.renderTarget, .shaderRead])
    let ztarget = texture(.depth32Float, targetSize.x, targetSize.y, [.renderTarget, .shaderRead])
    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = target
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1)
    pass.depthAttachment.texture = ztarget
    pass.depthAttachment.loadAction = .clear
    pass.depthAttachment.storeAction = .store
    pass.depthAttachment.clearDepth = 0
    let viewport = MTLViewport(originX: 0, originY: 0, width: Double(targetSize.x), height: Double(targetSize.y), znear: 0, zfar: 1)
    let commands = queue.makeCommandBuffer()!
    var ownerBuffer: (any MTLBuffer)?
    let rowBytes = (targetSize.x + 255) & ~255
    if experimental {
        let result = experiment.encode(commands: commands, pass: pass, viewport: viewport,
            projection: uniforms.currentClipFromEye, far: 1000, uniforms: uniforms, color: color, depth: depth)!
        require(experiment.pool.statistics.busy == 1, "Mask was returned before GPU completion")
        require(pass.colorAttachments[1].texture == nil, "Experiment mutated caller MRT descriptor")
        ownerBuffer = device.makeBuffer(length: rowBytes * targetSize.y, options: .storageModeShared)!
        let blit = commands.makeBlitCommandEncoder()!
        blit.copy(from: result.ownership, sourceSlice: 0, sourceLevel: 0, sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
            sourceSize: MTLSize(width: targetSize.x, height: targetSize.y, depth: 1), to: ownerBuffer!,
            destinationOffset: 0, destinationBytesPerRow: rowBytes, destinationBytesPerImage: rowBytes * targetSize.y)
        blit.endEncoding()
    } else {
        let encoder = commands.makeRenderCommandEncoder(descriptor: pass)!
        encoder.setViewport(viewport)
        encoder.setRenderPipelineState(baseline)
        encoder.setDepthStencilState(depthState)
        encoder.setCullMode(.none)
        var uniforms = uniforms
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
        encoder.setVertexTexture(depth, index: 0)
        encoder.setFragmentTexture(color, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4,
            instanceCount: Int(uniforms.sourceSize.x * uniforms.sourceSize.y))
        encoder.endEncoding()
    }
    commands.commit()
    commands.waitUntilCompleted()
    require(commands.status == .completed, "Actual Metal execution failed: \(String(describing: commands.error))")
    var owner = [UInt8](repeating: 0, count: targetSize.x * targetSize.y)
    if let ownerBuffer {
        let bytes = ownerBuffer.contents().bindMemory(to: UInt8.self, capacity: rowBytes * targetSize.y)
        for y in 0..<targetSize.y { for x in 0..<targetSize.x { owner[y * targetSize.x + x] = bytes[y * rowBytes + x] } }
    }
    // Completion handlers release the mask before waitUntilCompleted returns.
    require(experiment.pool.statistics.busy == 0, "GPU-complete mask leaked its lease")
    return Rendered(color: readColor(target), depth: readDepth(ztarget), owner: owner)
}
func sourceTextures(width: Int, height: Int, rgba: [UInt8], depths: [Float]) -> (any MTLTexture, any MTLTexture) {
    let color = texture(.rgba8Unorm, width, height, .shaderRead)
    let depth = texture(.r32Float, width, height, .shaderRead)
    rgba.withUnsafeBytes { color.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
        withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
    depths.withUnsafeBytes { depth.replace(region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
        withBytes: $0.baseAddress!, bytesPerRow: width * 4) }
    return (color, depth)
}
var cases = [[String: Any]]()
func compare(_ name: String, uniforms: GameWorldUniforms, color: any MTLTexture,
             depth: any MTLTexture, targetSize: SIMD2<Int>, expectFarGeometry: Bool = false) {
    let normal = render(uniforms: uniforms, color: color, depth: depth, targetSize: targetSize, experimental: false)
    let repaired = render(uniforms: uniforms, color: color, depth: depth, targetSize: targetSize, experimental: true)
    require(normal.color == repaired.color, "\(name): experiment changed original color")
    var counts = [0, 0, 0], finiteBeyondSupport = 0, maximumRadiusError: Float = 0
    for index in normal.depth.indices {
        let owner = Int(repaired.owner[index])
        require(owner < 3, "Unknown owner")
        counts[owner] += 1
        if owner != 1 {
            require(normal.depth[index].bitPattern == repaired.depth[index].bitPattern, "\(name): finite or uncovered depth changed")
            if owner == 0 { require(normal.depth[index] == 0, "Untouched depth was not clear") }
            if owner == 2 {
                let x = index % targetSize.x, y = index / targetSize.x
                let clip = SIMD4((Float(x) + 0.5) / Float(targetSize.x) * 2 - 1,
                    1 - (Float(y) + 0.5) / Float(targetSize.y) * 2, normal.depth[index], 1)
                let eye = uniforms.currentClipFromEye.inverse * clip
                if -eye.z / eye.w > 850 { finiteBeyondSupport += 1 }
            }
        } else {
            require(normal.depth[index] == 0 && repaired.depth[index] > 0 && repaired.depth[index] < 1,
                "\(name): sky support not strictly inside target depth range")
            let x = index % targetSize.x, y = index / targetSize.x
            let clip = SIMD4((Float(x) + 0.5) / Float(targetSize.x) * 2 - 1,
                1 - (Float(y) + 0.5) / Float(targetSize.y) * 2, repaired.depth[index], 1)
            let eye = uniforms.currentClipFromEye.inverse * clip
            let radius = simd_length(SIMD3(eye.x, eye.y, eye.z) / eye.w)
            maximumRadiusError = max(maximumRadiusError, abs(radius - 800))
        }
    }
    require(counts[1] > 100 && counts[2] > 100, "\(name): fixture did not exercise sky and finite world")
    require(maximumRadiusError < 0.2, "\(name): final GPU depth does not reconstruct to the defined 800m sphere")
    if expectFarGeometry { require(finiteBeyondSupport > 100, "900m geometry was not actually visible in final GPU result") }
    cases.append(["name": name, "uncovered": counts[0], "sky": counts[1], "finite": counts[2],
        "finite_beyond_850m_preserved": finiteBeyondSupport, "max_sphere_radius_error_m": maximumRadiusError,
        "rgb_byte_identical": true, "finite_uncovered_depth_bit_identical": true])
}

// Actual registered game color/depth, with the exact captured source/target
// matrices. It is a startup view, not the later user screenshots or same frame.
let sourceURL = capture.deletingLastPathComponent().appendingPathComponent("world-20261020-c07bd06c17a34cb9b238adde8e6209c5-source.ppm")
let (sw, sh, capturedColor) = try rgba(sourceURL)
let capturedDepth = try floats(capture.deletingLastPathComponent().appendingPathComponent("world-20261020-c07bd06c17a34cb9b238adde8e6209c5-source-depth.f32"), count: sw * sh)
let (gameColor, gameDepth) = sourceTextures(width: sw, height: sh, rgba: capturedColor, depths: capturedDepth)
let actualUniforms = GameWorldUniforms(sourceEyeFromClip: matrix("source_eye_from_clip"),
    currentEyeFromSourceEye: matrix("current_eye_from_source_eye"), currentClipFromEye: matrix("current_clip_from_eye"),
    sourceSize: SIMD2(UInt32(sw), UInt32(sh)))
compare("registered_physical_source_capture", uniforms: actualUniforms, color: gameColor, depth: gameDepth, targetSize: SIMD2(1888, 1792))

// Independent 900m plane depth from the projection, sky around it, invalid
// holes, and real black finite geometry. Black color never identifies owners.
let width = 160, height = 90
let projection = matrix("source_projection_from_eye")
let farClip = projection * SIMD4<Float>(0, 0, -900, 1)
let nearClip = projection * SIMD4<Float>(0, 0, -10, 1)
var syntheticDepth = [Float](repeating: 0, count: width * height)
var syntheticColor = [UInt8](repeating: 255, count: width * height * 4)
for y in 0..<height { for x in 0..<width {
    let index = y * width + x
    if x > 35 && x < 125 && y > 20 && y < 70 { syntheticDepth[index] = farClip.z / farClip.w }
    if x > 60 && x < 95 && y > 32 && y < 58 { syntheticDepth[index] = nearClip.z / nearClip.w }
    if x < 12 && y < 15 { syntheticDepth[index] = .nan }
    syntheticColor[index * 4] = 48
    syntheticColor[index * 4 + 1] = 110
    syntheticColor[index * 4 + 2] = 185
    if x > 60 && x < 95 && y > 32 && y < 58 {
        for channel in 0..<3 { syntheticColor[index * 4 + channel] = 0 }
    }
} }
let (fixtureColor, fixtureDepth) = sourceTextures(width: width, height: height, rgba: syntheticColor, depths: syntheticDepth)
var fixtureUniforms = GameWorldUniforms(sourceEyeFromClip: projection.inverse,
    currentEyeFromSourceEye: matrix_identity_float4x4, currentClipFromEye: projection,
    sourceSize: SIMD2(UInt32(width), UInt32(height)))
for transfer in UInt32(0)...2 {
    fixtureUniforms.colorTransfer = transfer
    compare("sky_900m_black_finite_invalid_transfer_\(transfer)", uniforms: fixtureUniforms,
        color: fixtureColor, depth: fixtureDepth, targetSize: SIMD2(320, 180), expectFarGeometry: true)
}

// Force overlapping sky and finite footprints in both draw orders. These
// source layers share the same production vertex/fragment stages, so every
// central pixel has both a sky candidate and a legitimate 900m candidate.
// Read back the final GPU depth: a naive inline 800m sky support would fail.
var overlapDepth = [Float](repeating: .nan, count: width * height)
for y in 20..<70 { for x in 35..<125 { overlapDepth[y * width + x] = farClip.z / farClip.w } }
let (overlapColor, overlapFinite) = sourceTextures(width: width, height: height, rgba: syntheticColor, depths: overlapDepth)
let (_, overlapSky) = sourceTextures(width: width, height: height, rgba: syntheticColor,
    depths: [Float](repeating: 0, count: width * height))
let ownerDescriptor = baselineDescriptor.copy() as! MTLRenderPipelineDescriptor
ownerDescriptor.fragmentFunction = library.makeFunction(name: "gameWorldSkyOwnershipFragment")!
ownerDescriptor.colorAttachments[1].pixelFormat = .r8Uint
let ownerPipeline = try device.makeRenderPipelineState(descriptor: ownerDescriptor)
for skyFirst in [true, false] {
    let target = texture(.rgba8Unorm, 320, 180, [.renderTarget, .shaderRead])
    let finalDepth = texture(.depth32Float, 320, 180, [.renderTarget, .shaderRead])
    let ownership = texture(.r8Uint, 320, 180, [.renderTarget, .shaderRead])
    let pass = MTLRenderPassDescriptor()
    pass.colorAttachments[0].texture = target
    pass.colorAttachments[0].loadAction = .clear
    pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[1].texture = ownership
    pass.colorAttachments[1].loadAction = .clear
    pass.colorAttachments[1].storeAction = .store
    pass.depthAttachment.texture = finalDepth
    pass.depthAttachment.loadAction = .clear
    pass.depthAttachment.storeAction = .store
    pass.depthAttachment.clearDepth = 0
    let viewport = MTLViewport(originX: 0, originY: 0, width: 320, height: 180, znear: 0, zfar: 1)
    let commands = queue.makeCommandBuffer()!
    let encoder = commands.makeRenderCommandEncoder(descriptor: pass)!
    encoder.setViewport(viewport)
    encoder.setRenderPipelineState(ownerPipeline)
    encoder.setDepthStencilState(depthState)
    encoder.setCullMode(.none)
    var uniforms = fixtureUniforms
    uniforms.colorTransfer = 0
    encoder.setVertexBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
    encoder.setFragmentBytes(&uniforms, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
    encoder.setFragmentTexture(overlapColor, index: 0)
    let layers = skyFirst ? [overlapSky, overlapFinite] : [overlapFinite, overlapSky]
    for layer in layers {
        encoder.setVertexTexture(layer, index: 0)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4, instanceCount: width * height)
    }
    encoder.endEncoding()
    require(experiment.encodeRepair(commands: commands, depth: finalDepth, slice: 0, ownership: ownership,
        viewport: viewport, projection: projection, radius: 800), "Overlap repair encode failed")
    commands.commit()
    commands.waitUntilCompleted()
    require(commands.status == .completed, "Overlap Metal command failed")
    let depths = readDepth(finalDepth)
    var owners = [UInt8](repeating: 0, count: 320 * 180)
    owners.withUnsafeMutableBytes { ownership.getBytes($0.baseAddress!, bytesPerRow: 320,
        from: MTLRegionMake2D(0, 0, 320, 180), mipmapLevel: 0) }
    var preservedFar = 0, supportedSky = 0
    for y in 0..<180 { for x in 0..<320 {
        let index = y * 320 + x
        if x > 80 && x < 235 && y > 50 && y < 130 {
            require(owners[index] == 2, "Far-world ownership lost to overlapping sky")
            let eye = projection.inverse * SIMD4((Float(x) + 0.5) / 160 - 1,
                1 - (Float(y) + 0.5) / 90, depths[index], 1)
            require(abs(-eye.z / eye.w - 900) < 0.2, "Final GPU depth hid 900m geometry with sky sphere")
            preservedFar += 1
        }
        if owners[index] == 1 { require(depths[index] > 0, "Unrepaired overlapped sky"); supportedSky += 1 }
    } }
    require(preservedFar > 1000 && supportedSky > 1000, "Overlap coverage did not exercise both owners")
    cases.append(["name": skyFirst ? "overlap_sky_then_900m" : "overlap_900m_then_sky",
        "900m_final_depth_verified_pixels": preservedFar, "supported_sky_pixels": supportedSky])
}

// Bounded allocation is enforced with all six masks busy, then with a change
// of dimensions while busy resources must remain retained.
let pool = GameWorldSkyOwnershipPool()
var busy = [any MTLTexture]()
for _ in 0..<6 { busy.append(pool.acquire(device: device, width: 64, height: 64)!) }
require(pool.acquire(device: device, width: 64, height: 64) == nil, "Pool exceeded six in-flight masks")
require(pool.acquire(device: device, width: 100_000, height: 100_000) == nil, "Unbounded mask accepted")
pool.release(busy[0])
require(pool.acquire(device: device, width: 128, height: 64) != nil, "Idle wrong-size entry was not replaceable")
require(pool.statistics.textures == 6 && pool.statistics.busy == 6, "Resize lost a busy mask or grew history")

let result: [String: Any] = ["passed": true, "device": device.name,
    "scope": "Actual offscreen Mac Metal of production optional helper and shaders; not Vision Pro system composition or proof of screenshot causality.",
    "physical_capture": capture.path, "cases": cases, "ownership_pool_bounds_passed": true,
    "default_off": !GameWorldFiniteSkySupport.requested]
try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys]).write(to: destination.appendingPathComponent("result.json"))
print("PASS \(cases.count) actual Mac Metal finite-sky support cases; RGB and all finite/uncovered depths preserved")

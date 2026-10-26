#if SV_GAME_RUNTIME
import Foundation
import CoreGraphics
import ImageIO
import Metal
import OSLog
import simd

// BEGIN GameWorldColorFile
/// A texture format describes storage, not the image's primaries or purpose.
/// Guest bytes are finished sRGB. CompositorServices consumes linear Display P3;
/// an sRGB attachment only changes how those linear values are stored.
enum GameWorldColorFile {
    enum Role: String {
        case guest = "finished_guest_srgb"
        case compositor = "compositor_display_p3"
    }

    static func colorSpace(role: Role, format: MTLPixelFormat) -> CGColorSpace? {
        if role == .guest { return CGColorSpace(name: CGColorSpace.sRGB) }
        let encoded = format == .rgba8Unorm_srgb || format == .bgra8Unorm_srgb
        return CGColorSpace(name: encoded ? CGColorSpace.displayP3 : CGColorSpace.linearDisplayP3)
    }

    /// PNG is a tagged viewing companion. The raw PPM/half-float file remains
    /// the numerical oracle. Float output keeps every raw bit separately; its
    /// PNG is explicitly a bounded, 16-bit linear-P3 preview, without tonemapping.
    static func writePNG(_ bytes: UnsafeRawBufferPointer, width: Int, height: Int,
                         rowBytes: Int, format: MTLPixelFormat, role: Role,
                         to url: URL) throws -> [String: Any] {
        let halfFloat = format == .rgba16Float
        let supported: Set<MTLPixelFormat> = [.rgba8Unorm, .bgra8Unorm, .rgba8Unorm_srgb, .bgra8Unorm_srgb, .rgba16Float]
        let bytesPerPixel = halfFloat ? 8 : 4
        let (packedRow, rowOverflow) = width.multipliedReportingOverflow(by: bytesPerPixel)
        let (byteCount, sizeOverflow) = rowBytes.multipliedReportingOverflow(by: height)
        guard supported.contains(format), !(halfFloat && role == .guest), width > 0, height > 0,
              !rowOverflow, !sizeOverflow, rowBytes >= packedRow, byteCount <= bytes.count,
              byteCount <= 96 * 1024 * 1024, let base = bytes.baseAddress,
              let space = colorSpace(role: role, format: format) else {
            throw NSError(domain: "GameWorldColorFile", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Invalid color export layout or role"])
        }
        let bgra = format == .bgra8Unorm || format == .bgra8Unorm_srgb
        var clipped = 0, nonfinite = 0
        var pixels = Data(count: width * height * bytesPerPixel)
        pixels.withUnsafeMutableBytes { destination in
            let output = destination.bindMemory(to: UInt8.self)
            for y in 0..<height { for x in 0..<width {
                let input = base.advanced(by: y * rowBytes + x * bytesPerPixel)
                let at = (y * width + x) * bytesPerPixel
                if halfFloat {
                    for channel in 0..<3 {
                        let value = Float(Float16(bitPattern: input.loadUnaligned(fromByteOffset: channel * 2, as: UInt16.self)))
                        if !value.isFinite { nonfinite += 1 }
                        else if value < 0 || value > 1 { clipped += 1 }
                        let bounded: Float = value.isFinite ? min(1, max(0, value)) : 0
                        let encoded = UInt16((bounded * 65535).rounded())
                        output[at + channel * 2] = UInt8(encoded >> 8)
                        output[at + channel * 2 + 1] = UInt8(encoded & 255)
                    }
                    output[at + 6] = 255; output[at + 7] = 255
                } else {
                    let input = input.assumingMemoryBound(to: UInt8.self)
                    output[at] = input[bgra ? 2 : 0]
                    output[at + 1] = input[1]
                    output[at + 2] = input[bgra ? 0 : 2]
                    output[at + 3] = 255
                }
            } }
        }
        let bitmap = CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue)
            .union(halfFloat ? .byteOrder16Big : [])
        guard let provider = CGDataProvider(data: pixels as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: halfFloat ? 16 : 8,
                  bitsPerPixel: bytesPerPixel * 8, bytesPerRow: width * bytesPerPixel, space: space,
                  bitmapInfo: bitmap, provider: provider, decode: nil, shouldInterpolate: false,
                  intent: .relativeColorimetric),
              let encoded = CFDataCreateMutable(nil, 0),
              let destination = CGImageDestinationCreateWithData(encoded, "public.png" as CFString, 1, nil) else {
            throw NSError(domain: "GameWorldColorFile", code: 2,
                          userInfo: [NSLocalizedDescriptionKey: "Could not create tagged PNG"])
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw NSError(domain: "GameWorldColorFile", code: 3,
                          userInfo: [NSLocalizedDescriptionKey: "Could not encode tagged PNG"])
        }
        try (encoded as Data).write(to: url, options: [.withoutOverwriting])
        return ["path": url.path, "role": role.rawValue,
                "icc_profile": space.name.map { $0 as String } ?? "unknown",
                "primaries": role == .guest ? "sRGB / Rec.709 D65" : "Display P3 D65",
                "transfer": role == .guest || format == .rgba8Unorm_srgb || format == .bgra8Unorm_srgb ? "sRGB" : "linear",
                "alpha": "opaque viewing companion; raw alpha is not used to darken the image",
                "bits_per_component": halfFloat ? 16 : 8,
                "preview_clipped_channels": clipped, "preview_nonfinite_channels_replaced_with_zero": nonfinite,
                "preview_note": halfFloat ? "Float RGB clipped to [0,1] and quantized to 16-bit linear Display P3; inspect raw rgba16f for extended values. No tonemapping."
                    : "RGB bytes unchanged apart from BGRA channel order; ICC profile defines interpretation."]
    }
}
// END GameWorldColorFile

// BEGIN GameWorldDepthFile
/// CPU-only export of an already completed readback. Rows are streamed from the
/// existing buffer; padding is omitted and every Float bit pattern is preserved.
enum GameWorldDepthFile {
    static func write(_ bytes: UnsafeRawBufferPointer, width: Int, height: Int,
                      rowBytes: Int, to url: URL) throws -> Int {
        let (packedRowBytes, rowOverflow) = width.multipliedReportingOverflow(by: 4)
        let (bufferBytes, bufferOverflow) = rowBytes.multipliedReportingOverflow(by: height)
        let (fileBytes, fileOverflow) = packedRowBytes.multipliedReportingOverflow(by: height)
        var endianProbe: UInt32 = 1
        let littleEndian = withUnsafeBytes(of: &endianProbe) { $0[0] == 1 }
        guard littleEndian, width > 0, height > 0, rowBytes > 0,
              !rowOverflow, !bufferOverflow, !fileOverflow,
              rowBytes >= packedRowBytes, bufferBytes <= bytes.count,
              fileBytes <= 96 * 1024 * 1024, let base = bytes.baseAddress else {
            throw NSError(domain: "GameWorldDepthFile", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Invalid or oversized Float32 readback layout"])
        }
        // Match the capture's other files: never replace a previous diagnostic.
        try Data().write(to: url, options: [.withoutOverwriting])
        let handle = try FileHandle(forWritingTo: url)
        do {
            for row in 0..<height {
                let data = Data(bytesNoCopy: UnsafeMutableRawPointer(mutating: base.advanced(by: row * rowBytes)),
                                count: packedRowBytes, deallocator: .none)
                try handle.write(contentsOf: data)
            }
            try handle.close()
        } catch {
            try? handle.close()
            throw error
        }
        return fileBytes
    }
}
// END GameWorldDepthFile

/// Optional, bounded readback of the actual production shader command.
/// CPU file work starts after completion, using independent shared buffers.
final class GameWorldDebugCapture: @unchecked Sendable {
    private struct Image {
        let buffer: any MTLBuffer
        let width: Int
        let height: Int
        let rowBytes: Int
        let format: MTLPixelFormat
    }
    private let prefix: URL
    private let sourceColor: Image
    private let sourceDepth: Image
    private let outputColor: Image?
    private let outputDepth: Image?
    private let sourceEyeFromClip: simd_float4x4
    private let currentEyeFromSourceEye: simd_float4x4
    private let currentClipFromEye: simd_float4x4
    private let nearFadeDistances: SIMD2<Float>
    private let metadata: [String: Any]
    private static let log = Logger(subsystem: "com.zephyrstudios.santosvision", category: "GameWorldCapture")

    static var minimumGameFrame: UInt64 {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--sv-world-debug-min-game-frame"),
              index + 1 < arguments.count, let value = UInt64(arguments[index + 1]) else { return 600 }
        return value
    }

    static func requestedPrefix() -> URL? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--sv-world-debug-capture"), index + 1 < arguments.count,
              !arguments[index + 1].hasPrefix("--") else { return nil }
        let value = arguments[index + 1]
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let base = value.hasPrefix("/") ? URL(fileURLWithPath: value) : documents.appendingPathComponent(value)
        for number in 0..<50 {
            let candidate = number == 0 ? base : URL(fileURLWithPath: base.path + String(format: "-%03d", number))
            if !FileManager.default.fileExists(atPath: candidate.path + "-report.json"),
               !FileManager.default.fileExists(atPath: candidate.path + "-source.ppm"),
               !FileManager.default.fileExists(atPath: candidate.path + "-output.ppm") { return candidate }
        }
        return nil
    }

    init?(prefix: URL, device: any MTLDevice, commands: any MTLCommandBuffer,
          color: any MTLTexture, depth: any MTLTexture,
          outputColor: any MTLTexture, outputDepth: any MTLTexture,
          slice: Int, viewport: MTLViewport, native: SVWorldFrame,
          sourceEyeFromClip: simd_float4x4, currentEyeFromSourceEye: simd_float4x4,
          currentClipFromEye: simd_float4x4, nearFadeDistances: SIMD2<Float>, timestamp: Double,
          outputViewIndex: Int = 0, renderingMethod: String = "unspecified",
          targetStage: GameWorldCompletedPair.TargetStage? = nil) {
        let formats: Set<MTLPixelFormat> = [.rgba8Unorm, .bgra8Unorm, .rgba8Unorm_srgb, .bgra8Unorm_srgb]
        let width = Int(viewport.width), height = Int(viewport.height)
        let x = Int(viewport.originX), y = Int(viewport.originY)
        guard formats.contains(color.pixelFormat), depth.pixelFormat == .r32Float,
              color.width == depth.width, color.height == depth.height,
              color.sampleCount == 1, depth.sampleCount == 1,
              !color.isFramebufferOnly, !depth.isFramebufferOnly,
              color.storageMode != .memoryless, depth.storageMode != .memoryless else { return nil }
        let outputSupported = (formats.contains(outputColor.pixelFormat) || outputColor.pixelFormat == .rgba16Float)
            && outputDepth.pixelFormat == .depth32Float
            && outputColor.sampleCount == 1 && outputDepth.sampleCount == 1
            && !outputColor.isFramebufferOnly && !outputDepth.isFramebufferOnly
            && outputColor.storageMode != .memoryless && outputDepth.storageMode != .memoryless
            && width > 0 && height > 0 && width <= 4096 && height <= 4096 && x >= 0 && y >= 0
            && x + width <= outputColor.width && y + height <= outputColor.height
            && x + width <= outputDepth.width && y + height <= outputDepth.height
            && slice >= 0 && slice < outputColor.arrayLength && slice < outputDepth.arrayLength
        func allocate(_ width: Int, _ height: Int, _ format: MTLPixelFormat) -> Image? {
            let rowBytes = (width * (format == .rgba16Float ? 8 : 4) + 255) & ~255
            guard let buffer = device.makeBuffer(length: rowBytes * height, options: .storageModeShared) else { return nil }
            return Image(buffer: buffer, width: width, height: height, rowBytes: rowBytes, format: format)
        }
        let sourceBytes = ((color.width * 4 + 255) & ~255) * color.height * 2
        let outputBytes = outputSupported
            ? (((width * (outputColor.pixelFormat == .rgba16Float ? 8 : 4) + 255) & ~255)
                + ((width * 4 + 255) & ~255)) * height : 0
        guard sourceBytes + outputBytes <= 96 * 1024 * 1024,
              let sourceColorBuffer = allocate(color.width, color.height, color.pixelFormat),
              let sourceDepthBuffer = allocate(depth.width, depth.height, depth.pixelFormat),
              let blit = commands.makeBlitCommandEncoder() else { return nil }
        let outputColorBuffer = outputSupported ? allocate(width, height, outputColor.pixelFormat) : nil
        let outputDepthBuffer = outputSupported ? allocate(width, height, outputDepth.pixelFormat) : nil
        self.prefix = prefix
        sourceColor = sourceColorBuffer
        sourceDepth = sourceDepthBuffer
        self.outputColor = outputColorBuffer
        self.outputDepth = outputDepthBuffer
        self.sourceEyeFromClip = sourceEyeFromClip
        self.currentEyeFromSourceEye = currentEyeFromSourceEye
        self.currentClipFromEye = currentClipFromEye
        self.nearFadeDistances = nearFadeDistances
        let publishedColor = UnsafeRawPointer(bitPattern: UInt(native.color_texture)).flatMap {
            Unmanaged<AnyObject>.fromOpaque($0).takeUnretainedValue() as? any MTLTexture
        }
        var details: [String: Any] = ["schema": 3, "scope": "Readback of actual leased textures and the normal compositor shader output before presentation; no substitute render.",
                    "color_export_contract": "Raw files preserve stored RGB. Tagged PNG companions distinguish guest sRGB from CompositorServices Display P3, including attachment transfer encoding. No display screenshot or physical appearance certification.",
                    "game_frame": native.game_frame, "request_frame": native.request_frame,
                    "head_frame": native.head_frame, "view_count": native.view_count,
                    "view_index": native.view_index, "presentation_time": timestamp,
                    "source_view_index": native.view_index, "output_view_index": outputViewIndex,
                    "rendering_method": renderingMethod,
                    "source_presentation_time": native.presentation_time,
                    "source_projection_from_eye": Self.values(sourceEyeFromClip.inverse),
                    "source_eye_from_clip": Self.values(sourceEyeFromClip),
                    "current_eye_from_source_eye": Self.values(currentEyeFromSourceEye),
                    "current_clip_from_eye": Self.values(currentClipFromEye),
                    "game_from_eye": Self.tupleValues(native.game_from_eye),
                    "game_from_stage": Self.tupleValues(native.game_from_stage),
                    "source_near": native.depth_near, "source_far": native.depth_far,
                    "near_fade_distances_m": [nearFadeDistances.x, nearFadeDistances.y],
                    "source_color_format": color.pixelFormat.rawValue,
                    "source_sampling_texture_format": color.pixelFormat.rawValue,
                    "native_published_final_color_format": publishedColor.map { $0.pixelFormat.rawValue as Any } ?? NSNull(),
                    "source_color_format_note": "The copied sampling texture may be a zero-copy sRGB view of the native UNORM allocation or the same-lease HUD-free allocation. Its format does not change the stored source bytes; native publication format is recorded separately.",
                    "source_depth_format": depth.pixelFormat.rawValue,
                    "output_color_format": outputColor.pixelFormat.rawValue,
                    "output_depth_format": outputDepth.pixelFormat.rawValue,
                    "source_color_usage": color.usage.rawValue,
                    "source_depth_usage": depth.usage.rawValue,
                    "output_color_usage": outputColor.usage.rawValue,
                    "output_depth_usage": outputDepth.usage.rawValue,
                    "output_color_storage_mode": outputColor.storageMode.rawValue,
                    "output_depth_storage_mode": outputDepth.storageMode.rawValue,
                    "output_capture_supported": outputColorBuffer != nil && outputDepthBuffer != nil,
                    "output_framebuffer_only": outputColor.isFramebufferOnly || outputDepth.isFramebufferOnly,
                    "output_capture_note": outputSupported ? "Actual compositor targets copied if buffers allocated."
                        : "No output blit encoded: framebuffer-only, memoryless, format, slice or viewport restrictions.",
                    "viewport": [viewport.originX, viewport.originY, viewport.width, viewport.height]]
        func copy(_ texture: any MTLTexture, _ image: Image, _ slice: Int, _ x: Int, _ y: Int) {
            blit.copy(from: texture, sourceSlice: slice, sourceLevel: 0,
                      sourceOrigin: MTLOrigin(x: x, y: y, z: 0),
                      sourceSize: MTLSize(width: image.width, height: image.height, depth: 1),
                      to: image.buffer, destinationOffset: 0, destinationBytesPerRow: image.rowBytes,
                      destinationBytesPerImage: image.rowBytes * image.height)
        }
        blit.label = "One-shot actual GTA source and compositor output readback"
        copy(color, sourceColorBuffer, 0, 0, 0)
        copy(depth, sourceDepthBuffer, 0, 0, 0)
        if let outputColorBuffer, let outputDepthBuffer {
            copy(outputColor, outputColorBuffer, slice, x, y)
            copy(outputDepth, outputDepthBuffer, slice, x, y)
        }
        blit.endEncoding()
        Self.log.info("Queued actual world shader diagnostic: \(prefix.path, privacy: .public)")
        if let targetStage { details["target_stage"] = targetStage.reportValue }
        metadata = details
    }

    func write(compositorAcknowledged: Bool) {
        DispatchQueue.global(qos: .utility).async { [self] in
            let started = Date()
            Self.log.info("Exporting actual world shader diagnostic: \(self.prefix.path, privacy: .public)")
            do {
                try FileManager.default.createDirectory(at: prefix.deletingLastPathComponent(), withIntermediateDirectories: true)
                let sourceStats = try saveColor(sourceColor, suffix: "-source", role: .guest)
                var report = metadata
                report["metal_command_status"] = "completed"
                report["compositor_acknowledged"] = compositorAcknowledged
                report["source_color"] = sourceStats
                report["source_depth"] = try saveDepth(sourceDepth, suffix: "-source-depth.f32",
                                                       pairedColorSuffix: "-source.ppm")
                if let outputColor, let outputDepth {
                    report["output_color"] = try saveColor(outputColor, suffix: "-output", role: .compositor)
                    report["output_depth"] = try saveDepth(outputDepth, suffix: "-output-depth.f32",
                        pairedColorSuffix: outputColor.format == .rgba16Float ? "-output.rgba16f" : "-output.ppm")
                }
                report["sampled_vertex_checks"] = vertexChecks()
                let data = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                try data.write(to: URL(fileURLWithPath: prefix.path + "-report.json"), options: [.withoutOverwriting])
                Self.log.info("Wrote actual world shader diagnostic in \(Date().timeIntervalSince(started)) seconds: \(self.prefix.path, privacy: .public)")
            } catch {
                Self.log.error("Actual world shader diagnostic failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    private static func values(_ matrix: simd_float4x4) -> [Float] {
        [matrix.columns.0, matrix.columns.1, matrix.columns.2, matrix.columns.3]
            .flatMap { [$0.x, $0.y, $0.z, $0.w] }
    }

    private static func tupleValues<T>(_ tuple: T) -> [Float] {
        withUnsafeBytes(of: tuple) { Array($0.bindMemory(to: Float.self)) }
    }

    private func saveColor(_ image: Image, suffix: String, role: GameWorldColorFile.Role) throws -> [String: Any] {
        let bytes = UnsafeRawBufferPointer(start: image.buffer.contents(), count: image.buffer.length)
        var report: [String: Any]
        if image.format == .rgba16Float {
            let packedRow = image.width * 8
            var raw = Data(capacity: packedRow * image.height)
            for y in 0..<image.height {
                raw.append(bytes.bindMemory(to: UInt8.self).baseAddress!.advanced(by: y * image.rowBytes), count: packedRow)
            }
            let path = prefix.path + suffix + ".rgba16f"
            try raw.write(to: URL(fileURLWithPath: path), options: [.withoutOverwriting])
            report = ["width": image.width, "height": image.height, "path": path,
                "encoding": "Raw IEEE754 binary16 RGBA little-endian, tightly packed rows, no header; all bits preserved",
                "transfer": "linear", "primaries": "Display P3 D65", "byte_count": raw.count,
                "raw_data_not_a_tagged_viewing_image": true]
        } else { report = try saveRawColor(image, suffix: suffix + ".ppm") }
        report["role"] = role.rawValue
        report["metal_pixel_format"] = image.format.rawValue
        report["primaries"] = role == .guest ? "sRGB / Rec.709 D65" : "Display P3 D65"
        report["transfer"] = role == .guest || image.format == .rgba8Unorm_srgb || image.format == .bgra8Unorm_srgb ? "sRGB" : "linear"
        report["tagged_png"] = try GameWorldColorFile.writePNG(bytes, width: image.width, height: image.height,
            rowBytes: image.rowBytes, format: image.format, role: role,
            to: URL(fileURLWithPath: prefix.path + suffix + ".png"))
        return report
    }

    private func saveRawColor(_ image: Image, suffix: String) throws -> [String: Any] {
        let bytes = image.buffer.contents().assumingMemoryBound(to: UInt8.self)
        let bgra = image.format == .bgra8Unorm || image.format == .bgra8Unorm_srgb
        var rgb = Data(count: image.width * image.height * 3)
        var nonblack = 0, maximum: UInt8 = 0, alphaNonzero = 0
        rgb.withUnsafeMutableBytes { destination in
            let output = destination.bindMemory(to: UInt8.self).baseAddress!
            for y in 0..<image.height { for x in 0..<image.width {
                let offset = y * image.rowBytes + x * 4
                let r = bytes[offset + (bgra ? 2 : 0)], g = bytes[offset + 1], b = bytes[offset + (bgra ? 0 : 2)]
                let index = (y * image.width + x) * 3
                output[index] = r; output[index + 1] = g; output[index + 2] = b
                if r != 0 || g != 0 || b != 0 { nonblack += 1 }
                maximum = max(maximum, max(r, max(g, b)))
                if bytes[offset + 3] != 0 { alphaNonzero += 1 }
            } }
        }
        var data = Data("P6\n\(image.width) \(image.height)\n255\n".utf8)
        data.append(rgb)
        let path = prefix.path + suffix
        try data.write(to: URL(fileURLWithPath: path), options: [.withoutOverwriting])
        return ["width": image.width, "height": image.height, "nonblack_pixels": nonblack,
                "nonzero_alpha_pixels": alphaNonzero, "maximum_channel_8bit": maximum, "path": path,
                "encoding": "Raw stored RGB bytes in P6 PPM; no ICC profile or transfer conversion",
                "raw_data_not_a_tagged_viewing_image": true]
    }

    private func depthStats(_ image: Image) -> [String: Any] {
        let bytes = image.buffer.contents()
        var minimum = Float.infinity, maximum = -Float.infinity
        var zeros = 0, ones = 0, invalid = 0, inRange = 0
        for y in 0..<image.height {
            let row = bytes.advanced(by: y * image.rowBytes).assumingMemoryBound(to: Float.self)
            for x in 0..<image.width {
                let value = row[x]
                if !value.isFinite { invalid += 1; continue }
                minimum = min(minimum, value); maximum = max(maximum, value)
                if value == 0 { zeros += 1 }; if value == 1 { ones += 1 }
                if (0...1).contains(value) { inRange += 1 }
            }
        }
        return ["width": image.width, "height": image.height,
                "minimum": minimum.isFinite ? minimum as Any : NSNull(),
                "maximum": maximum.isFinite ? maximum as Any : NSNull(),
                "zeros": zeros, "ones": ones, "nonfinite": invalid, "in_0_1": inRange]
    }

    private func saveDepth(_ image: Image, suffix: String, pairedColorSuffix: String) throws -> [String: Any] {
        let path = prefix.path + suffix
        let bytes = UnsafeRawBufferPointer(start: image.buffer.contents(), count: image.buffer.length)
        let written = try GameWorldDepthFile.write(bytes, width: image.width, height: image.height,
                                                    rowBytes: image.rowBytes, to: URL(fileURLWithPath: path))
        var report = depthStats(image)
        report["path"] = path
        report["encoding"] = "IEEE754 binary32 little-endian, tightly packed rows, no header"
        report["byte_count"] = written
        report["row_bytes"] = image.width * 4
        report["readback_row_bytes"] = image.rowBytes
        report["metal_pixel_format"] = image.format.rawValue
        report["pixel_coordinates"] = "Top-left origin; x increases right, y increases down; Float32 byte offset = (y * width + x) * 4"
        report["paired_color_path"] = prefix.path + pairedColorSuffix
        report["paired_color_registration"] = "Depth[x,y] and paired raw color[x,y] are the same captured texel, with identical dimensions and crop. Output coordinates are local to the report viewport crop."
        report["preserves_nonfinite_and_clear_bits"] = true
        return report
    }

    private func vertexChecks() -> [String: Any] {
        var total = 0, inverseInvalid = 0, clipInvalid = 0, inFrustum = 0, nearFaded = 0
        var backgroundSamples = 0, behindEye = 0, geometryDistanceSamples = 0
        let clearDepth: Float = sourceEyeFromClip.inverse.columns.2.z >= 0 ? 0 : 1
        var minimumDistance = Float.infinity, maximumDistance: Float = 0
        for y in stride(from: 0, to: sourceDepth.height, by: max(1, sourceDepth.height / 64)) {
            let row = sourceDepth.buffer.contents().advanced(by: y * sourceDepth.rowBytes).assumingMemoryBound(to: Float.self)
            for x in stride(from: 0, to: sourceDepth.width, by: max(1, sourceDepth.width / 64)) {
                total += 1
                let u = (Float(x) + 0.5) / Float(sourceDepth.width)
                let v = (Float(y) + 0.5) / Float(sourceDepth.height)
                var source = sourceEyeFromClip * SIMD4<Float>(u * 2 - 1, 1 - v * 2, row[x], 1)
                guard row[x].isFinite, (0...1).contains(row[x]),
                      source.x.isFinite, source.y.isFinite, source.z.isFinite, source.w.isFinite
                      else { inverseInvalid += 1; continue }
                let background = row[x] == clearDepth
                if background {
                    backgroundSamples += 1
                    source.w = 0
                } else {
                    guard abs(source.w) > 1e-7 else { inverseInvalid += 1; continue }
                    source /= source.w
                }
                let eye = currentEyeFromSourceEye * source
                var clip = currentClipFromEye * eye
                if background { clip.z = 0 }
                guard clip.x.isFinite, clip.y.isFinite, clip.z.isFinite, clip.w.isFinite
                      else { clipInvalid += 1; continue }
                if clip.w <= 0 { behindEye += 1 }
                if clip.w > 0, abs(clip.x) <= clip.w, abs(clip.y) <= clip.w, clip.z >= 0, clip.z <= clip.w { inFrustum += 1 }
                if background { continue }
                geometryDistanceSamples += 1
                let distance = simd_length(SIMD3(eye.x, eye.y, eye.z))
                minimumDistance = min(minimumDistance, distance); maximumDistance = max(maximumDistance, distance)
                if distance <= nearFadeDistances.x { nearFaded += 1 }
            }
        }
        return ["scope": "Sampled source texel-center checks of the original mesh before any final sky-support depth repair; actual output readback establishes final raster coverage/depth. Exact-clear rays rotate without translation; distance/fade statistics include finite geometry only.",
                "sampled": total, "inverse_invalid": inverseInvalid, "clip_invalid": clipInvalid,
                "exact_clear_background_samples": backgroundSamples, "behind_eye_samples": behindEye,
                "geometry_distance_samples": geometryDistanceSamples,
                "in_current_frustum": inFrustum, "fully_near_faded": nearFaded,
                "minimum_distance_m": minimumDistance.isFinite ? minimumDistance as Any : NSNull(),
                "maximum_distance_m": maximumDistance]
    }
}
#endif

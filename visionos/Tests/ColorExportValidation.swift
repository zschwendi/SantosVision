import CoreGraphics
import Foundation
import ImageIO
import Metal

@main
struct ColorExportValidation {
    static func main() throws {
        let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        var checks = 0
        func check(_ passed: @autoclosure () -> Bool, _ message: String) {
            precondition(passed(), message); checks += 1
        }
        func read(_ url: URL, space: CGColorSpace) throws -> (CGImage, [UInt8]) {
            let source = CGImageSourceCreateWithURL(url as CFURL, nil)!
            let image = CGImageSourceCreateImageAtIndex(source, 0, nil)!
            check(CFEqual(image.colorSpace, space), "PNG round trip retains the declared ICC color space")
            var rgba = [UInt8](repeating: 0, count: image.width * image.height * 4)
            rgba.withUnsafeMutableBytes { bytes in
                let bitmap = CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue)
                let context = CGContext(data: bytes.baseAddress, width: image.width, height: image.height,
                    bitsPerComponent: 8, bytesPerRow: image.width * 4, space: space,
                    bitmapInfo: bitmap.rawValue)!
                context.interpolationQuality = .none
                context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            }
            return (image, rgba)
        }
        let rgb: [[UInt8]] = [[0, 1, 2], [64, 128, 192], [255, 0, 128],
                             [7, 13, 23], [32, 96, 224], [0, 255, 0]]
        for format in [MTLPixelFormat.rgba8Unorm, .bgra8Unorm, .rgba8Unorm_srgb, .bgra8Unorm_srgb] {
            for role in [GameWorldColorFile.Role.guest, .compositor] {
                var bytes = [UInt8](repeating: 0xAF, count: 32)
                let bgra = format == .bgra8Unorm || format == .bgra8Unorm_srgb
                for i in 0..<6 {
                    let at = (i / 3) * 16 + (i % 3) * 4
                    bytes[at] = rgb[i][bgra ? 2 : 0]; bytes[at + 1] = rgb[i][1]
                    bytes[at + 2] = rgb[i][bgra ? 0 : 2]; bytes[at + 3] = UInt8(i * 47)
                }
                let url = output.appendingPathComponent("\(role.rawValue)-\(format.rawValue).png")
                let report = try bytes.withUnsafeBytes {
                    try GameWorldColorFile.writePNG($0, width: 3, height: 2, rowBytes: 16,
                        format: format, role: role, to: url)
                }
                let space = GameWorldColorFile.colorSpace(role: role, format: format)!
                let (image, decoded) = try read(url, space: space)
                check(image.width == 3 && image.height == 2, "padding omitted and exact extent retained")
                for i in 0..<6 { for channel in 0..<3 {
                    check(decoded[i * 4 + channel] == rgb[i][channel], "raw RGB survives PNG encoding without transfer conversion or alpha darkening")
                } }
                check(report["preview_clipped_channels"] as? Int == 0, "8-bit RGB is not clipped")
                check(report["role"] as? String == role.rawValue, "explicit role is retained")
                let linear = role == .compositor && (format == .rgba8Unorm || format == .bgra8Unorm)
                check(report["transfer"] as? String == (linear ? "linear" : "sRGB"), "attachment encoding is distinct from image role")
                do {
                    _ = try bytes.withUnsafeBytes {
                        try GameWorldColorFile.writePNG($0, width: 3, height: 2, rowBytes: 16,
                            format: format, role: role, to: url)
                    }
                    preconditionFailure("must never overwrite an earlier capture")
                } catch { checks += 1 }
            }
        }
        let half: [Float16] = [-0.25, 0.25, 1.25, 0, .nan, 0.5, 1, 0.2]
        let halfURL = output.appendingPathComponent("float-compositor.png")
        let halfReport = try half.withUnsafeBytes {
            try GameWorldColorFile.writePNG($0, width: 2, height: 1, rowBytes: 16,
                format: .rgba16Float, role: .compositor, to: halfURL)
        }
        let (halfImage, halfDecoded) = try read(halfURL, space: CGColorSpace(name: CGColorSpace.linearDisplayP3)!)
        check(halfImage.bitsPerComponent == 16, "half-float viewing companion retains 16-bit precision")
        check(halfReport["preview_clipped_channels"] as? Int == 2, "float clipping is explicitly counted")
        check(halfReport["preview_nonfinite_channels_replaced_with_zero"] as? Int == 1, "invalid float is explicitly counted")
        for (i, expected) in [0, 64, 255, 255, 0, 128, 255, 255].enumerated() where i % 4 != 3 {
            check(abs(Int(halfDecoded[i]) - expected) <= 1, "bounded float preview has the expected linear samples")
        }
        for (width, height, stride) in [(0, 1, 16), (2, 0, 16), (3, 2, 8), (3, 3, 16), (Int.max, 1, 16)] {
            do {
                _ = try half.withUnsafeBytes {
                    try GameWorldColorFile.writePNG($0, width: width, height: height, rowBytes: stride,
                        format: .rgba8Unorm, role: .guest, to: output.appendingPathComponent("invalid.png"))
                }
                preconditionFailure("invalid layout must reject")
            } catch { checks += 1 }
        }
        do {
            _ = try half.withUnsafeBytes {
                try GameWorldColorFile.writePNG($0, width: 2, height: 1, rowBytes: 16,
                    format: .rgba16Float, role: .guest, to: output.appendingPathComponent("invalid-guest.png"))
            }
            preconditionFailure("unknown guest float encoding must reject")
        } catch { checks += 1 }
        print("PASS: \(checks) CPU ImageIO PNG profile, exact RGB, stride, float-preview and refusal checks. No GPU or game run.")
    }
}

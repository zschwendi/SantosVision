import Metal

/// The guest's finished SDR RGB uses sRGB primaries and transfer. Compositor
/// Services consumes extended-linear Display P3, independently of texture
/// storage. An sRGB attachment encodes storage; it does not change primaries.
enum GameWorldColor {
    static func compositorFormat(supported: [MTLPixelFormat], fallback: MTLPixelFormat) -> MTLPixelFormat {
        // Preserve dark-tone precision without doubling the ordinary target's
        // bandwidth. Linear 8-bit is a last resort, never an encoded contract.
        [.bgra8Unorm_srgb, .rgba8Unorm_srgb, .rgba16Float, .bgra8Unorm, .rgba8Unorm]
            .first(where: supported.contains) ?? supported.first ?? fallback
    }

    /// A view shares the immutable publication's allocation and lifetime. Metal
    /// permits the linear/sRGB pair without .pixelFormatView usage. If a view
    /// cannot be made, the shader decodes each tap before interpolation instead.
    static func samplingTexture(_ texture: any MTLTexture) -> any MTLTexture {
        let format: MTLPixelFormat
        switch texture.pixelFormat {
        case .bgra8Unorm: format = .bgra8Unorm_srgb
        case .rgba8Unorm: format = .rgba8Unorm_srgb
        default: return texture
        }
        return texture.makeTextureView(pixelFormat: format) ?? texture
    }

    /// Kept in the existing uniform word; this describes only the source.
    /// 0: hardware-decoded sRGB; 2: encoded UNORM, requiring per-tap decoding.
    static func sourceTransfer(_ format: MTLPixelFormat) -> UInt32 {
        format == .bgra8Unorm || format == .rgba8Unorm ? 2 : 0
    }
}

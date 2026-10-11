#include <metal_stdlib>
using namespace metal;

struct GameWorldUniforms {
    float4x4 sourceEyeFromClip;
    float4x4 currentEyeFromSourceEye;
    float4x4 currentClipFromEye;
    uint2 sourceSize;
    uint sourceDepthReversed;
    uint colorTransfer;
    float2 nearFadeDistances;
    float2 reserved;
};
struct GameWorldVertex {
    float4 position [[position]];
    float3 sourceUVQ;
    float3 currentEyePosition;
    uint background [[flat]];
};

// A perspective plane has affine NDC depth. Every owner of a corner uses the
// same neighborhood, so a qualified continuous plane has identical shared
// vertices. Nine samples distinguish a shallow step from the slope suggested
// by just the two samples immediately straddling it.
static float continuousCornerDepth(uint2 corner, float ownDepth,
                                  constant GameWorldUniforms &u,
                                  texture2d<float, access::read> depth) {
    const float clearDepth = u.sourceDepthReversed != 0 ? 0.0 : 1.0;
    if (!isfinite(ownDepth) || ownDepth < 0 || ownDepth > 1 || ownDepth == clearDepth
        || any(u.sourceSize < 3)) { return ownDepth; }
    const int2 base = clamp(int2(corner) - 1, int2(0), int2(u.sourceSize) - 3);
    float samples[9];
    float magnitude = 0;
    for (uint y = 0; y < 3; ++y) {
        for (uint x = 0; x < 3; ++x) {
            const float z = depth.read(uint2(base + int2(x, y))).r;
            if (!isfinite(z) || z < 0 || z > 1 || z == clearDepth) { return ownDepth; }
            samples[y * 3 + x] = z;
            magnitude = max(magnitude, abs(z));
        }
    }
    const float dx = (samples[2] - samples[0]) * 0.5;
    const float dy = (samples[6] - samples[0]) * 0.5;
    const float tolerance = 4 * (nextafter(magnitude, 2.0f) - magnitude);
    for (uint y = 0; y < 3; ++y) {
        for (uint x = 0; x < 3; ++x) {
            if (abs(samples[y * 3 + x] - (samples[0] + dx * x + dy * y)) > tolerance) {
                return ownDepth;
            }
        }
    }
    // At the image edge the nearest three centers give a consistent one-sided
    // half-texel extrapolation, rather than flattening the plane at the border.
    const float2 offset = float2(corner) - float2(base) - 0.5;
    const float result = samples[0] + dx * offset.x + dy * offset.y;
    const float4 source = u.sourceEyeFromClip * float4(0, 0, result, 1);
    if (!isfinite(result) || result < 0 || result > 1 || !all(isfinite(source))
        || source.w <= 1e-7 || source.z >= 0) { return ownDepth; }
    return result;
}

// Each source texel owns its complete pixel footprint. Qualified continuous
// planes share corners; discontinuity/sky/invalid neighborhoods keep the
// original isolated footprint. Changed viewpoints can still expose background
// that was never present in the source image.
vertex GameWorldVertex gameWorldVertex(uint id [[vertex_id]],
                                      uint instance [[instance_id]],
                                      constant GameWorldUniforms &u [[buffer(0)]],
                                      texture2d<float, access::read> depth [[texture(0)]]) {
    const uint2 corners[4] = { uint2(0, 0), uint2(0, 1), uint2(1, 0), uint2(1, 1) };
    const uint2 pixel = uint2(instance % u.sourceSize.x, instance / u.sourceSize.x);
    const uint2 corner = pixel + corners[id];
    const float2 uv = float2(corner) / float2(u.sourceSize);
    const float z = depth.read(pixel).r;
    const bool background = z == (u.sourceDepthReversed != 0 ? 0.0 : 1.0);
    const float cornerDepth = continuousCornerDepth(corner, z, u, depth);
    float4 point = u.sourceEyeFromClip * float4(uv.x * 2 - 1, 1 - uv.y * 2, cornerDepth, 1);
    bool valid = isfinite(z) && z >= 0 && z <= 1 && all(isfinite(point));
    float sourceClipW = 1;
    if (background) {
        // Exact clear-depth samples represent the far/background limit. Rotate
        // their rays without translation; a finite tilted far plane clips sky.
        point.w = 0;
    } else {
        valid &= abs(point.w) > 1e-7;
        const float divisor = max(abs(point.w), 1e-7) * (point.w < 0 ? -1 : 1);
        sourceClipW = 1 / divisor;
        point /= divisor;
    }
    const float4 currentEye = u.currentEyeFromSourceEye * point;
    float4 currentPoint = u.currentClipFromEye * currentEye;
    if (background) { currentPoint.z = 0; }
    // Keep finite vertices behind/crossing the eye in homogeneous coordinates;
    // the rasterizer clips the whole primitive at the near/eye planes.
    valid &= all(isfinite(currentPoint));
    GameWorldVertex out;
    out.position = valid ? currentPoint : float4(0, 0, -1, 1);
    // Homogeneous source coordinates preserve registration under perspective.
    out.sourceUVQ = float3(uv, 1) * sourceClipW;
    out.currentEyePosition = currentEye.xyz;
    out.background = background;
    return out;
}

fragment float4 gameWorldFragment(GameWorldVertex in [[stage_in]],
                                  constant GameWorldUniforms &u [[buffer(0)]],
                                  texture2d<float> color [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    if (!all(isfinite(in.sourceUVQ)) || in.sourceUVQ.z <= 1e-7) { discard_fragment(); }
    // Stable ordered coverage removes geometry near the current camera while
    // leaving every surviving fragment's real reverse-Z depth unchanged.
    const float visibility = in.background != 0 ? 1 : smoothstep(u.nearFadeDistances.x, u.nearFadeDistances.y,
                                        length(in.currentEyePosition));
    const uint bayer[16] = { 0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5 };
    const uint2 pixel = uint2(in.position.xy);
    const float threshold = (float(bayer[(pixel.y & 3) * 4 + (pixel.x & 3)]) + 0.5) / 16;
    if (visibility <= threshold) { discard_fragment(); }
    float3 rgb = color.sample(s, in.sourceUVQ.xy / in.sourceUVQ.z).rgb;
    // Match sampled source RGB to the negotiated compositor target. Native sRGB
    // formats decode during sampling; sRGB destinations encode during storage.
    if (u.colorTransfer == 1) {
        rgb = select(1.055 * pow(max(rgb, 0.0), float3(1.0 / 2.4)) - 0.055,
                     12.92 * rgb, rgb <= 0.0031308);
    } else if (u.colorTransfer == 2) {
        rgb = select(pow((max(rgb, 0.0) + 0.055) / 1.055, float3(2.4)),
                     rgb / 12.92, rgb <= 0.04045);
    }
    return float4(rgb, 1);
}

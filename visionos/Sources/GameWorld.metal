#include <metal_stdlib>
using namespace metal;

struct GameWorldReticleUniforms { float4 clipPosition; float2 halfSizeNDC; float2 padding; };
struct GameWorldReticleVertex { float4 position [[position]]; float2 local; };
vertex GameWorldReticleVertex gameWorldReticleVertex(uint id [[vertex_id]],
        constant GameWorldReticleUniforms &u [[buffer(0)]]) {
    const float2 corners[4] = { float2(-1, -1), float2(-1, 1), float2(1, -1), float2(1, 1) };
    GameWorldReticleVertex out;
    out.position = u.clipPosition;
    out.position.xy += corners[id] * u.halfSizeNDC * out.position.w;
    out.local = corners[id];
    return out;
}
fragment float4 gameWorldReticleFragment(GameWorldReticleVertex in [[stage_in]]) {
    const float radius = length(in.local);
    if (radius > 1) { discard_fragment(); }
    return float4(radius < 0.55 ? float3(1) : float3(0), 1);
}

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
//
// This is the part that does not depend on which texel owns the vertex: the
// qualified NDC depth of one geometric corner, in [0, 1], or -1 when the
// neighborhood is not one continuous plane. The neighborhood always contains
// every texel that owns the corner, so an invalid or clear owner yields -1.
static float qualifiedCornerDepth(uint2 corner, constant GameWorldUniforms &u,
                                  texture2d<float, access::read> depth) {
    const float clearDepth = u.sourceDepthReversed != 0 ? 0.0 : 1.0;
    if (any(u.sourceSize < 3)) { return -1; }
    const int2 base = clamp(int2(corner) - 1, int2(0), int2(u.sourceSize) - 3);
    float samples[9];
    float magnitude = 0;
    for (uint y = 0; y < 3; ++y) {
        for (uint x = 0; x < 3; ++x) {
            const float z = depth.read(uint2(base + int2(x, y))).r;
            if (!isfinite(z) || z < 0 || z > 1 || z == clearDepth) { return -1; }
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
                return -1;
            }
        }
    }
    // At the image edge the nearest three centers give a consistent one-sided
    // half-texel extrapolation, rather than flattening the plane at the border.
    const float2 offset = float2(corner) - float2(base) - 0.5;
    const float result = samples[0] + dx * offset.x + dy * offset.y;
    const float4 source = u.sourceEyeFromClip * float4(0, 0, result, 1);
    if (!isfinite(result) || result < 0 || result > 1 || !all(isfinite(source))
        || source.w <= 1e-7 || source.z >= 0) { return -1; }
    return result;
}

// The corner depth one owning texel uses: the shared qualified depth, or the
// owner's own depth wherever the neighborhood does not qualify.
static float continuousCornerDepth(uint2 corner, float ownDepth,
                                  constant GameWorldUniforms &u,
                                  texture2d<float, access::read> depth) {
    const float clearDepth = u.sourceDepthReversed != 0 ? 0.0 : 1.0;
    if (!isfinite(ownDepth) || ownDepth < 0 || ownDepth > 1 || ownDepth == clearDepth
        || any(u.sourceSize < 3)) { return ownDepth; }
    const float shared = qualifiedCornerDepth(corner, u, depth);
    return shared >= 0 ? shared : ownDepth;
}

// Each source texel owns its complete pixel footprint. Qualified continuous
// planes share corners; discontinuity/sky/invalid neighborhoods keep the
// original isolated footprint. Changed viewpoints can still expose background
// that was never present in the source image.
static GameWorldVertex gameWorldCornerVertex(uint2 corner, float z, float cornerDepth,
                                             constant GameWorldUniforms &u) {
    const float2 uv = float2(corner) / float2(u.sourceSize);
    const bool background = z == (u.sourceDepthReversed != 0 ? 0.0 : 1.0);
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

static uint2 gameWorldCorner(uint id, uint2 pixel) {
    const uint2 corners[4] = { uint2(0, 0), uint2(0, 1), uint2(1, 0), uint2(1, 1) };
    return pixel + corners[id];
}

vertex GameWorldVertex gameWorldVertex(uint id [[vertex_id]],
                                      uint instance [[instance_id]],
                                      constant GameWorldUniforms &u [[buffer(0)]],
                                      texture2d<float, access::read> depth [[texture(0)]]) {
    const uint2 pixel = uint2(instance % u.sourceSize.x, instance / u.sourceSize.x);
    const uint2 corner = gameWorldCorner(id, pixel);
    const float z = depth.read(pixel).r;
    return gameWorldCornerVertex(corner, z, continuousCornerDepth(corner, z, u, depth), u);
}

// Optional two-pass form of the same geometry. The kernel evaluates each
// geometric corner once into a (width + 1) x (height + 1) texture; the vertex
// stage then needs its own depth and that one texel instead of the 3x3 fit.
kernel void gameWorldCornerDepths(uint2 corner [[thread_position_in_grid]],
                                  constant GameWorldUniforms &u [[buffer(0)]],
                                  texture2d<float, access::read> depth [[texture(0)]],
                                  texture2d<float, access::write> cornerDepths [[texture(1)]]) {
    // The dispatch is rounded up to whole thread groups.
    if (any(corner > u.sourceSize)) { return; }
    cornerDepths.write(qualifiedCornerDepth(corner, u, depth), corner);
}

vertex GameWorldVertex gameWorldPrecomputedVertex(uint id [[vertex_id]],
                                                 uint instance [[instance_id]],
                                                 constant GameWorldUniforms &u [[buffer(0)]],
                                                 texture2d<float, access::read> depth [[texture(0)]],
                                                 texture2d<float, access::read> cornerDepths [[texture(1)]]) {
    const uint2 pixel = uint2(instance % u.sourceSize.x, instance / u.sourceSize.x);
    const uint2 corner = gameWorldCorner(id, pixel);
    const float z = depth.read(pixel).r;
    // An invalid or clear owner is inside its corners' neighborhoods, so those
    // corners hold -1 and the owner keeps its own depth, as in gameWorldVertex.
    const float shared = cornerDepths.read(corner).r;
    return gameWorldCornerVertex(corner, z, shared >= 0 ? shared : z, u);
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

// ===========================================================================
// Experimental paths, all DEFAULT OFF. Everything above this line is the legacy
// pipeline and is unchanged; the functions below are reached only through the
// opt-in pipelines GameWorldRenderer creates for the matching environment flag.
// ===========================================================================

// ---- MUGUET_COMPOSITOR_FINITE_SKY_SUPPORT: explicit final ownership + repair ----
// RGB/fade arithmetic intentionally matches gameWorldFragment above. The MRT
// is written by the same surviving, depth-tested source fragment: 0 untouched,
// 1 exact-clear sky, 2 finite geometry. It is not a color/alpha inference.
struct GameWorldSkyOwnershipOut {
    float4 color [[color(0)]];
    uint owner [[color(1)]];
};

fragment GameWorldSkyOwnershipOut gameWorldSkyOwnershipFragment(GameWorldVertex in [[stage_in]],
                                  constant GameWorldUniforms &u [[buffer(0)]],
                                  texture2d<float> color [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    if (!all(isfinite(in.sourceUVQ)) || in.sourceUVQ.z <= 1e-7) { discard_fragment(); }
    const float visibility = in.background != 0 ? 1 : smoothstep(u.nearFadeDistances.x, u.nearFadeDistances.y,
                                        length(in.currentEyePosition));
    const uint bayer[16] = { 0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5 };
    const uint2 pixel = uint2(in.position.xy);
    const float threshold = (float(bayer[(pixel.y & 3) * 4 + (pixel.x & 3)]) + 0.5) / 16;
    if (visibility <= threshold) { discard_fragment(); }
    float3 rgb = color.sample(s, in.sourceUVQ.xy / in.sourceUVQ.z).rgb;
    if (u.colorTransfer == 1) {
        rgb = select(1.055 * pow(max(rgb, 0.0), float3(1.0 / 2.4)) - 0.055,
                     12.92 * rgb, rgb <= 0.0031308);
    } else if (u.colorTransfer == 2) {
        rgb = select(pow((max(rgb, 0.0) + 0.055) / 1.055, float3(2.4)),
                     rgb / 12.92, rgb <= 0.04045);
    }
    GameWorldSkyOwnershipOut out;
    out.color = float4(rgb, 1);
    out.owner = in.background != 0 ? 1u : 2u;
    return out;
}

struct GameWorldSkyRepairUniforms {
    float4x4 eyeFromClip;
    float4x4 clipFromEye;
    float2 viewportOrigin;
    float2 viewportSize;
    float radius;
    float padding;
    float2 reserved;
};
struct GameWorldSkyRepairVertex { float4 position [[position]]; };
struct GameWorldSkyRepairOut { float depth [[depth(any)]]; };
vertex GameWorldSkyRepairVertex gameWorldSkyRepairVertex(uint id [[vertex_id]]) {
    const float2 corners[3] = { float2(-1, -1), float2(3, -1), float2(-1, 3) };
    GameWorldSkyRepairVertex out;
    out.position = float4(corners[id], 0, 1);
    return out;
}
fragment GameWorldSkyRepairOut gameWorldSkyRepairFragment(GameWorldSkyRepairVertex in [[stage_in]],
                                         constant GameWorldSkyRepairUniforms &u [[buffer(0)]],
                                         texture2d<uint, access::read> ownership [[texture(0)]]) {
    const uint2 pixel = uint2(in.position.xy);
    if (pixel.x >= ownership.get_width() || pixel.y >= ownership.get_height()
        || ownership.read(pixel).r != 1u) { discard_fragment(); }
    const float2 uv = (in.position.xy - u.viewportOrigin) / u.viewportSize;
    float4 eyePoint = u.eyeFromClip * float4(uv.x * 2 - 1, 1 - uv.y * 2, 0.5, 1);
    if (!all(isfinite(eyePoint)) || abs(eyePoint.w) <= 1e-7) { discard_fragment(); }
    const float3 ray = eyePoint.xyz / eyePoint.w;
    const float rayLength = length(ray);
    if (!isfinite(rayLength) || rayLength <= 1e-7 || !isfinite(u.radius) || u.radius <= 0) { discard_fragment(); }
    const float4 support = u.clipFromEye * float4(ray * (u.radius / rayLength), 1);
    if (!all(isfinite(support)) || support.w <= 1e-7) { discard_fragment(); }
    const float z = support.z / support.w;
    // Strict validity, not an arbitrary epsilon. Source RGB stays untouched and
    // final finite-world owners can never enter this depth-only pass.
    if (!(z > 0 && z < 1)) { discard_fragment(); }
    GameWorldSkyRepairOut out;
    out.depth = z;
    return out;
}

// ---- MUGUET_COMPOSITOR_SOURCE_ANCHOR_COPY: one fullscreen triangle per eye ----
//
// The drawable is labelled with the pose the source was rendered at, so the
// current eye and the source eye share one origin by construction. A viewing
// direction therefore maps between the two projections independently of depth,
// and every output pixel reconstructs the source point at its own depth before
// re-projecting it into the current projection. Nothing here assumes 1 - z.
struct GameWorldCopyUniforms {
    float4x4 sourceEyeFromClip;
    float4x4 sourceClipFromEye;
    float4x4 currentEyeFromClip;
    float4x4 currentClipFromEye;
    // Present head relative to the source eye. Geometry and depth stay source-anchored
    // (the drawable carries the source anchor); only the near fade is judged from here.
    float4x4 currentEyeFromSourceEye;
    uint2 sourceSize;
    uint sourceDepthReversed;
    uint colorTransfer;
    float2 nearFadeDistances;
    float2 reserved;
};

struct GameWorldCopyVertex {
    float4 position [[position]];
    float2 ndc;
};

struct GameWorldCopyFragmentOut {
    float4 color [[color(0)]];
    float depth [[depth(any)]];
};

vertex GameWorldCopyVertex gameWorldCopyVertex(uint id [[vertex_id]]) {
    const float2 corners[3] = { float2(-1, -1), float2(3, -1), float2(-1, 3) };
    GameWorldCopyVertex out;
    out.position = float4(corners[id], 0, 1);
    out.ndc = corners[id];
    return out;
}

fragment GameWorldCopyFragmentOut gameWorldSourceCopyFragment(GameWorldCopyVertex in [[stage_in]],
                                                             constant GameWorldCopyUniforms &u [[buffer(0)]],
                                                             texture2d<float> color [[texture(0)]],
                                                             texture2d<float, access::read> depth [[texture(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    // A point on this pixel's viewing ray in eye space, then that ray in source clip space.
    float4 eyePoint = u.currentEyeFromClip * float4(in.ndc, 0.5, 1);
    if (!all(isfinite(eyePoint)) || abs(eyePoint.w) <= 1e-7) { discard_fragment(); }
    eyePoint /= eyePoint.w;
    const float4 sourceClip = u.sourceClipFromEye * eyePoint;
    if (!all(isfinite(sourceClip)) || sourceClip.w <= 1e-7) { discard_fragment(); }
    const float2 sourceNDC = sourceClip.xy / sourceClip.w;
    // Outside the source image: left uncovered, as the mesh leaves it.
    if (any(abs(sourceNDC) > 1)) { discard_fragment(); }
    const float2 uv = float2(sourceNDC.x * 0.5 + 0.5, 0.5 - sourceNDC.y * 0.5);
    const uint2 texel = min(uint2(uv * float2(u.sourceSize)), u.sourceSize - 1);
    const float z = depth.read(texel).r;
    const float clearDepth = u.sourceDepthReversed != 0 ? 0.0 : 1.0;
    // Non-finite or out-of-range source depth draws nothing, as the mesh clips that texel.
    if (!isfinite(z) || z < 0 || z > 1) { discard_fragment(); }
    const bool background = z == clearDepth;
    float outDepth = 0;
    float visibility = 1;
    if (!background) {
        float4 point = u.sourceEyeFromClip * float4(sourceNDC, z, 1);
        if (!all(isfinite(point)) || abs(point.w) <= 1e-7) { discard_fragment(); }
        point /= point.w;
        const float4 current = u.currentClipFromEye * point;
        if (!all(isfinite(current)) || current.w <= 1e-7) { discard_fragment(); }
        outDepth = current.z / current.w;
        // Outside the current depth range: clipped, as the rasterizer clips the mesh.
        if (!(outDepth >= 0 && outDepth <= 1)) { discard_fragment(); }
        const float4 currentEye = u.currentEyeFromSourceEye * point;
        if (!all(isfinite(currentEye))) { discard_fragment(); }
        visibility = smoothstep(u.nearFadeDistances.x, u.nearFadeDistances.y, length(currentEye.xyz));
    }
    const uint bayer[16] = { 0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5 };
    const uint2 pixel = uint2(in.position.xy);
    const float threshold = (float(bayer[(pixel.y & 3) * 4 + (pixel.x & 3)]) + 0.5) / 16;
    if (visibility <= threshold) { discard_fragment(); }
    float3 rgb = color.sample(s, uv).rgb;
    if (u.colorTransfer == 1) {
        rgb = select(1.055 * pow(max(rgb, 0.0), float3(1.0 / 2.4)) - 0.055,
                     12.92 * rgb, rgb <= 0.0031308);
    } else if (u.colorTransfer == 2) {
        rgb = select(pow((max(rgb, 0.0) + 0.055) / 1.055, float3(2.4)),
                     rgb / 12.92, rgb <= 0.04045);
    }
    GameWorldCopyFragmentOut out;
    out.color = float4(rgb, 1);
    out.depth = outDepth;
    return out;
}

// ---- MUGUET_COMPOSITOR_ADAPTIVE_MESH: 8x8 tiles, fan or exact per-texel fallback ----
//
// A tile is accepted only when every covered depth, its one-texel halo and all
// 81 of its geometric corners qualify under the legacy corner semantics and lie
// on one affine NDC plane within the stated tolerance, and no corner is behind
// the eye or inside the near-fade band. Accepted tiles draw a 32-triangle fan
// whose perimeter vertices are the same qualified corners the fine neighbours
// use, so coarse/fine seams share identical vertices. Every other tile draws
// the legacy four-vertex strip per texel through the same vertex arithmetic.
constant uint GAME_WORLD_TILE = 8;
constant uint GAME_WORLD_TILE_TEXELS = GAME_WORLD_TILE * GAME_WORLD_TILE;
constant uint GAME_WORLD_TILE_CORNERS = (GAME_WORLD_TILE + 1) * (GAME_WORLD_TILE + 1);
constant uint GAME_WORLD_TILE_HALO = (GAME_WORLD_TILE + 2) * (GAME_WORLD_TILE + 2);
constant uint GAME_WORLD_FAN_INDICES = 32 * 3;

struct GameWorldAdaptiveParams {
    uint2 tileCount;
    uint toleranceULPs;
    uint rejectBorderTiles;
    // Bit 0 opts into exact-clear sky fans. Zero preserves the prior classifier.
    uint reserved;
};

// The fan matches MTLDrawIndexedPrimitivesIndirectArguments (20 bytes) in a
// 32-byte record. The fallback matches MTLDrawPrimitivesIndirectArguments
// (16 bytes) in the second 32-byte record. Instance counts are the compaction
// counters; the padding keeps each record's address fixed for Metal and Swift.
struct GameWorldIndexedIndirectDraw {
    uint indexCount;
    atomic_uint instanceCount;
    uint indexStart;
    int baseVertex;
    uint baseInstance;
    uint padding[3];
};
struct GameWorldIndirectDraw {
    uint vertexCount;
    atomic_uint instanceCount;
    uint vertexStart;
    uint baseInstance;
    uint padding[4];
};
struct GameWorldAdaptiveDraws {
    GameWorldIndexedIndirectDraw fan;
    GameWorldIndirectDraw fallback;
};

kernel void gameWorldAdaptiveReset(uint id [[thread_position_in_grid]],
                                   device GameWorldAdaptiveDraws &draws [[buffer(0)]]) {
    if (id != 0) { return; }
    draws.fan.indexCount = GAME_WORLD_FAN_INDICES;
    atomic_store_explicit(&draws.fan.instanceCount, 0u, memory_order_relaxed);
    draws.fan.indexStart = 0;
    draws.fan.baseVertex = 0;
    draws.fan.baseInstance = 0;
    draws.fallback.vertexCount = 4;
    atomic_store_explicit(&draws.fallback.instanceCount, 0u, memory_order_relaxed);
    draws.fallback.vertexStart = 0;
    draws.fallback.baseInstance = 0;
}

static float gameWorldToleranceAt(float magnitude, uint ulps) {
    return float(ulps) * (nextafter(magnitude, 2.0f) - magnitude);
}

// One threadgroup of 8x8 threads per tile. Thread (x, y) owns texel (x, y) of
// the tile; corner work is spread over the first 81 thread slots.
kernel void gameWorldAdaptiveClassify(uint2 tile [[threadgroup_position_in_grid]],
                                      uint2 local [[thread_position_in_threadgroup]],
                                      uint localIndex [[thread_index_in_threadgroup]],
                                      constant GameWorldUniforms &u [[buffer(0)]],
                                      constant GameWorldAdaptiveParams &p [[buffer(1)]],
                                      device uint *fanTiles [[buffer(2)]],
                                      device uint *fallbackTiles [[buffer(3)]],
                                      device GameWorldAdaptiveDraws &draws [[buffer(4)]],
                                      device uint *classes [[buffer(5)]],
                                      texture2d<float, access::read> depth [[texture(0)]]) {
    threadgroup float halo[GAME_WORLD_TILE_HALO];
    threadgroup float corners[GAME_WORLD_TILE_CORNERS];
    threadgroup atomic_uint rejectFlags;
    threadgroup atomic_uint clearHaloSamples;
    if (any(tile >= p.tileCount)) { return; }
    if (localIndex == 0) {
        atomic_store_explicit(&rejectFlags, 0u, memory_order_relaxed);
        atomic_store_explicit(&clearHaloSamples, 0u, memory_order_relaxed);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    const uint tileIndex = tile.y * p.tileCount.x + tile.x;
    const uint2 origin = tile * GAME_WORLD_TILE;
    const float clearDepth = u.sourceDepthReversed != 0 ? 0.0 : 1.0;
    // Whole tiles only; partial tiles at the image edge always fall back.
    const bool partial = any(origin + GAME_WORLD_TILE > u.sourceSize);
    const bool border = p.rejectBorderTiles != 0
        && (tile.x == 0 || tile.y == 0 || origin.x + GAME_WORLD_TILE >= u.sourceSize.x || origin.y + GAME_WORLD_TILE >= u.sourceSize.y);
    if (partial || border || any(u.sourceSize < 3)) {
        if (localIndex == 0) { atomic_fetch_or_explicit(&rejectFlags, 1u, memory_order_relaxed); }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (atomic_load_explicit(&rejectFlags, memory_order_relaxed) == 0) {
        // Covered texels and the one-texel halo (10x10 window), clamped at the image edge.
        for (uint slot = localIndex; slot < GAME_WORLD_TILE_HALO; slot += GAME_WORLD_TILE_TEXELS) {
            const int2 offset = int2(slot % (GAME_WORLD_TILE + 2), slot / (GAME_WORLD_TILE + 2)) - 1;
            const int2 pixel = clamp(int2(origin) + offset, int2(0), int2(u.sourceSize) - 1);
            const float z = depth.read(uint2(pixel)).r;
            halo[slot] = z;
            if (!isfinite(z) || z < 0 || z > 1 || z == clearDepth) {
                atomic_fetch_or_explicit(&rejectFlags, 2u, memory_order_relaxed);
            }
            if ((p.reserved & 1u) != 0 && z == clearDepth) {
                atomic_fetch_add_explicit(&clearHaloSamples, 1u, memory_order_relaxed);
            }
        }
        // All 81 geometric corners through the legacy qualification, plus the
        // current-eye gates the fan would otherwise interpolate across.
        for (uint c = localIndex; c < GAME_WORLD_TILE_CORNERS; c += GAME_WORLD_TILE_TEXELS) {
            const uint2 corner = origin + uint2(c % (GAME_WORLD_TILE + 1), c / (GAME_WORLD_TILE + 1));
            const float shared = qualifiedCornerDepth(corner, u, depth);
            corners[c] = shared;
            if (shared < 0) {
                atomic_fetch_or_explicit(&rejectFlags, 4u, memory_order_relaxed);
            } else {
                const float2 uv = float2(corner) / float2(u.sourceSize);
                float4 point = u.sourceEyeFromClip * float4(uv.x * 2 - 1, 1 - uv.y * 2, shared, 1);
                const float divisor = max(abs(point.w), 1e-7) * (point.w < 0 ? -1 : 1);
                point /= divisor;
                const float4 currentEye = u.currentEyeFromSourceEye * point;
                const float4 currentPoint = u.currentClipFromEye * currentEye;
                // Behind or at the eye, inside the fade band, or outside the clip depth
                // range [0, w]: a large fan triangle would be clipped or faded by
                // interpolation that the fine quads do per texel, so fall back.
                if (!all(isfinite(currentPoint)) || currentPoint.w <= 1e-6
                    || currentPoint.z < 0 || currentPoint.z > currentPoint.w
                    || length(currentEye.xyz) < u.nearFadeDistances.y) {
                    atomic_fetch_or_explicit(&rejectFlags, 8u, memory_order_relaxed);
                }
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    // A fully clear 10x10 halo proves every covered owner is exact-clear and
    // every neighbouring fine texel at the seam has the same sky-ray rule.
    // The halo sets reject bit 2 and the still-running legacy corner loop sets
    // bit 4 for every clear owner, so the exact-clear signature is 2|4 = 6.
    // A single invalid or foreground sample prevents this branch. All 81
    // corner rays must also remain finite and in front of the current eye;
    // otherwise the exact per-texel strips retain ownership of clipping.
    if ((p.reserved & 1u) != 0 &&
        atomic_load_explicit(&rejectFlags, memory_order_relaxed) == 6u &&
        atomic_load_explicit(&clearHaloSamples, memory_order_relaxed) == GAME_WORLD_TILE_HALO) {
        for (uint c = localIndex; c < GAME_WORLD_TILE_CORNERS; c += GAME_WORLD_TILE_TEXELS) {
            const uint2 corner = origin + uint2(c % (GAME_WORLD_TILE + 1), c / (GAME_WORLD_TILE + 1));
            const float2 uv = float2(corner) / float2(u.sourceSize);
            float4 sourceRay = u.sourceEyeFromClip * float4(uv.x * 2 - 1, 1 - uv.y * 2, clearDepth, 1);
            const bool finiteSource = all(isfinite(sourceRay));
            sourceRay.w = 0;
            const float4 currentEye = u.currentEyeFromSourceEye * sourceRay;
            const float4 currentClip = u.currentClipFromEye * currentEye;
            if (!finiteSource || !all(isfinite(currentEye)) || !all(isfinite(currentClip))
                || currentClip.w <= 1e-6) {
                atomic_fetch_or_explicit(&rejectFlags, 64u, memory_order_relaxed);
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (localIndex != 0) { return; }
    uint flags = atomic_load_explicit(&rejectFlags, memory_order_relaxed);
    if (flags == 0) {
        // One affine NDC plane from three corners; every corner, covered centre
        // and halo centre must agree with it within the stated tolerance.
        const float c00 = corners[0];
        const float c80 = corners[GAME_WORLD_TILE];
        const float c08 = corners[GAME_WORLD_TILE * (GAME_WORLD_TILE + 1)];
        const float dx = (c80 - c00) / float(GAME_WORLD_TILE);
        const float dy = (c08 - c00) / float(GAME_WORLD_TILE);
        float magnitude = 0;
        for (uint c = 0; c < GAME_WORLD_TILE_CORNERS; ++c) { magnitude = max(magnitude, abs(corners[c])); }
        for (uint h = 0; h < GAME_WORLD_TILE_HALO; ++h) { magnitude = max(magnitude, abs(halo[h])); }
        const float tolerance = gameWorldToleranceAt(magnitude, p.toleranceULPs);
        for (uint c = 0; c < GAME_WORLD_TILE_CORNERS && flags == 0; ++c) {
            const float x = float(c % (GAME_WORLD_TILE + 1)), y = float(c / (GAME_WORLD_TILE + 1));
            if (abs(corners[c] - (c00 + dx * x + dy * y)) > tolerance) { flags |= 16u; }
        }
        for (uint h = 0; h < GAME_WORLD_TILE_HALO && flags == 0; ++h) {
            // Texel centres sit half a texel inside their corner lattice: offset -1 for the halo, +0.5 for the centre.
            const float x = float(h % (GAME_WORLD_TILE + 2)) - 0.5, y = float(h / (GAME_WORLD_TILE + 2)) - 0.5;
            if (abs(halo[h] - (c00 + dx * x + dy * y)) > tolerance) { flags |= 32u; }
        }
    }
    // Class 1 is the established foreground plane; class 3 is an exact-clear
    // sky tile. Both use the existing indexed fan and unchanged fragment path.
    const bool skyFan = (p.reserved & 1u) != 0 && flags == 6u &&
        atomic_load_explicit(&clearHaloSamples, memory_order_relaxed) == GAME_WORLD_TILE_HALO;
    if (flags == 0 || skyFan) {
        const uint slot = atomic_fetch_add_explicit(&draws.fan.instanceCount, 1u, memory_order_relaxed);
        fanTiles[slot] = tileIndex;
        classes[tileIndex] = skyFan ? 3u : 1u;
    } else {
        const uint base = atomic_fetch_add_explicit(&draws.fallback.instanceCount, GAME_WORLD_TILE_TEXELS, memory_order_relaxed);
        fallbackTiles[base / GAME_WORLD_TILE_TEXELS] = tileIndex;
        classes[tileIndex] = flags << 1;
    }
}

// Perimeter corner s (0..31) of a tile, clockwise from its top-left corner.
static uint2 gameWorldFanPerimeter(uint s) {
    if (s < 8) { return uint2(s, 0); }
    if (s < 16) { return uint2(8, s - 8); }
    if (s < 24) { return uint2(24 - s, 8); }
    return uint2(0, 32 - s);
}

vertex GameWorldVertex gameWorldAdaptiveFanVertex(uint id [[vertex_id]],
                                                  uint instance [[instance_id]],
                                                  constant GameWorldUniforms &u [[buffer(0)]],
                                                  constant GameWorldAdaptiveParams &p [[buffer(1)]],
                                                  const device uint *fanTiles [[buffer(2)]],
                                                  texture2d<float, access::read> depth [[texture(0)]]) {
    const uint tileIndex = fanTiles[instance];
    const uint2 origin = uint2(tileIndex % p.tileCount.x, tileIndex / p.tileCount.x) * GAME_WORLD_TILE;
    // Indexed triangles [0, 1+t, 1+(t+1)%32] reuse one centre and the 32
    // existing perimeter corners without changing their positions or order.
    const uint2 corner = origin + (id == 0 ? uint2(GAME_WORLD_TILE / 2) : gameWorldFanPerimeter(id - 1));
    // The owning texel inside this tile; for an accepted tile every owner is valid
    // and non-clear, so the owner only fixes `valid` and `background` as the fine path does.
    const uint2 owner = uint2(corner.x == origin.x + GAME_WORLD_TILE ? corner.x - 1 : corner.x,
                              corner.y == origin.y + GAME_WORLD_TILE ? corner.y - 1 : corner.y);
    const float z = depth.read(owner).r;
    return gameWorldCornerVertex(corner, z, continuousCornerDepth(corner, z, u, depth), u);
}

vertex GameWorldVertex gameWorldAdaptiveFallbackVertex(uint id [[vertex_id]],
                                                       uint instance [[instance_id]],
                                                       constant GameWorldUniforms &u [[buffer(0)]],
                                                       constant GameWorldAdaptiveParams &p [[buffer(1)]],
                                                       const device uint *fallbackTiles [[buffer(2)]],
                                                       texture2d<float, access::read> depth [[texture(0)]]) {
    const uint tileIndex = fallbackTiles[instance / GAME_WORLD_TILE_TEXELS];
    const uint texel = instance % GAME_WORLD_TILE_TEXELS;
    const uint2 pixel = uint2(tileIndex % p.tileCount.x, tileIndex / p.tileCount.x) * GAME_WORLD_TILE
        + uint2(texel % GAME_WORLD_TILE, texel / GAME_WORLD_TILE);
    if (any(pixel >= u.sourceSize)) {
        // Partial edge tile: texels outside the image draw nothing.
        GameWorldVertex out;
        out.position = float4(0, 0, -1, 1);
        out.sourceUVQ = float3(0, 0, 1);
        out.currentEyePosition = float3(0);
        out.background = 0;
        return out;
    }
    const uint2 corner = gameWorldCorner(id, pixel);
    const float z = depth.read(pixel).r;
    return gameWorldCornerVertex(corner, z, continuousCornerDepth(corner, z, u, depth), u);
}

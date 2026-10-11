#include <metal_stdlib>
using namespace metal;

// MUGUET_COMPOSITOR_ANCHOR_PROBE (default OFF): a small frozen analytic scene
// rendered into two coherent source eye textures from one retained ARKit
// anchor. It writes source colour and the exact NDC depth the probe's own
// projection produced, so the compositor paths consume it exactly like a game
// frame. This is a platform probe, never game content.

struct GameWorldProbeVertexIn {
    float4 position;   // stage space, w = 1
    float4 color;      // rgb, a = checker scale (0 = flat colour)
    float2 uv;
    float2 pad;
};

struct GameWorldProbeUniforms {
    float4x4 clipFromStage;
};

struct GameWorldProbeRaster {
    float4 position [[position]];
    float4 color;
    float2 uv;
};

struct GameWorldProbeOut {
    float4 color [[color(0)]];
    float depth [[color(1)]];
};

vertex GameWorldProbeRaster gameWorldProbeVertex(uint id [[vertex_id]],
                                                const device GameWorldProbeVertexIn *vertices [[buffer(0)]],
                                                constant GameWorldProbeUniforms &u [[buffer(1)]]) {
    GameWorldProbeRaster out;
    out.position = u.clipFromStage * vertices[id].position;
    out.color = vertices[id].color;
    out.uv = vertices[id].uv;
    return out;
}

fragment GameWorldProbeOut gameWorldProbeFragment(GameWorldProbeRaster in [[stage_in]]) {
    float3 rgb = in.color.rgb;
    if (in.color.a > 0) {
        const float2 cell = floor(in.uv * in.color.a);
        const float checker = fmod(cell.x + cell.y, 2.0) == 0 ? 1.0 : 0.55;
        rgb *= checker;
    }
    GameWorldProbeOut out;
    out.color = float4(rgb, 1);
    // The rasterizer's NDC depth for this fragment, identical to what the depth
    // attachment stores; the compositor reads it back as the source depth.
    out.depth = in.position.z;
    return out;
}

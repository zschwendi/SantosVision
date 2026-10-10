#include <metal_stdlib>
using namespace metal;

struct CalibrationVertex {
    float4 position;
    float4 color;
};
struct RasterVertex {
    float4 position [[position]];
    float4 color;
};

vertex RasterVertex calibrationVertex(uint id [[vertex_id]],
                                      const device CalibrationVertex *vertices [[buffer(0)]],
                                      constant float4x4 &viewProjection [[buffer(1)]]) {
    RasterVertex out;
    out.position = viewProjection * vertices[id].position;
    out.color = vertices[id].color;
    return out;
}

fragment float4 calibrationFragment(RasterVertex in [[stage_in]]) {
    return in.color;
}

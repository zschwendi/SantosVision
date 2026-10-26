// Test-only observability for the unchanged hit search and production qualified fill.
kernel void probeFillSampling(uint2 pixel [[thread_position_in_grid]],
        constant GameWorldDepthEdgeFillUniforms &u [[buffer(0)]],
        texture2d<uint,access::read> owner [[texture(0)]],
        texture2d<float,access::read> depth [[texture(1)]],
        texture2d<float,access::write> result [[texture(2)]]) {
    if(pixel.x>=owner.get_width()||pixel.y>=owner.get_height()) { return; }
    float4 value=0;
    if(owner.read(pixel).r==0) {
        const float2 screenUV=(float2(pixel)+.5-u.viewportOrigin)/u.viewportSize;
        const float4 e=u.currentEyeFromClip*float4(screenUV.x*2-1,1-screenUV.y*2,.5,1);
        const float3 ray=normalize(e.xyz/e.w);
        GameWorldDepthEdgeHit hit={uint2(0),0,0,false};
        if(gameWorldDepthEdgeSearch(ray,u,depth,hit)) {
            const float4 h0=u.sourceClipFromCurrentEye*float4(ray,0),h1=u.sourceClipFromCurrentEye[3];
            const float s=(h0.z-hit.sourceDepth*h0.w)/(hit.sourceDepth*h1.w-h1.z);
            const float4 h=h0+s*h1;
            value.xy=gameWorldDepthEdgeTexel(h,float2(u.sourceSize))-(float2(hit.texel)+.5);
            value.w=hit.sky ? 2:1;
            float2 uv;float4 point;
            if(gameWorldDepthEdgePlanarSample(ray,u,depth,hit,uv,point)) { value.z=1; }
        }
    }
    result.write(value,pixel);
}

kernel void probeFillDirect(constant GameWorldDepthEdgeFillUniforms &u [[buffer(0)]],
        constant GameWorldDepthEdgeHit &hit [[buffer(1)]],
        constant float3 &ray [[buffer(2)]],
        texture2d<float,access::read> depth [[texture(0)]],
        texture2d<float,access::write> result [[texture(1)]],
        texture2d<float> color [[texture(2)]],
        texture2d<float,access::write> filterResult [[texture(3)]]) {
    float2 uv=0;float4 point=0;
    const bool qualified=gameWorldDepthEdgePlanarSample(ray,u,depth,hit,uv,point);
    result.write(float4(uv,point.w,qualified ? 1:0),uint2(0));
    filterResult.write(qualified ? float4(gameWorldSampleLinearSRGB(color,uv,u.colorTransfer),1) : float4(0),uint2(0));
}

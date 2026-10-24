// CPU mirror of GameWorld.metal gameWorldDepthEdgeSearch and
// gameWorldDepthEdgeFillFragment, statement for statement. Shared by the CPU
// validation and the Metal parity harness; keep both in step with the shader.
import Foundation
import simd

struct Hit { var texel: SIMD2<UInt32>; var sourceDepth: Float; var inverseDistance: Float; var sky: Bool }
struct Fill { var texel: SIMD2<UInt32>; var depth: Float; var sky: Bool }

func finite(_ v: SIMD4<Float>) -> Bool { v.x.isFinite && v.y.isFinite && v.z.isFinite && v.w.isFinite }
func texelOf(_ clip: SIMD4<Float>, _ size: SIMD2<Float>) -> SIMD2<Float> {
    let ndc = SIMD2(clip.x, clip.y) / clip.w
    return SIMD2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * size
}
func search(_ ray: SIMD3<Float>, _ u: GameWorldDepthEdgeFillUniforms, _ depth: (SIMD2<UInt32>) -> Float) -> Hit? {
    let h0 = u.sourceClipFromCurrentEye * SIMD4(ray, 0)
    let h1 = u.sourceClipFromCurrentEye.columns.3
    guard finite(h0), finite(h1), h0.w > 1e-6, u.maximumInverseDistance.isFinite, u.maximumInverseDistance > 0,
          u.sourceSize.x >= 2, u.sourceSize.y >= 2, u.maximumSteps > 0 else { return nil }
    var end = u.maximumInverseDistance
    if h1.w < 0 { end = min(end, 0.5 * h0.w / -h1.w) }
    let size = SIMD2<Float>(Float(u.sourceSize.x), Float(u.sourceSize.y))
    let start = texelOf(h0, size)
    var last = h0 + end * h1
    var span = simd_distance(start, texelOf(last, size))
    guard span.isFinite else { return nil }
    let steps = Float(u.maximumSteps)
    if span > steps {
        let f = steps / span
        end *= f * h0.w / ((1 - f) * last.w + f * h0.w)
        last = h0 + end * h1
        span = steps
    }
    let count = UInt32(min(max(span.rounded(.up), 1), steps))
    let clear: Float = u.sourceDepthReversed != 0 ? 0 : 1
    var free = false
    var hit: Hit?
    for k in 0...count {
        let f = Float(k) / Float(count)
        let s = end * f * h0.w / ((1 - f) * last.w + f * h0.w)
        let h = h0 + s * h1
        guard finite(h), h.w > 1e-6 else { break }
        if abs(h.x) > h.w || abs(h.y) > h.w { break }
        let t = simd_max(texelOf(h, size), .zero)
        let texel = SIMD2(min(UInt32(t.x), u.sourceSize.x - 1), min(UInt32(t.y), u.sourceSize.y - 1))
        let z = depth(texel)
        if !z.isFinite || z < 0 || z > 1 { continue }
        let rayDepth = h.z / h.w
        let behind = z == clear || (u.sourceDepthReversed != 0 ? z < rayDepth : z > rayDepth)
        if behind && !free { hit = Hit(texel: texel, sourceDepth: z, inverseDistance: s, sky: z == clear) }
        free = behind
    }
    return hit
}
func fill(pixel: SIMD2<Int>, _ u: GameWorldDepthEdgeFillUniforms, _ depth: (SIMD2<UInt32>) -> Float) -> Fill? {
    let position = SIMD2<Float>(Float(pixel.x) + 0.5, Float(pixel.y) + 0.5)
    let uv = (position - u.viewportOrigin) / u.viewportSize
    let eyePoint = u.currentEyeFromClip * SIMD4(uv.x * 2 - 1, 1 - uv.y * 2, 0.5, 1)
    var ray = SIMD3(eyePoint.x, eyePoint.y, eyePoint.z) / eyePoint.w
    let rayLength = simd_length(ray)
    guard finite(eyePoint), abs(eyePoint.w) > 1e-7, rayLength.isFinite, rayLength > 1e-7, ray.z < 0,
          let hit = search(ray / rayLength, u, depth) else { return nil }
    ray /= rayLength
    let point: SIMD4<Float>
    if hit.sky { point = SIMD4(ray * u.skyRadius, 1) }
    else {
        let h0 = u.sourceClipFromCurrentEye * SIMD4(ray, 0)
        let h1 = u.sourceClipFromCurrentEye.columns.3
        let s = (h0.z - hit.sourceDepth * h0.w) / (hit.sourceDepth * h1.w - h1.z)
        point = SIMD4(ray, s.isFinite && s > 0 ? s : hit.inverseDistance)
    }
    let clip = u.currentClipFromEye * point
    let z = clip.z / clip.w
    return Fill(texel: hit.texel, depth: finite(clip) && clip.w > 1e-7 && z >= 0 && z <= 1 ? z : 0, sky: hit.sky)
}

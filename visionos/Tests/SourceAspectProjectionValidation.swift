import Foundation
import simd

/// Actual registered third-person matrices through unchanged production lens,
/// source-coverage, sampling-density and HUD-size functions. No new game image
/// is fabricated and no runtime/GPU/physical quality acceptance is inferred.
@main
struct SourceAspectProjectionValidation {
    static func main() throws {
        let url = URL(fileURLWithPath: CommandLine.arguments[1])
        let data = try Data(contentsOf: url)
        let report = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        precondition((report["schema"] as? NSNumber)?.intValue == 2)
        precondition(report["metal_command_status"] as? String == "completed")
        let raw = (report["current_clip_from_eye"] as! [NSNumber]).map(\.floatValue)
        precondition(raw.count == 16)
        let display = simd_float4x4(columns: (
            SIMD4(raw[0], raw[1], raw[2], raw[3]), SIMD4(raw[4], raw[5], raw[6], raw[7]),
            SIMD4(raw[8], raw[9], raw[10], raw[11]), SIMD4(raw[12], raw[13], raw[14], raw[15])))
        let viewport = (report["viewport"] as! [NSNumber]).map(\.intValue)
        let source = GameWorldSourceGuardBand.projection(display, scale: 1.3)!
        let conversion = GameWorldProjection.nativePinhole(display)!
        let inverseDisplay = double(display).inverse
        let guarded = double(source)
        var maximumRayError: Double = 0
        var points = 0
        // Independent dense frustum lattice: the guarded source must preserve
        // the recorded physical directions while placing them inside its margin.
        for z in [0.0, 0.1, 0.5, 0.9, 1.0] {
            for y in -20...20 { for x in -20...20 {
                let clip = SIMD4<Double>(Double(x) / 20, Double(y) / 20, z, 1)
                let point = inverseDisplay * clip
                let q = guarded * point
                let expected = SIMD2(clip.x, clip.y) / 1.3
                maximumRayError = max(maximumRayError, simd_length(SIMD2(q.x, q.y) / q.w - expected))
                points += 1
            }}
        }
        precondition(maximumRayError < GameWorldProjection.maximumNDCError)
        precondition(conversion.matrix[2][0] != 0 && conversion.matrix[2][1] != 0,
                     "asymmetric physical eye calibration is retained")
        let sizes = [SIMD2(2560, 1440), SIMD2(2560, 2304), SIMD2(3840, 2160)]
        let density = sizes.map { size in
            GameWorldSamplingDensity.measure(eye: 0, displayProjection: display,
                displayPixels: SIMD2(viewport[2], viewport[3]), sourceProjection: source, sourcePixels: size)!
        }
        precondition(density[1].sourcePixelsPerDegree.x == density[0].sourcePixelsPerDegree.x)
        precondition(abs(density[1].sourcePixelsPerDegree.y / density[0].sourcePixelsPerDegree.y - 1.6) < 1e-12)
        precondition(density[1].displayPixelsPerSourceTexel.y < density[2].displayPixelsPerSourceTexel.y)
        precondition(2560 * 2304 < 3840 * 2160)
        for size in sizes {
            let half = GameWorldHUDPolicy.halfSize(width: size.x, height: size.y, angularWidth: 60, distance: 2)!
            precondition(abs(half.x / half.y - Float(size.x) / Float(size.y)) < 1e-6,
                         "existing HUD canvas retains actual source aspect instead of assuming16:9")
        }
        let result: [String: Any] = [
            "scope": "Registered third-person projection transported through unchanged production math; not a new rendered image, game/HUD compatibility or physical fidelity result",
            "registered_report": url.path, "sampled_frustum_points": points,
            "maximum_guard_ray_error_ndc": maximumRayError,
            "density": density.map(\.reportValue),
            "native_projection_and_eye_asymmetry_unchanged": true]
        print(String(data: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), encoding: .utf8)!)
    }

    private static func double(_ m: simd_float4x4) -> simd_double4x4 {
        simd_double4x4(columns: (SIMD4<Double>(m[0]), SIMD4<Double>(m[1]), SIMD4<Double>(m[2]), SIMD4<Double>(m[3])))
    }
}

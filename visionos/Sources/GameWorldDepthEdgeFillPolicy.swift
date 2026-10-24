import Foundation
import simd

/// Mirrors `GameWorldDepthEdgeFillUniforms` in GameWorld.metal (240 bytes).
struct GameWorldDepthEdgeFillUniforms {
    var currentEyeFromClip: simd_float4x4
    var currentClipFromEye: simd_float4x4
    var sourceClipFromCurrentEye: simd_float4x4
    var viewportOrigin: SIMD2<Float>
    var viewportSize: SIMD2<Float>
    var sourceSize: SIMD2<UInt32>
    var sourceDepthReversed: UInt32
    var colorTransfer: UInt32
    var maximumInverseDistance: Float
    var skyRadius: Float
    var maximumSteps: UInt32
    var reserved: UInt32 = 0
}

/// Registered reprojection captures show opaque black owner-0 gaps beside
/// nearer geometry and between isolated texel footprints, absent from the
/// source frame. The fill copies the adjacent background texel from the same
/// source frame into those gaps only. It is opt-in until a physical A/B: the
/// launcher toggle persists the choice, and explicit QA values override it.
enum GameWorldDepthEdgeFillPolicy {
    static let storedKey = "SantosVisionDepthEdgeFillEnabled"
    /// Longest searched gap, in source texels along the epipolar line.
    static let maximumSteps: UInt32 = 64

    static var stored: Bool { UserDefaults.standard.bool(forKey: storedKey) }

    static func requested(environment: [String: String], arguments: [String], stored: Bool) -> Bool {
        if arguments.contains("--sv-world-disable-depth-edge-fill") { return false }
        switch environment["MUGUET_COMPOSITOR_DEPTH_EDGE_FILL"] {
        case "1": return true
        case "0": return false
        default: return arguments.contains("--sv-world-depth-edge-fill") || stored
        }
    }

    /// The fill reconstructs exactly the frame the world pass drew: the same
    /// source lease projection, relative eye and display projection. Nil
    /// leaves the gaps untouched rather than guessing a geometry.
    static func uniforms(sourceProjection: simd_float4x4, currentEyeFromSourceEye: simd_float4x4,
                         displayProjection: simd_float4x4, viewportOrigin: SIMD2<Float>,
                         viewportSize: SIMD2<Float>, sourceSize: SIMD2<UInt32>,
                         sourceDepthReversed: UInt32, colorTransfer: UInt32,
                         nearestDistance: Float, skyRadius: Float) -> GameWorldDepthEdgeFillUniforms? {
        func double(_ matrix: simd_float4x4) -> simd_double4x4 {
            simd_double4x4(columns: (SIMD4<Double>(matrix.columns.0), SIMD4<Double>(matrix.columns.1),
                                     SIMD4<Double>(matrix.columns.2), SIMD4<Double>(matrix.columns.3)))
        }
        func float(_ matrix: simd_double4x4) -> simd_float4x4 {
            simd_float4x4(columns: (SIMD4<Float>(matrix.columns.0), SIMD4<Float>(matrix.columns.1),
                                    SIMD4<Float>(matrix.columns.2), SIMD4<Float>(matrix.columns.3)))
        }
        func finite(_ matrix: simd_double4x4) -> Bool {
            [matrix.columns.0, matrix.columns.1, matrix.columns.2, matrix.columns.3].allSatisfy {
                $0.x.isFinite && $0.y.isFinite && $0.z.isFinite && $0.w.isFinite
            }
        }
        let relative = double(currentEyeFromSourceEye)
        let display = double(displayProjection)
        guard finite(relative), finite(display), finite(double(sourceProjection)),
              abs(relative.determinant) > 1e-6, abs(display.determinant) > 1e-12,
              viewportOrigin.x.isFinite, viewportOrigin.y.isFinite,
              viewportSize.x.isFinite, viewportSize.y.isFinite, viewportSize.x > 0, viewportSize.y > 0,
              sourceSize.x >= 2, sourceSize.y >= 2,
              nearestDistance.isFinite, nearestDistance > 0, skyRadius.isFinite, skyRadius > 0 else { return nil }
        let sourceClipFromCurrentEye = double(sourceProjection) * relative.inverse
        let currentEyeFromClip = display.inverse
        guard finite(sourceClipFromCurrentEye), finite(currentEyeFromClip) else { return nil }
        return GameWorldDepthEdgeFillUniforms(currentEyeFromClip: float(currentEyeFromClip),
            currentClipFromEye: displayProjection, sourceClipFromCurrentEye: float(sourceClipFromCurrentEye),
            viewportOrigin: viewportOrigin, viewportSize: viewportSize, sourceSize: sourceSize,
            sourceDepthReversed: sourceDepthReversed, colorTransfer: colorTransfer,
            maximumInverseDistance: 1 / nearestDistance, skyRadius: skyRadius, maximumSteps: maximumSteps)
    }
}

/// Angular sampling of one displayed eye straight ahead of that eye, from the
/// actual display projection/viewport and the actual leased source projection
/// and size. It quantifies source enlargement; it is not perceived sharpness.
struct GameWorldSamplingDensity: Sendable, Equatable {
    let eye: Int
    let displayPixels: SIMD2<Int>
    let sourcePixels: SIMD2<Int>
    let displayPixelsPerDegree: SIMD2<Double>
    let sourcePixelsPerDegree: SIMD2<Double>

    /// Display pixels covered by one source texel on each axis; above 1 is enlargement.
    var displayPixelsPerSourceTexel: SIMD2<Double> { displayPixelsPerDegree / sourcePixelsPerDegree }

    static func measure(eye: Int, displayProjection: simd_float4x4, displayPixels: SIMD2<Int>,
                        sourceProjection: simd_float4x4, sourcePixels: SIMD2<Int>) -> Self? {
        func perDegree(_ projection: simd_float4x4, _ pixels: SIMD2<Int>) -> SIMD2<Double>? {
            // Normalize to w = -z so focal terms are tangent-to-NDC slopes.
            let w = -Double(projection[2][3])
            guard w.isFinite, w > 0, pixels.x > 0, pixels.y > 0 else { return nil }
            let value = SIMD2(Double(projection[0][0]), Double(projection[1][1])) / w
                * SIMD2(Double(pixels.x), Double(pixels.y)) * 0.5 * .pi / 180
            return value.x.isFinite && value.y.isFinite && value.x > 0 && value.y > 0 ? value : nil
        }
        guard let display = perDegree(displayProjection, displayPixels),
              let source = perDegree(sourceProjection, sourcePixels) else { return nil }
        return Self(eye: eye, displayPixels: displayPixels, sourcePixels: sourcePixels,
                    displayPixelsPerDegree: display, sourcePixelsPerDegree: source)
    }

    var reportValue: [String: Any] {
        ["eye": eye, "display_viewport": [displayPixels.x, displayPixels.y],
         "source_size": [sourcePixels.x, sourcePixels.y],
         "display_center_pixels_per_degree": [displayPixelsPerDegree.x, displayPixelsPerDegree.y],
         "source_center_pixels_per_degree": [sourcePixelsPerDegree.x, sourcePixelsPerDegree.y],
         "display_pixels_per_source_texel": [displayPixelsPerSourceTexel.x, displayPixelsPerSourceTexel.y]]
    }
}

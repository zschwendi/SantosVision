import Foundation

/// Display resolution is independent of GTA's source resolution. Preserve the
/// system allocation until the player explicitly asks for the higher setting.
enum GameWorldDisplayQuality {
    static let storedKey = "SantosVisionHigherImmersiveResolution"
    static var stored: Bool { UserDefaults.standard.bool(forKey: storedKey) }
    static var requested: Bool {
        switch ProcessInfo.processInfo.environment["MUGUET_COMPOSITOR_HIGH_RESOLUTION"] {
        case "1": return true
        case "0": return false
        default: return stored
        }
    }
}

/// Controlled reconstruction comparison. Normal play retains the affine
/// surface policy until the curved-surface candidate has been checked in the
/// actual headset. This changes geometry continuity, never the source image.
enum GameWorldSurfaceContinuityPolicy {
    static func requested(environment: [String: String], arguments: [String]) -> Bool {
        if arguments.contains("--sv-world-disable-smooth-continuity") { return false }
        switch environment["MUGUET_COMPOSITOR_SMOOTH_CONTINUITY"] {
        case "1": return true
        case "0": return false
        default: return arguments.contains("--sv-world-smooth-continuity")
        }
    }

    static var requested: Bool {
        requested(environment: ProcessInfo.processInfo.environment,
                  arguments: ProcessInfo.processInfo.arguments)
    }
}

/// A separate color-only experiment. Geometry, source pixels and all UI stay
/// unchanged. The renderer can reject incompatible experimental paths.
enum GameWorldVerticalReconstructionPolicy {
    static func requested(environment: [String: String], arguments: [String]) -> Bool {
        if arguments.contains("--sv-world-disable-vertical-reconstruction") { return false }
        switch environment["MUGUET_COMPOSITOR_VERTICAL_RECONSTRUCTION"] {
        case "1": return true
        case "0": return false
        default: return arguments.contains("--sv-world-vertical-reconstruction")
        }
    }

    static var requested: Bool {
        requested(environment: ProcessInfo.processInfo.environment,
                  arguments: ProcessInfo.processInfo.arguments)
    }
}
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
/// source frame into those gaps only. The restored27 visual profile enables
/// this when there is no saved choice; explicit launcher/QA choices still win.
enum GameWorldDepthEdgeFillPolicy {
    static let storedKey = "SantosVisionDepthEdgeFillEnabled"
    /// Longest searched gap, in source texels along the epipolar line.
    static let maximumSteps: UInt32 = 64

    static func savedChoice(in defaults: UserDefaults) -> Bool {
        // The preferred27 session enabled repair through a launch override.
        // Normal relaunches must not silently drop that visual profile, while
        // an explicit saved false remains a deliberate opt-out.
        guard defaults.object(forKey: storedKey) != nil else { return true }
        return defaults.bool(forKey: storedKey)
    }

    static var stored: Bool { savedChoice(in: .standard) }

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

/// Diagnostic values only. Matrices retain every Float bit in column-major
/// order, including nonfinite bit patterns, without invalid JSON numbers.
struct GameWorldCompletedPair: Sendable {
    struct Eye: Sendable {
        let displayEye: Int
        let sourceEye: Int
        let lease: UInt64
        let game: UInt64
        let request: UInt64
        let head: UInt64
        let sourcePresentationTime: Double
        let retainedPoseTime: Double?
        let requestedGuard: Float?
        let guardProvenance: String
        let inferredFocalGuard: SIMD2<Double>?
        let sourceProjection: [UInt32]
        let sourceGameFromEye: [UInt32]
        let sourceGameFromStage: [UInt32]
        let displayProjection: [UInt32]
        let displayHeadFromEye: [UInt32]
        let displayWorldHeadFromEye: [UInt32]
        let currentEyeFromSourceEye: [UInt32]
        let viewport: [Double]
        let density: GameWorldSamplingDensity
        var path = "not_encoded"
        var orderedPreparation = "not_attempted"
        var sourceAnchorStageFromHead: [UInt32]?
        var sourceAnchorPoseTime: Double?

        var reportValue: [String: Any] {
            var row: [String: Any] = ["display_eye": displayEye, "source_eye": sourceEye,
                "lease_token": lease, "game_frame": game, "request_frame": request, "head_frame": head,
                "source_native_presentation_time_seconds": sourcePresentationTime,
                "guard_provenance": guardProvenance, "source_projection_float_bits": sourceProjection,
                "source_game_from_eye_float_bits": sourceGameFromEye, "source_game_from_stage_float_bits": sourceGameFromStage,
                "display_projection_float_bits": displayProjection, "display_head_from_eye_float_bits": displayHeadFromEye,
                "display_world_head_from_eye_float_bits": displayWorldHeadFromEye,
                "current_eye_from_source_eye_float_bits": currentEyeFromSourceEye,
                "display_viewport_xywh": viewport, "density": density.reportValue,
                "encoded_path": path, "ordered_preparation": orderedPreparation]
            if let sourceAnchorStageFromHead { row["copy_drawable_anchor_stage_from_head_float_bits"] = sourceAnchorStageFromHead }
            if let sourceAnchorPoseTime, sourceAnchorPoseTime.isFinite { row["copy_drawable_anchor_pose_timestamp_seconds"] = sourceAnchorPoseTime }
            if let retainedPoseTime, retainedPoseTime.isFinite { row["source_retained_pose_timestamp_seconds"] = retainedPoseTime }
            if let requestedGuard, requestedGuard.isFinite { row["source_requested_guard_scale"] = requestedGuard }
            if let inferredFocalGuard, inferredFocalGuard.x.isFinite, inferredFocalGuard.y.isFinite {
                row["inferred_display_to_source_focal_ratio_xy"] = [inferredFocalGuard.x, inferredFocalGuard.y]
            }
            return row
        }
    }
    let compositorFrame: UInt64
    let predictedDisplayTime: Double
    let displayRequest: UInt64?
    let displayHead: UInt64?
    let displayStageFromHead: [UInt32]
    let pairAcquiredAtomically: Bool
    let eyes: [Eye]

    var qualified: Bool {
        eyes.count == 2 && eyes.map(\.displayEye) == [0, 1] && eyes.map(\.sourceEye) == [0, 1]
            && eyes.allSatisfy { $0.lease != 0 && $0.game != 0 && $0.request != 0 && $0.head != 0
                && $0.path != "not_encoded" && $0.sourcePresentationTime.isFinite
                && $0.viewport.count == 4 && $0.viewport.allSatisfy(\.isFinite) }
            && eyes[0].lease != eyes[1].lease && predictedDisplayTime.isFinite
    }

    func reportValue(completedAt: Double, acknowledged: Bool) -> [String: Any] {
        var row: [String: Any] = ["compositor_frame": compositorFrame,
            "predicted_display_timestamp_seconds": predictedDisplayTime,
            "completion_observed_at_uptime_seconds": completedAt, "metal_status": "completed",
            "native_acknowledged": acknowledged, "pair_acquired_atomically": pairAcquiredAtomically,
            "display_stage_from_head_float_bits": displayStageFromHead, "eyes": eyes.map(\.reportValue)]
        if let displayRequest { row["display_request_frame"] = displayRequest }
        if let displayHead { row["display_head_frame"] = displayHead }
        return row
    }

    static func bits(_ matrix: simd_float4x4) -> [UInt32] {
        (0..<4).flatMap { column in (0..<4).map { matrix[column][$0].bitPattern } }
    }

    /// A projection slope comparison only. Scoped optics may affect it; this
    /// is never a substitute for an actual retained requested guard.
    static func focalRatio(display: simd_float4x4, source: simd_float4x4) -> SIMD2<Double>? {
        let displayW = -Double(display[2][3]), sourceW = -Double(source[2][3])
        guard displayW > 0, sourceW > 0 else { return nil }
        let ratio = SIMD2(Double(display[0][0]), Double(display[1][1])) / displayW
            / (SIMD2(Double(source[0][0]), Double(source[1][1])) / sourceW)
        return ratio.x.isFinite && ratio.y.isFinite && ratio.x > 0 && ratio.y > 0 ? ratio : nil
    }
}

/// Default-off, at most one sampled pair per second, newest 16 successful
/// completions retained. No texture/lease ownership, logging, JSON or file I/O.
/// Callers snapshot small value types during encoding; only report reads form
/// dictionaries, after this lock has been released.
final class GameWorldCompletedPairTelemetry: @unchecked Sendable {
    static let capacity = 16
    let enabled: Bool
    private let lock = NSLock()
    private var nextSample: Double = 0
    private var completed: [(pair: GameWorldCompletedPair, at: Double, acknowledged: Bool)] = []
    private var failed: UInt64 = 0
    private var published: UInt64 = 0
    private var reserved: UInt64 = 0
    private var unqualified: UInt64 = 0

    init(environment: [String: String] = ProcessInfo.processInfo.environment) {
        enabled = environment["MUGUET_COMPOSITOR_COMPLETED_PAIR_TELEMETRY"] == "1"
    }

    func shouldSample(now: Double) -> Bool {
        guard enabled, now.isFinite else { return false }
        lock.lock()
        defer { lock.unlock() }
        guard now >= nextSample else { return false }
        nextSample = now + 1
        reserved &+= 1
        return true
    }

    func discardUnqualifiedSample() {
        guard enabled else { return }
        lock.lock()
        unqualified &+= 1
        lock.unlock()
    }

    func complete(_ pair: GameWorldCompletedPair, successful: Bool, acknowledged: Bool, at: Double) {
        guard enabled, pair.qualified, at.isFinite else { return }
        lock.lock()
        defer { lock.unlock() }
        guard successful else { failed &+= 1; return }
        published &+= 1
        if completed.count == Self.capacity { completed.removeFirst() }
        completed.append((pair, at, acknowledged))
    }

    var reportValue: [String: Any] {
        lock.lock()
        let snapshot = completed, failedSnapshot = failed, publishedSnapshot = published,
            reservedSnapshot = reserved, unqualifiedSnapshot = unqualified
        lock.unlock()
        return ["schema": 1, "enabled": enabled, "maximum_pairs": Self.capacity,
            "minimum_sample_interval_seconds": 1, "failed_sampled_commands": failedSnapshot,
            "reserved_samples": reservedSnapshot, "unqualified_reserved_samples": unqualifiedSnapshot,
            "successful_sampled_commands": publishedSnapshot,
            "pairs": snapshot.map { $0.pair.reportValue(completedAt: $0.at, acknowledged: $0.acknowledged) },
            "scope": "Two encoded distinct actual source eyes on one built-in drawable and one successfully completed Metal command; native acknowledgement is separate. Completion is not physical scanout or visual acceptance. Rows follow callback order; compositor_frame identifies submission order. Independent acquisition may select different game/request/head generations, retained exactly per eye.",
            "clock_domains": "predicted_display_timestamp_seconds is LayerRenderer.Clock epoch; source_native_presentation_time_seconds is the unmodified native lease field; source_retained_pose_timestamp_seconds is the exact matched retained request timestamp; completion_observed_at_uptime_seconds is ProcessInfo.systemUptime. No cross-clock differences are calculated.",
            "matrix_encoding": "Float32 bit patterns, column-major, four columns of four values. Source matrices are the lease matrices used for rendering, never replaced by retained request matrices."]
    }
}

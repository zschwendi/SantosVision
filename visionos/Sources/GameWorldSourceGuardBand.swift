import Foundation
import simd

/// Extra source coverage for display-time head reprojection. The physical display projection is never
/// changed: the expanded matrix must travel through the request and source
/// lease so reprojection still uses the camera that actually rendered it.
enum GameWorldSourceGuardBand {
    // The user can outrun the old source image during a turn. Render a real
    // margin on both axes instead of stretching the edge or delaying the head.
    // 1.30 contains the angular directions of both registered head-turn captures
    // (maximum required scales 1.191 and 1.276). This is a bounded margin, not a
    // guarantee for arbitrary turns or translation disocclusions. The original
    // 1.0 projection remains available for controlled clarity comparisons.
    static let defaultScale: Float = 1.3
    static let allowedScales: ClosedRange<Float> = 1...3
    static let configuredScale = resolveScale(ProcessInfo.processInfo.environment)
    // An explicit QA scale disables adaptation, preserving controlled A/B runs.
    static let adaptiveEnabled = ProcessInfo.processInfo.environment["MUGUET_XR_SOURCE_GUARD_BAND"] == nil

    static func resolveScale(_ environment: [String: String]) -> Float {
        guard let value = environment["MUGUET_XR_SOURCE_GUARD_BAND"],
              let scale = Float(value), scale.isFinite, allowedScales.contains(scale) else { return defaultScale }
        return scale
    }

    static func projection(_ physical: simd_float4x4, scale: Float = configuredScale) -> simd_float4x4? {
        guard scale.isFinite, allowedScales.contains(scale),
              let converted = GameWorldProjection.nativePinhole(physical) else { return nil }
        var source = converted.matrix
        if scale == 1 { return source }
        // Scale complete clip X/Y rows, including asymmetric offsets. This
        // preserves the original frustum center while placing its boundaries
        // inside the source image by the requested NDC margin. Depth and eye
        // origins are unchanged. It trades source angular resolution for
        // coverage at the same texture size; it cannot reveal a whole unseen
        // hemisphere or repair translation disocclusions.
        for column in 0..<4 {
            source[column][0] /= scale
            source[column][1] /= scale
        }
        return source
    }
}

/// Render-thread-only coverage budget. Extra pixels must be rendered by GTA;
/// display reprojection cannot create the newly exposed world. Widen during a
/// turn using actual recent angular motion and source pose age, then contract
/// slowly after stopping. This trades angular resolution only while turning.
/// The cap is explicit: turns beyond it and translation disocclusions remain
/// unsupported, rather than stretching edge pixels or delaying the display pose.
struct GameWorldSourceCoverage {
    // Share the native lease admission bound. An admitted stalled source must
    // raise the horizon to its cap rather than silently losing its age signal.
    static let maximumSourceAge: Double = 1
    private var previous: (time: Double, pose: simd_float4x4)?
    private var speed: Double = 0
    private var horizon: Double = 0.18 // actual build18 source lag was 100–160ms
    private var horizonObservedAt: Double?
    private(set) var scale = GameWorldSourceGuardBand.configuredScale
    private(set) var saturated = false

    mutating func observeSourceAges(_ ages: [Double], timestamp: Double) {
        guard timestamp.isFinite,
              let age = ages.filter({ $0.isFinite && $0 >= 0 && $0 <= Self.maximumSourceAge }).max() else { return }
        let elapsed = timestamp - (horizonObservedAt ?? timestamp)
        guard elapsed >= 0 else { return }
        // One conservative rolling peak for the whole display pair. The old
        // 0.98 decay ran once per eye: swapping eyes or repeating a callback
        // changed the next source projection without any new motion evidence.
        // Preserve that decay at one observation per nominal 60 Hz frame
        // (time constant ~0.825 s), slower than the former stereo cadence.
        // Equal timestamps may raise the peak but never decay it; stale
        // timestamps cannot change the budget or its observation clock.
        let decay = exp(log(0.98) * 60 * elapsed)
        horizon = min(0.3, max(0.1, max(horizon * decay, age + 0.03)))
        horizonObservedAt = timestamp
    }

    mutating func sample(timestamp: Double, pose: simd_float4x4,
                         projections: [simd_float4x4], magnification: Float) -> Float {
        guard GameWorldSourceGuardBand.adaptiveEnabled else { return scale }
        guard timestamp.isFinite, !projections.isEmpty,
              magnification.isFinite, (1...10).contains(magnification),
              (0..<4).allSatisfy({ c in (0..<4).allSatisfy { pose[c][$0].isFinite } }) else { return scale }
        var dt = 0.0
        if let previous {
            dt = timestamp - previous.time
            if dt > 0, dt <= 0.25 {
                let old = simd_quatf(simd_float3x3(columns: (previous.pose[0].xyz, previous.pose[1].xyz, previous.pose[2].xyz)))
                let new = simd_quatf(simd_float3x3(columns: (pose[0].xyz, pose[1].xyz, pose[2].xyz)))
                let oldUnit = simd_normalize(SIMD4<Double>(old.vector))
                let newUnit = simd_normalize(SIMD4<Double>(new.vector))
                let dot = min(1.0, abs(simd_dot(oldUnit, newUnit)))
                let measured = 2 * acos(dot) / dt
                // Ignore sub-resolution quaternion noise, retain real turns.
                speed = max(measured > 0.03 ? measured : 0, speed * exp(-dt / 0.35))
            } else if dt > 0.25 { speed = 0 }
            else { return scale } // duplicate/out-of-order timestamps cannot reset history
        }
        previous = (timestamp, pose)
        let margin = min(.pi / 2, speed * horizon)
        let required = projections.map { Self.requiredScale($0, magnification: magnification, angle: margin) }
            .max() ?? Double(GameWorldSourceGuardBand.defaultScale)
        saturated = !required.isFinite || required > Double(GameWorldSourceGuardBand.allowedScales.upperBound)
        let target = Float(min(Double(GameWorldSourceGuardBand.allowedScales.upperBound),
            max(Double(GameWorldSourceGuardBand.defaultScale), required)))
        if target >= scale { scale = target }
        else { scale = max(target, scale - Float(max(0, dt)) * 0.75) }
        return scale
    }

    /// Bound either coordinate over the cone around every display-frustum
    /// corner. Projected coordinate extrema occur on its boundary; this closed
    /// form includes diagonal rotations and asymmetric principal points.
    static func requiredScale(_ physical: simd_float4x4, magnification: Float, angle: Double) -> Double {
        guard let conversion = GameWorldProjection.nativePinhole(physical),
              magnification.isFinite, (1...10).contains(magnification), angle.isFinite,
              angle >= 0, angle < .pi / 2 else { return .infinity }
        let p = conversion.matrix
        let a = Double(p[0][0] * magnification), b = Double(p[1][1] * magnification)
        let cx = -Double(p[2][0]), cy = -Double(p[2][1])
        let cosine = cos(angle), sine = sin(angle)
        var required = 1.0
        for x in [-1.0, 1.0] { for y in [-1.0, 1.0] {
            let ray = simd_normalize(SIMD3<Double>((x - cx) / a, (y - cy) / b, 1))
            let denominator = ray.z * cosine - sqrt(max(0, 1 - ray.z * ray.z)) * sine
            if denominator <= 1e-6 { return .infinity }
            let quadratic = ray.z * ray.z - sine * sine
            if quadratic <= 0 { return .infinity }
            // Tangency of the cone to v-q*z=0 gives the exact interval of
            // projected slopes. Retain its signs before adding the asymmetric
            // principal point: an absolute-value box would blur a still view.
            for (v, coefficient, center) in [(ray.x, a, cx), (ray.y, b, cy)] {
                let radius = sine * sqrt(max(0, v * v + ray.z * ray.z - sine * sine))
                let low = (v * ray.z - radius) / quadratic
                let high = (v * ray.z + radius) / quadratic
                required = max(required, abs(coefficient * low + center), abs(coefficient * high + center))
            }
        }}
        return required
    }
}

private extension SIMD4 where Scalar == Float {
    var xyz: SIMD3<Float> { SIMD3(x, y, z) }
}

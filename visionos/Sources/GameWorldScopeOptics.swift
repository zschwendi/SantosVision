import simd

/// Native aim-camera optical zoom, carried by the immutable source lease.
/// Its reference is that same aim camera's unzoomed control FOV; it does not
/// establish magnification relative to a different ordinary gameplay camera.
enum GameWorldScopeOptics {
    struct Optics: Sendable, Equatable {
        static let identity = Optics(flags: 0, magnification: 1, referenceFOV: 0,
                                     actualFOV: 0, scopeSequence: 0)!
        let flags: UInt32
        let magnification: Float
        let referenceFOV: Float
        let actualFOV: Float
        let scopeSequence: UInt64

        var scoped: Bool { flags == 1 }
        var sourceCopyAllowed: Bool { magnification == 1 }

        init?(flags: UInt32, magnification: Float, referenceFOV: Float,
              actualFOV: Float, scopeSequence: UInt64) {
            if flags == 0 {
                guard magnification == 1, referenceFOV == 0, actualFOV == 0,
                      scopeSequence == 0 else { return nil }
            } else {
                guard flags == 1, scopeSequence != 0,
                      let expected = GameWorldScopeOptics.magnification(
                        referenceFOV: referenceFOV, actualFOV: actualFOV),
                      magnification.isFinite, magnification >= 1, magnification <= 10,
                      abs(magnification - expected) <= 4 * expected.ulp else { return nil }
            }
            self.flags = flags
            self.magnification = magnification
            self.referenceFOV = referenceFOV
            self.actualFOV = actualFOV
            self.scopeSequence = scopeSequence
        }
    }

    struct Candidate: Sendable {
        let lease: UInt64
        let epoch: UInt64
        let gameFrame: UInt64
        let requestFrame: UInt64
        let headFrame: UInt64
        let cameraSequence: UInt64
        let presentationTime: Double
        let sourceEye: Int
        let viewCount: Int
        let optics: Optics

        var qualified: Bool {
            lease != 0 && epoch != 0 && gameFrame != 0 && requestFrame != 0
                && headFrame != 0 && cameraSequence != 0
                && presentationTime.isFinite && presentationTime > 0
                && (1...2).contains(viewCount) && (0..<viewCount).contains(sourceEye)
        }
    }

    struct ProjectionAdmission: Sendable {
        let passed: Bool
        let reason: String
        let maximumRayError: Double
        let maximumDepthError: Double
    }

    /// Match the native f64 tangent ratio followed by a single f32 rounding.
    /// The native camera clamps its FOV to 1...130 degrees. The currently
    /// qualified sniper metadata has reference45 and minimum5 degrees.
    static func magnification(referenceFOV: Float, actualFOV: Float) -> Float? {
        guard referenceFOV.isFinite, actualFOV.isFinite,
              (1...130).contains(referenceFOV), (1...130).contains(actualFOV),
              actualFOV <= referenceFOV else { return nil }
        if referenceFOV == actualFOV { return 1 }
        let result = Float(tan(Double(referenceFOV) * .pi / 360)
            / tan(Double(actualFOV) * .pi / 360))
        return result.isFinite && (1...10).contains(result) ? result : nil
    }

    /// One optical scale for the entire drawable. Natural alternating source
    /// frames may have different zoom samples; use the newest sample for both
    /// target eyes, while unprojecting each source with its OWN source matrix.
    /// A mode/weapon/scope transition cannot combine old and new source eyes.
    /// This selection does not certify a same-update stereo pair.
    static func selectedIndex(_ candidates: [Candidate?], viewCount: Int) -> Int? {
        guard (1...2).contains(viewCount), candidates.count == viewCount,
              let first = candidates.first ?? nil, first.qualified else { return nil }
        for index in 0..<viewCount {
            guard let item = candidates[index], item.qualified,
                  item.viewCount == viewCount, item.sourceEye == index,
                  item.epoch == first.epoch,
                  item.optics.flags == first.optics.flags,
                  item.optics.scopeSequence == first.optics.scopeSequence,
                  item.optics.referenceFOV.bitPattern == first.optics.referenceFOV.bitPattern else { return nil }
            if item.cameraSequence == first.cameraSequence,
               item.optics != first.optics { return nil }
        }
        return candidates.indices.max { lhs, rhs in
            let a = candidates[lhs]!, b = candidates[rhs]!
            return a.cameraSequence == b.cameraSequence
                ? a.gameFrame < b.gameFrame : a.cameraSequence < b.cameraSequence
        }
    }

    /// Magnify angles around THIS eye's forward-axis principal point. Scaling
    /// all clip XY about zero would incorrectly magnify asymmetric eye offsets.
    /// Z/W are byte-identical, so native depth direction/range stays unchanged.
    static func projection(_ physical: simd_float4x4, optics: Optics) -> simd_float4x4? {
        guard finitePerspective(physical) else { return nil }
        if optics.magnification == 1 { return physical }
        let m = optics.magnification
        let axis = physical * SIMD4<Float>(0, 0, -1, 0)
        let center = SIMD2(axis.x, axis.y) / axis.w
        guard center.x.isFinite, center.y.isFinite else { return nil }
        var result = physical
        for column in [0, 1, 3] {
            result[column][0] = m * physical[column][0] + (1 - m) * center.x * physical[column][3]
            result[column][1] = m * physical[column][1] + (1 - m) * center.y * physical[column][3]
        }
        // Column2 is the principal-axis ray itself; algebraically it is
        // unchanged. Preserve it exactly instead of cancelling rounded terms.
        return finitePerspective(result) ? result : nil
    }

    /// Rebuild the exact sparse source operation performed by camera_post.
    /// The retained request already contains the source coverage guard band.
    /// This must be checked against the lease before its optical scale is used.
    static func expectedSourceProjection(_ retainedRequest: simd_float4x4,
                                         optics: Optics, depthReversed: Bool) -> simd_float4x4? {
        guard finitePerspective(retainedRequest), retainedRequest[2][3] == -1,
              retainedRequest[2][2] > 0, retainedRequest[3][2] > 0,
              [1, 2, 3, 4, 6, 7, 12, 13, 15].allSatisfy({
                retainedRequest[$0 / 4][$0 % 4] == 0
              }) else { return nil }
        var result = retainedRequest
        if optics.magnification != 1 {
            result[0][0] *= optics.magnification
            result[1][1] *= optics.magnification
        }
        if !depthReversed {
            for column in 0..<4 { result[column][2] = result[column][3] - result[column][2] }
        }
        return finitePerspective(result) ? result : nil
    }

    /// Retain the existing WHOLE-CLIP conversion budget. A scoped source is
    /// compared with the exact physical optical frustum, including calibration
    /// residuals and the unchanged source guard band. A source depth flip is
    /// normalized here only; the leased unprojection matrix stays untouched.
    static func admitSource(_ source: simd_float4x4, depthReversed: Bool,
                            retainedPhysical: simd_float4x4, guardScale: Float,
                            optics: Optics) -> ProjectionAdmission {
        func failure(_ reason: String) -> ProjectionAdmission {
            .init(passed: false, reason: reason, maximumRayError: .infinity, maximumDepthError: .infinity)
        }
        guard guardScale.isFinite, (1...1.35).contains(guardScale),
              finitePerspective(source),
              let optical = projection(retainedPhysical, optics: optics) else { return failure("invalid_projection") }
        var target = optical
        for column in 0..<4 { target[column][0] /= guardScale; target[column][1] /= guardScale }
        var normalizedSource = source
        if !depthReversed {
            for column in 0..<4 { normalizedSource[column][2] = source[column][3] - source[column][2] }
        }
        func d(_ matrix: simd_float4x4) -> simd_double4x4 {
            simd_double4x4(columns: (SIMD4<Double>(matrix[0]), SIMD4<Double>(matrix[1]),
                SIMD4<Double>(matrix[2]), SIMD4<Double>(matrix[3])))
        }
        var mapping = d(normalizedSource) * d(target).inverse
        guard (0..<4).allSatisfy({ c in (0..<4).allSatisfy { mapping[c][$0].isFinite } }),
              mapping[3][3] > 0 else { return failure("singular_mapping") }
        mapping *= 1 / mapping[3][3]
        let denominator = (0..<3).reduce(0.0) { $0 + abs(mapping[$1][3]) }
        guard denominator.isFinite, denominator < 1 else { return failure("unbounded_mapping") }
        var bounds = SIMD3<Double>.zero
        for row in 0..<3 {
            let numerator = (0..<4).reduce(0.0) {
                $0 + abs(mapping[$1][row] - ($1 == row ? 1 : 0))
            }
            bounds[row] = (numerator + denominator) / (1 - denominator)
        }
        let ray = max(bounds.x, bounds.y), depth = bounds.z
        let passed = ray.isFinite && depth.isFinite
            && ray <= GameWorldProjection.maximumNDCError && depth <= GameWorldProjection.maximumNDCError
        return .init(passed: passed, reason: passed ? "ok" : "physical_clip_error",
                     maximumRayError: ray, maximumDepthError: depth)
    }

    private static func finitePerspective(_ projection: simd_float4x4) -> Bool {
        guard (0..<4).allSatisfy({ c in (0..<4).allSatisfy { projection[c][$0].isFinite } })
            && projection[0][0] > 0 && projection[1][1] > 0
            && abs(projection[2][3] + 1) <= 1e-4 && abs(projection[3][3]) <= 1e-4
            && projection.determinant.isFinite && abs(projection.determinant) > 1e-12 else { return false }
        let z = projection[2][2], w = projection[3][2]
        let reversed = z >= 0 && w > 0
        guard reversed || (z < 0 && w < 0) else { return false }
        let near = w / (reversed ? z + 1 : z)
        let far = w / (reversed ? z : z + 1)
        return near.isFinite && far.isFinite && near > 0 && far > near && far <= 1002
    }
}

import Foundation
import simd

/// Separate, default-off, manual third-person QA experiment. Stateless: no
/// target survives a drawable, tracking invalidation, stop or new renderer.
enum GameWorldCommonTargetStage {
    static func requested(environment: [String: String]) -> Bool {
        environment["MUGUET_COMPOSITOR_COMMON_TARGET_STAGE_QA"] == "1"
    }
    // Conservative QA cut limits, not a proof of raster/occlusion coverage.
    static let maximumStageOriginTranslation = 0.25
    static let maximumRotationDegrees = 5.0
    static let agreementTranslation = 0.001
    static let agreementRotationDegrees = 0.005
    static let minimumCoverageDepth = 0.05
    static let coverageClipLimit = 0.98

    struct Candidate: Sendable {
        let identity: GameWorldCompletedPair.TargetStage.SourceIdentity
        let actual: Bool
        let sourcePoseQualified: Bool
        let ordinaryOptics: Bool
        let gameFromStage: simd_float4x4
        let gameFromEye: simd_float4x4
        let sourceProjection: simd_float4x4
        let displayProjection: simd_float4x4
        let displayHeadFromEye: simd_float4x4
    }
    enum Reticle {
        case absent, perSource, reference(simd_double4x4), knownFirstPerson, epochMismatch
        var label: String {
            switch self {
            case .absent: "absent_manual_third_person_qa"
            case .perSource: "v1_per_source_fallback_unchanged"
            case .reference: "v2_common_reference_unchanged"
            case .knownFirstPerson: "known_first_person_excluded"
            case .epochMismatch: "aim_epoch_mismatch"
            }
        }
    }
    struct Decision: Sendable {
        let gameFromStage: simd_float4x4?
        let telemetry: GameWorldCompletedPair.TargetStage
    }
    static func reject(requested: Bool, reason: String, reticle: String = "not_evaluated") -> Decision {
        Decision(gameFromStage: nil, telemetry: .init(requested: requested, applied: false, reason: reason, reticlePolicy: reticle))
    }

    static func select(requested: Bool, candidates: [Candidate], selectedOpticalIndex: Int?,
                       stageFromHead: simd_float4x4?, blockedReason: String? = nil,
                       reticle: Reticle = .absent) -> Decision {
        func reject(_ reason: String) -> Decision { Self.reject(requested: requested, reason: reason, reticle: reticle.label) }
        guard requested else { return reject("not_requested") }
        if let blockedReason { return reject(blockedReason) }
        if case .knownFirstPerson = reticle { return reject("known_first_person") }
        if case .epochMismatch = reticle { return reject("aim_epoch_mismatch") }
        guard candidates.count == 2, let index = selectedOpticalIndex, candidates.indices.contains(index) else { return reject("no_qualified_two_eye_optical_selection") }
        let target = candidates[index]
        guard let stageFromHead, rigid(double(stageFromHead)) else { return reject("invalid_display_pose") }
        for (eye, source) in candidates.enumerated() {
            let id = source.identity
            guard source.actual, id.eye == eye, id.lease != 0, id.game != 0, id.request != 0,
                  id.head != 0, id.camera != 0, id.epoch != 0, id.trackingEpoch != 0 else { return reject("invalid_actual_source_identity") }
            guard source.sourcePoseQualified else { return reject("source_pose_unqualified") }
            guard source.ordinaryOptics else { return reject("scoped_or_nonordinary_optics") }
            guard id.epoch == target.identity.epoch, id.trackingEpoch == target.identity.trackingEpoch else { return reject("source_epoch_mismatch") }
            guard rigid(double(source.gameFromStage)), rigid(double(source.gameFromEye)), rigid(double(source.displayHeadFromEye)),
                  pinhole(double(source.sourceProjection)), pinhole(double(source.displayProjection)) else { return reject("invalid_source_or_display_transform") }
            guard id.camera <= target.identity.camera,
                  id.camera != target.identity.camera || id.game <= target.identity.game else { return reject("optical_target_not_newest") }
        }
        guard candidates[0].identity.lease != candidates[1].identity.lease else { return reject("shared_source_lease") }
        let common = double(target.gameFromStage)
        var maximumShift = 0.0, maximumAngle = 0.0
        for source in candidates {
            let delta = metrics(common.inverse * double(source.gameFromStage))
            maximumShift = max(maximumShift, delta.translation)
            maximumAngle = max(maximumAngle, delta.rotation)
            guard delta.translation <= maximumStageOriginTranslation, delta.rotation <= maximumRotationDegrees else { return reject("camera_discontinuity") }
            if source.identity.camera == target.identity.camera,
               (delta.translation > agreementTranslation || delta.rotation > agreementRotationDegrees) { return reject("same_camera_sequence_mapping_conflict") }
            if case .perSource = reticle,
               (delta.translation > agreementTranslation || delta.rotation > agreementRotationDegrees) { return reject("v1_reticle_target_differs") }
        }
        if case .reference(let reference) = reticle {
            guard rigid(reference) else { return reject("invalid_v2_reticle_reference") }
            let delta = metrics(common.inverse * reference)
            guard delta.translation <= agreementTranslation, delta.rotation <= agreementRotationDegrees else { return reject("v2_reticle_target_differs") }
        }
        // Coverage of the target pinhole frustum in each genuine old raster,
        // at z=-5cm and at infinity. Homogeneous side-plane inequalities are
        // affine along each ray; endpoints bound all depths >=5cm. This does
        // not certify occlusion/disocclusion or source content at those rays.
        for source in candidates {
            let sourceFromTargetEye = double(source.gameFromEye).inverse * common * double(stageFromHead) * double(source.displayHeadFromEye)
            guard covers(sourceProjection: double(source.sourceProjection), displayProjection: double(source.displayProjection),
                         sourceFromTargetEye: sourceFromTargetEye) else { return reject("source_frustum_coverage") }
        }
        var telemetry = GameWorldCompletedPair.TargetStage(requested: true, applied: true,
            reason: "newest_qualified_optical_source_common_target", reticlePolicy: reticle.label)
        telemetry.selectedSource = target.identity
        telemetry.gameFromStageBits = GameWorldCompletedPair.bits(target.gameFromStage)
        telemetry.maximumStageOriginTranslationMetres = maximumShift
        telemetry.maximumCameraRotationDegrees = maximumAngle
        return Decision(gameFromStage: target.gameFromStage, telemetry: telemetry)
    }

    static func double(_ matrix: simd_float4x4) -> simd_double4x4 {
        simd_double4x4(columns: (SIMD4<Double>(matrix[0]), SIMD4<Double>(matrix[1]), SIMD4<Double>(matrix[2]), SIMD4<Double>(matrix[3])))
    }
    static func rigid(_ matrix: simd_double4x4) -> Bool {
        guard (0..<4).allSatisfy({ c in (0..<4).allSatisfy { matrix[c][$0].isFinite } }),
              abs(matrix[0].w) < 1e-6, abs(matrix[1].w) < 1e-6, abs(matrix[2].w) < 1e-6, abs(matrix[3].w - 1) < 1e-6 else { return false }
        let basis = simd_double3x3(columns: (SIMD3(matrix[0].x,matrix[0].y,matrix[0].z), SIMD3(matrix[1].x,matrix[1].y,matrix[1].z), SIMD3(matrix[2].x,matrix[2].y,matrix[2].z)))
        let check = basis.transpose * basis
        return abs(simd_determinant(basis) - 1) <= 1e-3
            && (0..<3).allSatisfy { c in (0..<3).allSatisfy { abs(check[c][$0] - (c == $0 ? 1 : 0)) <= 1e-3 } }
    }
    static func pinhole(_ p: simd_double4x4) -> Bool {
        (0..<4).allSatisfy({ c in (0..<4).allSatisfy { p[c][$0].isFinite } })
            && p[0][0] > 0 && p[1][1] > 0 && p[2][3] < -0.9 && abs(p[3][3]) < 1e-5 && abs(simd_determinant(p)) > 1e-12
    }
    static func metrics(_ relative: simd_double4x4) -> (translation: Double, rotation: Double) {
        let translation = simd_length(SIMD3(relative[3].x,relative[3].y,relative[3].z))
        let skew = SIMD3(relative[1][2]-relative[2][1], relative[2][0]-relative[0][2], relative[0][1]-relative[1][0])
        let rotation = atan2(0.5 * simd_length(skew), 0.5 * (relative[0][0]+relative[1][1]+relative[2][2]-1)) * 180 / .pi
        return (translation, rotation)
    }
    static func covers(sourceProjection: simd_double4x4, displayProjection: simd_double4x4,
                       sourceFromTargetEye: simd_double4x4) -> Bool {
        let inverse = displayProjection.inverse
        for x in [-1.0, 1.0] { for y in [-1.0, 1.0] {
            let point = inverse * SIMD4(x,y,0.5,1)
            guard abs(point.w) > 1e-12 else { return false }
            let eye = point / point.w
            guard eye.z < 0 else { return false }
            let ray = SIMD4(eye.x / -eye.z,eye.y / -eye.z,-1,0)
            for sample in [SIMD4(ray.x*minimumCoverageDepth,ray.y*minimumCoverageDepth,-minimumCoverageDepth,1),ray] {
                let clip = sourceProjection * sourceFromTargetEye * sample
                guard (0..<4).allSatisfy({ clip[$0].isFinite }), clip.w > 1e-8,
                      abs(clip.x) <= coverageClipLimit * clip.w, abs(clip.y) <= coverageClipLimit * clip.w else { return false }
            }
        } }
        return true
    }
}

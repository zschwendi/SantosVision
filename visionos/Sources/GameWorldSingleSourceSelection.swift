import Foundation

/// Default-off diagnostic selection only. Returns an existing source index;
/// it never fabricates another eye, changes metadata, or certifies a scene pair.
enum GameWorldSingleSourceSelection {
    struct Candidate: Equatable, Sendable {
        let lease: UInt64
        let gameFrame: UInt64
        let requestFrame: UInt64
        let headFrame: UInt64
        let sourceViewIndex: Int
        let viewCount: Int
        let presentationTime: Double
        let retainedEpoch: UInt64
        let poseQualified: Bool
    }

    struct Decision: Equatable, Sendable {
        let index: Int?
        let reason: String
    }

    static func select(_ candidates: [Candidate], currentEpoch: UInt64?,
                       timestamp: Double, targetViewCount: Int) -> Decision {
        guard targetViewCount == 2 else { return Decision(index: nil, reason: "target_view_count") }
        guard timestamp.isFinite, let currentEpoch, currentEpoch != 0 else {
            return Decision(index: nil, reason: "current_pose_epoch")
        }
        let eligible = candidates.indices.filter { index in
            let candidate = candidates[index]
            return candidate.lease != 0 && candidate.gameFrame != 0
                && candidate.requestFrame != 0 && candidate.headFrame != 0
                && candidate.viewCount == 2 && (0..<2).contains(candidate.sourceViewIndex)
                && candidate.presentationTime.isFinite
                && abs(timestamp - candidate.presentationTime) <= 1
                && candidate.retainedEpoch == currentEpoch && candidate.poseQualified
        }
        guard let newest = eligible.max(by: { older(candidates[$0], than: candidates[$1]) }) else {
            return Decision(index: nil, reason: "no_qualified_current_epoch_source")
        }
        return Decision(index: newest, reason: "single_actual_source")
    }

    private static func older(_ left: Candidate, than right: Candidate) -> Bool {
        if left.gameFrame != right.gameFrame { return left.gameFrame < right.gameFrame }
        if left.requestFrame != right.requestFrame { return left.requestFrame < right.requestFrame }
        if left.headFrame != right.headFrame { return left.headFrame < right.headFrame }
        if left.presentationTime != right.presentationTime { return left.presentationTime < right.presentationTime }
        if left.sourceViewIndex != right.sourceViewIndex { return left.sourceViewIndex > right.sourceViewIndex }
        return left.lease < right.lease
    }
}

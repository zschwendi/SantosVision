import Foundation

@main
struct SingleSourceSelectionValidation {
    static func main() {
        typealias Candidate = GameWorldSingleSourceSelection.Candidate
        func candidate(_ lease: UInt64, _ game: UInt64, _ request: UInt64, _ head: UInt64,
                       _ eye: Int, epoch: UInt64 = 7, time: Double = 10,
                       qualified: Bool = true) -> Candidate {
            Candidate(lease: lease, gameFrame: game, requestFrame: request, headFrame: head,
                      sourceViewIndex: eye, viewCount: 2, presentationTime: time,
                      retainedEpoch: epoch, poseQualified: qualified)
        }
        let left = candidate(11, 100, 500, 600, 0)
        let right = candidate(12, 101, 499, 599, 1)
        let input = [left, right]
        let decision = GameWorldSingleSourceSelection.select(input, currentEpoch: 7, timestamp: 10.1, targetViewCount: 2)
        precondition(decision.index == 1) // Actual game frame wins over later request counters.
        precondition(input == [left, right])
        precondition(input[decision.index!].sourceViewIndex == 1 && input[decision.index!].lease == 12)
        // Swapping acquisition order cannot change the selected native source.
        let reversed = Array(input.reversed())
        let reversedChoice = GameWorldSingleSourceSelection.select(reversed, currentEpoch: 7, timestamp: 10.1, targetViewCount: 2)
        precondition(reversed[reversedChoice.index!] == right)
        let equalFrame = candidate(13, 100, 501, 590, 1)
        precondition(GameWorldSingleSourceSelection.select([left, equalFrame], currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 2).index == 1)
        let foreign = candidate(14, 999, 999, 999, 1, epoch: 8)
        precondition(GameWorldSingleSourceSelection.select([left, foreign], currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 2).index == 0)
        let stale = candidate(15, 999, 999, 999, 1, time: 8)
        let unqualified = candidate(16, 999, 999, 999, 1, qualified: false)
        precondition(GameWorldSingleSourceSelection.select([stale, unqualified], currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 2).index == nil)
        precondition(GameWorldSingleSourceSelection.select([], currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 2).index == nil)
        precondition(GameWorldSingleSourceSelection.select(input, currentEpoch: nil,
                        timestamp: 10.1, targetViewCount: 2).index == nil)
        precondition(GameWorldSingleSourceSelection.select(input, currentEpoch: 7,
                        timestamp: .nan, targetViewCount: 2).index == nil)
        precondition(GameWorldSingleSourceSelection.select(input, currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 1).index == nil)
        let tie = candidate(17, 100, 500, 600, 1)
        precondition(GameWorldSingleSourceSelection.select([tie, left], currentEpoch: 7,
                        timestamp: 10.1, targetViewCount: 2).index == 1)
        print("PASS: source ordering, acquisition order independence, original metadata, epoch/freshness/qualification rejection, empty and unsupported fallback")
    }
}

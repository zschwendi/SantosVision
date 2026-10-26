import Foundation

@main
struct OrderedSourceReusePolicyValidation {
    static func main() {
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            precondition(value(), message); checks += 1
        }
        let projection: [UInt32] = [1,0,0,0,0,1,0,0,0,0,0.0001,-1,0,0,0.1,0].map { Float($0).bitPattern }
        func source(_ id: UInt64 = 1, generation: UInt64 = 1, epoch: UInt64 = 2, game: UInt64 = 3,
                    request: UInt64 = 4, head: UInt64 = 5, eye: UInt32 = 0, views: UInt32 = 2,
                    handle: UInt64 = 0x1234, width: UInt32 = 2560, height: UInt32 = 2304,
                    presentation: Double = 123, bits: [UInt32]? = nil) -> GameWorldOrderedSourceIdentity {
            .init(sourceID: id, generation: generation, epoch: epoch, gameFrame: game,
                requestFrame: request, headFrame: head, eye: eye, viewCount: views, depthHandle: handle,
                width: width, height: height, presentationBits: presentation.bitPattern, projectionBits: bits ?? projection)
        }
        let queue = NSObject(), otherQueue = NSObject()
        func key(_ identity: GameWorldOrderedSourceIdentity? = nil, device: UInt64 = 1,
                 queueID: ObjectIdentifier? = nil, format: UInt64 = 55,
                 inverse: [UInt32]? = nil, reversed: UInt32 = 1, qualifiers: [UInt32] = [0,0]) -> GameWorldOrderedSourceKey {
            .init(identity: identity ?? source(), device: device, queue: queueID ?? ObjectIdentifier(queue),
                depthFormat: format, inverseProjectionBits: inverse ?? projection,
                depthReversed: reversed, qualifierBits: qualifiers)
        }
        let baseline = key()
        require(source().qualified, "Finite complete native identity qualifies")
        require(key() == baseline && Set([key(), baseline]).count == 1, "Same publication/qualifiers form exact stable key")
        let identities = [source(2), source(generation: 2), source(epoch: 3), source(game: 9), source(request: 9),
            source(head: 9), source(eye: 1), source(views: 1), source(handle: 0x5678), source(width: 1920),
            source(height: 1440), source(presentation: 124), source(bits: projection.enumerated().map { $0.offset == 0 ? Float(2).bitPattern : $0.element })]
        for identity in identities {
            require(key(identity) != baseline, "Every publication/provenance/frame/eye/resource/geometry field participates")
        }
        for alternate in [key(device: 2), key(queueID: ObjectIdentifier(otherQueue)), key(format: 99),
                          key(inverse: projection.map { $0 ^ 1 }), key(reversed: 0),
                          key(qualifiers: [Float(1).bitPattern,0]), key(qualifiers: [0,Float(1).bitPattern])] {
            require(alternate != baseline, "Device, queue and every actual preparation qualifier participate")
        }
        for invalid in [source(0), source(epoch: 0), source(handle: 0), source(width: 0), source(height: 0),
                        source(eye: 2), source(views: 0), source(views: 3), source(presentation: .nan),
                        source(bits: []), source(bits: [UInt32](repeating: Float.nan.bitPattern, count: 16))] {
            require(!invalid.qualified, "Missing/stale/malformed proof cannot be admitted to a source key")
        }
        require(GameWorldOrderedCornersBudget.maximumBytes == 384 << 20
            && GameWorldOrderedCornersBudget.maximumSets == 6
            && GameWorldOrderedCornersBudget.maximumCheckouts == 12, "Derived storage and reader metadata remain bounded")
        print("PASS ordered-source reuse policy: \(checks) exact identity, qualifier, queue, invalid proof and bound checks")
    }
}

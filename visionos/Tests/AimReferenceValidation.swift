import Foundation
import simd

@main
struct AimReferenceValidation {
    static func main() throws {
        let bytes = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        func m(_ offset: Int) -> simd_float4x4 {
            let v = (0..<16).map { i in Float(bitPattern: bytes.withUnsafeBytes { $0.loadUnaligned(fromByteOffset:offset+i*4,as:UInt32.self) }) }
            return .init(columns:(SIMD4(v[0],v[1],v[2],v[3]),SIMD4(v[4],v[5],v[6],v[7]),SIMD4(v[8],v[9],v[10],v[11]),SIMD4(v[12],v[13],v[14],v[15])))
        }
        func d(_ x:simd_float4x4)->simd_double4x4 {.init(columns:(SIMD4<Double>(x[0]),SIMD4<Double>(x[1]),SIMD4<Double>(x[2]),SIMD4<Double>(x[3])))}
        func xyz(_ x:SIMD4<Double>)->SIMD3<Double> {SIMD3(x.x,x.y,x.z)}
        func angle(_ a:SIMD3<Double>,_ b:SIMD3<Double>)->Double {acos(max(-1,min(1,simd_dot(simd_normalize(a),simd_normalize(b)))))*180 / .pi}
        let eyes=[m(32),m(160)], projections=[m(96),m(224)]
        precondition(MemoryLayout<SVXRAimSnapshot>.size == 104, "V1 ABI remains unchanged")
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.size == 280)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.aim) == 8)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.reference_camera_sequence) == 112)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.reference_request_frame) == 120)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.reference_head_frame) == 128)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.producer_camera_sequence) == 136)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.scope_sequence) == 144)
        precondition(MemoryLayout<SVXRAimReferenceSnapshotV2>.offset(of: \.game_from_stage) == 152)
        func valid(_ epoch: UInt64 = 1, _ producer: UInt64 = 10, _ reference: UInt64 = 11,
                   _ request: UInt64 = 12, _ head: UInt64 = 13,
                   _ pose: simd_double4x4 = matrix_identity_double4x4) -> Bool {
            GameWorldAimReferencePolicy.valid(epoch:epoch,producer:producer,reference:reference,
                request:request,head:head,gameFromStage:pose)
        }
        precondition(valid())
        precondition(!valid(0) && !valid(1,0) && !valid(1,12,11) && !valid(1,10,11,0) && !valid(1,10,11,12,0))
        var invalid = matrix_identity_double4x4; invalid[3].x = .nan
        precondition(!valid(1,10,11,12,13,invalid))
        invalid = matrix_identity_double4x4; invalid[0].x = 2
        precondition(!valid(1,10,11,12,13,invalid))
        invalid = matrix_identity_double4x4; invalid[3].w = 0
        precondition(!valid(1,10,11,12,13,invalid))
        precondition(GameWorldAimReferencePolicy.displayedSourceLag(producer:14,source:11) == 3)
        precondition(GameWorldAimReferencePolicy.displayedSourceLag(producer:14,source:14) == 0)
        precondition(GameWorldAimReferencePolicy.displayedSourceLag(producer:14,source:15) == nil)
        precondition(GameWorldAimReferencePolicy.displayedSourceLag(producer:14,source:0) == nil)
        let old = matrix_identity_double4x4
        let current = simd_double4x4(simd_quatd(angle:0.2,axis:SIMD3(0,1,0)))
        let rayDirection=xyz(current*SIMD4<Double>(0,0,-1,0)), rayOrigin=SIMD3<Double>(0,0,0)
        func target(_ eye:Int,base:simd_double4x4)->SIMD3<Double> {
            let gameFromCurrentEye=base*d(eyes[eye])
            let clip=GameWorldAimProjection.clip(origin:rayOrigin,direction:rayDirection,
                gameFromCurrentEye:gameFromCurrentEye,projection:projections[eye])!
            let reconstructedStage=d(eyes[eye])*d(projections[eye]).inverse*SIMD4<Double>(clip)
            return xyz(reconstructedStage)/reconstructedStage.w
        }
        let currentCodeLeft=target(0,base:old),currentCodeRight=target(1,base:current)
        let commonLeft=target(0,base:current),commonRight=target(1,base:current)
        let error=angle(currentCodeLeft,currentCodeRight), commonError=simd_length(commonLeft-commonRight)
        precondition(error>11.4 && error<11.5,"Current per-source base must expose independent-eye reference mismatch")
        precondition(commonError<0.0001,"A common native base reconstructs one finite target in both actual asymmetric eyes")
        let visibleOldBarrel=SIMD3<Double>(0,0,-1)
        precondition(angle(visibleOldBarrel,rayDirection)>11.4,"Newest actual ray cannot also match a visibly older gun pose")
        print("PASS actual production aim projection + captured AVP eye calibration: per-source bases create \(error) degree binocular reference mismatch for0.2rad native camera change; shared-base reconstructed target disagreement \(commonError)m. V1/V2 imported C ABI, finite qualified-reference and source-generation policy checks pass. Synthetic temporal/base scenario, not physical observation.")
    }
}

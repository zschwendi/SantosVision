import Foundation
import simd

@main struct Validation {
    static func main() throws {
        var checks=0
        func check(_ value:@autoclosure()->Bool,_ text:String){precondition(value(),text);checks+=1}
        let I=matrix_identity_float4x4
        func pose(_ x:Float=0,_ z:Float=0,yaw:Float=0)->simd_float4x4 {
            var m=simd_float4x4(simd_quatf(angle:yaw * .pi/180,axis:SIMD3(0,1,0)));m[3]=SIMD4(x,0,z,1);return m
        }
        var P=I;P[2]=SIMD4(0,0,0.0001,-1);P[3]=SIMD4(0,0,0.1,0)
        var guarded=P;guarded[0][0]/=3;guarded[1][1]/=3
        let Hs=[pose(0.01,yaw:0.5),pose(0.015,yaw:0.7)],Hd=pose(0.02,yaw:0.9),E=[pose(-0.032),pose(0.032)]
        func candidate(_ eye:Int,_ G:simd_float4x4=matrix_identity_float4x4,epoch:UInt64=3,tracking:UInt64=4,
                       actual:Bool=true,qualified:Bool=true,ordinary:Bool=true,camera:UInt64?=nil,
                       projection:simd_float4x4?=nil,lease:UInt64?=nil)->GameWorldCommonTargetStage.Candidate {
            .init(identity:.init(eye:eye,lease:lease ?? UInt64(eye+1),game:UInt64(100+eye),request:UInt64(200+eye),head:UInt64(300+eye),camera:camera ?? UInt64(10+eye),epoch:epoch,trackingEpoch:tracking),actual:actual,sourcePoseQualified:qualified,ordinaryOptics:ordinary,
                gameFromStage:G,gameFromEye:G*Hs[eye]*E[eye],sourceProjection:projection ?? guarded,displayProjection:P,displayHeadFromEye:E[eye])
        }
        func select(_ pair:[GameWorldCommonTargetStage.Candidate],index:Int?=1,
                    reticle:GameWorldCommonTargetStage.Reticle = .absent)->GameWorldCommonTargetStage.Decision {
            GameWorldCommonTargetStage.select(requested:true,candidates:pair,selectedOpticalIndex:index,stageFromHead:Hd,reticle:reticle)
        }
        check(!GameWorldCommonTargetStage.requested(environment:[:]),"QA default off")
        check(GameWorldCommonTargetStage.requested(environment:["MUGUET_COMPOSITOR_COMMON_TARGET_STAGE_QA":"1"]),"explicit QA only")
        check(GameWorldCommonTargetStage.select(requested:false,candidates:[],selectedOpticalIndex:nil,stageFromHead:nil).gameFromStage == nil,"disabled path has no target")
        let equal=select([candidate(0),candidate(1)])
        check(equal.telemetry.applied,"equal bases remain eligible")
        let X=SIMD4<Double>(0.4,0.2,-8,1)
        var results:[[String:Any]]=[]
        for (name,newG) in [("equal",I),("translation_2cm",pose(0.02)),("yaw_2deg",pose(yaw:2))] {
            let pair=[candidate(0),candidate(1,newG)]
            let before=pair.map{GameWorldCompletedPair.bits($0.gameFromEye)}
            let decision=select(pair)
            check(decision.telemetry.applied,"bounded change with guard coverage eligible")
            check(decision.telemetry.selectedSource?.camera == 11 && decision.telemetry.selectedSource?.eye == 1,"same newest optical source selected")
            check(pair.map{GameWorldCompletedPair.bits($0.gameFromEye)} == before,"genuine source cameras never rewritten")
            var oldStage:[SIMD4<Double>]=[],newStage:[SIMD4<Double>]=[]
            for i in 0..<2 {
                let source=Source(gameFromStage:pair[i].gameFromStage,gameFromEye:pair[i].gameFromEye)
                let point=GameWorldCommonTargetStage.double(source.gameFromEye).inverse*X
                let old=productionRelative(eye:source,stageFromHead:Hd,headFromEye:E[i])
                let new=productionRelative(eye:source,stageFromHead:Hd,headFromEye:E[i],targetGameFromStage:decision.gameFromStage)
                if name=="equal" {check((0..<4).allSatisfy{c in (0..<4).allSatisfy{abs(old[c][$0]-new[c][$0])<1e-12}},"equal target case unchanged")}
                let stageFromEye=GameWorldCommonTargetStage.double(Hd)*GameWorldCommonTargetStage.double(E[i])
                oldStage.append(stageFromEye*old*point);newStage.append(stageFromEye*new*point)
            }
            check(simd_length(newStage[0]-newStage[1])<1e-9,"common target fixes static point stage correspondence")
            if name != "equal" {check(simd_length(oldStage[0]-oldStage[1])>0.01,"legacy differs for changing game camera")}
            results.append(["case":name,"old_static_stage_disagreement":simd_length(oldStage[0]-oldStage[1]),"common_static_stage_disagreement":simd_length(newStage[0]-newStage[1]),"decision":decision.telemetry.reportValue])
        }
        let rejects:[(String,GameWorldCommonTargetStage.Decision)]=[
            ("invalid_actual_source_identity",select([candidate(0,actual:false),candidate(1)])),
            ("source_epoch_mismatch",select([candidate(0,epoch:2),candidate(1)])),
            ("source_epoch_mismatch",select([candidate(0,tracking:2),candidate(1)])),
            ("source_pose_unqualified",select([candidate(0,qualified:false),candidate(1)])),
            ("scoped_or_nonordinary_optics",select([candidate(0,ordinary:false),candidate(1)])),
            ("invalid_source_or_display_transform",select([candidate(0,simd_float4x4()),candidate(1)])),
            ("invalid_source_or_display_transform",select([candidate(0,projection:simd_float4x4()),candidate(1)])),
            ("optical_target_not_newest",select([candidate(0),candidate(1)],index:0)),
            ("camera_discontinuity",select([candidate(0),candidate(1,pose(0.3))])),
            ("camera_discontinuity",select([candidate(0),candidate(1,pose(yaw:6))])),
            ("same_camera_sequence_mapping_conflict",select([candidate(0,camera:11),candidate(1,pose(0.02),camera:11)])),
            ("source_frustum_coverage",select([candidate(0,projection:P),candidate(1,projection:P)])),
            ("source_frustum_coverage",select([candidate(0),candidate(1,pose(0,0.08))])),
            ("v1_reticle_target_differs",select([candidate(0),candidate(1,pose(0.02))],reticle:.perSource)),
            ("v2_reticle_target_differs",select([candidate(0),candidate(1,pose(0.02))],reticle:.reference(GameWorldCommonTargetStage.double(I)))),
            ("known_first_person",select([candidate(0),candidate(1)],reticle:.knownFirstPerson)),
            ("aim_epoch_mismatch",select([candidate(0),candidate(1)],reticle:.epochMismatch))]
        for (reason,decision) in rejects {check(decision.gameFromStage == nil && decision.telemetry.reason == reason,"explicit conservative fallback: "+reason)}
        check(select([candidate(0,tracking:0),candidate(1)]).telemetry.reason == "invalid_actual_source_identity","missing retained tracking epoch rejected")
        check(select([candidate(0),candidate(1,lease:1)]).telemetry.reason == "shared_source_lease","shared lease rejected")
        check(GameWorldCommonTargetStage.select(requested:true,candidates:[candidate(0),candidate(1)],selectedOpticalIndex:1,stageFromHead:simd_float4x4()).telemetry.reason == "invalid_display_pose","singular display pose rejected")
        check(select([candidate(0),candidate(1)],reticle:.reference(simd_double4x4())).telemetry.reason == "invalid_v2_reticle_reference","singular V2 reference rejected")
        check(select([candidate(0,camera:11),candidate(1,camera:11)]).telemetry.selectedSource?.game == 101,"camera tie uses same optical game-frame order")
        check(select([candidate(0,camera:11),candidate(1,camera:11)],index:0).telemetry.reason == "optical_target_not_newest","older game-frame tie selection rejected")
        check(select([candidate(0),candidate(1)],reticle:.perSource).telemetry.applied,"V1 untouched when mappings agree")
        check(select([candidate(0),candidate(1,pose(0.02))],reticle:.reference(GameWorldCommonTargetStage.double(pose(0.02)))).telemetry.applied,"V2 matching common reference preserved")
        check(GameWorldCommonTargetStage.select(requested:true,candidates:[],selectedOpticalIndex:nil,stageFromHead:Hd,blockedReason:"source_anchor_copy_active").telemetry.reason=="source_anchor_copy_active","copy path excluded")
        let afterTeardown=select([],index:nil)
        check(afterTeardown.gameFromStage == nil && !afterTeardown.telemetry.applied,"stateless selection cannot retain target after sources/renderer disappear")
        check(JSONSerialization.isValidJSONObject(equal.telemetry.reportValue),"provenance JSON safe")
        let report:[String:Any] = ["checks":checks,"static_cases":results,"scope":"PureCPU production transform/policy checks; no raster, avatar, disocclusion, device or GPU acceptance"]
        try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:URL(fileURLWithPath:CommandLine.arguments[1]))
        print("PASS: \(checks) common-target CPU checks; default off, source provenance retained; no GPU/device execution.")
    }
}

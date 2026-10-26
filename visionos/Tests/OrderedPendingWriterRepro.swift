import Foundation
import Metal
import simd

@main
struct OrderedPendingWriterRepro {
    static func main() throws {
        let root=URL(fileURLWithPath:CommandLine.arguments[1]), out=URL(fileURLWithPath:CommandLine.arguments[2])
        let device=MTLCreateSystemDefaultDevice()!, queue=device.makeCommandQueue()!
        let library=try device.makeLibrary(source:String(contentsOf:root.appendingPathComponent("visionos/Sources/GameWorld.metal"),encoding:.utf8),options:nil)
        let helper=GameWorldOrderedCorners(device:device,library:library,reuseSources:true)!
        let d=MTLTextureDescriptor.texture2DDescriptor(pixelFormat:.r32Float,width:16,height:12,mipmapped:false)
        d.storageMode = .shared;d.usage = .shaderRead
        let depth=device.makeTexture(descriptor:d)!, pixels=[Float](repeating:0.25,count:16*12)
        pixels.withUnsafeBytes{depth.replace(region:MTLRegionMake2D(0,0,16,12),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:16*4)}
        let p=simd_float4x4(columns:(SIMD4(1,0,0,0),SIMD4(0,1,0,0),SIMD4(0,0,0.0001,-1),SIMD4(0,0,0.1,0)))
        let u=GameWorldUniforms(sourceEyeFromClip:p.inverse,currentEyeFromSourceEye:matrix_identity_float4x4,currentClipFromEye:p,sourceSize:SIMD2(16,12))
        let source=GameWorldOrderedSourceIdentity(sourceID:1,generation:1,epoch:1,gameFrame:2,requestFrame:3,headFrame:4,eye:0,viewCount:2,
            depthHandle:UInt64(UInt(bitPattern:Unmanaged.passUnretained(depth as AnyObject).toOpaque())),width:16,height:12,presentationBits:Double(5).bitPattern,
            projectionBits:[p.columns.0,p.columns.1,p.columns.2,p.columns.3].flatMap{[$0.x.bitPattern,$0.y.bitPattern,$0.z.bitPattern,$0.w.bitPattern]})
        let gate=device.makeSharedEvent()!, first=queue.makeCommandBuffer()!
        first.encodeWaitForEvent(gate,value:1)
        let canonical=helper.prepare(commands:first,uniforms:u,depth:depth,source:source)!
        first.commit();precondition(first.status != .completed)
        let before=helper.statistics, second=queue.makeCommandBuffer()!
        let duplicate=helper.prepare(commands:second,uniforms:u,depth:depth,source:source)
        let report:[String:Any]=["schema":1,"device":device.name,"blocked_writer_actual":true,
            "pending_duplicate_returned_nil":duplicate == nil,"spare_set_capacity":before.sets<6,
            "spare_byte_capacity":before.bytes*2<384<<20,"sets_before":before.sets,"busy_before":before.busy,
            "duplicate_shared_canonical":duplicate.map{$0.indices === canonical.indices} ?? false,
            "performance_or_hardware_acceptance":false]
        if duplicate != nil {second.commit()}
        gate.signaledValue=1;first.waitUntilCompleted();if duplicate != nil {second.waitUntilCompleted()}
        precondition(first.status == .completed)
        try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:out,options:.withoutOverwriting)
        print(String(data:try JSONSerialization.data(withJSONObject:report,options:.sortedKeys),encoding:.utf8)!)
    }
}

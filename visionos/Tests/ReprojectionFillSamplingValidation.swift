import Foundation
import Metal
import CoreGraphics
import ImageIO
import simd

@main
struct ReprojectionFillSamplingValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 5,"Pass repo, registered comparison directory, output directory, frozen baseline shader")
        let root=URL(fileURLWithPath:CommandLine.arguments[1]),registryRoot=URL(fileURLWithPath:CommandLine.arguments[2]),out=URL(fileURLWithPath:CommandLine.arguments[3])
        try FileManager.default.createDirectory(at:out,withIntermediateDirectories:true)
        let device=MTLCreateSystemDefaultDevice()!,queue=device.makeCommandQueue()!
        func json(_ url:URL)throws->[String:Any]{try JSONSerialization.jsonObject(with:Data(contentsOf:url)) as! [String:Any]}
        func matrix(_ array:[NSNumber])->simd_float4x4{let a=array.map(\.floatValue);return .init(columns:(SIMD4(a[0],a[1],a[2],a[3]),SIMD4(a[4],a[5],a[6],a[7]),SIMD4(a[8],a[9],a[10],a[11]),SIMD4(a[12],a[13],a[14],a[15])))}
        func double(_ m:simd_float4x4)->simd_double4x4{.init(columns:(SIMD4<Double>(m[0]),SIMD4<Double>(m[1]),SIMD4<Double>(m[2]),SIMD4<Double>(m[3])))}
        func float(_ m:simd_double4x4)->simd_float4x4{.init(columns:(SIMD4<Float>(m[0]),SIMD4<Float>(m[1]),SIMD4<Float>(m[2]),SIMD4<Float>(m[3])))}
        func texture(_ f:MTLPixelFormat,_ w:Int,_ h:Int,_ usage:MTLTextureUsage)->any MTLTexture{let d=MTLTextureDescriptor.texture2DDescriptor(pixelFormat:f,width:w,height:h,mipmapped:false);d.storageMode = .shared;d.usage=usage;return device.makeTexture(descriptor:d)!}
        func ppm(_ path:URL)throws->(Int,Int,[UInt8]){let d=[UInt8](try Data(contentsOf:path));var p=0
            func token()->String{while d[p]<=32{p+=1};let a=p;while d[p]>32{p+=1};return String(bytes:d[a..<p],encoding:.ascii)!}
            precondition(token()=="P6");let w=Int(token())!,h=Int(token())!;precondition(token()=="255");p+=1;precondition(d.count-p==w*h*3)
            var rgba=[UInt8](repeating:255,count:w*h*4);for i in 0..<w*h{for c in 0..<3{rgba[i*4+c]=d[p+i*3+c]}};return(w,h,rgba)}
        func png(_ pixels:[UInt8],_ w:Int,_ h:Int,_ path:URL)throws{let image=CGImage(width:w,height:h,bitsPerComponent:8,bitsPerPixel:32,bytesPerRow:w*4,space:CGColorSpace(name:CGColorSpace.displayP3)!,bitmapInfo:CGBitmapInfo(rawValue:CGImageAlphaInfo.premultipliedLast.rawValue),provider:CGDataProvider(data:Data(pixels) as CFData)!,decode:nil,shouldInterpolate:false,intent:.relativeColorimetric)!
            let dest=CGImageDestinationCreateWithURL(path as CFURL,"public.png" as CFString,1,nil)!;CGImageDestinationAddImage(dest,image,nil);precondition(CGImageDestinationFinalize(dest))}
        let registry=try json(registryRoot.appendingPathComponent("registration.json")),fixed=try json(URL(fileURLWithPath:registry["fixed_view_input"] as! String))
        let path=URL(fileURLWithPath:(registry["profiles"] as! [String:String])["baseline1440"]!)
        let projections=(fixed["physical_output_projection_from_eyes"] as! [[NSNumber]]).map(matrix),eyes=(fixed["head_from_eyes"] as! [[NSNumber]]).map(matrix)
        let anchor=double(matrix(registry["common_game_from_stage"] as! [NSNumber]))
        let metal=try String(contentsOf:root.appendingPathComponent("visionos/Sources/GameWorld.metal"),encoding:.utf8)
        let probe=try String(contentsOf:root.appendingPathComponent("visionos/Tests/ReprojectionFillSamplingProbe.metal"),encoding:.utf8)
        let lib=try device.makeLibrary(source:metal+probe,options:nil)
        let original=try device.makeLibrary(source:String(contentsOfFile:CommandLine.arguments[4],encoding:.utf8),options:nil)
        let kernel=try device.makeComputePipelineState(function:lib.makeFunction(name:"probeFillSampling")!)
        let sky=GameWorldFiniteSkySupport(device:device,library:lib,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float,precomputeCorners:false,orderedCorners:false,reuseOrderedSources:false)!
        let fill=GameWorldDepthEdgeFill(device:device,library:lib,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float)!
        let oldSky=GameWorldFiniteSkySupport(device:device,library:original,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float,precomputeCorners:false,orderedCorners:false,reuseOrderedSources:false)!
        let oldFill=GameWorldDepthEdgeFill(device:device,library:original,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float)!
        let telemetry=GameWorldImageQualityTelemetry(fillRequested:true,fillAvailable:true)
        let width=1888,height=1792
        var rows=[[String:Any]](), timing=[[String:Any]]()
        for eye in 0..<2 {
            let metadata=try json(path.appendingPathComponent("eye\(eye)-report.json")),(w,h,rgba)=try ppm(path.appendingPathComponent("eye\(eye)-hud-world.ppm"))
            let color=texture(.rgba8Unorm_srgb,w,h,.shaderRead),depth=texture(.r32Float,w,h,.shaderRead)
            rgba.withUnsafeBytes{color.replace(region:MTLRegionMake2D(0,0,w,h),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:w*4)}
            let z=try Data(contentsOf:path.appendingPathComponent("eye\(eye)-source-depth.f32"));z.withUnsafeBytes{depth.replace(region:MTLRegionMake2D(0,0,w,h),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:w*4)}
            let sourceProjection=matrix(metadata["source_projection_from_eye"] as! [NSNumber]),gameFromEye=double(matrix(metadata["game_from_eye"] as! [NSNumber]))
            var turn=simd_float4x4(simd_quatf(angle:.pi/36,axis:SIMD3(0,1,0)));turn[3].x=0.025
            var small=matrix_identity_float4x4;small[3].x=0.001
            for (poseName,head) in [("neutral",matrix_identity_float4x4),("x1mm",small),("yaw5-x25mm",turn)] {
                let projection=projections[eye],relative=float((anchor*double(head)*double(eyes[eye])).inverse*gameFromEye)
                var u=GameWorldUniforms(sourceEyeFromClip:sourceProjection.inverse,currentEyeFromSourceEye:relative,currentClipFromEye:projection,sourceSize:SIMD2(UInt32(w),UInt32(h)))
                u.sourceDepthReversed=sourceProjection[2].z>=0 ? 1:0
                struct Rendered {let color:[UInt8];let depth:[UInt32];let owner:[UInt8];let values:[Float];let gpuMilliseconds:Double}
                func render(_ old:Bool, measureOnly:Bool=false)->Rendered {
                let target=texture(.rgba8Unorm_srgb,width,height,[.renderTarget,.shaderRead]),targetDepth=texture(.depth32Float,width,height,.renderTarget)
                let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target;pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store;pass.colorAttachments[0].clearColor=MTLClearColorMake(0,0,0,1)
                pass.depthAttachment.texture=targetDepth;pass.depthAttachment.loadAction = .clear;pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
                let viewport=MTLViewport(originX:0,originY:0,width:Double(width),height:Double(height),znear:0,zfar:1),cmd=queue.makeCommandBuffer()!
                let far=GameWorldDepthRange.selectedFar(nativeFar:(metadata["source_far"] as! NSNumber).floatValue)
                let world=(old ? oldSky:sky).encode(commands:cmd,pass:pass,viewport:viewport,projection:projection,far:far,uniforms:u,color:color,depth:depth)!
                var fu=GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection:sourceProjection,currentEyeFromSourceEye:relative,displayProjection:projection,viewportOrigin:.zero,viewportSize:SIMD2(Float(width),Float(height)),sourceSize:u.sourceSize,sourceDepthReversed:u.sourceDepthReversed,colorTransfer:0,nearestDistance:u.nearFadeDistances.y,skyRadius:world.radius)!
                let result=texture(.rgba32Float,width,height,[.shaderWrite,.shaderRead])
                if !old && !measureOnly {
                let ce=cmd.makeComputeCommandEncoder()!;ce.setComputePipelineState(kernel);ce.setBytes(&fu,length:MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride,index:0)
                ce.setTexture(world.ownership,index:0);ce.setTexture(depth,index:1);ce.setTexture(result,index:2)
                ce.dispatchThreads(MTLSize(width:width,height:height,depth:1),threadsPerThreadgroup:MTLSize(width:16,height:16,depth:1));ce.endEncoding()
                }
                pass.colorAttachments[0].loadAction = .load;pass.depthAttachment.loadAction = .load
                let e=cmd.makeRenderCommandEncoder(descriptor:pass)!;e.setViewport(viewport)
                precondition((old ? oldFill:fill).encode(e,commands:cmd,eye:2,uniforms:fu,ownership:world.ownership,color:color,depth:depth,telemetry:telemetry));e.endEncoding()
                let row=(width+255)&(~255),ownerBuffer=device.makeBuffer(length:row*height,options:.storageModeShared)!
                if !measureOnly {let blit=cmd.makeBlitCommandEncoder()!;blit.copy(from:world.ownership,sourceSlice:0,sourceLevel:0,sourceOrigin:MTLOrigin(),sourceSize:MTLSize(width:width,height:height,depth:1),to:ownerBuffer,destinationOffset:0,destinationBytesPerRow:row,destinationBytesPerImage:row*height);blit.endEncoding()}
                cmd.commit();cmd.waitUntilCompleted();precondition(cmd.status == .completed)
                let milliseconds=(cmd.gpuEndTime-cmd.gpuStartTime)*1000
                if measureOnly {return Rendered(color:[],depth:[],owner:[],values:[],gpuMilliseconds:milliseconds)}
                var values=[Float](repeating:0,count:width*height*4),pixels=[UInt8](repeating:0,count:width*height*4),z=[UInt32](repeating:0,count:width*height)
                if !old {values.withUnsafeMutableBytes{result.getBytes($0.baseAddress!,bytesPerRow:width*16,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}}
                pixels.withUnsafeMutableBytes{target.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}
                z.withUnsafeMutableBytes{targetDepth.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}
                let pointer=ownerBuffer.contents().bindMemory(to:UInt8.self,capacity:ownerBuffer.length)
                let owners=(0..<width*height).map{pointer[$0/width*row+$0%width]}
                return Rendered(color:pixels,depth:z,owner:owners,values:values,gpuMilliseconds:milliseconds)
                }
                // Serial, warmed command intervals exclude probe, readback and ownership blit.
                // These are M4 fixture command durations, not isolated shader cost or headset FPS.
                if eye==0 && poseName=="yaw5-x25mm" {
                    for _ in 0..<3 {for old in [true,false] {_=autoreleasepool{render(old,measureOnly:true)}}}
                    for pair in 0..<10 {for old in pair%2==0 ? [true,false]:[false,true] {
                        let measured=autoreleasepool{render(old,measureOnly:true)}
                        timing.append(["pair":pair,"pipeline":old ? "before":"after","gpu_command_ms":measured.gpuMilliseconds])
                    }}
                }
                let before=autoreleasepool{render(true)},after=autoreleasepool{render(false)}
                precondition(before.owner==after.owner,"No ownership changes")
                let values=after.values,pixels=after.color
                var finite=0,skyCount=0,planar=0,mismatched=0,maxDelta:Float=0,sum:Double=0
                var colorChanged=0,depthChanged=0,maxCodeChange=0,candidateChanged=[Int]()
                var mask=[UInt8](repeating:0,count:width*height*4)
                for i in 0..<width*height {mask[i*4+3]=255
                    if values[i*4+3]==1 {finite+=1;let delta=hypot(values[i*4],values[i*4+1]);if delta.isFinite{maxDelta=max(maxDelta,delta);sum+=Double(delta);if delta>0.5{mismatched+=1}}
                        mask[i*4]=255
                    }else if values[i*4+3]==2{skyCount+=1;mask[i*4+2]=255}
                    if values[i*4+2]==1{planar+=1;mask[i*4]=0;mask[i*4+1]=255}
                    let rgbChanged=(0..<4).contains{before.color[i*4+$0] != after.color[i*4+$0]}
                    let zChanged=before.depth[i] != after.depth[i]
                    if values[i*4+2] != 1 {precondition(!rgbChanged && !zChanged,"Unqualified fallback/covered/sky pixels remain exact")}
                    if before.owner[i] != 0 {precondition(!rgbChanged && !zChanged,"Covered source world unchanged")}
                    if rgbChanged {colorChanged+=1;candidateChanged.append(i);for c in 0..<3{maxCodeChange=max(maxCodeChange,abs(Int(before.color[i*4+c])-Int(after.color[i*4+c])))}}
                    if zChanged {depthChanged+=1}
                    precondition((before.depth[i] == 0) == (after.depth[i] == 0),"No new/lost finite coverage")
                }
                let name="\(poseName)-eye\(eye)";try png(pixels,width,height,out.appendingPathComponent(name+"-after.png"));try png(before.color,width,height,out.appendingPathComponent(name+"-before.png"));try png(mask,width,height,out.appendingPathComponent(name+"-mask.png"))
                if let center=candidateChanged.max(by:{a,b in
                    (0..<3).map{abs(Int(before.color[a*4+$0])-Int(after.color[a*4+$0]))}.max()! < (0..<3).map{abs(Int(before.color[b*4+$0])-Int(after.color[b*4+$0]))}.max()!
                }) {
                    let x=max(0,min(width-160,center%width-80)),y=max(0,min(height-160,center/width-80))
                    for (label,image) in [("before",before.color),("after",after.color)]{var crop=[UInt8]();for row in y..<y+160{crop.append(contentsOf:image[(row*width+x)*4..<(row*width+x+160)*4])};try png(crop,160,160,out.appendingPathComponent(name+"-crop-"+label+".png"))}
                }
                try values.withUnsafeBytes{try Data($0).write(to:out.appendingPathComponent(name+"-probe.f32"))}
                rows.append(["name":name,"eye":eye,"source_lease":metadata["lease"]!,"source_game_frame":metadata["game_frame"]!,"source_head_frame":metadata["head_frame"]!,"source_projection_from_eye":metadata["source_projection_from_eye"]!,"current_eye_from_source_eye":(0..<4).flatMap{c in (0..<4).map{r in relative[c][r]}},"finite_fill":finite,"sky_fill":skyCount,"planar_support":planar,"entry_to_intersection_above_half_texel":mismatched,"entry_to_intersection_max_texels":maxDelta,"entry_to_intersection_mean_texels":sum/Double(max(finite,1)),"changed_color_pixels":colorChanged,"changed_filled_depth_pixels":depthChanged,"max_color_code_delta":maxCodeChange,"covered_fallback_sky_color_and_depth_exact":true,"ownership_exact":true,"fill_coverage_exact":true])
                print("\(name): finite\(finite) sky\(skyCount) planar\(planar) RGBchanged\(colorChanged) depthChanged\(depthChanged) maxByte\(maxCodeChange)")
            }
        }
        var timingSummary=[String:Any]()
        for label in ["before","after"] {
            let a=timing.filter{$0["pipeline"] as! String == label}.map{$0["gpu_command_ms"] as! Double}.sorted()
            timingSummary[label] = ["median_ms":(a[4]+a[5])/2,"minimum_ms":a[0],"maximum_ms":a[9]]
        }
        let timingReport:[String:Any] = ["scope":"Serial full world+gap-fill command buffers, no probe/readback/blit; registered eye0 yaw5+25mm pose; 3 warm-up pairs then 10 alternating-order pairs. Local M4 only; no headset FPS or isolated shader cost inference.","samples":timing,"summary":timingSummary]
        try JSONSerialization.data(withJSONObject:["rows":rows,"device":device.name,"timing":timingReport,"world_renders":38,"registered_image_comparisons":6],options:[.prettyPrinted,.sortedKeys]).write(to:out.appendingPathComponent("report.json"))
    }
}

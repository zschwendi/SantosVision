import Foundation
import Metal
import simd

@main
struct OrderedSourceReuseImageValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 3, "Pass repo and new output JSON")
        let root = URL(fileURLWithPath: CommandLine.arguments[1]), output = URL(fileURLWithPath: CommandLine.arguments[2])
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let library = try device.makeLibrary(source: String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8), options: nil)
        let cached = GameWorldFiniteSkySupport(device: device, library: library, colorFormat: .rgba8Unorm,
            depthFormat: .depth32Float, precomputeCorners: false, orderedCorners: true, reuseOrderedSources: true)!
        let reference = GameWorldFiniteSkySupport(device: device, library: library, colorFormat: .rgba8Unorm,
            depthFormat: .depth32Float, precomputeCorners: false, orderedCorners: true, reuseOrderedSources: false)!
        let fill = GameWorldDepthEdgeFill(device: device, library: library, colorFormat: .rgba8Unorm, depthFormat: .depth32Float)!
        let telemetry = GameWorldImageQualityTelemetry(fillRequested: true, fillAvailable: true)
        var checks = 0
        func require(_ value: @autoclosure () -> Bool, _ message: String) {
            if !value() { FileHandle.standardError.write(Data(("FAIL: " + message + "\n").utf8)); preconditionFailure(message) }
            checks += 1
        }
        func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int, _ usage: MTLTextureUsage) -> any MTLTexture {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
            d.storageMode = .shared; d.usage = usage
            return device.makeTexture(descriptor: d)!
        }
        let width = 48, height = 36, outWidth = 192, outHeight = 144
        let color = texture(.rgba8Unorm,width,height,.shaderRead), depth = texture(.r32Float,width,height,.shaderRead)
        var pixels = [UInt8](repeating:255,count:width*height*4), z = [Float](repeating:0,count:width*height)
        for y in 0..<height { for x in 0..<width {
            let i=y*width+x
            pixels[i*4]=UInt8(x*5);pixels[i*4+1]=UInt8(y*7);pixels[i*4+2]=UInt8((x+y)*3)
            z[i] = y < 5 ? 0 : (x > 10 && x < 28 && y > 16 ? 0.25 : 0.05)
        } }
        pixels.withUnsafeBytes { color.replace(region:MTLRegionMake2D(0,0,width,height),mipmapLevel:0,
            withBytes:$0.baseAddress!,bytesPerRow:width*4) }
        z.withUnsafeBytes { depth.replace(region:MTLRegionMake2D(0,0,width,height),mipmapLevel:0,
            withBytes:$0.baseAddress!,bytesPerRow:width*4) }
        let projection=simd_float4x4(columns:(SIMD4(1,0,0,0),SIMD4(0,1.2,0,0),SIMD4(0,0,0.0001,-1),SIMD4(0,0,0.1,0)))
        let columns=[projection.columns.0,projection.columns.1,projection.columns.2,projection.columns.3]
        let source=GameWorldOrderedSourceIdentity(sourceID:1,generation:1,epoch:1,gameFrame:2,requestFrame:3,headFrame:4,
            eye:0,viewCount:2,depthHandle:UInt64(UInt(bitPattern:Unmanaged.passUnretained(depth as AnyObject).toOpaque())),
            width:UInt32(width),height:UInt32(height),presentationBits:Double(5).bitPattern,
            projectionBits:columns.flatMap {[$0.x.bitPattern,$0.y.bitPattern,$0.z.bitPattern,$0.w.bitPattern]})
        struct Image { let rgb:[UInt8]; let depth:[UInt32]; let owner:[UInt8] }
        var heldCanonical: (any MTLCommandBuffer)?
        func render(_ helper:GameWorldFiniteSkySupport,pose:Int,hold:Bool=false,preparation:String?=nil)->Image {
            var relative=simd_float4x4(simd_quatf(angle:Float(pose)*0.015,axis:SIMD3(0,1,0)))
            relative[3]=SIMD4(Float(pose)*0.015,Float(pose)*0.005,0,1)
            let values=GameWorldUniforms(sourceEyeFromClip:projection.inverse,currentEyeFromSourceEye:relative,
                currentClipFromEye:projection,sourceSize:SIMD2(UInt32(width),UInt32(height)))
            let target=texture(.rgba8Unorm,outWidth,outHeight,.renderTarget)
            let targetDepth=texture(.depth32Float,outWidth,outHeight,.renderTarget)
            let pass=MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture=target;pass.colorAttachments[0].loadAction = .clear
            pass.colorAttachments[0].storeAction = .store;pass.colorAttachments[0].clearColor=MTLClearColorMake(0,0,0,1)
            pass.depthAttachment.texture=targetDepth;pass.depthAttachment.loadAction = .clear
            pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
            let viewport=MTLViewport(originX:0,originY:0,width:Double(outWidth),height:Double(outHeight),znear:0,zfar:1)
            let command=queue.makeCommandBuffer()!
            let result=helper.encode(commands:command,pass:pass,viewport:viewport,projection:projection,far:1000,
                uniforms:values,color:color,depth:depth,orderedSource:source)!
            require(result.orderedCorners,"Assembled normal world draw uses ordered path")
            if let preparation { require(result.orderedPreparation == preparation,"Actual per-eye preparation label matches the exercised path") }
            let overlay=MTLRenderPassDescriptor()
            overlay.colorAttachments[0].texture=target;overlay.colorAttachments[0].loadAction = .load;overlay.colorAttachments[0].storeAction = .store
            overlay.depthAttachment.texture=targetDepth;overlay.depthAttachment.loadAction = .load;overlay.depthAttachment.storeAction = .store
            let e=command.makeRenderCommandEncoder(descriptor:overlay)!;e.setViewport(viewport)
            let fu=GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection:projection,currentEyeFromSourceEye:relative,
                displayProjection:projection,viewportOrigin:.zero,viewportSize:SIMD2(Float(outWidth),Float(outHeight)),
                sourceSize:values.sourceSize,sourceDepthReversed:1,colorTransfer:0,nearestDistance:0.1,skyRadius:800)!
            require(fill.encode(e,commands:command,eye:2,uniforms:fu,ownership:result.ownership,color:color,depth:depth,
                telemetry:telemetry),"Same production depth-edge gap fill executes after finite sky")
            e.endEncoding()
            let row=256, owners=device.makeBuffer(length:256*outHeight,options:.storageModeShared)!
            let blit=command.makeBlitCommandEncoder()!
            blit.copy(from:result.ownership,sourceSlice:0,sourceLevel:0,sourceOrigin:MTLOrigin(),
                sourceSize:MTLSize(width:outWidth,height:outHeight,depth:1),to:owners,destinationOffset:0,
                destinationBytesPerRow:row,destinationBytesPerImage:row*outHeight)
            blit.endEncoding()
            if hold {
                require(heldCanonical == nil,"One immutable canonical source writer is retained for overlapped cadence")
                heldCanonical=command
                return Image(rgb:[],depth:[],owner:[])
            }
            command.commit();command.waitUntilCompleted()
            require(command.status == .completed,"Assembled actual Metal image completed")
            var rgb=[UInt8](repeating:0,count:outWidth*outHeight*4), bits=[UInt32](repeating:0,count:outWidth*outHeight)
            rgb.withUnsafeMutableBytes {target.getBytes($0.baseAddress!,bytesPerRow:outWidth*4,
                from:MTLRegionMake2D(0,0,outWidth,outHeight),mipmapLevel:0)}
            bits.withUnsafeMutableBytes {targetDepth.getBytes($0.baseAddress!,bytesPerRow:outWidth*4,
                from:MTLRegionMake2D(0,0,outWidth,outHeight),mipmapLevel:0)}
            let p=owners.contents().bindMemory(to:UInt8.self,capacity:owners.length)
            return Image(rgb:rgb,depth:bits,owner:(0..<outWidth*outHeight).map {p[$0/outWidth*row+$0%outWidth]})
        }
        let poses=[0,-3,-2,-1,1,2,3]
        for pose in poses {
            let expected=autoreleasepool {render(reference,pose:pose)}
            let actual=autoreleasepool {render(cached,pose:pose)}
            require(actual.rgb==expected.rgb && actual.depth==expected.depth && actual.owner==expected.owner,
                "Cached source matches fresh preparation in full RGB/Float32 depth/final ownership after both postpasses")
            require(actual.owner.contains(1) && actual.owner.contains(2),"Fixture renders actual sky and finite discontinuous geometry")
        }
        require(cached.orderedCornerStatus()["source_cache_hits"] as? UInt64 == UInt64(poses.count-1),
            "Only the first source preparation executes; later distinct display poses hit completed cached data")
        require(reference.orderedCornerStatus()["source_cache_hits"] as? UInt64 == 0,"Cacheless reference never hits")
        let overlapped=GameWorldFiniteSkySupport(device:device,library:library,colorFormat:.rgba8Unorm,
            depthFormat:.depth32Float,precomputeCorners:false,orderedCorners:true,reuseOrderedSources:true)!
        _ = render(overlapped,pose:0,hold:true,preparation:"fresh_preparation")
        for pose in poses {
            let expected=autoreleasepool {render(reference,pose:pose)}
            let actual=autoreleasepool {render(overlapped,pose:pose,preparation:"pending_source_fresh_transient")}
            require(actual.rgb==expected.rgb && actual.depth==expected.depth && actual.owner==expected.owner,
                "Each overlapping display pose retains exact full RGB/Float32-depth/owner parity without fine fallback")
        }
        require(overlapped.orderedCornerStatus()["source_cache_hits"] as? UInt64 == 0,
            "No transient reads an uncommitted canonical writer")
        require(overlapped.orderedCornerStatus()["pending_transient_admissions"] as? UInt64 == UInt64(poses.count),
            "Every overlapped pose is an actual admitted fresh transient")
        heldCanonical!.commit();heldCanonical!.waitUntilCompleted();heldCanonical=nil
        _ = render(overlapped,pose:0,preparation:"completed_source_cache_hit")
        require(overlapped.orderedCornerStatus()["source_cache_hits"] as? UInt64 == 1,
            "Only completing the original canonical writer makes the source reusable")
        let report:[String:Any]=["schema":1,"device":device.name,"checks":checks,"pass":true,
            "completed_full_pipeline_renders":poses.count*4+2,"display_pose_indices":poses,"cache_hits":poses.count-1,
            "overlapped_pending_transient_renders":poses.count,"canonical_uncommitted_during_transients":true,
            "rgb_depth_final_owner_byte_identical":true,"finite_sky_and_gap_fill_actual":true,
            "source_fixture":"Immutable synthetic finite geometry, depth discontinuities and clear sky",
            "physical_game_quality_stereo_or_fps_acceptance":false]
        try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:output,options:.withoutOverwriting)
        print("PASS cached-source assembled images: \(checks) checks, \(poses.count*4+2) completed full-pipeline Metal renders, including \(poses.count) pending-source transients")
    }
}

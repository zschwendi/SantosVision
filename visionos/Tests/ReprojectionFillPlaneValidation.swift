// Bounded analytic controls for the production owner-0 fill. Artificial holes
// isolate interpolation error; they are never presented as actual game gaps.
import Foundation
import Metal
import simd

@main
struct ReprojectionFillPlaneValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 4,"Pass repository, new report JSON, frozen baseline shader")
        let root=URL(fileURLWithPath:CommandLine.arguments[1]),output=URL(fileURLWithPath:CommandLine.arguments[2])
        let device=MTLCreateSystemDefaultDevice()!,queue=device.makeCommandQueue()!
        let production=try String(contentsOf:root.appendingPathComponent("visionos/Sources/GameWorld.metal"),encoding:.utf8)
        let probe=try String(contentsOf:root.appendingPathComponent("visionos/Tests/ReprojectionFillSamplingProbe.metal"),encoding:.utf8)
        let current=try device.makeLibrary(source:production+probe,options:nil)
        let old=try device.makeLibrary(source:String(contentsOfFile:CommandLine.arguments[3],encoding:.utf8),options:nil)
        let currentFill=GameWorldDepthEdgeFill(device:device,library:current,colorFormat:.rgba32Float,depthFormat:.depth32Float)!
        let oldFill=GameWorldDepthEdgeFill(device:device,library:old,colorFormat:.rgba32Float,depthFormat:.depth32Float)!
        let kernel=try device.makeComputePipelineState(function:current.makeFunction(name:"probeFillSampling")!)
        let telemetry=GameWorldImageQualityTelemetry(fillRequested:true,fillAvailable:true)
        var checks=0
        func require(_ c:@autoclosure()->Bool,_ message:String){if !c(){fatalError(message)};checks+=1}
        func texture(_ f:MTLPixelFormat,_ w:Int,_ h:Int,_ usage:MTLTextureUsage)->any MTLTexture{let d=MTLTextureDescriptor.texture2DDescriptor(pixelFormat:f,width:w,height:h,mipmapped:false);d.storageMode = .shared;d.usage=usage;return device.makeTexture(descriptor:d)!}
        func double(_ m:simd_float4x4)->simd_double4x4{.init(columns:(SIMD4<Double>(m[0]),SIMD4<Double>(m[1]),SIMD4<Double>(m[2]),SIMD4<Double>(m[3])))}
        func decode(_ x:Double)->Double{x<=0.04045 ? x/12.92:pow((x+0.055)/1.055,2.4)}
        let sRGBXYZ=simd_double3x3(columns:(SIMD3(0.4123907992659595,0.2126390058715104,0.0193308187155918),SIMD3(0.357584339383878,0.715168678767756,0.119194779794626),SIMD3(0.1804807884018343,0.0721923153607337,0.9505321522496607)))
        let P3XYZ=simd_double3x3(columns:(SIMD3(0.4865709486482162,0.2289745640697488,0),SIMD3(0.2656676931690931,0.6917385218365064,0.0451133818589026),SIMD3(0.1982172852343625,0.079286914093745,1.043944368900976)))
        let toP3=P3XYZ.inverse*sRGBXYZ
        struct Case {let name:String;let mode:String;let shift:Float;let reversed:Bool;let raw:Bool;let sw:Int;let sh:Int}
        var cases=[Case]()
        for shift:Float in [-0.02,-0.005,0,0.005,0.02] {
            cases.append(Case(name:"sloped-x\(shift)",mode:"slope",shift:shift,reversed:true,raw:false,sw:32,sh:24))
        }
        cases += [Case(name:"sloped-raw-fallback",mode:"slope",shift:0.02,reversed:true,raw:true,sw:32,sh:24),
            Case(name:"forward-depth",mode:"slope",shift:0.02,reversed:false,raw:false,sw:32,sh:24),
            Case(name:"flat-plane",mode:"flat",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"alternating-thin-depth-edges",mode:"stripes",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"single-thin-occluder",mode:"bar",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"curved-unsupported",mode:"curved",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"clear-sky",mode:"sky",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"invalid-depth",mode:"invalid",shift:0.02,reversed:true,raw:false,sw:32,sh:24),
            Case(name:"small-source",mode:"flat",shift:0.02,reversed:true,raw:false,sw:2,sh:2)]
        let width=128,height=96
        var rows=[[String:Any]]()
        for test in cases {
            let sw=test.sw,sh=test.sh,base:Double=test.reversed ? 0.125:0.875
            let dx:Double=test.mode=="slope"||test.mode=="bar" ? 1.0/16384:0
            let dy:Double=test.mode=="slope"||test.mode=="bar" ? -1.0/32768:0
            var sourceDepth=[Float](repeating:0,count:sw*sh),sourceRGB=[UInt8](repeating:255,count:sw*sh*4)
            for y in 0..<sh {for x in 0..<sw {
                let i=y*sw+x
                sourceDepth[i]=Float(base+dx*(Double(x)+0.5)+dy*(Double(y)+0.5))
                if test.mode=="stripes" && x%2==0 {sourceDepth[i]=0.5}
                if test.mode=="bar" && x==sw/2 {sourceDepth[i]=0.5}
                if test.mode=="curved" {sourceDepth[i] += Float(x*x)/8192}
                if test.mode=="sky" {sourceDepth[i]=test.reversed ? 0:1}
                if test.mode=="invalid" {sourceDepth[i] = .nan}
                sourceRGB[i*4]=UInt8(x%2==0 ? 230:20);sourceRGB[i*4+1]=UInt8(y%2==0 ? 210:40)
                sourceRGB[i*4+2]=UInt8((x+y)%2==0 ? 190:60)
            }}
            let color=texture(test.raw ? .rgba8Unorm:.rgba8Unorm_srgb,sw,sh,.shaderRead),depth=texture(.r32Float,sw,sh,.shaderRead)
            sourceRGB.withUnsafeBytes{color.replace(region:MTLRegionMake2D(0,0,sw,sh),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:sw*4)}
            sourceDepth.withUnsafeBytes{depth.replace(region:MTLRegionMake2D(0,0,sw,sh),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:sw*4)}
            let projection=simd_float4x4(columns:(SIMD4(1.2,0,0,0),SIMD4(0,1.6,0,0),SIMD4(0,0,test.reversed ? 0.0001:-1.0001,-1),SIMD4(0,0,test.reversed ? 0.1:-0.1,0)))
            var relative=matrix_identity_float4x4;relative[3].x=test.shift
            var u=GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection:projection,currentEyeFromSourceEye:relative,displayProjection:projection,viewportOrigin:.zero,viewportSize:SIMD2(Float(width),Float(height)),sourceSize:SIMD2(UInt32(sw),UInt32(sh)),sourceDepthReversed:test.reversed ? 1:0,colorTransfer:test.raw ? 2:0,nearestDistance:0.1,skyRadius:800)!
            let owner=texture(.r8Uint,width,height,.shaderRead)
            // Interleave untouched pixels to prove the production owner gate.
            var owners=[UInt8](repeating:0,count:width*height);for i in 0..<owners.count{if i%7==0{owners[i]=2}}
            owners.withUnsafeBytes{owner.replace(region:MTLRegionMake2D(0,0,width,height),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:width)}
            struct Result {let rgb:[Float];let depth:[UInt32];let probe:[Float]}
            func render(_ before:Bool)->Result {
                let target=texture(.rgba32Float,width,height,.renderTarget),z=texture(.depth32Float,width,height,.renderTarget),probeTarget=texture(.rgba32Float,width,height,[.shaderRead,.shaderWrite])
                let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target;pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
                pass.colorAttachments[0].clearColor=MTLClearColorMake(0.125,0.25,0.375,1)
                pass.depthAttachment.texture=z;pass.depthAttachment.loadAction = .clear;pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
                let cmd=queue.makeCommandBuffer()!
                if !before {let e=cmd.makeComputeCommandEncoder()!;e.setComputePipelineState(kernel);e.setBytes(&u,length:MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride,index:0);e.setTexture(owner,index:0);e.setTexture(depth,index:1);e.setTexture(probeTarget,index:2);e.dispatchThreads(MTLSize(width:width,height:height,depth:1),threadsPerThreadgroup:MTLSize(width:16,height:16,depth:1));e.endEncoding()}
                let e=cmd.makeRenderCommandEncoder(descriptor:pass)!
                require((before ? oldFill:currentFill).encode(e,commands:cmd,eye:2,uniforms:u,ownership:owner,color:color,depth:depth,telemetry:telemetry),"Production repair encodes")
                e.endEncoding();cmd.commit();cmd.waitUntilCompleted();require(cmd.status == .completed,"Production repair completes")
                var rgb=[Float](repeating:0,count:width*height*4),zvalues=[UInt32](repeating:0,count:width*height),pvalues=[Float](repeating:0,count:width*height*4)
                rgb.withUnsafeMutableBytes{target.getBytes($0.baseAddress!,bytesPerRow:width*16,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}
                zvalues.withUnsafeMutableBytes{z.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}
                if !before {pvalues.withUnsafeMutableBytes{probeTarget.getBytes($0.baseAddress!,bytesPerRow:width*16,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}}
                return Result(rgb:rgb,depth:zvalues,probe:pvalues)
            }
            let before=render(true),after=render(false)
            var accepted=0,changed=0,oldError=0.0,newError=0.0,maxNewError=0.0,maxDepthError=0.0
            let invProjection=double(projection).inverse,sourceFromCurrent=double(projection)*double(relative).inverse
            // Analytic source plane is constructed independently from original
            // fixture parameters, never fitted from the helper's 3x3 samples.
            let plane=SIMD4<Double>(-dx*Double(sw)/2,dy*Double(sh)/2,1,-base-dx*Double(sw)/2-dy*Double(sh)/2)
            for y in 0..<height {for x in 0..<width {let i=y*width+x
                let rgbChanged=(0..<4).contains{before.rgb[i*4+$0].bitPattern != after.rgb[i*4+$0].bitPattern}
                let depthChanged=before.depth[i] != after.depth[i]
                if rgbChanged || depthChanged {changed+=1}
                require((before.depth[i]==0)==(after.depth[i]==0),"No added or lost successful finite fill")
                if after.probe[i*4+2] != 1 {require(!rgbChanged && !depthChanged,"Unsupported/sky/covered fallback stays byte-exact");continue}
                accepted+=1;require(owners[i]==0,"Only artificial uncovered pixels corrected")
                if test.mode=="slope"||test.mode=="flat"||test.mode=="bar" {
                    var ray=invProjection*SIMD4((Double(x)+0.5)/Double(width)*2-1,1-(Double(y)+0.5)/Double(height)*2,0.5,1)
                    ray=SIMD4(simd_normalize(SIMD3(ray.x,ray.y,ray.z)/ray.w),0)
                    let h0=sourceFromCurrent*ray,h1=sourceFromCurrent[3],s = -simd_dot(plane,h0)/simd_dot(plane,h1),h=h0+s*h1
                    let sourcePixel=SIMD2(h.x/h.w*0.5+0.5,0.5-h.y/h.w*0.5)*SIMD2(Double(sw),Double(sh))-0.5
                    let bx=Int(floor(sourcePixel.x)),by=Int(floor(sourcePixel.y)),f=sourcePixel-SIMD2(Double(bx),Double(by))
                    require(bx>=0 && by>=0 && bx+1<sw && by+1<sh,"Oracle complete filter footprint")
                    var expected=SIMD3<Double>.zero
                    for yy in 0...1 {for xx in 0...1 {
                        let k=(by+yy)*sw+bx+xx,weight=(xx==0 ? 1-f.x:f.x)*(yy==0 ? 1-f.y:f.y)
                        if test.mode=="bar" {require(bx+xx != sw/2,"No admitted background tap crosses thin foreground")}
                        for c in 0..<3{expected[c]+=decode(Double(sourceRGB[k*4+c])/255)*weight}
                    }}
                    expected=toP3*expected
                    for c in 0..<3 {let e=abs(Double(after.rgb[i*4+c])-expected[c]);newError+=e;oldError+=abs(Double(before.rgb[i*4+c])-expected[c]);maxNewError=max(maxNewError,e)}
                    let projected=double(projection)*SIMD4(ray.x,ray.y,ray.z,s)
                    maxDepthError=max(maxDepthError,abs(Double(Float(bitPattern:after.depth[i]))-projected.z/projected.w))
                }
            }}
            if ["stripes","curved","sky","invalid"].contains(test.mode) || test.sw<3 {require(accepted==0 && changed==0,"Adverse unsupported source fully retains baseline")}
            else {require(accepted>1000,"Analytic supported plane exercises real repairs");require(maxNewError<0.004,"Filtered color matches analytic plane oracle within hardware sampler precision");require(maxDepthError<0.000001,"Filled depth matches analytic ray-plane oracle");require(newError<oldError*0.02,"Analytic color error reduced at least98 percent")}
            rows.append(["name":test.name,"accepted_pixels":accepted,"changed_pixels":changed,"mean_before_linear_error":oldError/Double(max(accepted*3,1)),"mean_after_linear_error":newError/Double(max(accepted*3,1)),"max_after_linear_error":maxNewError,"max_after_depth_error":maxDepthError,"unsupported_and_covered_exact":true,"fill_coverage_exact":true])
            print("PASS \(test.name): admitted\(accepted), maxRGBerror\(maxNewError), maxDepthError\(maxDepthError)")
        }
        // Direct boundary probes invoke the same production helper with a
        // known selected hit, so rejected near/far or grazing configurations
        // cannot disappear merely because the preceding search skips them.
        let directKernel=try device.makeComputePipelineState(function:current.makeFunction(name:"probeFillDirect")!)
        struct Hit {var texel:SIMD2<UInt32>;var depth:Float;var inverseDistance:Float;var sky:UInt32}
        var boundaryRows=[[String:Any]]()
        let directProjection=simd_float4x4(columns:(SIMD4(1.2,0,0,0),SIMD4(0,1.6,0,0),SIMD4(0,0,0.0001,-1),SIMD4(0,0,0.1,0)))
        func boundary(_ name:String,_ size:SIMD2<Int>,_ values:[Float],_ hitPixel:SIMD2<UInt32>,_ sourceCenter:SIMD2<Float>,
                      relative:simd_float4x4 = matrix_identity_float4x4,maximum:Float=10,expected:Bool?,hardware:Bool=false) {
            let d=texture(.r32Float,size.x,size.y,.shaderRead),result=texture(.rgba32Float,1,1,[.shaderWrite,.shaderRead])
            let color=texture(hardware ? .rgba8Unorm_srgb:.rgba8Unorm,size.x,size.y,.shaderRead),filterResult=texture(.rgba32Float,1,1,[.shaderWrite,.shaderRead])
            var sentinel=[UInt8](repeating:255,count:size.x*size.y*4)
            let bx=max(0,min(Int(hitPixel.x)-1,size.x-3)),by=max(0,min(Int(hitPixel.y)-1,size.y-3))
            for y in by..<by+3{for x in bx..<bx+3{for c in 0..<3{sentinel[(y*size.x+x)*4+c]=0}}}
            sentinel.withUnsafeBytes{color.replace(region:MTLRegionMake2D(0,0,size.x,size.y),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:size.x*4)}
            values.withUnsafeBytes{d.replace(region:MTLRegionMake2D(0,0,size.x,size.y),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:size.x*4)}
            let ndc=(sourceCenter+0.5)/SIMD2(Float(size.x),Float(size.y))
            let p=directProjection.inverse*SIMD4(ndc.x*2-1,1-ndc.y*2,0.5,1)
            var ray=simd_normalize(SIMD3(p.x,p.y,p.z)/p.w)
            var u=GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection:directProjection,currentEyeFromSourceEye:relative,displayProjection:directProjection,viewportOrigin:.zero,viewportSize:SIMD2(1,1),sourceSize:SIMD2(UInt32(size.x),UInt32(size.y)),sourceDepthReversed:1,colorTransfer:0,nearestDistance:0.1,skyRadius:800)!
            u.maximumInverseDistance=maximum
            u.colorTransfer=hardware ? 0:2
            var hit=Hit(texel:hitPixel,depth:values[Int(hitPixel.y)*size.x+Int(hitPixel.x)],inverseDistance:1,sky:0)
            let cmd=queue.makeCommandBuffer()!,e=cmd.makeComputeCommandEncoder()!
            e.setComputePipelineState(directKernel);e.setBytes(&u,length:MemoryLayout<GameWorldDepthEdgeFillUniforms>.stride,index:0)
            e.setBytes(&hit,length:MemoryLayout<Hit>.stride,index:1);e.setBytes(&ray,length:MemoryLayout<SIMD3<Float>>.stride,index:2)
            e.setTexture(d,index:0);e.setTexture(result,index:1);e.setTexture(color,index:2);e.setTexture(filterResult,index:3);e.dispatchThreads(MTLSize(width:1,height:1,depth:1),threadsPerThreadgroup:MTLSize(width:1,height:1,depth:1));e.endEncoding()
            cmd.commit();cmd.waitUntilCompleted();require(cmd.status == .completed,"Direct production helper completes")
            var resultValues=[Float](repeating:0,count:4);resultValues.withUnsafeMutableBytes{result.getBytes($0.baseAddress!,bytesPerRow:16,from:MTLRegionMake2D(0,0,1,1),mipmapLevel:0)}
            var filtered=[Float](repeating:0,count:4);filtered.withUnsafeMutableBytes{filterResult.getBytes($0.baseAddress!,bytesPerRow:16,from:MTLRegionMake2D(0,0,1,1),mipmapLevel:0)}
            if let expected {require((resultValues[3]==1)==expected,"Production certificate boundary: "+name)}
            require(filtered[0]==0 && filtered[1]==0 && filtered[2]==0,"No filtered sentinel tap outside the certified3x3 source neighborhood")
            boundaryRows.append(["name":name,"qualified":resultValues[3]==1,"expected":expected.map{$0 as Any} ?? NSNull(),"inverse_distance":resultValues[2],"outside_tap_radiance":Array(filtered.prefix(3))])
            print("PASS boundary \(name): qualified=\(resultValues[3]==1), no outside taps")
        }
        let size=SIMD2(5,5),flat=[Float](repeating:0.125,count:25)
        boundary("left-top-clamped-patch",size,flat,SIMD2(0,0),SIMD2(0.25,0.25),expected:true)
        boundary("right-bottom-clamped-patch",size,flat,SIMD2(4,4),SIMD2(3.75,3.75),expected:true)
        boundary("upper-half-complete-footprint",size,flat,SIMD2(1,1),SIMD2(1.75,1.75),expected:true)
        boundary("outside-left-complete-footprint",size,flat,SIMD2(0,0),SIMD2(-0.25,0.5),expected:false)
        boundary("outside-right-complete-footprint",size,flat,SIMD2(4,4),SIMD2(4.25,3.5),expected:false)
        boundary("outside-local-patch",size,flat,SIMD2(1,1),SIMD2(2.25,1.25),expected:false)
        boundary("search-horizon-refusal",size,flat,SIMD2(2,2),SIMD2(2,2),maximum:0.5,expected:false)
        var four=flat;four[1*5+1]=Float(bitPattern:Float(0.125).bitPattern+4)
        var five=flat;five[1*5+1]=Float(bitPattern:Float(0.125).bitPattern+5)
        boundary("four-ulp-center-residual",size,four,SIMD2(0,0),SIMD2(0.75,0.75),expected:true)
        boundary("five-ulp-center-refusal",size,five,SIMD2(0,0),SIMD2(0.75,0.75),expected:false)
        var step=flat;for y in 0..<5{for x in 1..<5{step[y*5+x]=0.25}}
        boundary("two-column-depth-step-refusal",size,step,SIMD2(0,0),SIMD2(0.75,0.75),expected:false)
        var steep=flat;for y in 0..<5{for x in 0..<5{steep[y*5+x]=0.125+Float(x)/64-Float(y)/128}}
        boundary("steep-affine-plane",size,steep,SIMD2(2,2),SIMD2(2.25,2.25),expected:true)
        var grazing=matrix_identity_float4x4;grazing[3].z=0.1/(0.125+0.0001)
        boundary("grazing-plane-at-current-eye",size,flat,SIMD2(2,2),SIMD2(2,2),relative:grazing,expected:false)
        var farValues=[Float](repeating:0,count:32*24)
        let farCenter:Float=1.0101013e-6,farDX:Float=1.0101013e-8
        for y in 0..<24{for x in 0..<32{farValues[y*32+x]=farCenter+farDX*(Float(x)-15.5)}}
        var farRelative=matrix_identity_float4x4;farRelative[3].z = -10
        boundary("finite-old-depth-to-exact-far-refusal",SIMD2(32,24),farValues,SIMD2(16,12),SIMD2(15.5,11.5),relative:farRelative,expected:false)
        for y in 0..<24{for x in 0..<32{farValues[y*32+x]=farCenter-farDX*(Float(x)-16)}}
        boundary("old-zero-depth-no-finite-promotion",SIMD2(32,24),farValues,SIMD2(16,12),SIMD2(15.5,11.5),relative:farRelative,expected:false)
        for y in 0..<24{for x in 0..<32{farValues[y*32+x]=farCenter+farDX/4-farDX*(Float(x)-15.5)}}
        boundary("old-out-of-range-no-finite-promotion",SIMD2(32,24),farValues,SIMD2(16,12),SIMD2(15.5,11.5),relative:farRelative,expected:false)
        for (wideWidth,baseX) in [(31,15),(1888,15),(1920,31),(2560,63)] {
            let dimensions=SIMD2(wideWidth,5),values=[Float](repeating:0.125,count:wideWidth*5)
            for sampleX in [Float(baseX).nextDown,Float(baseX),Float(baseX).nextUp,Float(baseX+2).nextDown] {
                for hardware in [false,true] {
                    boundary("width\(wideWidth)-phase\(sampleX)-hardware\(hardware)",dimensions,values,SIMD2(UInt32(baseX+1),2),SIMD2(sampleX,2),expected:nil,hardware:hardware)
                }
            }
        }
        try JSONSerialization.data(withJSONObject:["schema":1,"device":device.name,"checks":checks,"case_count":cases.count,"rows":rows,"boundary_cases":boundaryRows,"scope":"Synthetic artificial owner0 masks; independent analytic source plane plus high frequency finished image. Not real scene coverage or hardware acceptance."],options:[.prettyPrinted,.sortedKeys]).write(to:output,options:.withoutOverwriting)
        print("PASS \(checks) checks")
    }
}

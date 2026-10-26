// Offscreen Mac Metal verification of the production vertical reconstruction.
import Foundation
import Metal
import simd
import Darwin

func require(_ value: @autoclosure () -> Bool, _ message: String) {
    if !value() { fatalError(message) }
}
let root = URL(fileURLWithPath: CommandLine.arguments[1])
let output = URL(fileURLWithPath: CommandLine.arguments[2])
let baselineURL = URL(fileURLWithPath: CommandLine.arguments[3])
let mode = CommandLine.arguments.count > 4 ? CommandLine.arguments[4] : ""
let device = MTLCreateSystemDefaultDevice()!
let queue = device.makeCommandQueue()!
let original = try String(contentsOf: baselineURL, encoding: .utf8)
let production = try String(contentsOf: root.appendingPathComponent("visionos/Sources/GameWorld.metal"), encoding: .utf8)
let diagnostic = """
struct AuditMapping { float2 origin; float2 dx; float2 dy; uint enabled; uint background; };
vertex float4 auditVertex(uint id [[vertex_id]]) {
    const float2 p[3] = {float2(-1,-1),float2(3,-1),float2(-1,3)};
    return float4(p[id],0,1);
}
fragment float4 auditSample(float4 p [[position]], constant GameWorldUniforms &u [[buffer(0)]],
        constant AuditMapping &m [[buffer(1)]], texture2d<float> color [[texture(0)]],
        texture2d<float,access::read> depth [[texture(1)]]) {
    const float2 texel = m.origin + (p.x-.5)*m.dx + (p.y-.5)*m.dy;
    const float2 uv = (texel+.5)/float2(u.sourceSize);
    const float2 dx = dfdx(uv)*float2(u.sourceSize), dy = dfdy(uv)*float2(u.sourceSize);
    GameWorldVerticalSample v = m.enabled != 0 ? gameWorldVerticalSample(uv,dx,dy,m.background,u,color,depth)
        : GameWorldVerticalSample{gameWorldSampleLinearSRGB(color,uv,u.colorTransfer),0};
    return float4(gameWorldLinearSRGBToP3(v.rgb),float(v.eligible));
}
fragment GameWorldSkyOwnershipOut auditWorldEligibility(GameWorldVertex in [[stage_in]],
        constant GameWorldUniforms &u [[buffer(0)]],texture2d<float> color [[texture(0)]],
        texture2d<float,access::read> depth [[texture(1)]]) {
    const float2 uv=in.sourceUVQ.xy/in.sourceUVQ.z;
    const float2 dx=dfdx(uv)*float2(u.sourceSize),dy=dfdy(uv)*float2(u.sourceSize);
    int2 base;float phase;
    const bool ok=gameWorldVerticalSupport(uv,dx,dy,in.background,u,depth,base,phase);
    GameWorldSkyOwnershipOut o;o.color=float4(float(ok),0,0,1);o.owner=in.background!=0?1u:2u;return o;
}
"""
let library = try device.makeLibrary(source: production + diagnostic, options: nil)
let oldLibrary = try device.makeLibrary(source: original, options: nil)
// Exercise missing-optional-function fallback using current color semantics.
// The frozen historical library remains an independent geometry oracle only.
let missingOptionalLibrary = try device.makeLibrary(source: production
    .replacingOccurrences(of: "gameWorldSkyOwnershipVerticalReconstructionFragment", with: "qaUnavailableVerticalFragment"), options: nil)
require(MemoryLayout<GameWorldUniforms>.stride == 224, "Production world uniform ABI changed")

func texture(_ format: MTLPixelFormat, _ w: Int, _ h: Int, _ usage: MTLTextureUsage) -> any MTLTexture {
    let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: w, height: h, mipmapped: false)
    d.storageMode = .shared; d.usage = usage
    return device.makeTexture(descriptor: d)!
}
func sRGB(_ f: MTLPixelFormat) -> Bool { f == .rgba8Unorm_srgb || f == .bgra8Unorm_srgb }
func bgra(_ f: MTLPixelFormat) -> Bool { f == .bgra8Unorm || f == .bgra8Unorm_srgb }
func transfer(_ source: MTLPixelFormat, _ destination: MTLPixelFormat) -> UInt32 {
    // Source-only contract: sampling supplies linear sRGB; all targets receive
    // linear Display P3, and an sRGB attachment encodes only the stored values.
    sRGB(source) ? 0 : 2
}
func decode(_ v:Double)->Double { v<=0.04045 ? v/12.92:pow((v+0.055)/1.055,2.4) }
func encode(_ v:Double)->Double { v<=0.0031308 ? 12.92*v:1.055*pow(max(v,0),1/2.4)-0.055 }
func projection(focal: SIMD2<Float> = SIMD2(0.5341740847,0.665856123), reversed: Bool = true) -> simd_float4x4 {
    let n: Float = 0.05, f: Float = 10000
    return simd_float4x4(columns: (SIMD4(focal.x,0,0,0),SIMD4(0,focal.y,0,0),
        SIMD4(0,0,reversed ? n/(f-n) : -f/(f-n),-1),SIMD4(0,0,reversed ? n*f/(f-n) : -n*f/(f-n),0)))
}
func z(_ distance: Float, reversed: Bool = true) -> Float {
    let p = projection(reversed: reversed) * SIMD4<Float>(0,0,-distance,1)
    return p.z/p.w
}
struct Source {
    let color: any MTLTexture
    let depth: any MTLTexture
    let width: Int
    let height: Int
    let rgba: [UInt8]
}
func makeSource(_ name: String, width: Int = 256, height: Int = 256,
                format: MTLPixelFormat = .rgba8Unorm, depthName: String = "plane", reversed: Bool = true) -> Source {
    var bytes = [UInt8](repeating: 255,count: width*height*4)
    var depths = [Float](repeating: z(2,reversed: reversed),count: width*height)
    for y in 0..<height { for x in 0..<width {
        let i=y*width+x
        let value: Float
        switch name {
        case "constant": value=0.37
        case "ramp": value=Float(y)/Float(height-1)
        case "horizontal": value=Float(x)/Float(width-1)
        case "edge": value=y < height/2 ? 0.1:0.9
        case "alternating": value=y & 1 == 0 ? 0.9:0.1
        case "sine": value=0.5+0.35*sin(2*Float.pi*0.18*Float(y))
        case "impulse": value=y == height/2 ? 0.9:0.1
        default: value=Float((x*7+y*13)&255)/255
        }
        // Analytic fixtures describe linear light; real guest storage is sRGB.
        let v=UInt8(min(255,max(0,(encode(Double(value))*255).rounded())))
        bytes[i*4]=v;bytes[i*4+1]=v;bytes[i*4+2]=v
        if depthName == "step" { depths[i]=z(y < height/2 ? 0.2:8,reversed: reversed) }
        if depthName == "vertical_shallow" { depths[i]=z(y < height/2 ? 1:1.005,reversed: reversed) }
        if depthName == "horizontal_step" { depths[i]=z(x < width/2 ? 0.2:8,reversed: reversed) }
        if depthName == "horizontal_shallow" { depths[i]=z(x < width/2 ? 1:1.005,reversed: reversed) }
        if depthName == "slender" { depths[i]=z(y == height/2 ? 0.7:3,reversed: reversed) }
        if depthName == "horizontal_slender" { depths[i]=z(x == width/2 ? 0.7:3,reversed: reversed) }
        if depthName == "sky" { depths[i]=reversed ? 0:1 }
        if depthName == "invalid" { depths[i] = .nan }
        if depthName == "affine" { depths[i]=z(2,reversed:reversed)+Float(x)*0.00001+Float(y)*0.000005 }
    }}
    let color=texture(format,width,height,.shaderRead),depth=texture(.r32Float,width,height,.shaderRead)
    bytes.withUnsafeBytes { color.replace(region:MTLRegionMake2D(0,0,width,height),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:width*4) }
    depths.withUnsafeBytes { depth.replace(region:MTLRegionMake2D(0,0,width,height),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:width*4) }
    return Source(color:color,depth:depth,width:width,height:height,rgba:bytes)
}
struct Mapping { var origin: SIMD2<Float>;var dx: SIMD2<Float>;var dy: SIMD2<Float>;var enabled: UInt32;var background: UInt32 }
var auditPipelines=[UInt:any MTLRenderPipelineState]()
func audit(_ source: Source, destination: MTLPixelFormat = .rgba32Float, mapping: Mapping,
           width: Int = 64, height: Int = 128, reversed: Bool = true) throws -> [Float] {
    let pipeline: any MTLRenderPipelineState
    if let cached=auditPipelines[destination.rawValue] { pipeline=cached }
    else {
        let d=MTLRenderPipelineDescriptor();d.vertexFunction=library.makeFunction(name:"auditVertex")!
        d.fragmentFunction=library.makeFunction(name:"auditSample")!;d.colorAttachments[0].pixelFormat=destination
        pipeline=try device.makeRenderPipelineState(descriptor:d);auditPipelines[destination.rawValue]=pipeline
    }
    let target=texture(destination,width,height,[.renderTarget,.shaderRead])
    let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target
    pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
    let commands=queue.makeCommandBuffer()!,encoder=commands.makeRenderCommandEncoder(descriptor:pass)!
    var uniforms=GameWorldUniforms(sourceEyeFromClip:projection(reversed:reversed).inverse,
        currentEyeFromSourceEye:matrix_identity_float4x4,currentClipFromEye:projection(),sourceSize:SIMD2(UInt32(source.width),UInt32(source.height)))
    uniforms.sourceDepthReversed=reversed ? 1:0
    uniforms.colorTransfer=transfer(source.color.pixelFormat,destination)
    var m=mapping
    encoder.setRenderPipelineState(pipeline);encoder.setFragmentBytes(&uniforms,length:MemoryLayout<GameWorldUniforms>.stride,index:0)
    encoder.setFragmentBytes(&m,length:MemoryLayout<Mapping>.stride,index:1)
    encoder.setFragmentTexture(source.color,index:0);encoder.setFragmentTexture(source.depth,index:1)
    encoder.drawPrimitives(type:.triangle,vertexStart:0,vertexCount:3);encoder.endEncoding()
    commands.commit();commands.waitUntilCompleted();require(commands.status == .completed,"Audit Metal command failed")
    if destination == .rgba32Float {
        var result=[Float](repeating:0,count:width*height*4)
        result.withUnsafeMutableBytes { target.getBytes($0.baseAddress!,bytesPerRow:width*16,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
        return result
    }
    var result=[UInt8](repeating:0,count:width*height*4)
    result.withUnsafeMutableBytes { target.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
    return result.map { Float($0)/255 }
}
func mapping(_ phase: Float = 0, enabled: Bool = true, dx: Float = 1.04, dy: Float = 0.617) -> Mapping {
    Mapping(origin:SIMD2(64,64+phase),dx:SIMD2(dx,0),dy:SIMD2(0,dy),enabled:enabled ? 1:0,background:0)
}
func rgbEqual(_ a:[Float],_ b:[Float],tolerance:Float=0)->Bool {
    stride(from:0,to:a.count,by:4).allSatisfy { i in (0..<3).allSatisfy { abs(a[i+$0]-b[i+$0])<=tolerance } }
}

// Synthetic shader checks. Alpha is a diagnostic eligibility bit only.
var checks=[[String:Any]]()
let constant=makeSource("constant")
let native=try audit(constant,mapping:mapping())
let eligible=stride(from:3,to:native.count,by:4).filter { native[$0]>0.5 }.count
require(eligible==64*128,"Native horizontal-minification/vertical-enlargement case is dormant")
checks.append(["name":"native_density_eligible","horizontal_source_pixels_per_output":1.04,
    "vertical_rows_per_output":0.617,"eligible":eligible,"pixels":64*128])
for name in ["constant","ramp","horizontal","edge","alternating","sine","impulse"] {
    let source=makeSource(name)
    let a=try audit(source,mapping:mapping(0.375,enabled:false))
    let b=try audit(source,mapping:mapping(0.375))
    require(stride(from:0,to:b.count,by:4).allSatisfy { b[$0]>=0 && b[$0]<=1 },"\(name): out-of-range ringing")
    if name == "constant" || name == "horizontal" { require(rgbEqual(a,b,tolerance:2e-7),"Constant/horizontal filtering changed") }
    // Central two rows are the exact local range clamp, including impulses.
    for y in 0..<128 {
        let p=64+Float(y)*0.617+0.375,low=Int(floor(p))
        if name != "horizontal" {
            let lo=Float(decode(Double(min(source.rgba[low*256*4],source.rgba[(low+1)*256*4]))/255))
            let hi=Float(decode(Double(max(source.rgba[low*256*4],source.rgba[(low+1)*256*4]))/255))
            for x in 0..<64 { let v=b[(y*64+x)*4];require(v>=lo-2e-6 && v<=hi+2e-6,"\(name): local central-row range exceeded") }
        }
    }
    let integerA=try audit(source,mapping:mapping(0,enabled:false,dx:1,dy:1),width:64,height:64)
    let integerB=try audit(source,mapping:mapping(0,dx:1,dy:1),width:64,height:64)
    require(rgbEqual(integerA,integerB,tolerance:2e-7),"\(name): integer center changed")
    checks.append(["name":name,"constant_integer_centers_and_local_range_passed":true])
}
for reversed in [false,true] {
    let sloped=try audit(makeSource("sine",depthName:"affine",reversed:reversed),mapping:mapping(),reversed:reversed)
    require(stride(from:3,to:sloped.count,by:4).allSatisfy { sloped[$0]==1 },"NDC affine plane rejected")
    checks.append(["name":"sloped_ndc_affine","source_reversed":reversed,"all_pixels_eligible":true])
    for kind in ["sky","invalid","step","vertical_shallow","horizontal_step","horizontal_shallow","slender","horizontal_slender"] {
        let source=makeSource("sine",depthName:kind,reversed:reversed)
        let a=try audit(source,mapping:mapping(0.37,enabled:false),reversed:reversed)
        let b=try audit(source,mapping:mapping(0.37),reversed:reversed)
        var checked=0
        for y in 0..<128 { for x in 0..<64 {
            let row=Int(floor(64+Float(y)*0.617+0.37)),i=(y*64+x)*4
            let column=Int(floor(64+Float(x)*1.04))
            let isHorizontal=kind.hasPrefix("horizontal")
            let first=isHorizontal ? column-1:row-1,last=isHorizontal ? column+1:row+2
            let crosses=kind == "sky" || kind == "invalid"
                || (kind.contains("slender") ? (first<=128 && last>=128):(first<128 && last>=128))
            if crosses { require(b[i+3]==0 && (0..<3).allSatisfy { a[i+$0]==b[i+$0] },"\(kind): unsafe support did not fall back exactly");checked+=1 }
        }}
        require(checked>0,"\(kind): empty fallback assertion")
        checks.append(["name":"\(kind)_fallback","source_reversed":reversed,"exact_fallback_pixels":checked])
    }
}
for kind in ["vertical_minification","border","background","degenerate"] {
    var m=mapping(0.37)
    if kind == "vertical_minification" { m.dy.y=1.1 }
    if kind == "border" { m.origin.y=0.2;m.dy.y=0.001 }
    if kind == "background" { m.background=1 }
    if kind == "degenerate" { m.dy = .zero }
    var old=m;old.enabled=0
    let a=try audit(makeSource("sine"),mapping:old),b=try audit(makeSource("sine"),mapping:m)
    require(rgbEqual(a,b) && stride(from:3,to:b.count,by:4).allSatisfy { b[$0]==0 },"\(kind): fallback changed")
    checks.append(["name":kind,"baseline_rgb_exact":true])
}
let formats:[MTLPixelFormat]=[.rgba8Unorm,.rgba8Unorm_srgb,.bgra8Unorm,.bgra8Unorm_srgb]
for sf in formats { for df in formats {
    let source=makeSource("constant",format:sf)
    let a=try audit(source,destination:df,mapping:mapping(0.375,enabled:false))
    let b=try audit(source,destination:df,mapping:mapping(0.375))
    require(rgbEqual(a,b),"Format/transfer constant changed \(sf.rawValue)->\(df.rawValue)")
    let centersA=try audit(makeSource("sine",format:sf),destination:df,mapping:mapping(0,enabled:false,dx:1,dy:1),width:64,height:64)
    let centersB=try audit(makeSource("sine",format:sf),destination:df,mapping:mapping(0,dx:1,dy:1),width:64,height:64)
    require(rgbEqual(centersA,centersB),"Format/transfer integer centers changed")
    let sine=makeSource("sine",format:sf)
    let fractional=try audit(sine,destination:df,mapping:mapping(0.375))
    var maximumReferenceError=0.0
    for y in 0..<128 {
        let position=64+Float(y)*0.617+0.375,base=Int(floor(position)),t=Double(position-Float(base))
        let t2=t*t,t3=t2*t
        let weights=[-0.5*t+t2-0.5*t3,1-2.5*t2+1.5*t3,0.5*t+2*t2-1.5*t3,-0.5*t2+0.5*t3]
        let samples=(0..<4).map { i->Double in decode(Double(sine.rgba[(base+i-1)*256*4])/255) }
        var expected=zip(samples,weights).reduce(0.0) { $0+$1.0*$1.1 }
        expected=min(max(expected,min(samples[1],samples[2])),max(samples[1],samples[2]))
        // Neutral RGB is invariant under the sRGB-to-P3 primary conversion.
        if sRGB(df) { expected=encode(expected) }
        for x in 0..<64 { maximumReferenceError=max(maximumReferenceError,abs(Double(fractional[(y*64+x)*4])-expected)) }
    }
    require(maximumReferenceError<=1.01/255,"Format/transfer fractional reference differs by more than one stored channel")
    checks.append(["name":"format_transfer","source_format":sf.rawValue,"destination_format":df.rawValue,
        "transfer":transfer(sf,df),"constant_and_integer_color_exact":true,"maximum_fractional_reference_error":maximumReferenceError])
}}
for name in ["sine","alternating"] {
    let source=makeSource(name)
    var oldErrors=[Double](),newErrors=[Double](),oldContrast=0.0,newContrast=0.0
    for phaseIndex in 0..<32 {
        let phase=Float(phaseIndex)/32
        let a=try audit(source,mapping:mapping(phase,enabled:false)),b=try audit(source,mapping:mapping(phase))
        var oldSquared=0.0,newSquared=0.0
        for y in 0..<128 {
            let position=64+phase+Float(y)*0.617
            let truth:Float=name == "sine" ? 0.5+0.35*sin(2*Float.pi*0.18*position) : 0.5+0.4*cos(Float.pi*position)
            let av=a[(y*64+32)*4],bv=b[(y*64+32)*4]
            oldSquared+=Double((av-truth)*(av-truth));newSquared+=Double((bv-truth)*(bv-truth))
            oldContrast+=abs(Double(av)-0.5);newContrast+=abs(Double(bv)-0.5)
        }
        oldErrors.append(sqrt(oldSquared/128));newErrors.append(sqrt(newSquared/128))
    }
    let oldRMS=oldErrors.reduce(0,+)/32,newRMS=newErrors.reduce(0,+)/32
    require(newRMS < oldRMS,"\(name): no analytic source-phase reconstruction improvement")
    require(newContrast>oldContrast,"\(name): no vertical contrast improvement")
    checks.append(["name":"\(name)_32_source_phases","bilinear_mean_phase_rmse":oldRMS,
        "candidate_mean_phase_rmse":newRMS,"bilinear_phase_rmse":oldErrors,"candidate_phase_rmse":newErrors,
        "mean_absolute_contrast_ratio":newContrast/oldContrast])
}
try JSONSerialization.data(withJSONObject:["passed":true,"device":device.name,"scope":"Synthetic actual Mac Metal production sampling helper; no physical acceptance","checks":checks],options:[.prettyPrinted,.sortedKeys])
    .write(to:output.appendingPathComponent("sampling-result.json"))
print("PASS \(checks.count) actual GPU sampling checks; native-density eligible \(eligible)/8192; phase error and vertical contrast improved.")

struct GeometryResult { let color:[UInt8];let depth:[Float];let owner:[UInt8];let gpuMS:Double }
var geometryPipelines=[String:any MTLRenderPipelineState]()
let depthDescriptor=MTLDepthStencilDescriptor();depthDescriptor.depthCompareFunction = .greaterEqual;depthDescriptor.isDepthWriteEnabled=true
let geometryDepthState=device.makeDepthStencilState(descriptor:depthDescriptor)!
func geometry(_ source:Source,uniforms:GameWorldUniforms,width:Int,height:Int,kind:String,
              format:MTLPixelFormat = .rgba8Unorm_srgb,ownership:Bool=true)throws->GeometryResult {
    let key="\(kind)-\(format.rawValue)-\(ownership)"
    let pipeline:any MTLRenderPipelineState
    if let cached=geometryPipelines[key] { pipeline=cached }
    else {
        let l=kind == "frozen" ? oldLibrary:library
        let d=MTLRenderPipelineDescriptor();d.vertexFunction=l.makeFunction(name:"gameWorldVertex")!
        let name:String
        if kind == "eligibility" { name="auditWorldEligibility" }
        else if kind == "vertical" { name=ownership ? "gameWorldSkyOwnershipVerticalReconstructionFragment":"gameWorldVerticalReconstructionFragment" }
        else { name=ownership ? "gameWorldSkyOwnershipFragment":"gameWorldFragment" }
        d.fragmentFunction=l.makeFunction(name:name)!;d.colorAttachments[0].pixelFormat=format
        if ownership { d.colorAttachments[1].pixelFormat = .r8Uint }
        d.depthAttachmentPixelFormat = .depth32Float
        pipeline=try device.makeRenderPipelineState(descriptor:d);geometryPipelines[key]=pipeline
    }
    let target=texture(format,width,height,[.renderTarget,.shaderRead])
    let depth=texture(.depth32Float,width,height,[.renderTarget,.shaderRead])
    let owner=ownership ? texture(.r8Uint,width,height,[.renderTarget,.shaderRead]):nil
    let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target
    pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor=MTLClearColorMake(0,0,0,1)
    if let owner { pass.colorAttachments[1].texture=owner;pass.colorAttachments[1].loadAction = .clear;pass.colorAttachments[1].storeAction = .store }
    pass.depthAttachment.texture=depth;pass.depthAttachment.loadAction = .clear;pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
    let commands=queue.makeCommandBuffer()!,encoder=commands.makeRenderCommandEncoder(descriptor:pass)!
    encoder.setRenderPipelineState(pipeline);encoder.setDepthStencilState(geometryDepthState);encoder.setCullMode(.none)
    encoder.setViewport(MTLViewport(originX:0,originY:0,width:Double(width),height:Double(height),znear:0,zfar:1))
    var u=uniforms;u.colorTransfer=transfer(source.color.pixelFormat,format)
    encoder.setVertexBytes(&u,length:MemoryLayout<GameWorldUniforms>.stride,index:0)
    encoder.setFragmentBytes(&u,length:MemoryLayout<GameWorldUniforms>.stride,index:0)
    encoder.setVertexTexture(source.depth,index:0);encoder.setFragmentTexture(source.color,index:0)
    if kind == "vertical" || kind == "eligibility" { encoder.setFragmentTexture(source.depth,index:1) }
    encoder.drawPrimitives(type:.triangleStrip,vertexStart:0,vertexCount:4,instanceCount:source.width*source.height)
    encoder.endEncoding();commands.commit();commands.waitUntilCompleted()
    require(commands.status == .completed,"Geometry command failed: \(String(describing:commands.error))")
    var color=[UInt8](repeating:0,count:width*height*4),z=[Float](repeating:0,count:width*height),owners=[UInt8](repeating:0,count:ownership ? width*height:0)
    color.withUnsafeMutableBytes { target.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
    z.withUnsafeMutableBytes { depth.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
    if let owner { owners.withUnsafeMutableBytes { owner.getBytes($0.baseAddress!,bytesPerRow:width,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) } }
    return GeometryResult(color:color,depth:z,owner:owners,gpuMS:(commands.gpuEndTime-commands.gpuStartTime)*1000)
}
func sameGeometry(_ a:GeometryResult,_ b:GeometryResult)->Bool {
    a.owner==b.owner && zip(a.depth,b.depth).allSatisfy { $0.bitPattern==$1.bitPattern }
}
func worldUniforms(_ source:Source,reversed:Bool=true)->GameWorldUniforms {
    var pose=simd_float4x4(simd_quatf(angle:0.025,axis:SIMD3(0,1,0)));pose.columns.3=SIMD4(0.03,-0.01,0,1)
    var u=GameWorldUniforms(sourceEyeFromClip:projection(reversed:reversed).inverse,
        currentEyeFromSourceEye:pose.inverse,currentClipFromEye:projection(focal:SIMD2(0.7320510745,0.9125128984)),sourceSize:SIMD2(UInt32(source.width),UInt32(source.height)))
    u.sourceDepthReversed=reversed ? 1:0
    return u
}
var geometryChecks=[[String:Any]]()
for reversed in [false,true] {
    let source=makeSource("sine",width:192,height:160,reversed:reversed)
    let u=worldUniforms(source,reversed:reversed)
    let old=try geometry(source,uniforms:u,width:256,height:256,kind:"frozen")
    let baseline=try geometry(source,uniforms:u,width:256,height:256,kind:"baseline")
    let sharp=try geometry(source,uniforms:u,width:256,height:256,kind:"vertical")
    let normal=try geometry(source,uniforms:u,width:256,height:256,kind:"vertical",ownership:false)
    require(sameGeometry(old,baseline),"Historical geometry/depth/owner changed")
    require(sameGeometry(baseline,sharp),"Vertical reconstruction changed geometry/depth/owner")
    require(normal.color==sharp.color && zip(normal.depth,sharp.depth).allSatisfy { $0.bitPattern==$1.bitPattern },"Normal/MRT vertical fragments disagree")
    geometryChecks.append(["source_reversed":reversed,"frozen_original_geometry_exact":true,
        "historical_color_equality_required":false,"historical_color_note":"Color intentionally changed to before-filter linear sampling and Display P3; historical shader remains geometry oracle.",
        "geometry_depth_owner_exact":true,"normal_and_ownership_fragments_exact":true])
}

// Compile and call the production Swift encoder too. Its private ownership
// texture is copied before completion releases the lease. No replacement PSO
// or test-only implementation substitutes for production selection/binding.
func finiteSkyGeometry(_ helper:GameWorldFiniteSkySupport,_ source:Source,
                       uniforms:GameWorldUniforms,allow:Bool,expectPrecompute:Bool)->(GeometryResult,Bool) {
    let width=256,height=256
    let target=texture(.rgba8Unorm_srgb,width,height,[.renderTarget,.shaderRead])
    let depth=texture(.depth32Float,width,height,[.renderTarget,.shaderRead])
    let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target
    pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
    pass.colorAttachments[0].clearColor=MTLClearColorMake(0,0,0,1)
    pass.depthAttachment.texture=depth;pass.depthAttachment.loadAction = .clear
    pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
    let commands=queue.makeCommandBuffer()!
    var u=uniforms;u.colorTransfer=transfer(source.color.pixelFormat,target.pixelFormat)
    let result=helper.encode(commands:commands,pass:pass,
        viewport:MTLViewport(originX:0,originY:0,width:Double(width),height:Double(height),znear:0,zfar:1),
        projection:u.currentClipFromEye,far:10000,uniforms:u,color:source.color,depth:source.depth,
        allowVerticalReconstruction:allow)!
    require(result.cornersPrecomputed==expectPrecompute,"Production helper did not exercise requested vertex path")
    let rowBytes=(width+255) & ~255
    let buffer=device.makeBuffer(length:rowBytes*height,options:.storageModeShared)!
    let blit=commands.makeBlitCommandEncoder()!
    blit.copy(from:result.ownership,sourceSlice:0,sourceLevel:0,sourceOrigin:MTLOrigin(x:0,y:0,z:0),
        sourceSize:MTLSize(width:width,height:height,depth:1),to:buffer,destinationOffset:0,
        destinationBytesPerRow:rowBytes,destinationBytesPerImage:rowBytes*height)
    blit.endEncoding();commands.commit();commands.waitUntilCompleted()
    require(commands.status == .completed,"Production finite sky command failed: \(String(describing:commands.error))")
    var color=[UInt8](repeating:0,count:width*height*4),z=[Float](repeating:0,count:width*height)
    color.withUnsafeMutableBytes { target.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
    z.withUnsafeMutableBytes { depth.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0) }
    let bytes=buffer.contents().bindMemory(to:UInt8.self,capacity:rowBytes*height)
    var owner=[UInt8](repeating:0,count:width*height)
    for y in 0..<height { for x in 0..<width { owner[y*width+x]=bytes[y*rowBytes+x] } }
    return (GeometryResult(color:color,depth:z,owner:owner,gpuMS:(commands.gpuEndTime-commands.gpuStartTime)*1000),result.verticalReconstruction)
}
for precompute in [false,true] {
    let optional=GameWorldFiniteSkySupport(device:device,library:library,colorFormat:.rgba8Unorm_srgb,
        depthFormat:.depth32Float,precomputeCorners:precompute,verticalReconstruction:true)!
    let disabled=GameWorldFiniteSkySupport(device:device,library:library,colorFormat:.rgba8Unorm_srgb,
        depthFormat:.depth32Float,precomputeCorners:precompute,verticalReconstruction:false)!
    let missing=GameWorldFiniteSkySupport(device:device,library:missingOptionalLibrary,colorFormat:.rgba8Unorm_srgb,
        depthFormat:.depth32Float,precomputeCorners:precompute,verticalReconstruction:true)!
    for reversed in [false,true] {
        let source=makeSource("sine",width:192,height:160,reversed:reversed)
        let u=worldUniforms(source,reversed:reversed)
        let (base,baseSelected)=finiteSkyGeometry(optional,source,uniforms:u,allow:false,expectPrecompute:precompute)
        let (sharp,sharpSelected)=finiteSkyGeometry(optional,source,uniforms:u,allow:true,expectPrecompute:precompute)
        let (off,offSelected)=finiteSkyGeometry(disabled,source,uniforms:u,allow:true,expectPrecompute:precompute)
        let (fallback,fallbackSelected)=finiteSkyGeometry(missing,source,uniforms:u,allow:true,expectPrecompute:precompute)
        let directBase=try geometry(source,uniforms:u,width:256,height:256,kind:"baseline")
        let directSharp=try geometry(source,uniforms:u,width:256,height:256,kind:"vertical")
        require(!baseSelected && sharpSelected && !offSelected && !fallbackSelected,"Production finite sky selection/fallback report wrong")
        require(base.color==directBase.color && sameGeometry(base,directBase),"Production allow=false changed baseline")
        require(sharp.color==directSharp.color && sameGeometry(sharp,directSharp),"Production allow=true binding differs from actual entry point")
        require(sameGeometry(base,sharp),"Production vertical selection changed depth/owner")
        require(off.color==base.color && sameGeometry(off,base),"Production disabled-init fallback differs")
        require(fallback.color==base.color && sameGeometry(fallback,base),"Production missing-new-fragment fallback differs")
        require(sharp.color != base.color,"Production helper fixture did not exercise reconstruction")
        geometryChecks.append(["production_finite_sky_encoder":true,"source_reversed":reversed,"precomputed_vertices":precompute,
            "allow_false_baseline_exact":true,"allow_true_actual_entry_exact":true,"selection_report_correct":true,
            "unavailable_optional_pipeline_baseline_exact":true,"depth_owner_exact":true])
    }
}
if mode == "--timing" {
    func median(_ a:[Double])->Double { let s=a.sorted();return (s[(s.count-1)/2]+s[s.count/2])*0.5 }
    var timing=[[String:Any]]()
    for reversed in [false,true] {
        let source=makeSource("sine",width:2560,height:1440,reversed:reversed)
        let u=worldUniforms(source,reversed:reversed)
        let base=try geometry(source,uniforms:u,width:1888,height:1792,kind:"baseline")
        let sharp=try geometry(source,uniforms:u,width:1888,height:1792,kind:"vertical")
        let mask=try geometry(source,uniforms:u,width:1888,height:1792,kind:"eligibility")
        require(sameGeometry(base,sharp),"Native-size geometry changed")
        let covered=base.owner.filter { $0 != 0 }.count
        let qualified=stride(from:0,to:mask.color.count,by:4).filter { mask.color[$0]>127 }.count
        require(qualified>covered*9/10,"Native-size shader unexpectedly dormant")
        for i in 0..<4 {
            _=try geometry(source,uniforms:u,width:1888,height:1792,kind:i%2==0 ? "baseline":"vertical")
            _=try geometry(source,uniforms:u,width:1888,height:1792,kind:i%2==0 ? "vertical":"baseline")
        }
        var oldTimes=[Double](),newTimes=[Double]()
        for i in 0..<12 {
            let a:GeometryResult,b:GeometryResult
            if i%2==0 { a=try geometry(source,uniforms:u,width:1888,height:1792,kind:"baseline");b=try geometry(source,uniforms:u,width:1888,height:1792,kind:"vertical") }
            else { b=try geometry(source,uniforms:u,width:1888,height:1792,kind:"vertical");a=try geometry(source,uniforms:u,width:1888,height:1792,kind:"baseline") }
            oldTimes.append(a.gpuMS);newTimes.append(b.gpuMS)
        }
        timing.append(["source_reversed":reversed,"covered_pixels":covered,"eligible_pixels":qualified,
            "eligible_fraction":Double(qualified)/Double(covered),"baseline_gpu_ms":oldTimes,"candidate_gpu_ms":newTimes,
            "baseline_median_ms":median(oldTimes),"candidate_median_ms":median(newTimes),
            "median_paired_delta_ms":median(zip(oldTimes,newTimes).map { $1-$0 })])
    }
    try JSONSerialization.data(withJSONObject:["passed":true,"device":device.name,"scope":"Warmed alternating actual Mac Metal mesh+sampling only; no guest/system/headset timing",
        "source_size":[2560,1440],"output_size":[1888,1792],"source_focal":[0.5341740847,0.665856123],"display_focal":[0.7320510745,0.9125128984],
        "near_far_m":[0.05,10000],"warm_pairs":4,"measured_pairs":12,"cases":timing],options:[.prettyPrinted,.sortedKeys])
        .write(to:output.appendingPathComponent("timing-result.json"))
    for c in timing { print("TIMING reverse=\(c["source_reversed"]!) eligible=\(c["eligible_fraction"]!) old=\(c["baseline_median_ms"]!) vertical=\(c["candidate_median_ms"]!) delta=\(c["median_paired_delta_ms"]!)ms") }
}
try JSONSerialization.data(withJSONObject:["passed":true,"device":device.name,"checks":geometryChecks],options:[.prettyPrinted,.sortedKeys])
    .write(to:output.appendingPathComponent("geometry-result.json"))
print("PASS actual production normal/MRT fragments: historical geometry, depth and ownership exact; current-color optional fallback exact, both Z conventions.")

if mode == "--replay" {
    let prefix=ProcessInfo.processInfo.environment["SV_REGISTERED_CAPTURE_PREFIX"] ?? root.appendingPathComponent("target/verification/device-session-20261020/candidate7/world-normal7-a6e1eec5a70a4ec084e39db09a8fcde5").path
    let metadata=try JSONSerialization.jsonObject(with:Data(contentsOf:URL(fileURLWithPath:prefix+"-report.json"))) as! [String:Any]
    func matrix(_ key:String)->simd_float4x4 {
        let v=(metadata[key] as! [NSNumber]).map(\.floatValue)
        return simd_float4x4(columns:(SIMD4(v[0],v[1],v[2],v[3]),SIMD4(v[4],v[5],v[6],v[7]),SIMD4(v[8],v[9],v[10],v[11]),SIMD4(v[12],v[13],v[14],v[15])))
    }
    let ppm=[UInt8](try Data(contentsOf:URL(fileURLWithPath:prefix+"-source.ppm")))
    var offset=0
    func token()->String {
        while ppm[offset]<=32 { offset+=1 };let a=offset
        while ppm[offset]>32 { offset+=1 };return String(bytes:ppm[a..<offset],encoding:.ascii)!
    }
    require(token()=="P6","Registered P6 source expected")
    let sw=Int(token())!,sh=Int(token())!;require(token()=="255","Registered byte source expected");offset+=1
    require(ppm.count-offset==sw*sh*3,"Registered RGB extent mismatch")
    var rgba=[UInt8](repeating:255,count:sw*sh*4)
    for i in 0..<(sw*sh) { for c in 0..<3 { rgba[i*4+c]=ppm[offset+i*3+c] } }
    let color=texture(.rgba8Unorm,sw,sh,.shaderRead),depth=texture(.r32Float,sw,sh,.shaderRead)
    rgba.withUnsafeBytes { color.replace(region:MTLRegionMake2D(0,0,sw,sh),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:sw*4) }
    let depthBytes=try Data(contentsOf:URL(fileURLWithPath:prefix+"-source-depth.f32"))
    require(depthBytes.count==sw*sh*4,"Registered depth extent mismatch")
    depthBytes.withUnsafeBytes { depth.replace(region:MTLRegionMake2D(0,0,sw,sh),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:sw*4) }
    let source=Source(color:color,depth:depth,width:sw,height:sh,rgba:rgba)
    var u=GameWorldUniforms(sourceEyeFromClip:matrix("source_eye_from_clip"),currentEyeFromSourceEye:matrix("current_eye_from_source_eye"),
        currentClipFromEye:matrix("current_clip_from_eye"),sourceSize:SIMD2(UInt32(sw),UInt32(sh)))
    u.sourceDepthReversed=1
    let w=1888,h=1792
    let old=try geometry(source,uniforms:u,width:w,height:h,kind:"frozen")
    let base=try geometry(source,uniforms:u,width:w,height:h,kind:"baseline")
    let sharp=try geometry(source,uniforms:u,width:w,height:h,kind:"vertical")
    let mask=try geometry(source,uniforms:u,width:w,height:h,kind:"eligibility")
    require(sameGeometry(base,old),"Registered historical geometry/depth/owner changed")
    require(sameGeometry(base,sharp),"Registered sampling changed geometry/depth/ownership")
    var eligible=0,changed=0,fallbackChanged=0,maxChannel=0
    var sumDifference=0
    for i in 0..<(w*h) {
        let accepted=mask.color[i*4]>127
        if accepted && base.owner[i] != 0 { eligible+=1 }
        var pixelChanged=false
        for c in 0..<3 {
            let delta=abs(Int(base.color[i*4+c])-Int(sharp.color[i*4+c]));sumDifference+=delta;maxChannel=max(maxChannel,delta)
            if delta>0 { pixelChanged=true }
        }
        if pixelChanged { changed+=1;if !accepted { fallbackChanged+=1 } }
    }
    require(fallbackChanged==0,"Registered fallback pixels changed")
    require(eligible>1000 && changed>0,"Registered reconstruction is dormant")
    func save(_ name:String,_ r:GeometryResult,x:Int=0,y:Int=0,width:Int=1888,height:Int=1792)throws {
        var image=Data("P6\n\(width) \(height)\n255\n".utf8)
        for yy in y..<(y+height) { for xx in x..<(x+width) { let i=(yy*w+xx)*4;image.append(contentsOf:r.color[i..<(i+3)]) } }
        try image.write(to:output.appendingPathComponent(name+".ppm"))
    }
    for (name,r) in [("baseline",base),("vertical",sharp)] {
        try save(name,r);try save(name+"-body-crop",r,x:1020,y:680,width:300,height:810)
        try save(name+"-world-crop",r,x:450,y:450,width:500,height:500)
    }
    var timesOld=[Double](),timesNew=[Double]()
    for i in 0..<8 {
        let a:GeometryResult,b:GeometryResult
        if i%2==0 { a=try geometry(source,uniforms:u,width:w,height:h,kind:"baseline");b=try geometry(source,uniforms:u,width:w,height:h,kind:"vertical") }
        else { b=try geometry(source,uniforms:u,width:w,height:h,kind:"vertical");a=try geometry(source,uniforms:u,width:w,height:h,kind:"baseline") }
        if i>=2 { timesOld.append(a.gpuMS);timesNew.append(b.gpuMS) }
    }
    func median(_ a:[Double])->Double { let s=a.sorted();return (s[(s.count-1)/2]+s[s.count/2])*0.5 }
    let covered=base.owner.filter { $0 != 0 }.count
    let result:[String:Any]=["passed":true,"device":device.name,"scope":"Actual historical registered candidate7 third-person texture replay; not fresh first-person or physical fidelity acceptance",
        "registered_prefix":prefix,"source_size":[sw,sh],"output_size":[w,h],"eligible_pixels":eligible,"covered_pixels":covered,
        "eligible_fraction":Double(eligible)/Double(covered),"color_changed_pixels":changed,"fallback_changed_pixels":fallbackChanged,
        "maximum_channel_delta_8bit":maxChannel,"mean_absolute_channel_delta_over_covered":Double(sumDifference)/Double(covered*3),
        "geometry_depth_owner_bit_identical":true,"frozen_original_geometry_exact":true,
        "historical_color_equality_required":false,"output_ppm_color_space":"Stored sRGB transfer, Display P3 primaries, untagged raw PPM",
        "baseline_gpu_ms":timesOld,"candidate_gpu_ms":timesNew,
        "baseline_median_ms":median(timesOld),"candidate_median_ms":median(timesNew),"median_paired_delta_ms":median(zip(timesOld,timesNew).map { $1-$0 })]
    try JSONSerialization.data(withJSONObject:result,options:[.prettyPrinted,.sortedKeys]).write(to:output.appendingPathComponent("replay-result.json"))
    print("REPLAY eligible=\(eligible)/\(covered), changedRGB=\(changed), maxchannel=\(maxChannel), fallbackchanges=\(fallbackChanged); paired delta=\(result["median_paired_delta_ms"]!)ms")
}

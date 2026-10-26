import Foundation
import Metal
import CoreGraphics
import ImageIO
import CryptoKit
import simd

/// Offscreen verification only. Original source RGB, depth, HUD coverage and
/// camera identities are loaded unchanged from registered native-game captures.
/// The two display poses are prescribed replays, not new headset observations.
@main
struct ColorPipelineMetalValidation {
    static func main() throws {
        precondition(CommandLine.arguments.count == 4, "Pass repository, registration directory, and new output directory")
        let root = URL(fileURLWithPath: CommandLine.arguments[1])
        let registryRoot = URL(fileURLWithPath: CommandLine.arguments[2])
        let output = URL(fileURLWithPath: CommandLine.arguments[3])
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        var checks = 0, renders = 0
        func require(_ condition: @autoclosure () -> Bool, _ message: String) {
            guard condition() else { fatalError("FAIL: " + message) }
            checks += 1
        }
        for (available,expected):([MTLPixelFormat],MTLPixelFormat) in [
            ([.bgra8Unorm,.rgba16Float,.rgba8Unorm_srgb,.bgra8Unorm_srgb],.bgra8Unorm_srgb),
            ([.bgra8Unorm,.rgba16Float,.rgba8Unorm_srgb],.rgba8Unorm_srgb),
            ([.bgra8Unorm,.rgba16Float],.rgba16Float),([.rgba8Unorm,.bgra8Unorm],.bgra8Unorm),
            ([.rgba8Unorm],.rgba8Unorm),([],.rgba16Float)] {
            require(GameWorldColor.compositorFormat(supported:available,fallback:.rgba16Float)==expected,"Actual compositor format policy preserves supported sRGB precision")
        }
        for format:MTLPixelFormat in [.bgra8Unorm,.rgba8Unorm] {require(GameWorldColor.sourceTransfer(format)==2,"Encoded native format requires per-tap decode fallback")}
        for format:MTLPixelFormat in [.bgra8Unorm_srgb,.rgba8Unorm_srgb,.rgba16Float] {require(GameWorldColor.sourceTransfer(format)==0,"Hardware-linear sampling format never double-decodes")}
        func json(_ url: URL) throws -> [String: Any] {
            try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as! [String: Any]
        }
        func hash(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
        func matrix(_ array: [NSNumber]) -> simd_float4x4 {
            let a = array.map(\.floatValue)
            return .init(columns: (SIMD4(a[0],a[1],a[2],a[3]), SIMD4(a[4],a[5],a[6],a[7]),
                SIMD4(a[8],a[9],a[10],a[11]), SIMD4(a[12],a[13],a[14],a[15])))
        }
        func double(_ m: simd_float4x4) -> simd_double4x4 {
            .init(columns: (SIMD4<Double>(m[0]), SIMD4<Double>(m[1]), SIMD4<Double>(m[2]), SIMD4<Double>(m[3])))
        }
        func float(_ m: simd_double4x4) -> simd_float4x4 {
            .init(columns: (SIMD4<Float>(m[0]), SIMD4<Float>(m[1]), SIMD4<Float>(m[2]), SIMD4<Float>(m[3])))
        }
        func texture(_ format: MTLPixelFormat, _ width: Int, _ height: Int,
                     _ usage: MTLTextureUsage) -> any MTLTexture {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: width, height: height, mipmapped: false)
            d.storageMode = .shared; d.usage = usage
            return device.makeTexture(descriptor: d)!
        }
        func rgba(_ texture: any MTLTexture) -> [UInt8] {
            var a = [UInt8](repeating: 0, count: texture.width * texture.height * 4)
            a.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: texture.width * 4,
                from: MTLRegionMake2D(0,0,texture.width,texture.height), mipmapLevel: 0) }
            return a
        }
        func png(_ pixels: [UInt8], _ width: Int, _ height: Int, _ path: URL,
                 space: CFString = CGColorSpace.displayP3) throws {
            let color = CGColorSpace(name: space)!
            let provider = CGDataProvider(data: Data(pixels) as CFData)!
            let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                bytesPerRow: width * 4, space: color, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                provider: provider, decode: nil, shouldInterpolate: false, intent: .relativeColorimetric)!
            let dest = CGImageDestinationCreateWithURL(path as CFURL, "public.png" as CFString, 1, nil)!
            CGImageDestinationAddImage(dest, image, nil)
            require(CGImageDestinationFinalize(dest), "PNG export completes")
            let source = CGImageSourceCreateWithURL(path as CFURL, nil)!
            let read = CGImageSourceCreateImageAtIndex(source, 0, nil)!
            require(read.colorSpace?.name == space, "Export preserves explicit color-space tag")
        }
        func crop(_ pixels: [UInt8], _ width: Int, _ box: [Int]) -> [UInt8] {
            var result = [UInt8]()
            for y in box[1]..<box[3] { result.append(contentsOf: pixels[(y*width+box[0])*4..<(y*width+box[2])*4]) }
            return result
        }
        let registry = try json(registryRoot.appendingPathComponent("registration.json"))
        let fixed = try json(URL(fileURLWithPath: registry["fixed_view_input"] as! String))
        let capture = URL(fileURLWithPath: (registry["profiles"] as! [String:String])["baseline1440"]!)
        let headEyes = (fixed["head_from_eyes"] as! [[NSNumber]]).map(matrix)
        let projections = (fixed["physical_output_projection_from_eyes"] as! [[NSNumber]]).map(matrix)
        let anchor = double(matrix(registry["common_game_from_stage"] as! [NSNumber]))
        let width = 1888, height = 1792
        var inputHashes = [String: String]()
        func bytes(_ path: URL) throws -> Data {
            let data = try Data(contentsOf: path); inputHashes[path.path] = hash(data)
            if let registered = (registry["inputs_sha256"] as? [String:String])?[path.path] {
                require(hash(data) == registered, "Registered input hash unchanged: " + path.lastPathComponent)
            }
            return data
        }
        func ppm(_ path: URL) throws -> (Int, Int, [UInt8]) {
            let d = [UInt8](try bytes(path)); var p = 0
            func token() -> String {
                while d[p] <= 32 { p += 1 }; let start = p
                while d[p] > 32 { p += 1 }
                return String(bytes: d[start..<p], encoding: .ascii)!
            }
            require(token() == "P6", "Source P6 format")
            let w = Int(token())!, h = Int(token())!; require(token() == "255", "Source 8-bit format"); p += 1
            require(d.count-p == w*h*3, "Exact source raster length")
            var a = [UInt8](repeating: 255, count: w*h*4)
            for i in 0..<w*h { for c in 0..<3 { a[i*4+c] = d[p+i*3+c] } }
            return (w,h,a)
        }
        struct Source {
            let metadata: [String:Any]
            let pixels: [UInt8]
            let finalPixels: [UInt8]
            let hudBytes: Data
            let color: any MTLTexture
            let srgb: any MTLTexture
            let depth: any MTLTexture
            let hud: any MTLTexture
            let projection: simd_float4x4
            let gameFromEye: simd_double4x4
        }
        var sources = [Source]()
        for eye in 0..<2 {
            let metadata = try JSONSerialization.jsonObject(with: bytes(capture.appendingPathComponent("eye\(eye)-report.json"))) as! [String:Any]
            let (w,h,pixels) = try ppm(capture.appendingPathComponent("eye\(eye)-hud-world.ppm"))
            let (fw,fh,finalPixels) = try ppm(capture.appendingPathComponent("eye\(eye)-source.ppm"))
            require(fw == w && fh == h, "Same lease final/world shapes agree")
            let color = texture(.bgra8Unorm,w,h,.shaderRead)
            // Match the actual native BGRA format; the fixture PPM changes only channel order.
            var bgra = pixels
            for i in 0..<w*h { bgra[i*4] = pixels[i*4+2]; bgra[i*4+2] = pixels[i*4] }
            bgra.withUnsafeBytes { color.replace(region: MTLRegionMake2D(0,0,w,h), mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: w*4) }
            let srgb = GameWorldColor.samplingTexture(color)
            require(srgb.pixelFormat == .bgra8Unorm_srgb,"Actual sampling helper produces zero-copy sRGB view")
            require(GameWorldColor.samplingTexture(srgb) === srgb,"Already-sRGB source sampling retains the same object")
            require(!color.usage.contains(.pixelFormatView), "Fixture proves sRGB view on shaderRead-only source")
            let depth = texture(.r32Float,w,h,.shaderRead), hud = texture(.rgba16Float,w,h,.shaderRead)
            let z = try bytes(capture.appendingPathComponent("eye\(eye)-source-depth.f32"))
            let hdata = try bytes(capture.appendingPathComponent("eye\(eye)-hud-contribution.rgba16f"))
            require(z.count == w*h*4 && hdata.count == w*h*8, "Exact native depth/HUD lengths")
            z.withUnsafeBytes { depth.replace(region: MTLRegionMake2D(0,0,w,h), mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: w*4) }
            hdata.withUnsafeBytes { hud.replace(region: MTLRegionMake2D(0,0,w,h), mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: w*8) }
            sources.append(Source(metadata: metadata, pixels: pixels, finalPixels:finalPixels, hudBytes:hdata, color: color, srgb: srgb, depth: depth, hud: hud,
                projection: matrix(metadata["source_projection_from_eye"] as! [NSNumber]),
                gameFromEye: double(matrix(metadata["game_from_eye"] as! [NSNumber]))))
            try png(pixels,w,h,output.appendingPathComponent("source-eye\(eye)-sRGB.png"),space: CGColorSpace.sRGB)
        }
        let beforeText = String(data: try bytes(output.appendingPathComponent("build/GameWorld-before.metal")), encoding: .utf8)!
        let afterText = String(data: try bytes(root.appendingPathComponent("visionos/Sources/GameWorld.metal")), encoding: .utf8)!
        // This test vertex selects exact UVs without duplicating any production
        // color arithmetic. Production world and HUD fragment functions execute unchanged.
        let sampleVertex = """
        vertex GameWorldVertex colorValidationVertex(uint id [[vertex_id]], constant float2 &uv [[buffer(1)]]) {
            const float2 p[3] = {float2(-1,-1),float2(3,-1),float2(-1,3)};
            GameWorldVertex out; out.position=float4(p[id],0,1); out.sourceUVQ=float3(uv,1);
            out.currentEyePosition=float3(0,0,-2); out.background=0; return out;
        }
        vertex GameWorldHUDVertex colorValidationHUDVertex(uint id [[vertex_id]]) {
            const float2 p[3] = {float2(-1,-1),float2(3,-1),float2(-1,3)};
            GameWorldHUDVertex out; out.position=float4(p[id],0,1); out.uv=float2(.5); return out;
        }
        """
        let before = try device.makeLibrary(source: beforeText + sampleVertex, options: nil)
        let after = try device.makeLibrary(source: afterText + sampleVertex, options: nil)
        // Independent Double oracle constructs the gamut transform through XYZ.
        // These standard primary matrices are deliberately not the shader's combined constants.
        let sRGBtoXYZ = simd_double3x3(columns: (SIMD3(0.4123907992659595,0.2126390058715104,0.0193308187155918),
            SIMD3(0.3575843393838780,0.7151686787677560,0.1191947797946260),
            SIMD3(0.1804807884018343,0.0721923153607337,0.9505321522496607)))
        let P3toXYZ = simd_double3x3(columns: (SIMD3(0.4865709486482162,0.2289745640697488,0),
            SIMD3(0.2656676931690931,0.6917385218365064,0.0451133818589026),
            SIMD3(0.1982172852343625,0.0792869140937450,1.0439443689009760)))
        let toP3 = P3toXYZ.inverse * sRGBtoXYZ
        func decode(_ v: Double) -> Double { v <= 0.04045 ? v/12.92 : pow((v+0.055)/1.055,2.4) }
        func encode(_ v: Double) -> Double { v <= 0.0031308 ? v*12.92 : 1.055*pow(max(v,0),1/2.4)-0.055 }
        func decoded(_ v: SIMD3<Double>) -> SIMD3<Double> { SIMD3(decode(v.x),decode(v.y),decode(v.z)) }
        func linearSample(_ source: Source, _ uv: SIMD2<Float>) -> SIMD3<Double> {
            let point = SIMD2<Double>(Double(uv.x)*Double(source.color.width)-0.5,Double(uv.y)*Double(source.color.height)-0.5)
            let x = Int(floor(point.x)), y = Int(floor(point.y)), f = point-SIMD2(Double(x),Double(y))
            var rgb = SIMD3<Double>.zero
            for dy in 0...1 { for dx in 0...1 {
                let sx = min(max(x+dx,0),source.color.width-1), sy = min(max(y+dy,0),source.color.height-1)
                let weight = (dx == 0 ? 1-f.x:f.x)*(dy == 0 ? 1-f.y:f.y)
                for c in 0..<3 { rgb[c] += decode(Double(source.pixels[(sy*source.color.width+sx)*4+c])/255) * weight }
            } }
            return toP3 * rgb
        }
        func samplingPipeline(_ library: any MTLLibrary) throws -> any MTLRenderPipelineState {
            let d = MTLRenderPipelineDescriptor(); d.vertexFunction = library.makeFunction(name:"colorValidationVertex")
            d.fragmentFunction = library.makeFunction(name:"gameWorldFragment"); d.colorAttachments[0].pixelFormat = .rgba32Float
            return try device.makeRenderPipelineState(descriptor:d)
        }
        let beforeSample = try samplingPipeline(before), afterSample = try samplingPipeline(after)
        func sample(_ pipeline: any MTLRenderPipelineState, _ source: Source, _ uv: SIMD2<Float>, view: Bool) -> SIMD3<Double> {
            let target = texture(.rgba32Float,1,1,.renderTarget), pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture=target;pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
            let cmd=queue.makeCommandBuffer()!, e=cmd.makeRenderCommandEncoder(descriptor:pass)!
            var values=GameWorldUniforms(sourceEyeFromClip:matrix_identity_float4x4,currentEyeFromSourceEye:matrix_identity_float4x4,
                currentClipFromEye:matrix_identity_float4x4,sourceSize:SIMD2(UInt32(source.color.width),UInt32(source.color.height)))
            values.colorTransfer=view ? 0:2
            var uv=uv;e.setVertexBytes(&uv,length:MemoryLayout<SIMD2<Float>>.stride,index:1)
            e.setFragmentBytes(&values,length:MemoryLayout<GameWorldUniforms>.stride,index:0)
            e.setFragmentTexture(view ? source.srgb:source.color,index:0);e.setRenderPipelineState(pipeline)
            e.drawPrimitives(type:.triangle,vertexStart:0,vertexCount:3);e.endEncoding();cmd.commit();cmd.waitUntilCompleted()
            require(cmd.status == .completed,"Actual production sampling fragment completed");renders += 1
            var pixels=[Float](repeating:0,count:4)
            pixels.withUnsafeMutableBytes{target.getBytes($0.baseAddress!,bytesPerRow:16,from:MTLRegionMake2D(0,0,1,1),mipmapLevel:0)}
            return SIMD3(Double(pixels[0]),Double(pixels[1]),Double(pixels[2]))
        }
        var oracleRows=[[String:Any]](), maxAfter=0.0, maxView=0.0, maxBefore=0.0
        for eye in 0..<2 { let s=sources[eye]
            // Fixed fractional samples of actual game pixels include dark floor,
            // bright signs and saturated detail. Exact texel centers also test primaries.
            for point in [SIMD2<Float>(0.5,0.5),SIMD2(113.5,84.5),SIMD2(620,455),SIMD2(1320.25,624.75),
                          SIMD2(2130.5,502.5),SIMD2(1791,940),SIMD2(400.25,1200.25),SIMD2(2559.5,1439.5)] {
                let uv=point/SIMD2(Float(s.color.width),Float(s.color.height)), expected=linearSample(s,uv)
                let old=sample(beforeSample,s,uv,view:false), manual=sample(afterSample,s,uv,view:false), hardware=sample(afterSample,s,uv,view:true)
                func error(_ value:SIMD3<Double>)->Double{simd_reduce_max(abs(value-expected))}
                let oldError=error(old),newError=error(manual),viewError=error(hardware)
                maxBefore=max(maxBefore,oldError);maxAfter=max(maxAfter,newError);maxView=max(maxView,viewError)
                require(newError < 0.001,"Raw-source manual filtering matches independent linear-light P3 oracle")
                require(viewError < 0.003,"sRGB source view matches independent linear-light P3 oracle")
                oracleRows.append(["eye":eye,"source_pixel":[point.x,point.y],"before_error":oldError,"after_manual_error":newError,"after_srgb_view_error":viewError])
            }
        }
        require(maxBefore > 0.005,"Frozen baseline is a failing negative control for missing primary conversion/filtering")
        let contrastPixels:[UInt8]=[0,0,0,255,255,255,255,255,0,0,0,255,255,255,255,255]
        let contrastTexture=texture(.rgba8Unorm,2,2,.shaderRead)
        contrastPixels.withUnsafeBytes{contrastTexture.replace(region:MTLRegionMake2D(0,0,2,2),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:8)}
        let contrast=Source(metadata:[:],pixels:contrastPixels,finalPixels:contrastPixels,hudBytes:Data(),color:contrastTexture,
            srgb:GameWorldColor.samplingTexture(contrastTexture),depth:sources[0].depth,hud:sources[0].hud,
            projection:matrix_identity_float4x4,gameFromEye:matrix_identity_double4x4)
        let contrastExpected=linearSample(contrast,SIMD2(0.5,0.5))
        let contrastBefore=sample(beforeSample,contrast,SIMD2(0.5,0.5),view:false)
        let contrastAfter=sample(afterSample,contrast,SIMD2(0.5,0.5),view:false)
        require(simd_reduce_max(abs(contrastBefore-contrastExpected)) > 0.28,"Black/white edge reproduces encoded-filter darkening in frozen baseline")
        require(simd_reduce_max(abs(contrastAfter-contrastExpected)) < 0.00001,"Black/white edge averages linear radiance correctly")

        // Use the production HUD fragment with explicit destination fetch. The
        // independent reference is the original encoded-domain source-over
        // equation, including accepted additive RGB at zero alpha.
        func hudPipeline(_ lib:any MTLLibrary,before:Bool,format:MTLPixelFormat)throws->any MTLRenderPipelineState {
            let d=MTLRenderPipelineDescriptor();d.vertexFunction=lib.makeFunction(name:"colorValidationHUDVertex")
            d.fragmentFunction=lib.makeFunction(name:"gameWorldHUDFragment");d.colorAttachments[0].pixelFormat=format
            d.colorAttachments[0].isBlendingEnabled=before
            d.colorAttachments[0].sourceRGBBlendFactor = .one;d.colorAttachments[0].destinationRGBBlendFactor = .oneMinusSourceAlpha
            d.colorAttachments[0].sourceAlphaBlendFactor = .one;d.colorAttachments[0].destinationAlphaBlendFactor = .oneMinusSourceAlpha
            return try device.makeRenderPipelineState(descriptor:d)
        }
        struct HUDUniforms {var clipFromHead:simd_float4x4;var halfSize:SIMD2<Float>;var distance:Float;var decodeForLinearTarget:UInt32}
        let hudOld=try hudPipeline(before,before:true,format:.rgba8Unorm_srgb)
        let hudNew=try hudPipeline(after,before:false,format:.rgba8Unorm_srgb)
        func hudSample(_ pipeline:any MTLRenderPipelineState,contribution:SIMD4<Float16>,background:SIMD3<Double>,alpha:Double,before:Bool,
                       format:MTLPixelFormat = .rgba8Unorm_srgb)->[Double] {
            let t=texture(.rgba16Float,1,1,.shaderRead),target=texture(format,1,1,.renderTarget)
            var input=contribution;withUnsafeBytes(of:&input){t.replace(region:MTLRegionMake2D(0,0,1,1),mipmapLevel:0,withBytes:$0.baseAddress!,bytesPerRow:8)}
            let p=MTLRenderPassDescriptor();p.colorAttachments[0].texture=target;p.colorAttachments[0].loadAction = .clear;p.colorAttachments[0].storeAction = .store
            let backdrop=toP3*decoded(background)*alpha
            p.colorAttachments[0].clearColor=MTLClearColorMake(backdrop.x,backdrop.y,backdrop.z,alpha)
            let cmd=queue.makeCommandBuffer()!,e=cmd.makeRenderCommandEncoder(descriptor:p)!
            var u=HUDUniforms(clipFromHead:matrix_identity_float4x4,halfSize:SIMD2(1,1),distance:1,decodeForLinearTarget:before ? 1:0)
            e.setRenderPipelineState(pipeline);e.setFragmentTexture(t,index:0);e.setFragmentBytes(&u,length:MemoryLayout<HUDUniforms>.stride,index:0)
            e.drawPrimitives(type:.triangle,vertexStart:0,vertexCount:3);e.endEncoding();cmd.commit();cmd.waitUntilCompleted()
            require(cmd.status == .completed,"Actual HUD fragment completes");renders += 1
            if format == .rgba16Float {
                var values=[Float16](repeating:0,count:4)
                values.withUnsafeMutableBytes{target.getBytes($0.baseAddress!,bytesPerRow:8,from:MTLRegionMake2D(0,0,1,1),mipmapLevel:0)}
                return [encode(Double(values[0]))*255,encode(Double(values[1]))*255,encode(Double(values[2]))*255,Double(values[3])*255]
            }
            var values=rgba(target).map(Double.init)
            if format == .bgra8Unorm_srgb || format == .bgra8Unorm {values.swapAt(0,2)}
            if format == .bgra8Unorm {for c in 0..<3{values[c]=encode(values[c]/255)*255}}
            return values
        }
        func hudReference(_ h:SIMD4<Float16>,_ background:SIMD3<Double>,_ backgroundAlpha:Double)->SIMD4<Double> {
            let a=min(max(Double(h.w),0),1),outAlpha=a+backgroundAlpha*(1-a),scale=outAlpha>0 ? outAlpha:1
            let rgb=SIMD3(Double(h.x),Double(h.y),Double(h.z))+background*backgroundAlpha*(1-a)
            let encoded=simd_clamp(rgb/scale,SIMD3(repeating:0),SIMD3(repeating:1))
            let linear=toP3*decoded(encoded)*scale
            return SIMD4(encode(linear.x),encode(linear.y),encode(linear.z),outAlpha)*255
        }
        var hudRows=[[String:Any]](),hudMaximum=0.0,hudBeforeMaximum=0.0
        let hudCases:[(String,SIMD4<Float16>,SIMD3<Double>,Double)]=[
            ("half-red-over-gray",SIMD4(0.5,0,0,0.5),SIMD3(repeating:0.5),1),
            ("dark-quarter-alpha",SIMD4(0.0125,0.025,0.05,0.25),SIMD3(repeating:0.02),1),
            ("zero-alpha-additive-over-gray",SIMD4(0.25,0.25,0.25,0),SIMD3(repeating:0.5),1),
            ("zero-alpha-additive-empty",SIMD4(0.25,0.25,0.25,0),.zero,0),
            ("saturated-H-greater-than-alpha",SIMD4(1.5,0.3,0.1,0.25),SIMD3(0.8,0.3,0.7),1),
            ("coverage-over-transparent",SIMD4(0.2,0.05,0.1,0.5),.zero,0),
            ("coverage-over-partial-background",SIMD4(0.2,0.1,0.05,0.5),SIMD3(0.2,0.4,0.6),0.5)]
        for (name,h,b,a) in hudCases {
            let expected=hudReference(h,b,a),actual=hudSample(hudNew,contribution:h,background:b,alpha:a,before:false)
            let old=hudSample(hudOld,contribution:h,background:b,alpha:a,before:true)
            let error=(0..<4).map{abs(Double(actual[$0])-expected[$0])}.max()!
            let oldError=(0..<4).map{abs(Double(old[$0])-expected[$0])}.max()!
            require(error <= 2.1,"HUD encoded-domain reference: "+name)
            hudMaximum=max(hudMaximum,error);hudBeforeMaximum=max(hudBeforeMaximum,oldError)
            hudRows.append(["name":name,"maximum_after_byte_error":error,"maximum_before_byte_error":oldError,"actual_rgba":actual])
        }
        require(hudBeforeMaximum > 30,"Frozen HUD baseline is a failing negative control")
        var hudFormatRows=[[String:Any]]()
        for (format,label) in [(MTLPixelFormat.bgra8Unorm_srgb,"bgra8Unorm_srgb"),(.rgba16Float,"rgba16Float"),(.bgra8Unorm,"bgra8Unorm")] {
            let pipeline=try hudPipeline(after,before:false,format:format)
            for (name,h,b,a) in hudCases {
                let expected=hudReference(h,b,a),actual=hudSample(pipeline,contribution:h,background:b,alpha:a,before:false,format:format)
                let error=(0..<4).map{abs(actual[$0]-expected[$0])}.max()!
                let linearError=(0..<3).map{abs(decode(actual[$0]/255)-decode(expected[$0]/255))}.max()!
                if format == .bgra8Unorm {
                    // Two storage quantizations: the initial destination and the
                    // result. Linear 8-bit is intentionally lower dark precision.
                    require(linearError<=0.005,"Linear 8-bit HUD fetch within two UNORM roundings")
                } else {require(error<=2.1,"Preferred/floating HUD fetch target: "+label+" "+name)}
                require(abs(actual[3]-expected[3])<=1,"HUD alpha retains format quantization")
                hudFormatRows.append(["format":label,"case":name,"maximum_encoded_byte_error":error,"maximum_linear_error":linearError])
            }
        }
        // Real same-lease original fragment contributions, background and final
        // guest pixels: preserve their original f16/UNORM rounding limits.
        for eye in 0..<2 {let s=sources[eye]
            let values=s.hudBytes.withUnsafeBytes{Array($0.bindMemory(to:UInt16.self))}
            var indices=[Int]()
            for i in stride(from:0,to:s.color.width*s.color.height,by:53) {
                let a=Float(Float16(bitPattern:values[i*4+3]))
                if a>0.05 && a<0.95 {indices.append(i);if indices.count==12{break}}
            }
            require(indices.count==12,"Captured HUD contains fractional original alpha samples")
            for i in indices {
                let h=SIMD4<Float16>(Float16(bitPattern:values[i*4]),Float16(bitPattern:values[i*4+1]),Float16(bitPattern:values[i*4+2]),Float16(bitPattern:values[i*4+3]))
                let b=SIMD3(Double(s.pixels[i*4]),Double(s.pixels[i*4+1]),Double(s.pixels[i*4+2]))/255
                let actual=hudSample(hudNew,contribution:h,background:b,alpha:1,before:false),expected=hudReference(h,b,1)
                let error=(0..<4).map{abs(Double(actual[$0])-expected[$0])}.max()!
                require(error<=2.1,"Captured HUD matches independent encoded source-over oracle")
                let finalEncoded=SIMD3(Double(s.finalPixels[i*4]),Double(s.finalPixels[i*4+1]),Double(s.finalPixels[i*4+2]))/255
                let finalP3=toP3*decoded(finalEncoded)
                let guestExpected=SIMD3(encode(finalP3.x),encode(finalP3.y),encode(finalP3.z))*255
                let guestError=(0..<3).map{abs(Double(actual[$0])-guestExpected[$0])}.max()!
                require(guestError<=3.1,"Captured original final pixel retained within f16/UNORM rounding")
                hudRows.append(["name":"actual-eye\(eye)-pixel\(i)","maximum_after_byte_error":error,"maximum_original_final_byte_error":guestError])
                hudMaximum=max(hudMaximum,error)
            }
        }
        struct Pipelines { let sky:GameWorldFiniteSkySupport;let fill:GameWorldDepthEdgeFill;let hud:any MTLRenderPipelineState }
        func pipelines(_ lib:any MTLLibrary, before:Bool)throws->Pipelines {
            let descriptor=MTLRenderPipelineDescriptor();descriptor.vertexFunction=lib.makeFunction(name:"gameWorldHUDVertex")
            descriptor.fragmentFunction=lib.makeFunction(name:"gameWorldHUDFragment");descriptor.colorAttachments[0].pixelFormat = .rgba8Unorm_srgb
            descriptor.depthAttachmentPixelFormat = .depth32Float
            descriptor.colorAttachments[0].isBlendingEnabled=before
            descriptor.colorAttachments[0].sourceRGBBlendFactor = .one;descriptor.colorAttachments[0].destinationRGBBlendFactor = .oneMinusSourceAlpha
            descriptor.colorAttachments[0].sourceAlphaBlendFactor = .one;descriptor.colorAttachments[0].destinationAlphaBlendFactor = .oneMinusSourceAlpha
            return Pipelines(sky:GameWorldFiniteSkySupport(device:device,library:lib,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float,
                precomputeCorners:false,orderedCorners:false,reuseOrderedSources:false)!,
                fill:GameWorldDepthEdgeFill(device:device,library:lib,colorFormat:.rgba8Unorm_srgb,depthFormat:.depth32Float)!,
                hud:try device.makeRenderPipelineState(descriptor:descriptor))
        }
        let oldPipelines=try pipelines(before,before:true),newPipelines=try pipelines(after,before:false)
        let hudSource=sources.max{($0.metadata["game_frame"] as! NSNumber).uint64Value < ($1.metadata["game_frame"] as! NSNumber).uint64Value}!
        struct Image {let world:[UInt8];let final:[UInt8];let depth:[UInt32];let owner:[UInt8]}
        let telemetry=GameWorldImageQualityTelemetry(fillRequested:true,fillAvailable:true)
        func render(_ p:Pipelines, _ eye:Int, _ head:simd_float4x4, before:Bool, view:Bool) -> Image {
            let source=sources[eye],projection=projections[eye],color=view ? source.srgb:source.color
            let relative=(anchor*double(head)*double(headEyes[eye])).inverse*source.gameFromEye
            var u=GameWorldUniforms(sourceEyeFromClip:source.projection.inverse,currentEyeFromSourceEye:float(relative),currentClipFromEye:projection,
                sourceSize:SIMD2(UInt32(source.color.width),UInt32(source.color.height)))
            u.sourceDepthReversed=source.projection[2].z>=0 ? 1:0;u.colorTransfer=view ? 0:2
            let target=texture(.rgba8Unorm_srgb,width,height,[.renderTarget,.shaderRead]),depth=texture(.depth32Float,width,height,.renderTarget)
            let pass=MTLRenderPassDescriptor();pass.colorAttachments[0].texture=target;pass.colorAttachments[0].loadAction = .clear;pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor=MTLClearColorMake(0,0,0,1);pass.depthAttachment.texture=depth
            pass.depthAttachment.loadAction = .clear;pass.depthAttachment.storeAction = .store;pass.depthAttachment.clearDepth=0
            let viewport=MTLViewport(originX:0,originY:0,width:Double(width),height:Double(height),znear:0,zfar:1)
            let cmd=queue.makeCommandBuffer()!
            let far=GameWorldDepthRange.selectedFar(nativeFar:(source.metadata["source_far"] as! NSNumber).floatValue)
            let result=p.sky.encode(commands:cmd,pass:pass,viewport:viewport,projection:projection,far:far,uniforms:u,color:color,depth:source.depth)!
            require(!result.orderedCorners,"Normal fine path used; no experimental reconstruction")
            pass.colorAttachments[0].loadAction = .load;pass.depthAttachment.loadAction = .load
            let fill=cmd.makeRenderCommandEncoder(descriptor:pass)!;fill.setViewport(viewport)
            let fu=GameWorldDepthEdgeFillPolicy.uniforms(sourceProjection:source.projection,currentEyeFromSourceEye:u.currentEyeFromSourceEye,
                displayProjection:projection,viewportOrigin:.zero,viewportSize:SIMD2(Float(width),Float(height)),sourceSize:u.sourceSize,
                sourceDepthReversed:u.sourceDepthReversed,colorTransfer:u.colorTransfer,nearestDistance:u.nearFadeDistances.y,skyRadius:result.radius)!
            require(p.fill.encode(fill,commands:cmd,eye:2,uniforms:fu,ownership:result.ownership,color:color,depth:source.depth,telemetry:telemetry),"Actual gap fill encoded")
            fill.endEncoding()
            let row=((width*4+255)/256)*256,ownerRow=((width+255)/256)*256
            let world=device.makeBuffer(length:row*height,options:.storageModeShared)!,owners=device.makeBuffer(length:ownerRow*height,options:.storageModeShared)!
            let blit=cmd.makeBlitCommandEncoder()!
            blit.copy(from:target,sourceSlice:0,sourceLevel:0,sourceOrigin:MTLOrigin(),sourceSize:MTLSize(width:width,height:height,depth:1),to:world,destinationOffset:0,destinationBytesPerRow:row,destinationBytesPerImage:row*height)
            blit.copy(from:result.ownership,sourceSlice:0,sourceLevel:0,sourceOrigin:MTLOrigin(),sourceSize:MTLSize(width:width,height:height,depth:1),to:owners,destinationOffset:0,destinationBytesPerRow:ownerRow,destinationBytesPerImage:ownerRow*height)
            blit.endEncoding()
            let half=GameWorldHUDPolicy.halfSize(width:hudSource.color.width,height:hudSource.color.height,angularWidth:60,distance:2)!
            let placement=GameWorldHUDPolicy.placement(halfSize:half,nativeScope:false,sourceSize:SIMD2(UInt32(hudSource.color.width),UInt32(hudSource.color.height)))
            var hu=HUDUniforms(clipFromHead:GameWorldHUDPolicy.clipFromHead(headFromEye:headEyes[eye],projection:projection,placementY:placement.y,nativeScope:false),
                halfSize:placement.halfSize,distance:2,decodeForLinearTarget:before ? 1:0)
            let overlay=cmd.makeRenderCommandEncoder(descriptor:pass)!;overlay.setViewport(viewport);overlay.setRenderPipelineState(p.hud)
            let ds=MTLDepthStencilDescriptor();ds.depthCompareFunction = .always;ds.isDepthWriteEnabled=false
            overlay.setDepthStencilState(device.makeDepthStencilState(descriptor:ds));overlay.setCullMode(.none)
            overlay.setVertexBytes(&hu,length:MemoryLayout<HUDUniforms>.stride,index:0);overlay.setFragmentBytes(&hu,length:MemoryLayout<HUDUniforms>.stride,index:0)
            overlay.setFragmentTexture(hudSource.hud,index:0);overlay.drawPrimitives(type:.triangleStrip,vertexStart:0,vertexCount:4);overlay.endEncoding()
            cmd.commit();cmd.waitUntilCompleted();require(cmd.status == .completed,"Assembled shader/sky/fill/HUD execution completed");renders += 1
            var w=[UInt8](repeating:0,count:width*height*4),o=[UInt8](repeating:0,count:width*height),z=[UInt32](repeating:0,count:width*height)
            let wp=world.contents().bindMemory(to:UInt8.self,capacity:world.length),op=owners.contents().bindMemory(to:UInt8.self,capacity:owners.length)
            for y in 0..<height {for x in 0..<width {o[y*width+x]=op[y*ownerRow+x];for c in 0..<4 {w[(y*width+x)*4+c]=wp[y*row+x*4+c]}}}
            z.withUnsafeMutableBytes{depth.getBytes($0.baseAddress!,bytesPerRow:width*4,from:MTLRegionMake2D(0,0,width,height),mipmapLevel:0)}
            return Image(world:w,final:rgba(target),depth:z,owner:o)
        }
        var turned=simd_float4x4(simd_quatf(angle:.pi/36,axis:SIMD3(0,1,0)));turned[3].x=0.025
        var rows=[[String:Any]]()
        let rois=(try json(registryRoot.appendingPathComponent("static-rois.json")))["rows"] as! [[String:Any]]
        for (poseName,head) in [("neutral",matrix_identity_float4x4),("declared-yaw5deg-x25mm",turned)] {for eye in 0..<2 {
            let old=autoreleasepool{render(oldPipelines,eye,head,before:true,view:false)}
            let manual=autoreleasepool{render(newPipelines,eye,head,before:false,view:false)}
            let hardware=autoreleasepool{render(newPipelines,eye,head,before:false,view:true)}
            require(old.owner==manual.owner,"Finished-image corrections preserve all ownership bytes")
            // The planar gap repair intentionally corrects depth only at an
            // already uncovered pixel. ReprojectionFillPlaneValidation holds
            // the independent surface/color oracle; SamplingValidation checks
            // registered images, gates and fallback. Retain strict covered-depth
            // and coverage checks here against the historical color baseline.
            var filledDepthChanges = 0, coveredDepthChanges = 0, finiteCoverageChanges = 0
            for i in 0..<width*height {
                if old.depth[i] != manual.depth[i] {
                    if old.owner[i] == 0 { filledDepthChanges += 1 } else { coveredDepthChanges += 1 }
                }
                if (old.depth[i] == 0) != (manual.depth[i] == 0) { finiteCoverageChanges += 1 }
            }
            require(coveredDepthChanges == 0,"Finished-image corrections preserve every covered depth bit")
            require(finiteCoverageChanges == 0,"Gap repair preserves finite-depth coverage")
            require(manual.depth==hardware.depth && manual.owner==hardware.owner,"Hardware and fallback preserve depth/ownership")
            var changed=0,maximum=0,total=0,viewMaximum=0,viewTotal=0
            for i in 0..<width*height {var different=false;for c in 0..<3 {
                let d=abs(Int(old.world[i*4+c])-Int(hardware.world[i*4+c]));total+=d;maximum=max(maximum,d);different = different || d>0
                let v=abs(Int(manual.world[i*4+c])-Int(hardware.world[i*4+c]));viewTotal+=v;viewMaximum=max(viewMaximum,v)
            };if different{changed+=1}}
            // Hardware sRGB approximation is allowed a small code-value delta.
            require(viewMaximum <= 3,"Manual decode fallback and hardware sRGB view agree within 3 output bytes")
            let name="\(poseName)-eye\(eye)"
            for (tag,image) in [("before",old),("after",hardware)] {
                try png(image.world,width,height,output.appendingPathComponent(name+"-"+tag+"-world-P3.png"))
                try png(image.final,width,height,output.appendingPathComponent(name+"-"+tag+"-HUD-P3.png"))
                if eye==0 && poseName=="neutral" {for roi in rois {
                    let box=roi["box_pixels"] as! [Int],label=roi["name"] as! String
                    try png(crop(image.world,width,box),box[2]-box[0],box[3]-box[1],output.appendingPathComponent(label+"-"+tag+"-P3.png"))
                }}
            }
            rows.append(["name":name,"source_eye":eye,"source_lease":sources[eye].metadata["lease"]!,"source_game_frame":sources[eye].metadata["game_frame"]!,
                "source_projection":sources[eye].metadata["source_projection_from_eye"]!,"source_game_from_eye":sources[eye].metadata["game_from_eye"]!,
                "original_readiness_at_snapshot":sources[eye].metadata["readiness_at_snapshot"]!,"original_deadline_fallback":sources[eye].metadata["diagnostic_deadline_fallback"]!,
                "changed_world_pixels":changed,"maximum_world_code_delta":maximum,"mean_world_code_delta":Double(total)/Double(width*height*3),
                "manual_hardware_maximum_code_delta":viewMaximum,"manual_hardware_mean_code_delta":Double(viewTotal)/Double(width*height*3),
                "covered_depth_and_all_ownership_byte_identical":true,"filled_depth_changed_pixels":filledDepthChanges,
                "before_depth_sha256":old.depth.withUnsafeBytes{hash(Data($0))},
                "after_depth_sha256":hardware.depth.withUnsafeBytes{hash(Data($0))},"owner_sha256":hash(Data(old.owner))])
            print("PASS \(name): covered depth/owner exact; \(filledDepthChanges) planar fill depth corrections; color changes \(changed)/\(width*height), hardware/fallback max \(viewMaximum) bytes")
        }}
        let report:[String:Any]=["schema":1,"device":device.name,"checks":checks,"completed_metal_renders":renders,"pass":true,
            "source_hashes":inputHashes,"oracle_samples":oracleRows,"maximum_before_oracle_error":maxBefore,"maximum_after_manual_oracle_error":maxAfter,
            "maximum_after_hardware_oracle_error":maxView,"rows":rows,"output_width":width,"output_height":height,
            "hud_oracle_rows":hudRows,"maximum_hud_after_byte_error":hudMaximum,"maximum_hud_before_byte_error":hudBeforeMaximum,
            "hud_target_format_rows":hudFormatRows,
            "black_white_edge_linear_expected":[contrastExpected.x,contrastExpected.y,contrastExpected.z],
            "black_white_edge_linear_before":[contrastBefore.x,contrastBefore.y,contrastBefore.z],
            "black_white_edge_linear_after":[contrastAfter.x,contrastAfter.y,contrastAfter.z],
            "export_color_space":"Display P3 (sRGB transfer)","source_export_color_space":"sRGB","target_format":"rgba8Unorm_srgb",
            "before_contract":"Frozen177acd5 rawBGRA→encoded-filter→manualdecode→sRGBtarget; output interpreted per CompositorServices P3 contract",
            "after_contract":"linear-light sampling→sRGBprimaries-to-P3→sRGBtarget; encoded-domain HUD source-over through destination fetch",
            "no_resolution_guard_covered_geometry_or_graphics_setting_change":true,"physical_quality_acceptance":false,"live_game_run":false,"simulator":false,
            "limits":"Existing independently captured eye leases and their registered source matrices. Prescribed neutral and turned display poses. Original failed capture-readiness streak retained. Filled depth can change under the separately tested planar gap-repair certificate; covered depth and all ownership remain exact. Offline color/geometry verification; no new headset frame, flicker, latency or FPS acceptance."]
        try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:output.appendingPathComponent("report.json"),options:.withoutOverwriting)
        print("PASS color pipeline: \(checks) checks, \(renders) completed Metal renders")
    }
}

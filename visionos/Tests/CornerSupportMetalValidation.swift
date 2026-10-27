import Foundation
import Metal
import simd

/// Runs the production corner function on real Metal. Pass current shader and
/// the pre-change shader; GameWorldUniforms is extracted from the renderer.
@main struct CornerSupportMetalValidation {
    static func main() throws {
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let probe = """
        kernel void cornerSupportProbe(uint id [[thread_position_in_grid]],
                constant GameWorldUniforms &u [[buffer(0)]], device float *out [[buffer(1)]],
                texture2d<float, access::read> depth [[texture(0)]]) {
            if (id < 81) { out[id] = qualifiedCornerDepth(uint2(id % 9, id / 9), u, depth); }
        }
        """
        let pipelines = try CommandLine.arguments.dropFirst().map { path in
            let source = try String(contentsOfFile: path, encoding: .utf8)
            let library = try device.makeLibrary(source: source + probe, options: nil)
            return try device.makeComputePipelineState(function: library.makeFunction(name: "cornerSupportProbe")!)
        }
        precondition(pipelines.count == 2, "Pass current and pre-change shaders")
        var checks = 0, recovered = 0
        func check(_ value: Bool, _ message: String) { precondition(value, message); checks += 1 }
        func run(_ samples: [Float], _ reversed: Bool, _ smooth: Bool, _ pipeline: any MTLComputePipelineState) -> [Float] {
            let near: Float = 0.05, far: Float = 100
            let projection = simd_float4x4(columns: (SIMD4(1,0,0,0), SIMD4(0,1,0,0),
                SIMD4(0,0,reversed ? near/(far-near) : -far/(far-near),-1),
                SIMD4(0,0,reversed ? near*far/(far-near) : -near*far/(far-near),0)))
            var u = GameWorldUniforms(sourceEyeFromClip: projection.inverse,
                currentEyeFromSourceEye: matrix_identity_float4x4, currentClipFromEye: projection, sourceSize: SIMD2(8,8))
            u.sourceDepthReversed = reversed ? 1 : 0
            u.reserved.x = smooth ? 1 : 0
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .r32Float, width: 8, height: 8, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .shaderRead
            let depth = device.makeTexture(descriptor: descriptor)!
            samples.withUnsafeBytes { depth.replace(region: MTLRegionMake2D(0,0,8,8), mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: 32) }
            let output = device.makeBuffer(length: 81*4, options: .storageModeShared)!
            let command = queue.makeCommandBuffer()!, encoder = command.makeComputeCommandEncoder()!
            encoder.setComputePipelineState(pipeline)
            encoder.setBytes(&u, length: MemoryLayout<GameWorldUniforms>.stride, index: 0)
            encoder.setBuffer(output, offset: 0, index: 1); encoder.setTexture(depth, index: 0)
            encoder.dispatchThreads(MTLSize(width: 81,height: 1,depth: 1), threadsPerThreadgroup: MTLSize(width: 32,height: 1,depth: 1))
            encoder.endEncoding(); command.commit(); command.waitUntilCompleted()
            check(command.status == .completed, "Metal execution")
            return Array(UnsafeBufferPointer(start: output.contents().bindMemory(to: Float.self, capacity: 81), count: 81))
        }
        for reversed in [false, true] {
            let plane: [Float] = (0..<64).map { i in
                let z = Float(1)/16 + Float(i%8)/1024 + Float(i/8)/2048
                return reversed ? z : 1-z
            }
            for axis in [0,1,2] { for obstruction in [Float(0),Float(0.5),Float.nan] {
                var samples = plane
                for y in 0..<8 { for x in 0..<8 {
                    if (axis != 1 && x == 5) || (axis != 0 && y == 5) {
                        samples[y*8+x] = reversed ? obstruction : 1-obstruction
                    }
                }}
                let current = run(samples,reversed,false,pipelines[0]), old = run(samples,reversed,false,pipelines[1])
                check(old[4*9+4] == -1, "Original stencil reproduces false rejection")
                let z = Float(1)/16 + Float(3.5)/1024 + Float(3.5)/2048
                check(abs(current[4*9+4] - (reversed ? z : 1-z)) < 1e-7, "Enclosing exact plane restores shared corner")
                recovered += 1
                let boundary = axis == 0 ? 4*9+5 : 5*9+4
                check(current[boundary] == -1, "Actual occlusion/sky/invalid boundary stays isolated")
                for i in 0..<81 where old[i] >= 0 {
                    check(current[i].bitPattern == old[i].bitPattern, "Previously accepted corners stay bit exact")
                }
                let smooth = run(samples,reversed,true,pipelines[0]), oldSmooth = run(samples,reversed,true,pipelines[1])
                check(zip(smooth,oldSmooth).allSatisfy { $0.bitPattern == $1.bitPattern }, "Opt-in smooth behavior unchanged")
            }}
            for y in 3...4 { for x in 3...4 {
                var samples = plane; samples[y*8+x] = reversed ? 0 : 1
                check(run(samples,reversed,false,pipelines[0])[4*9+4] == -1, "Every actual corner owner participates in admission")
            }}
        }
        print("PASS: \(checks) Metal checks; \(recovered) old failures corrected; true boundaries and original successful corners preserved.")
    }
}

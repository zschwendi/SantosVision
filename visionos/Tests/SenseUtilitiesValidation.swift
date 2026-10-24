import Foundation
import Metal
import simd

@main struct SenseUtilitiesValidation {
    static func main() throws {
        var checks = 0
        func require(_ condition: @autoclosure () -> Bool, _ message: String) {
            precondition(condition(), message); checks += 1
        }
        let actions = GameWorldSenseUtilityLayout.actions
        require(actions.count == 6 && Set(actions).count == 6, "Only Phone, View and D-pad utilities")
        for action in actions {
            require(GameWorldSenseUtilityLayout.action(identifier: GameWorldSenseUtilityLayout.identifier(action)) == action,
                    "System tracking identifiers round trip")
        }
        require(GameWorldSenseUtilityLayout.action(identifier: 7) == nil, "Unrelated tracking areas cannot activate buttons")
        require(GameWorldSenseUtilityLayout.action(identifier: GameWorldSenseUtilityLayout.namespace | 999) == nil,
                "Unknown actions cannot be injected")
        var interaction = GameWorldSenseUtilityInteraction<Int>()
        require(interaction.end(1, action: .phone) == nil, "Release without press does nothing")
        interaction.begin(1, action: .phone)
        interaction.begin(1, action: .changeView)
        require(interaction.end(1, action: .changeView) == nil, "Dragging cannot activate another button")
        require(!interaction.isActive, "Invalid release clears ownership")
        interaction.begin(1, action: .phone); interaction.begin(2, action: .up)
        require(interaction.end(1, action: .phone) == .phone && interaction.isActive, "Second input retains ownership")
        require(interaction.end(1, action: .phone) == nil, "Repeated release cannot repeat an action")
        interaction.cancel(2); require(!interaction.isActive, "Cancellation releases ownership")
        interaction.begin(3, action: .left); interaction.clear()
        require(!interaction.isActive && interaction.end(3, action: .left) == nil, "Menu/lifecycle cleanup removes held actions")
        interaction.begin(4, action: .down); interaction.begin(5, action: .down)
        require(interaction.activeActions == [.down], "Multiple input IDs share one held-direction lease")
        _ = interaction.end(4, action: .down)
        require(interaction.activeActions == [.down], "First release cannot release another input's held direction")
        interaction.update(5, action: nil)
        require(!interaction.isActive, "Dragging outside releases the actual held direction immediately")
        interaction.update(5, action: .phone)
        require(interaction.end(5, action: .phone) == nil, "A cancelled drag cannot activate another button")
        interaction.update(6, action: nil); interaction.update(6, action: .phone)
        require(!interaction.isActive, "A pinch begun outside cannot turn into an accidental gameplay utility")
        _ = interaction.end(6, action: .phone)
        interaction.begin(7, action: .down)
        interaction.suspend()
        require(!interaction.isActive && interaction.activeActions.isEmpty, "Tracking loss clears every held action without a cancelled event")
        interaction.update(7, action: .down)
        require(!interaction.isActive && interaction.end(7, action: .down) == nil, "Tracking recovery cannot rearm the old held gesture")
        interaction.begin(8, action: .down)
        require(interaction.isActive, "A fresh gesture can rearm after recovery")
        interaction.clear()
        interaction.rejectUntilEnd(9)
        interaction.update(9, action: .phone)
        require(!interaction.isActive && interaction.end(9, action: .phone) == nil, "Gesture begun while unavailable cannot activate on recovery")
        interaction.begin(10, action: .phone)
        require(interaction.end(10, action: .phone) == .phone, "Fresh gesture after unavailable interaction works")
        require(MemoryLayout<SenseUtilityTestUniforms>.stride == 96, "Actual production uniform layout")
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { fatalError("Metal unavailable") }
        let source = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
        let library = try device.makeLibrary(source: source, options: nil)
        let size = 512
        func texture(_ format: MTLPixelFormat) -> any MTLTexture {
            let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: size, height: size, mipmapped: false)
            d.usage = [.renderTarget, .shaderRead]; d.storageMode = .shared
            return device.makeTexture(descriptor: d)!
        }
        let atlasDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm, width: 6, height: 1, mipmapped: false)
        atlasDescriptor.storageMode = .shared
        let atlas = device.makeTexture(descriptor: atlasDescriptor)!
        let atlasBytes: [UInt8] = [255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,0,255, 255,0,255,255, 0,255,255,255]
        atlasBytes.withUnsafeBytes { atlas.replace(region: MTLRegionMake2D(0,0,6,1), mipmapLevel: 0, withBytes: $0.baseAddress!, bytesPerRow: 24) }
        let depthDescriptor = MTLDepthStencilDescriptor(); depthDescriptor.depthCompareFunction = .always; depthDescriptor.isDepthWriteEnabled = true
        let depthState = device.makeDepthStencilState(descriptor: depthDescriptor)!
        for format in [MTLPixelFormat.r8Uint, .r16Uint] {
            let p = MTLRenderPipelineDescriptor()
            p.vertexFunction = library.makeFunction(name: "gameWorldSenseUtilityVertex")
            p.fragmentFunction = library.makeFunction(name: "gameWorldSenseUtilityFragment")
            p.colorAttachments[0].pixelFormat = .rgba8Unorm
            p.colorAttachments[1].pixelFormat = format
            p.depthAttachmentPixelFormat = .depth32Float
            let pipeline = try device.makeRenderPipelineState(descriptor: p)
            for eyeOffset: Float in [-0.03275, 0.03275] {
                let color = texture(.rgba8Unorm), ids = texture(format), depth = texture(.depth32Float)
                let pass = MTLRenderPassDescriptor()
                for (index, texture) in [color, ids].enumerated() {
                    pass.colorAttachments[index].texture = texture
                    pass.colorAttachments[index].loadAction = .clear; pass.colorAttachments[index].storeAction = .store
                }
                pass.colorAttachments[0].clearColor = MTLClearColorMake(0.1, 0.2, 0.3, 1)
                pass.depthAttachment.texture = depth; pass.depthAttachment.loadAction = .clear; pass.depthAttachment.storeAction = .store
                pass.depthAttachment.clearDepth = 0
                let command = queue.makeCommandBuffer()!, encoder = command.makeRenderCommandEncoder(descriptor: pass)!
                encoder.setRenderPipelineState(pipeline); encoder.setDepthStencilState(depthState); encoder.setCullMode(.none)
                encoder.setFragmentTexture(atlas, index: 0)
                let projection = simd_float4x4(columns: (SIMD4(0.7,0,0,0),SIMD4(0,1,0,0),SIMD4(0,0,0,-1),SIMD4(0,0,0.05,0)))
                var eyeFromHead = matrix_identity_float4x4; eyeFromHead.columns.3.x = -eyeOffset
                let clipFromHead = projection * eyeFromHead
                for index in actions.indices {
                    var u = SenseUtilityTestUniforms(clipFromHead: clipFromHead, center: GameWorldSenseUtilityLayout.center(index: index),
                        halfSize: GameWorldSenseUtilityLayout.halfSize, distance: GameWorldSenseUtilityLayout.distance,
                        atlasIndex: UInt32(index), trackingValue: UInt32(index + 1))
                    encoder.setVertexBytes(&u, length: MemoryLayout<SenseUtilityTestUniforms>.stride, index: 0)
                    encoder.setFragmentBytes(&u, length: MemoryLayout<SenseUtilityTestUniforms>.stride, index: 0)
                    encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
                }
                encoder.endEncoding(); command.commit(); command.waitUntilCompleted()
                require(command.status == .completed, "Actual utility MRT/depth pass completes")
                let idStride = format == .r8Uint ? 1 : 2
                var idBytes = [UInt8](repeating: 0, count: size * size * idStride)
                idBytes.withUnsafeMutableBytes { ids.getBytes($0.baseAddress!, bytesPerRow: size * idStride, from: MTLRegionMake2D(0,0,size,size), mipmapLevel: 0) }
                var depthPixels = [Float](repeating: 0, count: size * size)
                depthPixels.withUnsafeMutableBytes { depth.getBytes($0.baseAddress!, bytesPerRow: size * 4, from: MTLRegionMake2D(0,0,size,size), mipmapLevel: 0) }
                for index in actions.indices {
                    let center = GameWorldSenseUtilityLayout.center(index: index)
                    let clip = clipFromHead * SIMD4<Float>(center.x, center.y, -GameWorldSenseUtilityLayout.distance, 1)
                    let x = Int((clip.x / clip.w * 0.5 + 0.5) * Float(size))
                    let y = Int((0.5 - clip.y / clip.w * 0.5) * Float(size))
                    require((0..<size).contains(x) && (0..<size).contains(y), "All six buttons fit in both eyes")
                    let offset = (y * size + x) * idStride
                    let value = UInt32(idBytes[offset]) | (idStride == 2 ? UInt32(idBytes[offset+1]) << 8 : 0)
                    require(value == UInt32(index + 1), "Visible button pixel has exact system interaction ID")
                    require(abs(depthPixels[y * size + x] - 0.025) < 1e-6, "Interaction depth is the finite 2m plane")
                }
                require(idBytes.prefix(64).allSatisfy { $0 == 0 }, "Outside pixels have no tracking region")
                require(depthPixels[0] == 0, "Uncovered world depth is preserved")
            }
        }
        print("Sense utilities: \(checks) policy and actual Metal assertions passed")
    }
}

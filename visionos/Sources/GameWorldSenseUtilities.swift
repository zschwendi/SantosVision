import Foundation
import simd

enum GameWorldSenseUtilityLayout {
    static let distance: Float = 2
    static let halfSize = SIMD2<Float>(repeating: 0.0675)
    static let namespace: UInt64 = 0x5356555400000000
    static var actions: [SantosSenseUtilityAction] { [.phone, .changeView, .up, .down, .left, .right] }
    static func identifier(_ action: SantosSenseUtilityAction) -> UInt64 { namespace | UInt64(action.rawValue) }
    static func action(identifier: UInt64) -> SantosSenseUtilityAction? {
        guard identifier & 0xffff_ffff_0000_0000 == namespace else { return nil }
        return SantosSenseUtilityAction(rawValue: UInt32(truncatingIfNeeded: identifier))
    }
    static func center(index: Int) -> SIMD2<Float> { SIMD2(-0.98 + Float(index) * 0.15, -0.79) }
}

struct GameWorldSenseUtilityInteraction<ID: Hashable> {
    private var pressed: [ID: SantosSenseUtilityAction] = [:]
    private var cancelled = Set<ID>()
    var isActive: Bool { !pressed.isEmpty }
    var activeActions: Set<SantosSenseUtilityAction> { Set(pressed.values) }
    mutating func begin(_ id: ID, action: SantosSenseUtilityAction) { update(id, action: action) }
    mutating func update(_ id: ID, action: SantosSenseUtilityAction?) {
        guard !cancelled.contains(id) else { return }
        if let original = pressed[id] {
            if original != action { pressed.removeValue(forKey: id); cancelled.insert(id) }
        } else if let action { pressed[id] = action }
        else { cancelled.insert(id) }
    }
    mutating func end(_ id: ID, action: SantosSenseUtilityAction?) -> SantosSenseUtilityAction? {
        cancelled.remove(id)
        guard let original = pressed.removeValue(forKey: id), original == action else { return nil }
        return original
    }
    mutating func cancel(_ id: ID) { pressed.removeValue(forKey: id); cancelled.remove(id) }
    mutating func clear() { pressed.removeAll(keepingCapacity: true); cancelled.removeAll(keepingCapacity: true) }
}

#if SV_GAME_RUNTIME
import CompositorServices
import Metal
import UIKit

/// Small in-scene buttons use compositor tracking areas for system hover and
/// indirect selection. They never open another window or expose raw gaze data.
final class GameWorldSenseUtilities: @unchecked Sendable {
    private let atlas: any MTLTexture
    private let pipeline: any MTLRenderPipelineState
    private let depth: any MTLDepthStencilState
    private let lock = NSLock()
    private var visible = false
    private struct Uniforms {
        var clipFromHead: simd_float4x4
        var center: SIMD2<Float>
        var halfSize: SIMD2<Float>
        var distance: Float
        var atlasIndex: UInt32
        var trackingValue: UInt32
        var padding: UInt32 = 0
    }

    @MainActor static func makeAtlas(device: any MTLDevice) -> (any MTLTexture)? {
        let size = CGSize(width: 768, height: 128)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            for (index, action) in GameWorldSenseUtilityLayout.actions.enumerated() {
                let x = CGFloat(index * 128)
                let symbol = UIImage(systemName: action.symbol,
                    withConfiguration: UIImage.SymbolConfiguration(pointSize: 40, weight: .medium))?
                    .withTintColor(.white, renderingMode: .alwaysOriginal)
                if let symbol {
                    let ratio = min(54 / symbol.size.width, 54 / symbol.size.height)
                    let glyphSize = CGSize(width: symbol.size.width * ratio, height: symbol.size.height * ratio)
                    symbol.draw(in: CGRect(x: x + (128 - glyphSize.width) / 2, y: 22,
                                           width: glyphSize.width, height: glyphSize.height))
                }
                let title = action == .changeView ? "View" : action == .phone ? "Phone" : ""
                let attributes: [NSAttributedString.Key: Any] = [.font: UIFont.systemFont(ofSize: 20, weight: .medium),
                    .foregroundColor: UIColor.white]
                let text = NSAttributedString(string: title, attributes: attributes)
                text.draw(at: CGPoint(x: x + (128 - text.size().width) / 2, y: 88))
            }
        }
        guard let cg = image.cgImage else { return nil }
        var pixels = [UInt8](repeating: 0, count: 768 * 128 * 4)
        let rendered = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: 768, height: 128,
                bitsPerComponent: 8, bytesPerRow: 768 * 4, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.translateBy(x: 0, y: 128)
            context.scaleBy(x: 1, y: -1)
            context.draw(cg, in: CGRect(origin: .zero, size: size))
            return true
        }
        guard rendered else { return nil }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
            width: 768, height: 128, mipmapped: false)
        descriptor.usage = .shaderRead
        descriptor.storageMode = .shared
        guard let texture = device.makeTexture(descriptor: descriptor) else { return nil }
        pixels.withUnsafeBytes { bytes in
            texture.replace(region: MTLRegionMake2D(0, 0, 768, 128), mipmapLevel: 0,
                withBytes: bytes.baseAddress!, bytesPerRow: 768 * 4)
        }
        texture.label = "Sense Phone/View/D-pad system-symbol atlas"
        return texture
    }

    init?(device: any MTLDevice, library: any MTLLibrary, atlas: any MTLTexture,
          colorFormat: MTLPixelFormat, depthFormat: MTLPixelFormat, trackingFormat: MTLPixelFormat) {
        guard trackingFormat == .r16Uint || trackingFormat == .r8Uint,
              let vertex = library.makeFunction(name: "gameWorldSenseUtilityVertex"),
              let fragment = library.makeFunction(name: "gameWorldSenseUtilityFragment") else { return nil }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "Bottom-left Sense utilities and compositor interaction regions"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = colorFormat
        descriptor.colorAttachments[1].pixelFormat = trackingFormat
        descriptor.depthAttachmentPixelFormat = depthFormat
        let state = MTLDepthStencilDescriptor()
        state.depthCompareFunction = .always
        // Interaction depth belongs to the actual 2m button plane.
        state.isDepthWriteEnabled = true
        guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor),
              let depth = device.makeDepthStencilState(descriptor: state) else { return nil }
        self.pipeline = pipeline
        self.depth = depth
        self.atlas = atlas
    }

    func setVisible(_ value: Bool) { lock.lock(); visible = value; lock.unlock() }

    func encode(drawable: LayerRenderer.Drawable, commands: any MTLCommandBuffer, tracked: Bool) {
        lock.lock(); let show = visible && tracked; lock.unlock()
        let tracking = drawable.trackingAreasTextures
        guard !tracking.isEmpty else { return }
        let areas = show ? GameWorldSenseUtilityLayout.actions.map { action -> (SantosSenseUtilityAction, UInt32) in
            let identifier = LayerRenderer.Drawable.TrackingArea.Identifier(rawValue: GameWorldSenseUtilityLayout.identifier(action))
            let area = drawable.addTrackingArea(identifier: identifier)
            area.addHoverEffect(.automatic)
            return (action, UInt32(area.renderValue.rawValue))
        } : []
        var cleared = Set<String>()
        for (eye, view) in drawable.views.enumerated() {
            let map = view.textureMap
            guard tracking.indices.contains(map.textureIndex) else { continue }
            let first = cleared.insert("\(map.textureIndex):\(map.sliceIndex)").inserted
            let pass = MTLRenderPassDescriptor()
            if !areas.isEmpty {
                pass.colorAttachments[0].texture = drawable.colorTextures[map.textureIndex]
                pass.colorAttachments[0].slice = map.sliceIndex
                pass.colorAttachments[0].loadAction = .load
                pass.colorAttachments[0].storeAction = .store
                pass.colorAttachments[1].texture = tracking[map.textureIndex]
                pass.colorAttachments[1].slice = map.sliceIndex
                pass.colorAttachments[1].loadAction = first ? .clear : .load
                pass.colorAttachments[1].storeAction = .store
                pass.colorAttachments[1].clearColor = MTLClearColorMake(0, 0, 0, 0)
                pass.depthAttachment.texture = drawable.depthTextures[map.textureIndex]
                pass.depthAttachment.slice = map.sliceIndex
                pass.depthAttachment.loadAction = .load
                pass.depthAttachment.storeAction = .store
            } else {
                // Clear old interaction regions even after a menu, disconnect,
                // or tracking loss hides every button.
                pass.colorAttachments[0].texture = tracking[map.textureIndex]
                pass.colorAttachments[0].slice = map.sliceIndex
                pass.colorAttachments[0].loadAction = first ? .clear : .load
                pass.colorAttachments[0].storeAction = .store
                pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
            }
            guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { continue }
            encoder.label = "Sense utilities: eye \(eye)"
            encoder.setViewport(map.viewport)
            if !areas.isEmpty {
                encoder.setRenderPipelineState(pipeline)
                encoder.setDepthStencilState(depth)
                encoder.setCullMode(.none)
                encoder.setFragmentTexture(atlas, index: 0)
                let projection = drawable.computeProjection(convention: .rightUpBack, viewIndex: eye)
                for (action, value) in areas {
                    guard let index = GameWorldSenseUtilityLayout.actions.firstIndex(of: action) else { continue }
                    var uniforms = Uniforms(clipFromHead: projection * view.transform.inverse,
                        center: GameWorldSenseUtilityLayout.center(index: index),
                        halfSize: GameWorldSenseUtilityLayout.halfSize,
                        distance: GameWorldSenseUtilityLayout.distance,
                        atlasIndex: UInt32(index), trackingValue: value)
                    encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
                    encoder.setFragmentBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 0)
                    encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
                }
            }
            encoder.endEncoding()
        }
    }

    static func clearTracking(drawable: LayerRenderer.Drawable, commands: any MTLCommandBuffer) {
        let textures = drawable.trackingAreasTextures
        var cleared = Set<String>()
        for view in drawable.views {
            let map = view.textureMap
            guard textures.indices.contains(map.textureIndex),
                  cleared.insert("\(map.textureIndex):\(map.sliceIndex)").inserted else { continue }
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = textures[map.textureIndex]
            pass.colorAttachments[0].slice = map.sliceIndex
            pass.colorAttachments[0].loadAction = .clear
            pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
            commands.makeRenderCommandEncoder(descriptor: pass)?.endEncoding()
        }
    }
}
#endif

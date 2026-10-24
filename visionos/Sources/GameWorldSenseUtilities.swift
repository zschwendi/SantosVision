import Foundation
import simd

enum GameWorldSenseUtilityLayout {
    static let distance: Float = 2
    static let halfSize = SIMD2<Float>(repeating: 0.0675)
    static let horizontalPitch: Float = 0.15
    // The current 60-degree, 16:9 HUD shares this 2m plane. Keep the
    // two utility buttons centered and clear of the HUD corners.
    static let verticalCenter: Float = -0.56
    static let namespace: UInt64 = 0x5356555400000000
    static var actions: [SantosSenseUtilityAction] { [.phone, .changeView] }
    static var atlasActions: [SantosSenseUtilityAction] { [.phone, .changeView, .up, .down, .left, .right] }
    static func visibleActions(phoneOpen: Bool) -> [SantosSenseUtilityAction] { phoneOpen ? atlasActions : actions }
    static func identifier(_ action: SantosSenseUtilityAction) -> UInt64 { namespace | UInt64(action.rawValue) }
    static func action(identifier: UInt64) -> SantosSenseUtilityAction? {
        guard identifier & 0xffff_ffff_0000_0000 == namespace else { return nil }
        guard let action = SantosSenseUtilityAction(rawValue: UInt32(truncatingIfNeeded: identifier)),
              atlasActions.contains(action) else { return nil }
        return action
    }
    static func center(index: Int) -> SIMD2<Float> {
        let midpoint = Float(actions.count - 1) * 0.5
        return SIMD2((Float(index) - midpoint) * horizontalPitch, verticalCenter)
    }
    // Peel's exact 48x34-point arrow buttons with 10-point gaps, on the same
    // scale as the 44-point main control. Up sits above Left / Down / Right.
    static let metresPerPoint: Float = 0.135 / 44
    static func halfSize(for action: SantosSenseUtilityAction) -> SIMD2<Float> {
        action.isDirection ? SIMD2(24, 17) * metresPerPoint : halfSize
    }
    static func center(for action: SantosSenseUtilityAction, phoneOpen: Bool) -> SIMD2<Float> {
        guard phoneOpen else { return center(index: action == .phone ? 0 : 1) }
        let pitch = 58 * metresPerPoint
        let mainX = 82 * metresPerPoint + 10 * metresPerPoint + halfSize.x
        switch action {
        case .phone: return SIMD2(-mainX, verticalCenter)
        case .changeView: return SIMD2(mainX, verticalCenter)
        case .up: return SIMD2(0, verticalCenter + 44 * metresPerPoint)
        case .down: return SIMD2(0, verticalCenter)
        case .left: return SIMD2(-pitch, verticalCenter)
        case .right: return SIMD2(pitch, verticalCenter)
        }
    }
}

struct GameWorldSenseUtilityInteraction<ID: Hashable> {
    private var pressed: [ID: SantosSenseUtilityAction] = [:]
    private var cancelled = Set<ID>()
    private var terminalTimes: [ID: Double] = [:]
    private var terminalOrder: [ID] = []
    var isActive: Bool { !pressed.isEmpty }
    var activeActions: Set<SantosSenseUtilityAction> { Set(pressed.values) }
    mutating func begin(_ id: ID, action: SantosSenseUtilityAction) { update(id, action: action) }
    mutating func update(_ id: ID, action: SantosSenseUtilityAction?) {
        guard !cancelled.contains(id) else { return }
        terminalTimes.removeValue(forKey: id)
        terminalOrder.removeAll { $0 == id }
        if let original = pressed[id] {
            if action.map({ $0 != original }) == true || (action == nil && original.isDirection) {
                pressed.removeValue(forKey: id); cancelled.insert(id)
            }
        } else if let action { pressed[id] = action }
    }
    mutating func end(_ id: ID, action: SantosSenseUtilityAction?, timestamp: Double = 0) -> SantosSenseUtilityAction? {
        guard timestamp.isFinite, terminalTimes[id] != timestamp else { return nil }
        let wasCancelled = cancelled.remove(id) != nil
        let original = pressed.removeValue(forKey: id)
        rememberTerminal(id, timestamp: timestamp)
        // The system may provide the tracking-area hit only on the terminal
        // event. Apple’s compositor sample activates that valid ended ID;
        // requiring an earlier active hit silently discards legitimate clicks.
        guard !wasCancelled, let action, original == nil || original == action else { return nil }
        return action
    }
    mutating func cancel(_ id: ID, timestamp: Double = 0) {
        pressed.removeValue(forKey: id); cancelled.remove(id)
        rememberTerminal(id, timestamp: timestamp)
    }
    private mutating func rememberTerminal(_ id: ID, timestamp: Double) {
        terminalTimes[id] = timestamp
        terminalOrder.removeAll { $0 == id }
        terminalOrder.append(id)
        if terminalOrder.count > 64 { terminalTimes.removeValue(forKey: terminalOrder.removeFirst()) }
    }
    mutating func suspend() {
        cancelled.formUnion(pressed.keys)
        pressed.removeAll(keepingCapacity: true)
    }
    mutating func rejectUntilEnd(_ id: ID) { pressed.removeValue(forKey: id); cancelled.insert(id) }
    mutating func clear() {
        pressed.removeAll(keepingCapacity: true); cancelled.removeAll(keepingCapacity: true)
        terminalTimes.removeAll(keepingCapacity: true); terminalOrder.removeAll(keepingCapacity: true)
    }
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
    private var phoneOpen = false
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
        let size = CGSize(width: 896, height: 128)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            for index in 0..<7 {
                let action = index == 6 ? SantosSenseUtilityAction.phone : GameWorldSenseUtilityLayout.atlasActions[index]
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
                if index == 6 {
                    UIImage(systemName: "xmark.circle.fill", withConfiguration:
                        UIImage.SymbolConfiguration(pointSize: 24, weight: .semibold))?
                        .withTintColor(.white, renderingMode: .alwaysOriginal)
                        .draw(in: CGRect(x: x + 73, y: 56, width: 28, height: 28))
                }
                let title = index == 6 ? "Close" : action == .changeView ? "View" : action == .phone ? "Phone" : ""
                let attributes: [NSAttributedString.Key: Any] = [.font: UIFont.systemFont(ofSize: 20, weight: .medium),
                    .foregroundColor: UIColor.white]
                let text = NSAttributedString(string: title, attributes: attributes)
                text.draw(at: CGPoint(x: x + (128 - text.size().width) / 2, y: 88))
            }
        }
        guard let cg = image.cgImage else { return nil }
        var pixels = [UInt8](repeating: 0, count: 896 * 128 * 4)
        let rendered = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: 896, height: 128,
                bitsPerComponent: 8, bytesPerRow: 896 * 4, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(cg, in: CGRect(origin: .zero, size: size))
            return true
        }
        guard rendered else { return nil }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
            width: 896, height: 128, mipmapped: false)
        descriptor.usage = .shaderRead
        descriptor.storageMode = .shared
        guard let texture = device.makeTexture(descriptor: descriptor) else { return nil }
        pixels.withUnsafeBytes { bytes in
            texture.replace(region: MTLRegionMake2D(0, 0, 896, 128), mipmapLevel: 0,
                withBytes: bytes.baseAddress!, bytesPerRow: 896 * 4)
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
    func setPhoneOpen(_ value: Bool) { lock.lock(); phoneOpen = value; lock.unlock() }

    func encode(drawable: LayerRenderer.Drawable, commands: any MTLCommandBuffer, tracked: Bool) {
        lock.lock(); let show = visible && tracked, phoneOpen = self.phoneOpen; lock.unlock()
        let tracking = drawable.trackingAreasTextures
        guard !tracking.isEmpty else { return }
        let areas = show ? GameWorldSenseUtilityLayout.visibleActions(phoneOpen: phoneOpen).map { action -> (SantosSenseUtilityAction, UInt32) in
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
                    guard let index = GameWorldSenseUtilityLayout.atlasActions.firstIndex(of: action) else { continue }
                    var uniforms = Uniforms(clipFromHead: projection * view.transform.inverse,
                        center: GameWorldSenseUtilityLayout.center(for: action, phoneOpen: phoneOpen),
                        halfSize: GameWorldSenseUtilityLayout.halfSize(for: action),
                        distance: GameWorldSenseUtilityLayout.distance,
                        atlasIndex: UInt32(action == .phone && phoneOpen ? 6 : index), trackingValue: value)
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

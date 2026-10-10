import ARKit
import CompositorServices
import Foundation
import Metal
import SwiftUI
import simd

struct CalibrationConfiguration: CompositorLayerConfiguration {
    func makeConfiguration(capabilities: LayerRenderer.Capabilities,
                           configuration: inout LayerRenderer.Configuration) {
        configuration.colorFormat = .bgra8Unorm_srgb
        configuration.depthFormat = .depth32Float
        configuration.isFoveationEnabled = false
        let layouts = capabilities.supportedLayouts(options: [])
        // Separate passes use each view's texture map, including array slices.
        configuration.layout = layouts.contains(.dedicated) ? .dedicated
            : layouts.contains(.layered) ? .layered : .shared
        // CompositorServices uses reverse-Z: far first, near second.
        configuration.defaultDepthRange = SIMD2<Float>(100, 0.1)
    }
}

private enum DiagnosticError: LocalizedError {
    case unsupportedTracking, missingMetalResource, missingShader
    var errorDescription: String? {
        switch self {
        case .unsupportedTracking:
            return "World tracking is unavailable. A physical Vision Pro is required for the head tracking check."
        case .missingMetalResource:
            return "Metal could not allocate the calibration renderer."
        case .missingShader:
            return "The calibration Metal shaders are missing."
        }
    }
}

private struct CalibrationVertex {
    var position: SIMD4<Float>
    var color: SIMD4<Float>
}

enum CalibrationEvent: Sendable {
    case status(String)
    case ended
}

/// Owns one diagnostic renderer. No GTA engine or game symbols are linked.
/// Only the render thread touches frame state and calibration geometry.
final class CalibrationRenderer: @unchecked Sendable {
    private let layer: LayerRenderer
    private let session = ARKitSession()
    private let tracking = WorldTrackingProvider()
    private let queue: MTLCommandQueue
    private let pipeline: MTLRenderPipelineState
    private let depthState: MTLDepthStencilState
    private let vertices: MTLBuffer
    private let vertexCount: Int
    private let report: @Sendable (CalibrationEvent) -> Void
    private let stopLock = NSLock()
    private var stopped = false
    private let inFlight = DispatchSemaphore(value: 3)
    private var originFromCalibration: simd_float4x4?
    private var hasReportedTracking = false
    private var lastMissingPoseReport: TimeInterval = 0

    init(layer: LayerRenderer, report: @escaping @Sendable (CalibrationEvent) -> Void) throws {
        self.layer = layer
        self.report = report
        let device = layer.device
        guard let queue = device.makeCommandQueue(),
              let library = device.makeDefaultLibrary(),
              let vertex = library.makeFunction(name: "calibrationVertex"),
              let fragment = library.makeFunction(name: "calibrationFragment") else {
            throw DiagnosticError.missingShader
        }
        self.queue = queue
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "SantosVision calibration per-eye pipeline"
        descriptor.vertexFunction = vertex
        descriptor.fragmentFunction = fragment
        descriptor.colorAttachments[0].pixelFormat = layer.configuration.colorFormat
        descriptor.depthAttachmentPixelFormat = layer.configuration.depthFormat
        pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
        let depthDescriptor = MTLDepthStencilDescriptor()
        depthDescriptor.depthCompareFunction = .greaterEqual
        depthDescriptor.isDepthWriteEnabled = true
        guard let depth = device.makeDepthStencilState(descriptor: depthDescriptor) else {
            throw DiagnosticError.missingMetalResource
        }
        depthState = depth
        let geometry = Self.makeCalibrationWorld()
        vertexCount = geometry.count
        guard let buffer = device.makeBuffer(bytes: geometry,
                                             length: geometry.count * MemoryLayout<CalibrationVertex>.stride,
                                             options: .storageModeShared) else {
            throw DiagnosticError.missingMetalResource
        }
        buffer.label = "Calibration grid and world markers"
        vertices = buffer
    }

    func start() async throws {
        guard WorldTrackingProvider.isSupported else { throw DiagnosticError.unsupportedTracking }
        try Task.checkCancellation()
        try await session.run([tracking])
        try Task.checkCancellation()
        guard !isStopped else { session.stop(); return }
        report(.status("Waiting for a tracked headset pose. Calibration scene only."))
        let thread = Thread { [self] in renderLoop() }
        thread.name = "SantosVision immersive calibration renderer"
        thread.qualityOfService = .userInteractive
        thread.start()
    }

    func stop() {
        stopLock.lock()
        stopped = true
        stopLock.unlock()
        session.stop()
    }

    private var isStopped: Bool {
        stopLock.lock()
        defer { stopLock.unlock() }
        return stopped
    }

    private func renderLoop() {
        defer {
            session.stop()
            report(.ended)
        }
        while !isStopped {
            switch layer.state {
            case .invalidated:
                report(.status("Immersive space ended. Tracking renderer released."))
                return
            case .paused:
                // A short bounded wait allows explicit stop even while paused.
                Thread.sleep(forTimeInterval: 0.01)
            case .running:
                autoreleasepool { renderFrame() }
            @unknown default:
                report(.status("Unknown compositor state; tracking renderer stopped."))
                return
            }
        }
    }

    private func renderFrame() {
        guard let frame = layer.queryNextFrame() else {
            Thread.sleep(forTimeInterval: 0.001)
            return
        }
        frame.startUpdate()
        frame.endUpdate()
        guard let timing = frame.predictTiming() else { return }
        LayerRenderer.Clock().wait(until: timing.optimalInputTime)
        guard !isStopped, layer.state == .running else { return }
        // A finite wait bounds shutdown; no allocation changes while GPU reads it.
        guard inFlight.wait(timeout: .now() + 0.1) == .success else { return }
        frame.startSubmission()
        let drawables = frame.queryDrawables()
        // An empty query invalidates the frame; never access it again, even to end submission.
        guard !drawables.isEmpty else {
            inFlight.signal()
            return
        }
        defer { frame.endSubmission() }
        guard let commands = queue.makeCommandBuffer() else {
            inFlight.signal()
            return
        }
        commands.label = "SantosVision calibration frame"
        for drawable in drawables {
            // Query AFTER optimalInputTime, using the drawable's presentation time.
            let duration = LayerRenderer.Clock.Instant.epoch.duration(to: drawable.frameTiming.presentationTime)
            let timestamp = Double(duration.components.seconds)
                + Double(duration.components.attoseconds) / 1e18
            let anchor = tracking.state == .running
                ? tracking.queryDeviceAnchor(atTimestamp: timestamp) : nil
            let trackedAnchor = anchor?.isTracked == true ? anchor : nil
            drawable.deviceAnchor = trackedAnchor
            if let trackedAnchor, originFromCalibration == nil {
                var transform = matrix_identity_float4x4
                // Place the test world around the first tracked head position.
                transform.columns.3 = trackedAnchor.originFromAnchorTransform.columns.3
                originFromCalibration = transform
            }
            if trackedAnchor != nil, !hasReportedTracking {
                report(.status("Headset pose acquired. Rendering the stereo calibration world with depth."))
                hasReportedTracking = true
            } else if trackedAnchor == nil, timestamp - lastMissingPoseReport > 2 {
                report(.status("Headset pose unavailable; scene drawing is suspended until tracking returns."))
                lastMissingPoseReport = timestamp
                hasReportedTracking = false
            }
            encode(drawable: drawable, anchor: trackedAnchor, commands: commands)
            drawable.encodePresent(commandBuffer: commands)
        }
        commands.addCompletedHandler { [inFlight, report] buffer in
            inFlight.signal()
            if buffer.status == .error {
                report(.status("Metal frame failed: \(buffer.error?.localizedDescription ?? "unknown GPU error")"))
            }
        }
        commands.commit()
    }

    private func encode(drawable: LayerRenderer.Drawable,
                        anchor: DeviceAnchor?, commands: MTLCommandBuffer) {
        var clearedTextures = Set<String>()
        for (eyeIndex, view) in drawable.views.enumerated() {
            let map = view.textureMap
            let key = "\(map.textureIndex):\(map.sliceIndex)"
            let firstPass = clearedTextures.insert(key).inserted
            let pass = MTLRenderPassDescriptor()
            pass.colorAttachments[0].texture = drawable.colorTextures[map.textureIndex]
            pass.colorAttachments[0].slice = map.sliceIndex
            pass.colorAttachments[0].loadAction = firstPass ? .clear : .load
            pass.colorAttachments[0].storeAction = .store
            pass.colorAttachments[0].clearColor = MTLClearColor(red: 0.012, green: 0.018, blue: 0.03, alpha: 1)
            pass.depthAttachment.texture = drawable.depthTextures[map.textureIndex]
            pass.depthAttachment.slice = map.sliceIndex
            pass.depthAttachment.loadAction = firstPass ? .clear : .load
            pass.depthAttachment.storeAction = .store
            pass.depthAttachment.clearDepth = 0 // reverse-Z
            guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { continue }
            encoder.label = "Calibration eye \(eyeIndex)"
            encoder.setViewport(map.viewport)
            if let anchor, let world = originFromCalibration {
                let originFromEye = anchor.originFromAnchorTransform * view.transform
                var mvp = drawable.computeProjection(convention: .rightUpBack, viewIndex: eyeIndex)
                    * simd_inverse(originFromEye) * world
                encoder.setRenderPipelineState(pipeline)
                encoder.setDepthStencilState(depthState)
                encoder.setCullMode(.none)
                encoder.setVertexBuffer(vertices, offset: 0, index: 0)
                encoder.setVertexBytes(&mvp, length: MemoryLayout<simd_float4x4>.stride, index: 1)
                encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: vertexCount)
            }
            encoder.endEncoding()
        }
    }

    private static func makeCalibrationWorld() -> [CalibrationVertex] {
        var result: [CalibrationVertex] = []
        func quad(_ a: SIMD3<Float>, _ b: SIMD3<Float>, _ c: SIMD3<Float>,
                  _ d: SIMD3<Float>, _ color: SIMD4<Float>) {
            for p in [a, b, c, a, c, d] {
                result.append(CalibrationVertex(position: SIMD4(p, 1), color: color))
            }
        }
        func box(center: SIMD3<Float>, size: SIMD3<Float>, color: SIMD4<Float>) {
            let lo = center - size / 2, hi = center + size / 2
            let a = SIMD3(lo.x, lo.y, lo.z), b = SIMD3(hi.x, lo.y, lo.z)
            let c = SIMD3(hi.x, hi.y, lo.z), d = SIMD3(lo.x, hi.y, lo.z)
            let e = SIMD3(lo.x, lo.y, hi.z), f = SIMD3(hi.x, lo.y, hi.z)
            let g = SIMD3(hi.x, hi.y, hi.z), h = SIMD3(lo.x, hi.y, hi.z)
            quad(a, b, c, d, color); quad(f, e, h, g, color * SIMD4(0.7, 0.7, 0.7, 1))
            quad(e, a, d, h, color * SIMD4(0.85, 0.85, 0.85, 1)); quad(b, f, g, c, color)
            quad(d, c, g, h, color); quad(e, f, b, a, color)
        }
        let floor: Float = -1.5
        quad(SIMD3(-12, floor, -12), SIMD3(12, floor, -12),
             SIMD3(12, floor, 12), SIMD3(-12, floor, 12), SIMD4(0.025, 0.035, 0.05, 1))
        for line in -12...12 {
            let p = Float(line)
            let color = SIMD4<Float>(0.12, 0.18, 0.24, 1)
            box(center: SIMD3(p, floor + 0.005, 0), size: SIMD3(0.012, 0.006, 24), color: color)
            box(center: SIMD3(0, floor + 0.005, p), size: SIMD3(24, 0.006, 0.012), color: color)
        }
        // X = red, Y = green, Z = blue; real objects with occlusion cues.
        box(center: SIMD3(2, -0.5, 0), size: SIMD3(0.18, 2, 0.18), color: SIMD4(0.85, 0.12, 0.09, 1))
        box(center: SIMD3(0, -0.5, -3), size: SIMD3(0.18, 2, 0.18), color: SIMD4(0.15, 0.85, 0.25, 1))
        box(center: SIMD3(0, -0.5, 3), size: SIMD3(0.18, 2, 0.18), color: SIMD4(0.14, 0.35, 0.95, 1))
        box(center: SIMD3(-3, -1, 0), size: SIMD3(1, 1, 1), color: SIMD4(0.95, 0.65, 0.1, 1))
        box(center: SIMD3(1, -1.1, -2), size: SIMD3(0.8, 0.8, 0.8), color: SIMD4(0.5, 0.3, 0.85, 1))
        return result
    }
}

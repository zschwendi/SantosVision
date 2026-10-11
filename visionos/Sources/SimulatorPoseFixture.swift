import Foundation
import simd

/// Explicit simulator test input. Never available in a physical-device build.
/// The arguments are rotation/translation amplitudes; the slowly varying pose
/// starts at identity so the native camera hook can establish its reference.
final class SimulatorPoseFixture: @unchecked Sendable {
    static let shared: SimulatorPoseFixture? = {
        #if targetEnvironment(simulator)
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--sv-simulate-head-pose"),
              index + 1 < arguments.count else { return nil }
        let components = arguments[index + 1].split(separator: ",").compactMap { Float($0) }
        guard components.count == 2, components.allSatisfy(\.isFinite),
              abs(components[0]) <= 180, abs(components[1]) <= 89 else { return nil }
        var position = SIMD3<Float>(repeating: 0)
        var roll: Float = 0
        if let positionIndex = arguments.firstIndex(of: "--sv-simulate-head-position") {
            guard positionIndex + 1 < arguments.count else { return nil }
            let values = arguments[positionIndex + 1].split(separator: ",").compactMap { Float($0) }
            guard values.count == 3, values.allSatisfy({ $0.isFinite && abs($0) <= 2 }) else { return nil }
            position = SIMD3(values[0], values[1], values[2])
        }
        if let rollIndex = arguments.firstIndex(of: "--sv-simulate-head-roll") {
            guard rollIndex + 1 < arguments.count, let value = Float(arguments[rollIndex + 1]),
                  value.isFinite, abs(value) <= 180 else { return nil }
            roll = value
        }
        return SimulatorPoseFixture(yaw: components[0], pitch: components[1], roll: roll, position: position)
        #else
        return nil
        #endif
    }()

    private let yaw: Float
    private let pitch: Float
    private let roll: Float
    private let position: SIMD3<Float>
    private let lock = NSLock()
    private var firstTimestamp: Double?

    private init(yaw: Float, pitch: Float, roll: Float, position: SIMD3<Float>) {
        self.yaw = yaw * .pi / 180
        self.pitch = pitch * .pi / 180
        self.roll = roll * .pi / 180
        self.position = position
    }

    var reportValue: [String: Any] {
        ["yaw_amplitude_degrees": yaw * 180 / .pi, "pitch_amplitude_degrees": pitch * 180 / .pi,
         "roll_amplitude_degrees": roll * 180 / .pi,
         "position_amplitude_meters": [position.x, position.y, position.z]]
    }

    func pose(at timestamp: Double) -> simd_float4x4 {
        lock.lock()
        let first = firstTimestamp ?? timestamp
        firstTimestamp = first
        lock.unlock()
        let elapsed = Float(max(0, timestamp - first))
        let turn = simd_quatf(angle: yaw * sin(elapsed * 0.5), axis: SIMD3(0, 1, 0))
        let tilt = simd_quatf(angle: pitch * sin(elapsed * 0.35), axis: SIMD3(1, 0, 0))
        let rotation = roll == 0 ? turn * tilt
            : turn * tilt * simd_quatf(angle: roll * sin(elapsed * 0.31), axis: SIMD3(0, 0, 1))
        var matrix = simd_float4x4(rotation)
        matrix.columns.3 = SIMD4(position.x * sin(elapsed * 0.27), position.y * sin(elapsed * 0.19),
                                 position.z * sin(elapsed * 0.23), 1)
        return matrix
    }
}

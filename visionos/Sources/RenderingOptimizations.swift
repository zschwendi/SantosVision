import Foundation

/// Launcher choices are read before the native engine or compositor renderer starts.
/// Explicit 1/0 environment values are reserved for reproducible diagnostic runs.
enum RenderingOptimizations {
    static let adaptiveKey = "SantosVisionAdaptiveRenderingEnabled"
    static let frozenDepthKey = "SantosVisionFrozenDepthHandoffEnabled"

    static var storedAdaptive: Bool { UserDefaults.standard.bool(forKey: adaptiveKey) }
    static var storedFrozenDepth: Bool {
        guard UserDefaults.standard.object(forKey: frozenDepthKey) != nil else { return true }
        return UserDefaults.standard.bool(forKey: frozenDepthKey)
    }

    private static func resolved(_ name: String, stored: Bool) -> Bool {
        switch ProcessInfo.processInfo.environment[name] {
        case "1": true
        case "0": false
        default: stored
        }
    }

    static var adaptiveMeshEnabled: Bool {
        resolved("MUGUET_COMPOSITOR_ADAPTIVE_MESH", stored: storedAdaptive)
    }

    static var skyFanEnabled: Bool {
        resolved("MUGUET_COMPOSITOR_ADAPTIVE_SKY_FAN", stored: storedAdaptive)
    }
}

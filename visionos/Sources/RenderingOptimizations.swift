import Foundation

/// Launcher choices are read before the native engine or compositor renderer starts.
/// Explicit 1/0 environment values are reserved for reproducible diagnostic runs.
enum RenderingOptimizations {
    static let adaptiveKey = "SantosVisionAdaptiveRenderingEnabled"
    static let frozenDepthKey = "SantosVisionFrozenDepthHandoffEnabled"
    static let drawSetupKey = "SantosVisionDrawSetupOverheadEnabled"

    static var storedAdaptive: Bool { UserDefaults.standard.bool(forKey: adaptiveKey) }
    static var storedFrozenDepth: Bool {
        guard UserDefaults.standard.object(forKey: frozenDepthKey) != nil else { return true }
        return UserDefaults.standard.bool(forKey: frozenDepthKey)
    }
    static var storedDrawSetup: Bool { UserDefaults.standard.bool(forKey: drawSetupKey) }

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

    static var renderPrepareEnabled: Bool {
        resolved("MUGUET_RENDER_PREPARE_OPT", stored: storedDrawSetup)
    }

    static var uniformStagePackEnabled: Bool {
        resolved("MUGUET_UNIFORM_STAGE_PACK_OPT", stored: storedDrawSetup)
    }

    static var uniformSnapshotEnabled: Bool {
        resolved("MUGUET_UNIFORM_SNAPSHOT_CACHE", stored: storedDrawSetup)
    }

    static var xrEmissionFastpathEnabled: Bool {
        resolved("MUGUET_XR_EMISSION_FASTPATH", stored: storedDrawSetup)
    }

    static var bindGroupPrepareEnabled: Bool {
        resolved("MUGUET_BIND_GROUP_PREPARE_OPT", stored: storedDrawSetup)
    }

    static var metalSkipEmptyTransitEnabled: Bool {
        resolved("MUGUET_METAL_SKIP_EMPTY_TRANSIT", stored: storedDrawSetup)
    }

    static var metalSkipEmptyPrepassEnabled: Bool {
        resolved("MUGUET_METAL_SKIP_EMPTY_PREPASS", stored: storedDrawSetup)
    }
}

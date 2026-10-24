import Foundation

@main
struct NativeGraphicsSettingsValidation {
    static func main() throws {
        var checks = 0
        func check(_ value: @autoclosure () throws -> Bool, _ message: String) rethrows {
            let result = try value()
            precondition(result, message); checks += 1
        }
        func rejects(_ operation: () throws -> Void) {
            do { try operation(); preconditionFailure("invalid graphics selection accepted") }
            catch { checks += 1 }
        }
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("graphics-quality-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appendingPathComponent("native-graphics.json")
        let defaults = try NativeGraphicsSettings.load(from: path)
        check(defaults.width == 960 && defaults.height == 540 && defaults.fpsLimit == 60 && defaults.preset == .existing,
              "quality options preserve default resolution and cap")
        for preset in NativeGraphicsSettings.Preset.allCases {
            for resolution in NativeGraphicsSettings.Resolution.allCases {
                for cap in [30, 60] {
                    var settings = defaults
                    try settings.apply(arguments: ["--sv-resolution", resolution.rawValue, "--sv-fps-limit", "\(cap)", "--sv-graphics-preset", preset.rawValue])
                    try settings.save(to: path)
                    try check(NativeGraphicsSettings.load(from: path) == settings, "all supported quality/resolution/cap selections persist")
                    check(settings.summary.contains("selected"), "selection is not claimed to be actual GTA quality")
                }
            }
        }
        let legacy = Data(#"{"width":1920,"height":1080,"fps_limit":60,"graphics_preset":"low"}"#.utf8)
        try legacy.write(to: path)
        let migrated = try NativeGraphicsSettings.load(from: path)
        check(migrated.preset == .existing && migrated.graphicsPreset == "existing", "legacy low does not reapply Performance")
        check(migrated.width == 1920 && migrated.height == 1080, "migration keeps resolution")
        try migrated.save(to: path)
        let saved = try Data(contentsOf: path)
        var invalid = migrated; invalid.graphicsPreset = "ultra"
        rejects { try invalid.save(to: path) }
        try check(Data(contentsOf: path) == saved, "failed validation preserves prior atomic file")
        for arguments in [["--sv-graphics-preset", "ultra"], ["--sv-graphics-preset"], ["--sv-fps-limit", "90"], ["--sv-resolution", "2560x1080"]] {
            rejects { var settings = defaults; try settings.apply(arguments: arguments) }
        }
        try Data("broken".utf8).write(to: path)
        rejects { _ = try NativeGraphicsSettings.load(from: path) }
        try Data(repeating: 32, count: 65_537).write(to: path)
        rejects { _ = try NativeGraphicsSettings.load(from: path) }
        print("PASS: \(checks) native graphics profile, CLI, migration, persistence and invalid-write checks")
    }
}

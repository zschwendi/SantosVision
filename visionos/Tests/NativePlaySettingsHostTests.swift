import Foundation

@main
struct NativePlaySettingsHostTests {
    static func main() throws {
        let manager = FileManager.default
        let root = manager.temporaryDirectory.appendingPathComponent("santos-play-settings-\(UUID().uuidString)")
        try manager.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: root) }

        let legacy = root.appendingPathComponent("legacy")
        try manager.createDirectory(at: legacy, withIntermediateDirectories: true)
        let audio = legacy.appendingPathComponent("native-audio.json")
        let original = Data("{\"master_volume\":0,\"other_setting\":17}\n".utf8)
        try original.write(to: audio)
        var settings = try NativePlaySettings.load(from: legacy)
        precondition(settings.masterVolume == 1 && settings.lookSensitivityPercent == 75)
        let backups = try manager.contentsOfDirectory(at: legacy, includingPropertiesForKeys: nil)
            .filter { $0.lastPathComponent.hasPrefix("native-audio-before-restore-") }
        precondition(backups.count == 1)
        let backupData = try Data(contentsOf: backups[0])
        precondition(backupData == original)
        let migrated = try JSONSerialization.jsonObject(with: Data(contentsOf: audio)) as! [String: Any]
        precondition(migrated["other_setting"] as? Int == 17)

        // A user choosing mute after this migration must stay muted on the next
        // process launch; customized controller sensitivity must survive too.
        settings.masterVolume = 0
        settings.lookSensitivityPercent = 40
        try settings.save(to: legacy)
        let reloaded = try NativePlaySettings.load(from: legacy)
        precondition(reloaded == settings)
        let laterBackups = try manager.contentsOfDirectory(at: legacy, includingPropertiesForKeys: nil)
            .filter { $0.lastPathComponent.hasPrefix("native-audio-before-restore-") }
        precondition(laterBackups.count == 1)

        let fresh = root.appendingPathComponent("fresh")
        let initial = try NativePlaySettings.load(from: fresh)
        precondition(initial == NativePlaySettings())
        try initial.save(to: fresh)
        let nativeController = try JSONSerialization.jsonObject(
            with: Data(contentsOf: fresh.appendingPathComponent("native-controller.json"))) as! [String: Any]
        precondition(nativeController["look_sensitivity_percent"] as? Double == 75)

        let custom = root.appendingPathComponent("custom")
        try manager.createDirectory(at: custom, withIntermediateDirectories: true)
        try Data("{\"master_volume\":0.5}".utf8).write(to: custom.appendingPathComponent("native-audio.json"))
        let customSettings = try NativePlaySettings.load(from: custom)
        precondition(customSettings.masterVolume == 0.5)

        for invalid in ["{\"look_sensitivity_percent\":101}", "{\"look_sensitivity_percent\":true}", "{\"look_sensitivity_percent\":\"75\"}"] {
            try Data(invalid.utf8).write(to: fresh.appendingPathComponent("native-controller.json"))
            do { _ = try NativePlaySettings.load(from: fresh); preconditionFailure("Invalid controller setting accepted") }
            catch { }
        }
        print("Native play settings host tests passed: mute migration backup, preserved future choices, defaults, exact native JSON, validation")
    }
}

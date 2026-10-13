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

        // Developer mode is opt-in and persists without altering unrelated
        // gameplay preferences. Switching back must restore normal Story Mode.
        precondition(initial.developerMode == false)
        let gameplayURL = fresh.appendingPathComponent("native-gameplay.json")
        try Data("{\"developer_mode\":false,\"other_setting\":9}".utf8).write(to: gameplayURL)
        var developerSettings = try NativePlaySettings.load(from: fresh)
        developerSettings.developerMode = true
        try developerSettings.save(to: fresh)
        let developerReload = try NativePlaySettings.load(from: fresh)
        precondition(developerReload.developerMode)
        precondition(developerReload.reportValue["game_start_mode"] as? String == "sandbox")
        let gameplayObject = try JSONSerialization.jsonObject(with: Data(contentsOf: gameplayURL)) as! [String: Any]
        precondition(gameplayObject["other_setting"] as? Int == 9)
        developerSettings.developerMode = false
        try developerSettings.save(to: fresh)
        let storyReload = try NativePlaySettings.load(from: fresh)
        precondition(!storyReload.developerMode)
        precondition(storyReload.reportValue["game_start_mode"] as? String == "story")
        for invalid in ["{\"developer_mode\":1}", "{\"developer_mode\":\"true\"}", "{\"developer_mode\":null}"] {
            try Data(invalid.utf8).write(to: gameplayURL)
            do { _ = try NativePlaySettings.load(from: fresh); preconditionFailure("Invalid developer setting accepted") }
            catch { }
        }
        try developerSettings.save(to: fresh)

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
        print("Native play settings host tests passed: mute migration backup, preserved future choices, defaults, exact native JSON, developer mode persistence and validation")
    }
}

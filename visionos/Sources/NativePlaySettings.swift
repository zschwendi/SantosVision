import Foundation
import CoreFoundation

/// Host audio gain and right-stick speed. These are separate from GTA's own
/// settings, whose native save files remain under the game's control.
struct NativePlaySettings: Equatable, Sendable {
    var masterVolume = 1.0
    /// Separate from masterVolume so unmuting restores the chosen level.
    var mute = false
    var lookSensitivityPercent = 75.0
    var developerMode = false

    private static let audioName = "native-audio.json"
    private static let controllerName = "native-controller.json"
    private static let gameplayName = "native-gameplay.json"
    private static let audioMigrationName = "native-audio-restored-v1.json"

    var reportValue: [String: Any] {
        ["master_volume": masterVolume, "mute": mute,
         "controller_look_sensitivity_percent": lookSensitivityPercent,
         "controller_look_semantics": "percent_of_original_host_right_stick_speed",
         "developer_mode": developerMode,
         "game_start_mode": developerMode ? "sandbox" : "story"]
    }

    static func load(from root: URL) throws -> Self {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        var audio = try readObject(root.appendingPathComponent(audioName))
        let migration = root.appendingPathComponent(audioMigrationName)
        if !FileManager.default.fileExists(atPath: migration.path) {
            // The prior app deliberately persisted zero for Zach's temporary
            // mute. Back it up and undo that once, not on every subsequent boot.
            if let volume = try number(audio, key: "master_volume", range: 0...1), volume == 0 {
                let original = root.appendingPathComponent(audioName)
                let backup = root.appendingPathComponent("native-audio-before-restore-\(UUID().uuidString).json")
                try FileManager.default.copyItem(at: original, to: backup)
                audio["master_volume"] = 1.0
                try writeObject(audio, to: original)
            }
            try writeObject(["schema": 1], to: migration)
        }
        let controller = try readObject(root.appendingPathComponent(controllerName))
        let gameplay = try readObject(root.appendingPathComponent(gameplayName))
        return Self(masterVolume: try number(audio, key: "master_volume", range: 0...1) ?? 1,
                    mute: try boolean(audio, key: "mute") ?? false,
                    lookSensitivityPercent: try number(controller, key: "look_sensitivity_percent", range: 0...100) ?? 75,
                    developerMode: try boolean(gameplay, key: "developer_mode") ?? false)
    }

    func save(to root: URL) throws {
        guard masterVolume.isFinite, (0...1).contains(masterVolume),
              lookSensitivityPercent.isFinite, (0...100).contains(lookSensitivityPercent) else {
            throw NativeGraphicsSettings.SettingsError.invalid("Choose sound and controller sensitivity from 0 to 100 percent.")
        }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let audioURL = root.appendingPathComponent(Self.audioName)
        var audio = try Self.readObject(audioURL)
        audio["master_volume"] = masterVolume
        audio["mute"] = mute
        try Self.writeObject(audio, to: audioURL)
        let controllerURL = root.appendingPathComponent(Self.controllerName)
        var controller = try Self.readObject(controllerURL)
        controller["look_sensitivity_percent"] = lookSensitivityPercent
        try Self.writeObject(controller, to: controllerURL)
        let gameplayURL = root.appendingPathComponent(Self.gameplayName)
        var gameplay = try Self.readObject(gameplayURL)
        gameplay["developer_mode"] = developerMode
        try Self.writeObject(gameplay, to: gameplayURL)
        // An intentional mute saved through this UI must never be mistaken for
        // the old temporary mute, even when saving precedes the first load.
        try Self.writeObject(["schema": 1], to: root.appendingPathComponent(Self.audioMigrationName))
    }

    private static func number(_ object: [String: Any], key: String, range: ClosedRange<Double>) throws -> Double? {
        guard let value = object[key] else { return nil }
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(),
              number.doubleValue.isFinite, range.contains(number.doubleValue) else {
            throw NativeGraphicsSettings.SettingsError.invalid("Invalid \(key) in the play settings.")
        }
        return number.doubleValue
    }

    private static func boolean(_ object: [String: Any], key: String) throws -> Bool? {
        guard let value = object[key] else { return nil }
        guard let number = value as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() else {
            throw NativeGraphicsSettings.SettingsError.invalid("Invalid \(key) in the play settings.")
        }
        return number.boolValue
    }

    private static func readObject(_ url: URL) throws -> [String: Any] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
        guard (try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0) <= 65_536 else {
            throw NativeGraphicsSettings.SettingsError.invalid("The play settings file is too large.")
        }
        let data = try Data(contentsOf: url)
        guard data.count <= 65_536,
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw NativeGraphicsSettings.SettingsError.invalid("The play settings file must contain a JSON object.")
        }
        return object
    }

    private static func writeObject(_ object: [String: Any], to url: URL) throws {
        let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: url, options: .atomic)
    }
}

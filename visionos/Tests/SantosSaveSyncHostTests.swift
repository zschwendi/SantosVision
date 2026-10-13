import CryptoKit
import Foundation

@main
struct SantosSaveSyncHostTests {
    static func main() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("santos-save-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let cloud = root.appendingPathComponent("cloud")
        let first = root.appendingPathComponent("device-a")
        let second = root.appendingPathComponent("device-b")
        let relative = "userdata/Documents/Rockstar Games/GTA V/Profiles/ABCD/SGTA50000"
        let manager = FileManager.default

        func write(_ text: String, to saveRoot: URL) throws {
            let file = saveRoot.appendingPathComponent(relative)
            try manager.createDirectory(at: file.deletingLastPathComponent(),
                                        withIntermediateDirectories: true)
            try Data(text.utf8).write(to: file, options: .atomic)
        }
        func read(_ saveRoot: URL) throws -> String {
            String(decoding: try Data(contentsOf: saveRoot.appendingPathComponent(relative)),
                   as: UTF8.self)
        }
        func engine(_ saveRoot: URL, offline: Bool = false) -> SantosSaveSync {
            let config = SantosSaveSync.Configuration(
                saveRoot: saveRoot,
                archiveRoot: saveRoot.appendingPathComponent("sync-archive"),
                cloudRoot: { offline ? nil : cloud },
                cloudIdentity: { offline ? nil : "synthetic-account" },
                discover: { root in
                    (try? FileManager.default.contentsOfDirectory(at: root,
                        includingPropertiesForKeys: nil)) ?? []
                }, settleNanoseconds: 1_000_000)
            return SantosSaveSync(configuration: config)
        }

        try write("A", to: first)
        let deviceA = engine(first)
        let publishedA = try await deviceA.snapshotAndUpload()
        precondition(publishedA.publishedRevision != nil)
        precondition(publishedA.cloudUploadPending) // Synthetic directory cannot confirm upload.
        let firstA = try read(first)
        precondition(firstA == "A")

        let deviceB = engine(second)
        let restoredB = try await deviceB.restoreBeforeLaunch()
        precondition(restoredB.restoredRevision == publishedA.publishedRevision)
        let secondA = try read(second)
        precondition(secondA == "A")

        await deviceB.markGameplayStarted()
        do {
            _ = try await deviceB.restoreBeforeLaunch()
            fatalError("restore was permitted during gameplay")
        } catch SantosSaveSyncError.restoreDuringGameplay {}
        try write("B", to: second)
        let publishedB = try await deviceB.snapshotAndUpload()
        precondition(publishedB.publishedRevision != nil)
        let secondB = try read(second)
        precondition(secondB == "B")
        await deviceB.markGameplayStopped()

        // Device A has its old baseline and can safely take the newer descendant at launch.
        let updatedA = try await deviceA.restoreBeforeLaunch()
        precondition(updatedA.restoredRevision == publishedB.publishedRevision)
        let firstB = try read(first)
        precondition(firstB == "B")

        // Divergent local gameplay is retained; the cloud revision files remain immutable.
        try write("C-local", to: first)
        try write("D-remote", to: second)
        _ = try await deviceB.snapshotAndUpload()
        let before = try manager.contentsOfDirectory(at: cloud,
                                                      includingPropertiesForKeys: nil).count
        let conflict = try await deviceA.restoreBeforeLaunch()
        precondition(conflict.restoredRevision == nil)
        precondition(conflict.divergentRemoteRevisions > 0)
        let firstC = try read(first)
        precondition(firstC == "C-local")
        _ = try await deviceA.snapshotAndUpload()
        let after = try manager.contentsOfDirectory(at: cloud,
                                                     includingPropertiesForKeys: nil).count
        precondition(after == before + 1)
        let history = try await deviceA.availableRevisions()
        precondition(history.contains { $0.id == publishedB.publishedRevision })
        let recovered = try await deviceA.restoreRevisionBeforeLaunch(
            publishedB.publishedRevision!)
        precondition(recovered.restoredRevision == publishedB.publishedRevision)
        let recoveredB = try read(first)
        precondition(recoveredB == "B")

        let broken = cloud.appendingPathComponent("\(UUID().uuidString).svsave")
        try Data("incomplete revision".utf8).write(to: broken)
        do {
            _ = try await deviceA.restoreBeforeLaunch()
            fatalError("damaged cloud history was accepted")
        } catch {}
        let afterBroken = try read(first)
        precondition(afterBroken == "B")
        try manager.removeItem(at: broken)

        // The quit checkpoint writes only to local history. A new process uploads it later.
        try write("E-final", to: first)
        let countBeforeCheckpoint = try manager.contentsOfDirectory(
            at: cloud, includingPropertiesForKeys: nil).count
        let localResult = try await deviceA.checkpointLocal()
        precondition(!localResult.cloudAvailable && localResult.publishedRevision == nil)
        let countAfterCheckpoint = try manager.contentsOfDirectory(
            at: cloud, includingPropertiesForKeys: nil).count
        precondition(countAfterCheckpoint == countBeforeCheckpoint)
        let nextLaunch = engine(first)
        let resumed = try await nextLaunch.restoreBeforeLaunch()
        precondition(resumed.restoredRevision == nil)
        let uploadedFinal = try await nextLaunch.snapshotAndUpload()
        precondition(uploadedFinal.publishedRevision != nil)
        let finalData = try read(first)
        precondition(finalData == "E-final")

        let noCloud = engine(root.appendingPathComponent("offline"), offline: true)
        let offlineRestore = try await noCloud.restoreBeforeLaunch()
        precondition(!offlineRestore.cloudAvailable)

        let trapCloud = SantosSaveSync(configuration: .init(
            saveRoot: first,
            archiveRoot: first.appendingPathComponent("sync-archive"),
            cloudRoot: { fatalError("checkpoint queried cloud root") },
            cloudIdentity: { fatalError("checkpoint queried account") },
            discover: { _ in fatalError("checkpoint discovered cloud history") },
            settleNanoseconds: 1_000_000))
        _ = try await trapCloud.checkpointLocal()

        // Match the physical listing: PC settings exist before any Profiles directory.
        let settingsCloud = root.appendingPathComponent("settings-cloud")
        let settingsA = root.appendingPathComponent("settings-a")
        let settingsB = root.appendingPathComponent("settings-b")
        let gta = "userdata/Documents/Rockstar Games/GTA V"
        let gtaRoot = settingsA.appendingPathComponent(gta)
        try manager.createDirectory(at: gtaRoot.appendingPathComponent("cache"),
                                    withIntermediateDirectories: true)
        try Data("pc-state".utf8).write(to: gtaRoot.appendingPathComponent("pc_settings.bin"))
        try Data("graphics".utf8).write(to: gtaRoot.appendingPathComponent("settings.xml"))
        try Data("older-graphics".utf8).write(
            to: gtaRoot.appendingPathComponent("settings.xml.native-low-backup.1"))
        try Data("cache".utf8).write(to: gtaRoot.appendingPathComponent("cache/data"))
        func settingsEngine(_ saveRoot: URL) -> SantosSaveSync {
            SantosSaveSync(configuration: .init(
                saveRoot: saveRoot,
                archiveRoot: saveRoot.appendingPathComponent("archive"),
                cloudRoot: { settingsCloud }, cloudIdentity: { "synthetic-account" },
                discover: { root in
                    (try? FileManager.default.contentsOfDirectory(
                        at: root, includingPropertiesForKeys: nil)) ?? []
                }, settleNanoseconds: 1_000_000))
        }
        let settingsPublisher = settingsEngine(settingsA)
        let settingsResult = try await settingsPublisher.snapshotAndUpload()
        precondition(settingsResult.localFiles == 1)
        let settingsReceiver = settingsEngine(settingsB)
        let restoredSettings = try await settingsReceiver.restoreBeforeLaunch()
        precondition(restoredSettings.localFiles == 1)
        let pcData = try Data(contentsOf: settingsB.appendingPathComponent(
            gta + "/pc_settings.bin"))
        precondition(String(decoding: pcData, as: UTF8.self) == "pc-state")
        precondition(!manager.fileExists(atPath: settingsB.appendingPathComponent(
            gta + "/settings.xml").path))
        precondition(!manager.fileExists(atPath: settingsB.appendingPathComponent(
            gta + "/Profiles").path))

        // An existing schema-1 profile-only cloud revision remains recoverable.
        let legacyCloud = root.appendingPathComponent("legacy-cloud")
        let legacyDevice = root.appendingPathComponent("legacy-device")
        try manager.createDirectory(at: legacyCloud, withIntermediateDirectories: true)
        let legacyBytes = Data("old-slot".utf8)
        let legacyHash = SHA256.hash(data: legacyBytes).map {
            String(format: "%02x", $0)
        }.joined()
        let legacyPath = "ABCD/SGTA50000"
        let legacyFingerprint = SHA256.hash(data: Data(
            (legacyPath + "\0" + legacyHash + "\0").utf8
        )).map { String(format: "%02x", $0) }.joined()
        let legacy = SantosSaveRevision(
            version: 1, id: UUID(), parent: nil, createdAt: Date(),
            fingerprint: legacyFingerprint,
            files: [.init(path: legacyPath, sha256: legacyHash, data: legacyBytes)])
        try JSONEncoder().encode(legacy).write(to: legacyCloud.appendingPathComponent(
            "\(legacy.id.uuidString).svsave"))
        let legacyEngine = SantosSaveSync(configuration: .init(
            saveRoot: legacyDevice,
            archiveRoot: legacyDevice.appendingPathComponent("archive"),
            cloudRoot: { legacyCloud }, cloudIdentity: { "synthetic-account" },
            discover: { root in
                (try? FileManager.default.contentsOfDirectory(
                    at: root, includingPropertiesForKeys: nil)) ?? []
            }, settleNanoseconds: 1_000_000))
        let migrated = try await legacyEngine.restoreBeforeLaunch()
        precondition(migrated.localFiles == 1)
        let migratedBytes = try Data(contentsOf: legacyDevice.appendingPathComponent(
            gta + "/Profiles/" + legacyPath))
        precondition(migratedBytes == legacyBytes)

        print("Santos save sync host tests passed")
    }
}

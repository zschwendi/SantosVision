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

        let noCloud = engine(root.appendingPathComponent("offline"), offline: true)
        let offlineRestore = try await noCloud.restoreBeforeLaunch()
        precondition(!offlineRestore.cloudAvailable)

        print("Santos save sync host tests passed")
    }
}

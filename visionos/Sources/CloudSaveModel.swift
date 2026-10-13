import Combine
import Darwin
import Foundation

@MainActor
final class CloudSaveModel: ObservableObject {
    static let containerIdentifier = "iCloud.com.zephyrstudios.santosvision"
    @Published private(set) var status = "Save backups are ready."
    @Published private(set) var isBusy = false
    @Published private(set) var revisions: [SantosSaveSync.RevisionSummary] = []
    private let engine: SantosSaveSync
    private var periodic: Task<Void, Never>?

    init(saveRoot: URL) {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        engine = SantosSaveSync(configuration: .live(
            saveRoot: saveRoot,
            archiveRoot: support.appendingPathComponent("SantosVision/SaveSync", isDirectory: true),
            containerIdentifier: Self.containerIdentifier))
    }

    func prepareForLaunch() async -> Bool {
        guard !isBusy else { return false }
        isBusy = true
        defer { isBusy = false }
        status = "Checking iCloud saves…"
        do {
            let recovery = try await engine.restoreBeforeLaunch()
            let upload = try await engine.snapshotAndUpload()
            describe(recovery.divergentRemoteRevisions > 0 || recovery.restoredRevision != nil ? recovery : upload)
            return true
        } catch {
            status = "Save recovery needs attention: \(error.localizedDescription)"
            return false
        }
    }

    func gameplayStarted() async {
        await engine.markGameplayStarted()
        periodic?.cancel()
        periodic = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                guard let self, !Task.isCancelled else { return }
                await self.backupNow()
            }
        }
    }

    func backupNow() async {
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        do { describe(try await engine.snapshotAndUpload()) }
        catch { status = "Backup will retry: \(error.localizedDescription)" }
    }

    func loadHistory() async {
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        do { revisions = try await engine.availableRevisions() }
        catch { status = "Could not load save history: \(error.localizedDescription)" }
    }

    func restore(_ revision: UUID) async {
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        do { describe(try await engine.restoreRevisionBeforeLaunch(revision)) }
        catch { status = "Save recovery needs attention: \(error.localizedDescription)" }
    }

    func finishBackup() async -> Bool {
        periodic?.cancel()
        periodic = nil
        // Let a snapshot already underway finish before the final local backup.
        for _ in 0..<100 where isBusy {
            try? await Task.sleep(for: .milliseconds(100))
        }
        for attempt in 0..<3 {
            do {
                _ = try await engine.checkpointLocal()
                status = "Final local save backup verified."
                return true
            } catch {
                status = "Final save backup could not finish: \(error.localizedDescription)"
                if attempt < 2 { try? await Task.sleep(for: .milliseconds(350)) }
            }
        }
        return false
    }

    private func describe(_ result: SantosSaveSync.Result) {
        if result.divergentRemoteRevisions > 0 {
            status = "Different save versions preserved. Choose a backup in Save history."
        } else if result.restoredRevision != nil {
            status = "Verified iCloud save restored; previous save backed up."
        } else if result.cloudAvailable {
            status = result.publishedRevision == nil ? "iCloud save history checked."
                : (result.cloudUploadPending ? "Saved locally · iCloud upload queued." : "Save backed up locally and uploaded to iCloud.")
        } else {
            status = "Local backups active · iCloud Drive unavailable."
        }
    }
}

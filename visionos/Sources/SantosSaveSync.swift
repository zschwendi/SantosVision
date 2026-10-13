import CryptoKit
import Darwin
import Foundation

/// A revision contains one complete, immutable snapshot. Its single cloud file is published
/// atomically, so another device never has to assemble a partially uploaded file set.
struct SantosSaveRevision: Codable, Sendable {
    struct File: Codable, Sendable {
        let path: String
        let sha256: String
        let data: Data
    }

    let version: Int
    let id: UUID
    let parent: UUID?
    let createdAt: Date
    let fingerprint: String
    let files: [File]
}

enum SantosSaveSyncError: LocalizedError {
    case unsafePath(String)
    case tooLarge
    case changingFiles
    case damagedRevision(String)
    case cloudNotReady
    case restoreDuringGameplay
    case checkpointInProgress

    var errorDescription: String? {
        switch self {
        case .unsafePath(let path): "Unsafe save path: \(path)"
        case .tooLarge: "GTA save data exceeds the sync safety limit."
        case .changingFiles: "A GTA save was still changing. Sync will retry later."
        case .damagedRevision(let name): "A saved revision failed verification: \(name)."
        case .cloudNotReady: "iCloud has not finished downloading the save history."
        case .restoreDuringGameplay: "Cloud restores are allowed only before the engine starts."
        case .checkpointInProgress: "Save recovery stopped because the game is closing."
        }
    }
}

/// The native engine always owns its ordinary local files. This actor only restores before
/// startup; while running it can publish stable snapshots but never mutates gameplay files.
actor SantosSaveSync {
    typealias Discovery = @Sendable (URL) async throws -> [URL]

    struct Configuration: Sendable {
        let saveRoot: URL
        let archiveRoot: URL
        let cloudRoot: @Sendable () -> URL?
        let cloudIdentity: @Sendable () -> String?
        let discover: Discovery
        let settleNanoseconds: UInt64

        init(saveRoot: URL, archiveRoot: URL,
             cloudRoot: @escaping @Sendable () -> URL?,
             cloudIdentity: @escaping @Sendable () -> String?,
             discover: @escaping Discovery = SantosSaveCloudDiscovery.revisions,
             settleNanoseconds: UInt64 = 350_000_000) {
            self.saveRoot = saveRoot
            self.archiveRoot = archiveRoot
            self.cloudRoot = cloudRoot
            self.cloudIdentity = cloudIdentity
            self.discover = discover
            self.settleNanoseconds = settleNanoseconds
        }

        /// The container identifier must also appear in the app's signed iCloud entitlement.
        static func live(saveRoot: URL, archiveRoot: URL,
                         containerIdentifier: String) -> Configuration {
            Configuration(saveRoot: saveRoot, archiveRoot: archiveRoot,
                          cloudRoot: {
                FileManager.default.url(forUbiquityContainerIdentifier: containerIdentifier)?
                    .appendingPathComponent("Documents/SantosVisionSaveSync/v1/revisions",
                                            isDirectory: true)
            }, cloudIdentity: {
                guard let token = FileManager.default.ubiquityIdentityToken,
                      let data = try? NSKeyedArchiver.archivedData(
                        withRootObject: token, requiringSecureCoding: false) else { return nil }
                return SantosSaveSync.digest(data)
            })
        }
    }

    struct Result: Sendable {
        let cloudAvailable: Bool
        let restoredRevision: UUID?
        let publishedRevision: UUID?
        let divergentRemoteRevisions: Int
        let localFiles: Int
        /// A cloud-container write is only queued until File Provider confirms upload.
        let cloudUploadPending: Bool

        init(cloudAvailable: Bool, restoredRevision: UUID?, publishedRevision: UUID?,
             divergentRemoteRevisions: Int, localFiles: Int,
             cloudUploadPending: Bool = false) {
            self.cloudAvailable = cloudAvailable
            self.restoredRevision = restoredRevision
            self.publishedRevision = publishedRevision
            self.divergentRemoteRevisions = divergentRemoteRevisions
            self.localFiles = localFiles
            self.cloudUploadPending = cloudUploadPending
        }
    }

    struct RevisionSummary: Sendable {
        let id: UUID
        let parent: UUID?
        let createdAt: Date
        let fileCount: Int
        let inCloud: Bool
    }

    private struct State: Codable {
        var cloudIdentity: String?
        var appliedRevision: UUID?
        var appliedFingerprint: String?
        var localRevision: UUID?
        var localFingerprint: String?
        var pendingRestore: UUID?
    }

    private let configuration: Configuration
    private let manager = FileManager.default
    private var gameplayStarted = false
    private var finalCheckpointRequested = false
    private static let maxFiles = 256
    private static let maxFileBytes = 16 * 1024 * 1024
    private static let maxTotalBytes = 128 * 1024 * 1024

    init(configuration: Configuration) { self.configuration = configuration }

    func markGameplayStarted() {
        gameplayStarted = true
        finalCheckpointRequested = false
    }
    func markGameplayStopped() { gameplayStarted = false }

    /// Quit-time durability boundary. No cloud lookup or file coordination can hold this up.
    /// An in-flight upload may finish later, but cannot replace this local revision pointer.
    func checkpointLocal() async throws -> Result {
        finalCheckpointRequested = true
        try prepareArchives()
        let snapshot: Snapshot
        do {
            snapshot = try await stableSnapshot()
        } catch SantosSaveSyncError.changingFiles {
            // A save already in progress can settle shortly after the pause request.
            try await Task.sleep(nanoseconds: 200_000_000)
            snapshot = try await stableSnapshot()
        }
        var state = try readState()
        guard !snapshot.files.isEmpty else {
            return Result(cloudAvailable: false, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: 0,
                          localFiles: 0)
        }
        if state.localFingerprint == snapshot.fingerprint,
           let id = state.localRevision {
            _ = try readRevision(at: localRevisionURL(id))
        } else {
            let revision = makeRevision(from: snapshot, parent: state.appliedRevision)
            try archive(revision)
            state.localRevision = revision.id
            state.localFingerprint = revision.fingerprint
            try writeState(state)
        }
        return Result(cloudAvailable: false, restoredRevision: nil,
                      publishedRevision: nil, divergentRemoteRevisions: 0,
                      localFiles: snapshot.files.count)
    }

    /// Supports a future recovery picker without exposing mutable cloud paths to the UI.
    func availableRevisions() async throws -> [RevisionSummary] {
        try prepareArchives()
        let local = try manager.contentsOfDirectory(at: archiveDirectory,
                                                     includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "svsave" }
            .map { try readRevision(at: $0) }
        let remote: [SantosSaveRevision]
        if let root = configuration.cloudRoot(), configuration.cloudIdentity() != nil {
            remote = try await readCloudHistory(root)
        } else {
            remote = []
        }
        var result: [UUID: RevisionSummary] = [:]
        for revision in local {
            result[revision.id] = RevisionSummary(
                id: revision.id, parent: revision.parent, createdAt: revision.createdAt,
                fileCount: revision.files.count, inCloud: false)
        }
        for revision in remote {
            result[revision.id] = RevisionSummary(
                id: revision.id, parent: revision.parent, createdAt: revision.createdAt,
                fileCount: revision.files.count, inCloud: true)
        }
        return result.values.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt > $1.createdAt }
            return $0.id.uuidString > $1.id.uuidString
        }
    }

    /// Explicit recovery always preserves the current local snapshot before changing it.
    func restoreRevisionBeforeLaunch(_ id: UUID) async throws -> Result {
        guard !gameplayStarted else { throw SantosSaveSyncError.restoreDuringGameplay }
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        try prepareArchives()
        let localURL = localRevisionURL(id)
        let selected: SantosSaveRevision
        if manager.fileExists(atPath: localURL.path) {
            selected = try readRevision(at: localURL)
        } else if let root = configuration.cloudRoot(),
                  configuration.cloudIdentity() != nil {
            guard let remote = try await readCloudHistory(root).first(where: { $0.id == id }) else {
                throw SantosSaveSyncError.damagedRevision(id.uuidString)
            }
            selected = remote
        } else {
            throw SantosSaveSyncError.cloudNotReady
        }
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        var state = try readState()
        let current = try await stableSnapshot()
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        if current.fingerprint != selected.fingerprint {
            if !current.files.isEmpty {
                let backup = makeRevision(from: current, parent: state.appliedRevision)
                try archive(backup)
                state.localRevision = backup.id
                state.localFingerprint = backup.fingerprint
                try writeState(state)
            }
            try archive(selected)
            state.pendingRestore = selected.id
            try writeState(state)
            try install(selected)
        }
        state.pendingRestore = nil
        state.appliedRevision = selected.id
        state.appliedFingerprint = try await stableSnapshot().fingerprint
        try writeState(state)
        return Result(cloudAvailable: configuration.cloudRoot() != nil,
                      restoredRevision: selected.id, publishedRevision: nil,
                      divergentRemoteRevisions: 0, localFiles: selected.files.count)
    }

    /// Call and await before NativeRuntime.start. A failed cloud read leaves local saves intact.
    func restoreBeforeLaunch() async throws -> Result {
        guard !gameplayStarted else { throw SantosSaveSyncError.restoreDuringGameplay }
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        try prepareArchives()
        var state = try readState()
        if let pending = state.pendingRestore {
            let revision = try readRevision(at: localRevisionURL(pending))
            try install(revision)
            state.appliedRevision = pending
            state.appliedFingerprint = try await stableSnapshot().fingerprint
            state.pendingRestore = nil
            try writeState(state)
        }

        let local = try await stableSnapshot()
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        guard let root = configuration.cloudRoot(),
              let identity = configuration.cloudIdentity() else {
            return Result(cloudAvailable: false, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: 0,
                          localFiles: local.files.count)
        }
        if state.cloudIdentity != identity {
            state.cloudIdentity = identity
            state.appliedRevision = nil
            state.appliedFingerprint = nil
            try writeState(state)
        }

        let revisions = try await readCloudHistory(root)
        guard !finalCheckpointRequested else { throw SantosSaveSyncError.checkpointInProgress }
        let heads = revisions.filter { candidate in
            !revisions.contains { other in
                other.id != candidate.id && isAncestor(candidate.id, of: other, in: revisions)
            }
        }
        guard let newest = heads.max(by: revisionOrder) else {
            return Result(cloudAvailable: true, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: 0,
                          localFiles: local.files.count)
        }
        let divergent = revisions.filter { $0.id != newest.id &&
            !isAncestor($0.id, of: newest, in: revisions) }.count
        guard local.fingerprint != newest.fingerprint else {
            state.appliedRevision = newest.id
            state.appliedFingerprint = newest.fingerprint
            try writeState(state)
            return Result(cloudAvailable: true, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: divergent,
                          localFiles: local.files.count)
        }

        let canRestore = local.files.isEmpty ||
            (state.appliedFingerprint == local.fingerprint &&
             (state.appliedRevision == nil ||
              isAncestor(state.appliedRevision!, of: newest, in: revisions)))
        guard canRestore else {
            // Both branches remain in immutable cloud history. Gameplay keeps its local branch.
            return Result(cloudAvailable: true, restoredRevision: nil,
                          publishedRevision: nil,
                          divergentRemoteRevisions: divergent + 1,
                          localFiles: local.files.count)
        }

        if !local.files.isEmpty {
            let backup = makeRevision(from: local, parent: state.appliedRevision)
            try archive(backup)
            state.localRevision = backup.id
            state.localFingerprint = backup.fingerprint
            try writeState(state)
        }
        try archive(newest)
        state.pendingRestore = newest.id
        try writeState(state)
        try install(newest)
        state.pendingRestore = nil
        state.appliedRevision = newest.id
        state.appliedFingerprint = try await stableSnapshot().fingerprint
        try writeState(state)
        return Result(cloudAvailable: true, restoredRevision: newest.id,
                      publishedRevision: nil, divergentRemoteRevisions: divergent,
                      localFiles: newest.files.count)
    }

    /// Safe during gameplay or on pause/quit. It never reads an unstable file set or replaces a
    /// cloud revision. A locally archived revision remains available if iCloud is offline.
    func snapshotAndUpload() async throws -> Result {
        try prepareArchives()
        let snapshot = try await stableSnapshot()
        if finalCheckpointRequested {
            return Result(cloudAvailable: false, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: 0,
                          localFiles: snapshot.files.count)
        }
        var state = try readState()
        guard !snapshot.files.isEmpty else {
            return Result(cloudAvailable: configuration.cloudRoot() != nil,
                          restoredRevision: nil, publishedRevision: nil,
                          divergentRemoteRevisions: 0, localFiles: 0)
        }
        let revision: SantosSaveRevision
        if state.localFingerprint == snapshot.fingerprint,
           let id = state.localRevision {
            revision = try readRevision(at: localRevisionURL(id))
        } else {
            revision = makeRevision(from: snapshot, parent: state.appliedRevision)
            try archive(revision)
            state.localRevision = revision.id
            state.localFingerprint = revision.fingerprint
            try writeState(state)
        }
        guard let root = configuration.cloudRoot(),
              let identity = configuration.cloudIdentity() else {
            return Result(cloudAvailable: false, restoredRevision: nil,
                          publishedRevision: nil, divergentRemoteRevisions: 0,
                          localFiles: revision.files.count)
        }
        if state.cloudIdentity != identity {
            state.cloudIdentity = identity
            state.appliedRevision = nil
            state.appliedFingerprint = nil
            try writeState(state)
        }
        try manager.createDirectory(at: root, withIntermediateDirectories: true)
        let destination = root.appendingPathComponent("\(revision.id.uuidString).svsave")
        if manager.fileExists(atPath: destination.path) {
            let existing = try readRevision(at: destination)
            guard existing.fingerprint == revision.fingerprint else {
                throw SantosSaveSyncError.damagedRevision(destination.lastPathComponent)
            }
        } else {
            try coordinatedWrite(encoded(revision), to: destination)
        }
        // A quit checkpoint can run while a cloud operation awaits. Reload to preserve its
        // newer local pointer; the cloud publish still records its own applied base.
        state = try readState()
        state.appliedRevision = revision.id
        state.appliedFingerprint = revision.fingerprint
        try writeState(state)
        return Result(cloudAvailable: true, restoredRevision: nil,
                      publishedRevision: revision.id, divergentRemoteRevisions: 0,
                      localFiles: revision.files.count,
                      cloudUploadPending: !uploadConfirmed(at: destination))
    }

    private struct Snapshot {
        let files: [SantosSaveRevision.File]
        let fingerprint: String
    }

    private var gameSaveRoot: URL {
        configuration.saveRoot.appendingPathComponent(
            "userdata/Documents/Rockstar Games/GTA V", isDirectory: true)
            .resolvingSymlinksInPath()
    }

    private var stateURL: URL { configuration.archiveRoot.appendingPathComponent("state-v1.json") }
    private var archiveDirectory: URL {
        configuration.archiveRoot.appendingPathComponent("revisions", isDirectory: true)
    }
    private func localRevisionURL(_ id: UUID) -> URL {
        archiveDirectory.appendingPathComponent("\(id.uuidString).svsave")
    }

    private func prepareArchives() throws {
        try manager.createDirectory(at: archiveDirectory, withIntermediateDirectories: true)
    }

    private func readState() throws -> State {
        guard manager.fileExists(atPath: stateURL.path) else { return State() }
        return try JSONDecoder().decode(State.self, from: Data(contentsOf: stateURL))
    }

    private func writeState(_ state: State) throws {
        try JSONEncoder().encode(state).write(to: stateURL, options: .atomic)
    }

    private func makeRevision(from snapshot: Snapshot, parent: UUID?) -> SantosSaveRevision {
        SantosSaveRevision(version: 2, id: UUID(), parent: parent, createdAt: Date(),
                           fingerprint: snapshot.fingerprint, files: snapshot.files)
    }

    private func archive(_ revision: SantosSaveRevision) throws {
        let url = localRevisionURL(revision.id)
        if manager.fileExists(atPath: url.path) {
            let existing = try readRevision(at: url)
            guard existing.fingerprint == revision.fingerprint else {
                throw SantosSaveSyncError.damagedRevision(url.lastPathComponent)
            }
            return
        }
        let temporary = archiveDirectory.appendingPathComponent(".\(UUID().uuidString).tmp")
        defer { try? manager.removeItem(at: temporary) }
        try encoded(revision).write(to: temporary, options: .atomic)
        try manager.moveItem(at: temporary, to: url)
    }

    private func encoded(_ revision: SantosSaveRevision) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(revision)
    }

    private func readRevision(at url: URL) throws -> SantosSaveRevision {
        let attributes = try manager.attributesOfItem(atPath: url.path)
        let encodedLimit = (Self.maxTotalBytes * 4 / 3) + 8 * 1024 * 1024
        guard let size = attributes[.size] as? NSNumber,
              size.intValue <= encodedLimit else {
            throw SantosSaveSyncError.damagedRevision(url.lastPathComponent)
        }
        let revision = try JSONDecoder().decode(SantosSaveRevision.self, from: Data(contentsOf: url))
        guard revision.version == 1 || revision.version == 2,
              url.lastPathComponent == "\(revision.id.uuidString).svsave",
              revision.files.count <= Self.maxFiles else {
            throw SantosSaveSyncError.damagedRevision(url.lastPathComponent)
        }
        var total = 0
        var seen = Set<String>()
        for file in revision.files {
            try validatePath(file.path, legacyProfileRelative: revision.version == 1)
            total += file.data.count
            guard seen.insert(file.path).inserted,
                  file.data.count <= Self.maxFileBytes,
                  total <= Self.maxTotalBytes,
                  Self.digest(file.data) == file.sha256 else {
                throw SantosSaveSyncError.damagedRevision(url.lastPathComponent)
            }
        }
        guard Self.fingerprint(revision.files) == revision.fingerprint else {
            throw SantosSaveSyncError.damagedRevision(url.lastPathComponent)
        }
        if revision.version == 1 {
            let files = revision.files.map {
                SantosSaveRevision.File(path: "Profiles/" + $0.path,
                                        sha256: $0.sha256, data: $0.data)
            }
            return SantosSaveRevision(version: 2, id: revision.id,
                                      parent: revision.parent, createdAt: revision.createdAt,
                                      fingerprint: Self.fingerprint(files), files: files)
        }
        return revision
    }

    private func stableSnapshot() async throws -> Snapshot {
        let first = try snapshot()
        try await Task.sleep(nanoseconds: configuration.settleNanoseconds)
        let second = try snapshot()
        guard first.fingerprint == second.fingerprint else {
            throw SantosSaveSyncError.changingFiles
        }
        return second
    }

    private func snapshot() throws -> Snapshot {
        guard manager.fileExists(atPath: gameSaveRoot.path) else {
            return Snapshot(files: [], fingerprint: Self.fingerprint([]))
        }
        guard let canonicalPointer = realpath(gameSaveRoot.path, nil) else {
            throw SantosSaveSyncError.changingFiles
        }
        let canonicalRoot = URL(fileURLWithPath: String(cString: canonicalPointer))
        free(canonicalPointer)
        guard let enumerator = manager.enumerator(at: canonicalRoot,
              includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey],
              options: [.skipsHiddenFiles]) else {
            throw SantosSaveSyncError.changingFiles
        }
        var files: [SantosSaveRevision.File] = []
        var total = 0
        for case let url as URL in enumerator {
            let values = try url.resourceValues(forKeys: [
                .isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey
            ])
            let prefix = canonicalRoot.path + "/"
            guard url.path.hasPrefix(prefix) else { throw SantosSaveSyncError.unsafePath(url.path) }
            let path = String(url.path.dropFirst(prefix.count))
            if path == "Profiles" {
                guard values.isSymbolicLink != true else {
                    throw SantosSaveSyncError.unsafePath(url.path)
                }
                continue
            }
            if values.isDirectory == true && path != "Profiles" && !path.hasPrefix("Profiles/") {
                enumerator.skipDescendants()
                continue
            }
            guard path == "pc_settings.bin" || path.hasPrefix("Profiles/") else { continue }
            guard values.isSymbolicLink != true else {
                throw SantosSaveSyncError.unsafePath(url.path)
            }
            guard values.isRegularFile == true else { continue }
            try validatePath(path)
            var sampledURL = url
            let before = try sampledURL.resourceValues(
                forKeys: [.fileSizeKey, .contentModificationDateKey])
            let data = try Data(contentsOf: sampledURL)
            sampledURL.removeAllCachedResourceValues()
            let after = try sampledURL.resourceValues(
                forKeys: [.fileSizeKey, .contentModificationDateKey])
            guard before.fileSize == after.fileSize,
                  before.contentModificationDate == after.contentModificationDate,
                  after.fileSize == data.count else {
                throw SantosSaveSyncError.changingFiles
            }
            total += data.count
            guard data.count <= Self.maxFileBytes, total <= Self.maxTotalBytes,
                  files.count < Self.maxFiles else { throw SantosSaveSyncError.tooLarge }
            files.append(.init(path: path, sha256: Self.digest(data), data: data))
        }
        files.sort { $0.path < $1.path }
        return Snapshot(files: files, fingerprint: Self.fingerprint(files))
    }

    private func validatePath(_ path: String, legacyProfileRelative: Bool = false) throws {
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        guard !parts.isEmpty, parts.count <= 8,
              parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." && !$0.contains("\\") }),
              !path.hasPrefix("/") else { throw SantosSaveSyncError.unsafePath(path) }
        if !legacyProfileRelative {
            guard path == "pc_settings.bin" ||
                    (parts.count >= 2 && parts.first == "Profiles") else {
                throw SantosSaveSyncError.unsafePath(path)
            }
        }
    }

    private func install(_ revision: SantosSaveRevision) throws {
        // A pending revision is stored locally before any gameplay file is touched. Replaying
        // these atomic per-file writes after a crash completes the same verified snapshot.
        for file in revision.files {
            try validatePath(file.path)
            let destination = gameSaveRoot.appendingPathComponent(file.path)
            try manager.createDirectory(at: destination.deletingLastPathComponent(),
                                        withIntermediateDirectories: true)
            if manager.fileExists(atPath: destination.path) {
                let current = try Data(contentsOf: destination)
                if Self.digest(current) == file.sha256 { continue }
            }
            try file.data.write(to: destination, options: .atomic)
        }
    }

    private func readCloudHistory(_ root: URL) async throws -> [SantosSaveRevision] {
        let paths = Set(try await configuration.discover(root)).sorted { $0.path < $1.path }
        guard paths.count <= 4_096 else { throw SantosSaveSyncError.tooLarge }
        var revisions: [SantosSaveRevision] = []
        for url in paths where url.pathExtension == "svsave" {
            try await waitForDownload(url)
            revisions.append(try readRevision(at: url))
        }
        return revisions
    }

    private func waitForDownload(_ url: URL) async throws {
        let keys: Set<URLResourceKey> = [.isUbiquitousItemKey,
                                         .ubiquitousItemDownloadingStatusKey]
        let initial = try url.resourceValues(forKeys: keys)
        guard initial.isUbiquitousItem == true else { return }
        if initial.ubiquitousItemDownloadingStatus == .current { return }
        try manager.startDownloadingUbiquitousItem(at: url)
        for _ in 0..<60 {
            try await Task.sleep(nanoseconds: 500_000_000)
            let values = try url.resourceValues(forKeys: keys)
            if values.ubiquitousItemDownloadingStatus == .current { return }
        }
        throw SantosSaveSyncError.cloudNotReady
    }

    private func coordinatedWrite(_ data: Data, to url: URL) throws {
        let coordinator = NSFileCoordinator()
        var coordinationError: NSError?
        var writeError: Error?
        coordinator.coordinate(writingItemAt: url, options: [], error: &coordinationError) {
            coordinatedURL in
            do {
                let temporary = coordinatedURL.deletingLastPathComponent()
                    .appendingPathComponent(".\(UUID().uuidString).tmp")
                defer { try? self.manager.removeItem(at: temporary) }
                try data.write(to: temporary, options: .atomic)
                try self.manager.moveItem(at: temporary, to: coordinatedURL)
            }
            catch { writeError = error }
        }
        if let error = coordinationError { throw error }
        if let error = writeError { throw error }
    }

    private func uploadConfirmed(at url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [
            .isUbiquitousItemKey, .ubiquitousItemIsUploadedKey
        ]) else { return false }
        return values.isUbiquitousItem == true && values.ubiquitousItemIsUploaded == true
    }

    private func revisionOrder(_ lhs: SantosSaveRevision, _ rhs: SantosSaveRevision) -> Bool {
        if lhs.createdAt != rhs.createdAt { return lhs.createdAt < rhs.createdAt }
        return lhs.id.uuidString < rhs.id.uuidString
    }

    private func isAncestor(_ ancestor: UUID, of revision: SantosSaveRevision,
                            in revisions: [SantosSaveRevision]) -> Bool {
        let byID = Dictionary(uniqueKeysWithValues: revisions.map { ($0.id, $0) })
        var current: UUID? = revision.id
        var visited = Set<UUID>()
        while let id = current, visited.insert(id).inserted {
            if id == ancestor { return true }
            current = byID[id]?.parent
        }
        return false
    }

    fileprivate static func digest(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func fingerprint(_ files: [SantosSaveRevision.File]) -> String {
        var hasher = SHA256()
        for file in files.sorted(by: { $0.path < $1.path }) {
            hasher.update(data: Data(file.path.utf8))
            hasher.update(data: Data([0]))
            hasher.update(data: Data(file.sha256.utf8))
            hasher.update(data: Data([0]))
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}

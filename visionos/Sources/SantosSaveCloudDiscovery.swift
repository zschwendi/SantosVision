import Foundation

/// An initial NSMetadataQuery gather distinguishes a genuinely empty iCloud history from an
/// incomplete File Provider view. A plain directory listing is insufficient for first restore.
@MainActor
final class SantosSaveCloudDiscovery {
    private let root: URL
    private let query = NSMetadataQuery()
    private var observer: NSObjectProtocol?
    private var timeout: Task<Void, Never>?
    private var continuation: CheckedContinuation<[URL], Error>?
    private var finished = false

    private init(root: URL) { self.root = root.standardizedFileURL }

    static func revisions(in root: URL) async throws -> [URL] {
        let discovery = SantosSaveCloudDiscovery(root: root)
        return try await discovery.gather()
    }

    private func gather() async throws -> [URL] {
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            query.operationQueue = .main
            query.searchScopes = [NSMetadataQueryUbiquitousDocumentsScope]
            query.predicate = NSPredicate(value: true)
            observer = NotificationCenter.default.addObserver(
                forName: .NSMetadataQueryDidFinishGathering, object: query, queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in self?.completeGather() }
            }
            guard query.start() else {
                finish(.failure(SantosSaveSyncError.cloudNotReady))
                return
            }
            timeout = Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 15_000_000_000)
                self?.finish(.failure(SantosSaveSyncError.cloudNotReady))
            }
        }
    }

    private func completeGather() {
        query.disableUpdates()
        let prefix = root.path + "/"
        let discovered = query.results.compactMap { item -> URL? in
            guard let metadata = item as? NSMetadataItem,
                  let url = metadata.value(forAttribute: NSMetadataItemURLKey) as? URL else {
                return nil
            }
            let standard = url.standardizedFileURL
            return standard.path.hasPrefix(prefix) && standard.pathExtension == "svsave"
                ? standard : nil
        }
        // Include revisions this device just published, which may not have reached metadata
        // indexing yet. The metadata gather itself must still finish successfully.
        let local = (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: nil)) ?? []
        let paths = Set(discovered + local.filter { $0.pathExtension == "svsave" })
        finish(.success(paths.sorted { $0.path < $1.path }))
    }

    private func finish(_ result: Result<[URL], Error>) {
        guard !finished else { return }
        finished = true
        timeout?.cancel()
        timeout = nil
        query.stop()
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        continuation?.resume(with: result)
        continuation = nil
    }
}

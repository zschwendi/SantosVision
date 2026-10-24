import Foundation

enum AppBuildInfo {
    static let label = "Build \(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown")"
}

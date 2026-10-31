import Darwin
import Foundation

/// Prelaunch choices read by the native runtime's config::setup_native. Each
/// file is optional and keeps keys it does not own; absent files keep the
/// previous V launch unchanged.
struct NativeMultiplayerSettings: Equatable, Sendable {
    static let defaultPort = 47520
    static let defaultPlayerName = "Vision Pro"
    var enabled = false
    var host = false
    var server = ""
    var playerName = ""

    private static let fileName = "native-multiplayer.json"

    var reportValue: [String: Any] {
        ["multiplayer": enabled, "role": host ? "host" : "join", "server": server, "player_name": playerName]
    }

    static func load(from root: URL) throws -> Self {
        let object = try NativeJSONFile.read(root.appendingPathComponent(fileName))
        func text(_ key: String) throws -> String {
            guard let value = object[key] else { return "" }
            guard let text = value as? String else { throw NativeGraphicsSettings.SettingsError.invalid("Invalid \(key) in the multiplayer settings.") }
            return text
        }
        return Self(enabled: try NativeJSONFile.boolean(object, key: "multiplayer") ?? false,
                    host: try NativeJSONFile.boolean(object, key: "host") ?? false,
                    server: try text("server"), playerName: try text("player_name"))
    }

    func save(to root: URL) throws {
        let url = root.appendingPathComponent(Self.fileName)
        var object = try NativeJSONFile.read(url)
        object["multiplayer"] = enabled
        object["host"] = host
        object["server"] = server.trimmingCharacters(in: .whitespaces)
        object["player_name"] = playerName.trimmingCharacters(in: .whitespaces)
        try NativeJSONFile.write(object, to: url)
    }

    /// Mirrors config::NativeMultiplayer so Play reports problems before the
    /// engine starts. Returns nil when the session can start.
    var problem: String? {
        guard enabled else { return nil }
        if playerName.trimmingCharacters(in: .whitespaces).count > 24 { return "Use a player name of at most 24 characters." }
        if host { return nil }
        return Self.addressProblem(server)
    }

    static func addressProblem(_ raw: String) -> String? {
        let text = raw.trimmingCharacters(in: .whitespaces)
        if text.isEmpty { return "Enter the host's address to join." }
        let example = "Use the host's IPv4 address or name, for example 192.168.1.20."
        let parts = text.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count <= 2, !text.contains("[") else { return example }
        if parts.count == 2 {
            guard let port = Int(parts[1]), (1...65_535).contains(port) else { return "The port must be a number from 1 to 65535." }
        }
        let host = String(parts[0])
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        let allowed = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-")
        let validName = host.count <= 253 && labels.allSatisfy { label in
            !label.isEmpty && label.count <= 63 && !label.hasPrefix("-") && !label.hasSuffix("-") && label.allSatisfy(allowed.contains)
        }
        let numeric = labels.allSatisfy { $0.allSatisfy(\.isNumber) }
        if !validName { return example }
        if numeric {
            guard labels.count == 4, labels.allSatisfy({ Int($0).map { (0...255).contains($0) } ?? false }) else { return example }
        }
        return nil
    }
}

/// The launcher's Textures choice, applied after any quality preset.
enum NativeTextureChoice {
    static let labels = ["Normal", "High", "Very High"]
    private static let fileName = "native-textures.json"

    static func load(from root: URL) throws -> Int? {
        let object = try NativeJSONFile.read(root.appendingPathComponent(fileName))
        guard let value = object["texture_quality"], !(value is NSNull) else { return nil }
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(),
              let quality = Int(exactly: number.doubleValue), (0...2).contains(quality) else {
            throw NativeGraphicsSettings.SettingsError.invalid("Invalid texture_quality in the texture settings.")
        }
        return quality
    }

    static func save(_ quality: Int?, to root: URL) throws {
        let url = root.appendingPathComponent(fileName)
        var object = try NativeJSONFile.read(url)
        object["texture_quality"] = quality.map { $0 as Any } ?? NSNull()
        try NativeJSONFile.write(object, to: url)
    }

    /// GTA's own current TextureQuality, read without changing the file.
    static func gtaValue(saveRoot: URL) -> Int? {
        let url = saveRoot.appendingPathComponent("userdata/Documents/Rockstar Games/GTA V/settings.xml")
        guard (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize).map({ ($0 ?? 0) <= 512 << 10 }) == true,
              let xml = try? String(contentsOf: url, encoding: .utf8),
              let match = xml.range(of: #"<TextureQuality\s+value\s*=\s*"([0-2])""#, options: .regularExpression) else { return nil }
        return xml[match].last(where: \.isNumber).flatMap { Int(String($0)) }
    }
}

enum NativeJSONFile {
    static func read(_ url: URL) throws -> [String: Any] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
        guard (try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0) <= 65_536 else {
            throw NativeGraphicsSettings.SettingsError.invalid("\(url.lastPathComponent) is too large.")
        }
        guard let object = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any] else {
            throw NativeGraphicsSettings.SettingsError.invalid("\(url.lastPathComponent) must contain a JSON object.")
        }
        return object
    }

    static func boolean(_ object: [String: Any], key: String) throws -> Bool? {
        guard let value = object[key] else { return nil }
        guard let number = value as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() else {
            throw NativeGraphicsSettings.SettingsError.invalid("Invalid \(key) in the launcher settings.")
        }
        return number.boolValue
    }

    static func write(_ object: [String: Any], to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: url, options: .atomic)
    }
}

/// IPv4 addresses friends can use to join this host.
func localIPv4Addresses() -> [String] {
    var result: [String] = []
    var interfaces: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&interfaces) == 0, let first = interfaces else { return [] }
    defer { freeifaddrs(interfaces) }
    for pointer in sequence(first: first, next: { $0.pointee.ifa_next }) {
        guard let address = pointer.pointee.ifa_addr, address.pointee.sa_family == UInt8(AF_INET),
              (pointer.pointee.ifa_flags & UInt32(IFF_UP)) != 0 else { continue }
        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        guard getnameinfo(address, socklen_t(address.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
        let ip = String(decoding: host.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        if !ip.hasPrefix("127."), !ip.hasPrefix("169.254."), !result.contains(ip) { result.append(ip) }
    }
    return result
}

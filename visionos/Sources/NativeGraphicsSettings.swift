import Foundation

struct NativeGraphicsSettings: Codable, Equatable, Sendable {
    enum Preset: String, CaseIterable, Identifiable, Hashable, Sendable {
        case existing, performance, clarity
        var id: String { rawValue }
        var label: String { switch self { case .existing: "Existing GTA"; case .performance: "Performance"; case .clarity: "Clarity" } }
        var detail: String { switch self {
        case .existing: "Keep your current GTA quality settings."
        case .performance: "Apply lower graphics quality once, keeping resolution and frame cap."
        case .clarity: "Sharper textures and 16× filtering, with depth of field and motion blur off. Other GTA choices are kept."
        } }
    }
    enum Resolution: String, CaseIterable, Identifiable, Hashable, Sendable {
        case small = "640x360"
        case standard = "960x540"
        case large = "1280x720"
        case fullHD = "1920x1080"
        case quadHD = "2560x1440"
        case ultraHD = "3840x2160"

        var id: String { rawValue }
        var dimensions: (width: Int, height: Int) {
            switch self {
            case .small: (640, 360)
            case .standard: (960, 540)
            case .large: (1280, 720)
            case .fullHD: (1920, 1080)
            case .quadHD: (2560, 1440)
            case .ultraHD: (3840, 2160)
            }
        }
        var label: String { "\(dimensions.width)×\(dimensions.height)" }
    }

    enum SettingsError: LocalizedError {
        case invalid(String)
        var errorDescription: String? {
            switch self { case .invalid(let message): message }
        }
    }

    var width = 960
    var height = 540
    var fpsLimit = 60
    var graphicsPreset = "existing"

    private enum CodingKeys: String, CodingKey {
        case width, height
        case fpsLimit = "fps_limit"
        case graphicsPreset = "graphics_preset"
    }

    var resolution: Resolution {
        Resolution.allCases.first { $0.dimensions.width == width && $0.dimensions.height == height } ?? .standard
    }
    var preset: Preset { Preset(rawValue: graphicsPreset) ?? .existing }
    var summary: String { "\(width)×\(height) · \(fpsLimit) FPS cap · \(preset.label) selected" }
    var reportValue: [String: Any] {
        ["width": width, "height": height, "fps_limit": fpsLimit, "graphics_preset": graphicsPreset]
    }

    func validate() throws {
        guard Resolution.allCases.contains(where: { $0.dimensions.width == width && $0.dimensions.height == height }) else {
            throw SettingsError.invalid("Choose one of the supported resolutions from 640×360 through 3840×2160.")
        }
        guard fpsLimit == 30 || fpsLimit == 60 else {
            throw SettingsError.invalid("Choose a 30 or 60 FPS frame cap.")
        }
        guard Preset(rawValue: graphicsPreset) != nil else {
            throw SettingsError.invalid("Choose Existing GTA, Performance or Clarity quality.")
        }
    }

    static func load(from url: URL) throws -> Self {
        guard FileManager.default.fileExists(atPath: url.path) else { return Self() }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard size <= 65_536 else { throw SettingsError.invalid("The graphics settings file is too large.") }
        let data = try Data(contentsOf: url)
        guard data.count <= 65_536 else { throw SettingsError.invalid("The graphics settings file is too large.") }
        var settings = try JSONDecoder().decode(Self.self, from: data)
        if settings.graphicsPreset == "low" { settings.graphicsPreset = "existing" }
        try settings.validate()
        return settings
    }

    mutating func apply(arguments: [String]) throws {
        func value(_ flag: String) throws -> String? {
            guard let index = arguments.firstIndex(of: flag) else { return nil }
            guard index + 1 < arguments.count, !arguments[index + 1].hasPrefix("--") else {
                throw SettingsError.invalid("Missing value for \(flag).")
            }
            return arguments[index + 1]
        }
        if let value = try value("--sv-resolution") {
            guard let resolution = Resolution(rawValue: value) else {
                throw SettingsError.invalid("Unsupported --sv-resolution: \(value).")
            }
            width = resolution.dimensions.width
            height = resolution.dimensions.height
        }
        if let value = try value("--sv-fps-limit") {
            guard let cap = Int(value), cap == 30 || cap == 60 else {
                throw SettingsError.invalid("Unsupported --sv-fps-limit: \(value).")
            }
            fpsLimit = cap
        }
        if let value = try value("--sv-graphics-preset") { graphicsPreset = value == "low" ? "existing" : value }
        try validate()
    }

    func save(to url: URL) throws {
        try validate()
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(self).write(to: url, options: .atomic)
    }
}

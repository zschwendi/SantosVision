// muguet - Launcher.swift
// Constantin Clerc (c22dev) - 0.1

import GameController
import SwiftUI
import UIKit
import UniformTypeIdentifiers

let configURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("config.json")
let gtaSettingsURL = configURL.deletingLastPathComponent()
    .appendingPathComponent("saves/userdata/Documents/Rockstar Games/GTA V/settings.xml")
let buildNumber = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown"

private enum Theme {
    static let background = Color(red: 0.035, green: 0.018, blue: 0.065)
    static let surface = Color(red: 0.16, green: 0.08, blue: 0.24)
    static let accent = Color(red: 0.62, green: 0.43, blue: 1)
    static let glow = Color(red: 0.42, green: 0.82, blue: 0.95)
    static let error = Color(red: 1, green: 0.5, blue: 0.56)
}

/// What the launcher shows. Fallbacks match what Rust uses when a key is absent.
struct LaunchValues: Equatable {
    var mode = 1, scale = 1.0, fps = 60.0, textures = 0, touch = 1.0
    var mute = false, multiplayer = false, host = false, server = "", name = ""
}

final class Settings: ObservableObject {
    static let scales = [0.35, 0.5, 0.75, 1.0]
    static let textureNames = ["Normal (less memory)", "High", "Very High"]
    /// Rust's DEFAULT_CONFIG (iOS start mode), used when config.json is missing or unreadable.
    static var rustDefaults: [String: Any] {
        ["scale": 0.5, "mouse_sensitivity": 2.0, "fullscreen": true, "fps_limit": 60, "start_mode": 1, "mute": false,
         "multiplayer": false, "host": false, "server": "", "player_name": "", "extra_args": [String]()]
    }

    @Published var v = LaunchValues()
    var texturesTouched = false
    private(set) var loaded = LaunchValues()
    private(set) var extra: [String: Any]
    private(set) var exists: Bool
    let gtaTextures: Int?

    init() {
        let json = (try? Data(contentsOf: configURL)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
        exists = json != nil
        extra = json ?? Settings.rustDefaults
        gtaTextures = Settings.readGTATextures()
        var v = LaunchValues()
        if let m = num("start_mode").map(Settings.int), (1...2).contains(m) { v.mode = m }
        v.scale = num("scale") ?? 1.0
        v.fps = num("fps_limit") ?? 60
        v.textures = num("texture_quality").map { min(max(Settings.int($0), 0), 2) } ?? gtaTextures ?? 0
        v.touch = num("touch_sensitivity") ?? 1.0
        v.mute = flag("mute")
        v.multiplayer = flag("multiplayer")
        v.host = flag("host")
        v.server = extra["server"] as? String ?? ""
        v.name = extra["player_name"] as? String ?? ""
        self.v = v
        loaded = v
    }

    var hasTextureKey: Bool { extra["texture_quality"] != nil }

    var summary: String {
        var parts = [v.mode == 2 ? "Sandbox" : "Story Mode", Settings.percent(v.scale), v.fps <= 0 ? "No frame limit" : v.fps <= 30 ? "30 fps" : "60 fps"]
        if v.multiplayer { parts.append(v.host ? "Hosting friends" : "Joining a friend") }
        return parts.joined(separator: " · ")
    }

    /// Why Play/Done cannot continue, in words a player can act on.
    func problem() -> String? {
        guard v.multiplayer, !v.host else { return nil }
        return Settings.addressProblem(v.server)
    }

    /// Writes only what the player changed; every other key in config.json is kept as it was.
    func save() throws {
        if exists && v == loaded && !texturesTouched { return }
        var json = extra
        if v.mode != loaded.mode { json["start_mode"] = v.mode }
        if v.scale != loaded.scale { json["scale"] = v.scale }
        if v.fps != loaded.fps { json["fps_limit"] = Settings.int(v.fps) }
        if texturesTouched { json["texture_quality"] = v.textures }
        if v.touch != loaded.touch { json["touch_sensitivity"] = (v.touch * 100).rounded() / 100 }
        if v.mute != loaded.mute { json["mute"] = v.mute }
        if v.multiplayer != loaded.multiplayer { json["multiplayer"] = v.multiplayer }
        if v.host != loaded.host { json["host"] = v.host }
        if v.server != loaded.server {
            v.server = v.server.trimmingCharacters(in: .whitespacesAndNewlines)
            json["server"] = v.server
        }
        if v.name != loaded.name {
            v.name = Settings.cleanName(v.name)
            json["player_name"] = v.name
        }
        if json["extra_args"] == nil { json["extra_args"] = [String]() }
        let data = try JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: configURL, options: .atomic)
        extra = json
        exists = true
        texturesTouched = false
        loaded = v
    }

    private func num(_ key: String) -> Double? {
        guard let n = extra[key] as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        return n.doubleValue
    }

    private func flag(_ key: String) -> Bool {
        guard let n = extra[key] as? NSNumber, CFGetTypeID(n) == CFBooleanGetTypeID() else { return false }
        return n.boolValue
    }

    static func int(_ x: Double) -> Int { Int(min(max(x, -1e9), 1e9)) }
    static func percent(_ x: Double) -> String { "\(int((x * 100).rounded())) %" }
    static func number(_ x: Double) -> String {
        var s = String(format: "%.2f", x)
        while s.hasSuffix("0") { s.removeLast() }
        if s.hasSuffix(".") { s.removeLast() }
        return s
    }

    /// Same rules as proto::clean_name: no control characters, at most 24 characters, trimmed.
    static func cleanName(_ raw: String) -> String {
        var out = String.UnicodeScalarView()
        out.append(contentsOf: raw.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars
            .filter { $0.properties.generalCategory != .control }.prefix(24))
        return String(out).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func addressProblem(_ raw: String) -> String? {
        let address = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if address.isEmpty { return "Enter the host's address to join, like 192.168.1.20." }
        if address.contains(where: \.isWhitespace) { return "The host address can't contain spaces." }
        let parts = address.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count <= 2 else { return "Use an IPv4 address or host name, optionally with a port, like 192.168.1.20:47520." }
        if parts.count == 2 {
            let port = parts[1]
            guard !port.isEmpty, port.allSatisfy({ $0.isASCII && $0.isNumber }), let p = Int(port), (1...65535).contains(p) else {
                return "The port after \u{201C}:\u{201D} must be a number from 1 to 65535."
            }
        }
        guard validHost(parts[0]) else {
            return "\u{201C}\(parts[0])\u{201D} isn't a valid address. Use an IP address like 192.168.1.20 or a host name."
        }
        return nil
    }

    private static func validHost(_ host: Substring) -> Bool {
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        if host.allSatisfy({ $0 == "." || ($0.isASCII && $0.isNumber) }) {
            return labels.count == 4 && labels.allSatisfy { !$0.isEmpty && $0.count <= 3 && (Int($0) ?? 256) <= 255 }
        }
        return host.count <= 253 && labels.allSatisfy { label in
            !label.isEmpty && label.count <= 63 && label.first != "-" && label.last != "-"
                && label.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }
        }
    }

    /// GTA's own TextureQuality, shown read-only while config.json has no texture_quality.
    static func readGTATextures() -> Int? {
        guard let xml = try? String(contentsOf: gtaSettingsURL, encoding: .utf8),
              let r = xml.range(of: #"<TextureQuality value="\d+" />"#, options: .regularExpression),
              let n = Int(xml[r].filter { $0.isASCII && $0.isNumber }) else { return nil }
        return min(max(n, 0), 2)
    }
}

@_silgen_name("muguet_import")
func muguet_import(_ zip: UnsafePointer<CChar>, _ progress: @convention(c) (UInt64, UInt64) -> Void) -> Bool
@_silgen_name("muguet_import_error")
func muguet_import_error() -> UnsafePointer<CChar>?
@_silgen_name("muguet_game_ready")
func muguet_game_ready() -> Bool
@_silgen_name("muguet_has_game_code")
func muguet_has_game_code() -> Bool

final class Importer: ObservableObject {
    static let shared = Importer()
    @Published var ready = muguet_game_ready()
    @Published var progress: Double?
    @Published var error: String?

    func run(_ url: URL) {
        error = nil
        progress = 0
        DispatchQueue.global(qos: .userInitiated).async {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let ok = url.path.withCString { muguet_import($0) { done, total in
                let p = total > 0 ? Double(done) / Double(total) : 0
                DispatchQueue.main.async { Importer.shared.progress = p }
            } }
            let message = ok ? nil : muguet_import_error().map { String(cString: $0) } ?? "unknown error"
            DispatchQueue.main.async {
                self.progress = nil
                self.ready = muguet_game_ready()
                if let message { self.error = "Import failed: \(message)" }
            }
        }
    }
}

func controllerSummary() -> String {
    let pads = GCController.controllers()
    guard let pad = pads.first else { return "Touch controls" }
    if pads.count > 1 { return "\(pads.count) controllers connected" }
    let kind = pad.productCategory
    return "\(["", "MFi", "Unknown"].contains(kind) ? pad.vendorName ?? "Controller" : kind) connected"
}

private func surface(_ radius: CGFloat = 22, opacity: Double = 0.45) -> some View {
    RoundedRectangle(cornerRadius: radius, style: .continuous).fill(Theme.surface.opacity(opacity))
        .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(Theme.accent.opacity(0.18)))
}

private func errorText(_ message: String) -> some View {
    Label(message, systemImage: "exclamationmark.circle.fill")
        .font(.callout).foregroundStyle(Theme.error).fixedSize(horizontal: false, vertical: true)
}

@MainActor @ViewBuilder private func prominentButton(_ title: String, _ icon: String, action: @escaping () -> Void) -> some View {
    let button = Button(action: action) {
        Label(title, systemImage: icon).font(.title3.weight(.semibold)).frame(maxWidth: 240)
    }
    .controlSize(.large)
    if #available(iOS 26.0, *) {
        button.buttonStyle(.glassProminent)
    } else {
        button.buttonStyle(.borderedProminent)
    }
}

private struct PressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .opacity(configuration.isPressed ? 0.85 : 1)
            .animation(.easeOut(duration: 0.15), value: configuration.isPressed)
    }
}

struct LauncherView: View {
    @StateObject private var s = Settings()
    @ObservedObject private var importer = Importer.shared
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @State private var error: String?
    @State private var errorInOptions = false
    @State private var picking = false
    @State private var options = false
    @State private var launching = false
    @State private var controller = controllerSummary()
    private let hasCode = muguet_has_game_code()
    let play: () -> Void

    var body: some View {
        GeometryReader { geo in
            Group {
                if verticalSizeClass == .compact || geo.size.height < 560 {
                    compact(width: geo.size.width)
                } else {
                    regular
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
            .fileImporter(isPresented: $picking, allowedContentTypes: [.zip]) { result in
                if case .success(let url) = result { importer.run(url) }
            }
        }
        .background(backdrop.ignoresSafeArea())
        .preferredColorScheme(.dark)
        .tint(Theme.accent)
        .dynamicTypeSize(...DynamicTypeSize.xxxLarge)
        .sheet(isPresented: $options, onDismiss: optionsClosed) { OptionsView(s: s) }
        .onAppear { controller = controllerSummary() }
        .onReceive(NotificationCenter.default.publisher(for: .GCControllerDidConnect)) { _ in controller = controllerSummary() }
        .onReceive(NotificationCenter.default.publisher(for: .GCControllerDidDisconnect)) { _ in controller = controllerSummary() }
    }

    private var backdrop: some View {
        ZStack {
            Theme.background
            RadialGradient(gradient: Gradient(colors: [Theme.accent.opacity(0.2), .clear]), center: .topLeading, startRadius: 0, endRadius: 760)
            RadialGradient(gradient: Gradient(colors: [Theme.glow.opacity(0.08), .clear]), center: .bottomTrailing, startRadius: 0, endRadius: 620)
        }
    }

    /// iPad: one centered card, status row along the bottom.
    private var regular: some View {
        VStack(spacing: 20) {
            GeometryReader { g in
                ScrollView(showsIndicators: false) {
                    VStack(spacing: 26) {
                        titleButton(compact: false)
                        actions
                    }
                    .padding(.horizontal, 40)
                    .padding(.vertical, 34)
                    .frame(maxWidth: 640)
                    .background(surface())
                    .frame(maxWidth: .infinity, minHeight: g.size.height)
                }
            }
            statusRow
        }
        .padding(28)
    }

    /// Phone landscape: V tile on the left, actions and status on the right.
    private func compact(width: CGFloat) -> some View {
        VStack(spacing: 10) {
            HStack(spacing: 14) {
                titleButton(compact: true)
                    .frame(width: min(max(width * 0.36, 220), 320))
                GeometryReader { g in
                    ScrollView(showsIndicators: false) {
                        actions
                            .padding(.horizontal, 20)
                            .padding(.vertical, 16)
                            .frame(maxWidth: .infinity, minHeight: g.size.height)
                    }
                }
                .background(surface())
            }
            statusRow
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    private func titleButton(compact: Bool) -> some View {
        Button { options = true } label: {
            VStack(spacing: compact ? 8 : 10) {
                Text("V")
                    .font(.system(size: compact ? 72 : 104, weight: .heavy, design: .rounded))
                    .foregroundStyle(LinearGradient(gradient: Gradient(colors: [Theme.glow, Theme.accent]), startPoint: .topLeading, endPoint: .bottomTrailing))
                    .shadow(color: Theme.accent.opacity(0.45), radius: 18)
                Text("GTA V").font(.headline).foregroundStyle(.secondary)
                HStack(spacing: 6) {
                    Image(systemName: "slider.horizontal.3")
                    Text("Options")
                    Image(systemName: "chevron.right").font(.caption.weight(.bold))
                }
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.accent)
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .background(Theme.accent.opacity(0.16), in: Capsule())
                Text(s.summary)
                    .font(.caption).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).lineLimit(2)
            }
            .padding(compact ? 16 : 4)
            .frame(maxWidth: .infinity, maxHeight: compact ? .infinity : nil)
            .background(surface().opacity(compact ? 1 : 0))
            .contentShape(Rectangle())
        }
        .buttonStyle(PressStyle())
        .hoverEffect()
        .accessibilityLabel("V options")
        .accessibilityValue(s.summary)
        .accessibilityHint("Mode, resolution, textures, frame limit, look sensitivity, audio and play with friends")
    }

    private var actions: some View {
        VStack(spacing: 14) {
            if !hasCode {
                errorText("This build has no game code \u{2014} build with ./make-ios.sh <game.zip>")
            } else if let p = importer.progress {
                Text(verbatim: "Importing\u{2026} \(Int(p * 100)) %").font(.title3.weight(.semibold).monospacedDigit())
                ProgressView(value: p).frame(maxWidth: 360)
                Text("Keep V open until the import finishes.").font(.footnote).foregroundStyle(.secondary)
            } else if !importer.ready {
                Text("Game files required").font(.title3.weight(.semibold))
                prominentButton("Import game data", "square.and.arrow.down") { picking = true }
                directions
            } else {
                Label("Ready to play", systemImage: "checkmark.circle.fill").font(.headline).foregroundStyle(Theme.glow)
                prominentButton("Play", "play.fill", action: start).disabled(launching)
            }
            if let message = importer.error, importer.progress == nil { errorText(message) }
            if let error {
                errorText(error)
                if errorInOptions {
                    Button("Open V options") { options = true }.font(.footnote.weight(.semibold))
                }
            }
            if hasCode && importer.ready && importer.progress == nil {
                Button("Import other game data\u{2026}") { picking = true }.font(.footnote).foregroundStyle(.secondary)
            }
        }
        .multilineTextAlignment(.center)
    }

    private var directions: some View {
        let device = UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone"
        return VStack(alignment: .leading, spacing: 8) {
            step(1, "Tap Import game data and pick your GTA V game .zip in Files.")
            step(2, "Zip not on this \(device) yet? Copy it over first: on a Mac, open Finder \u{203A} your \(device) \u{203A} Files \u{203A} V and drag the .zip in.")
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.leading)
        .frame(maxWidth: 460, alignment: .leading)
    }

    private func step(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "\(n).circle.fill").foregroundStyle(Theme.accent)
            Text(text).fixedSize(horizontal: false, vertical: true)
        }
    }

    private var statusRow: some View {
        HStack(spacing: 12) {
            Text(verbatim: "\(shortStatus) \u{00B7} Build \(buildNumber)")
            Spacer(minLength: 12)
            Label(controller, systemImage: "gamecontroller")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .lineLimit(1)
    }

    private var shortStatus: String {
        if !hasCode { return "No game code" }
        if let p = importer.progress { return "Importing\u{2026} \(Int(p * 100)) %" }
        return importer.ready ? "Ready to play" : "Game files required"
    }

    private func optionsClosed() {
        if let problem = s.problem() { error = problem; errorInOptions = true; return }
        persist()
    }

    @discardableResult private func persist() -> Bool {
        do {
            try s.save()
            error = nil
            return true
        } catch {
            self.error = "Could not save settings: \(error.localizedDescription)"
            errorInOptions = false
            return false
        }
    }

    func start() {
        guard !launching else { return }
        if let problem = s.problem() { error = problem; errorInOptions = true; return }
        guard persist() else { return }
        launching = true
        play()
    }
}

struct OptionsView: View {
    @ObservedObject var s: Settings
    @Environment(\.dismiss) private var dismiss
    @State private var addressError: String?
    @State private var saveError: String?

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if let saveError { errorText(saveError) }
                    card {
                        header("Mode", "map")
                        Picker("Mode", selection: $s.v.mode) {
                            Text("Story Mode").tag(1)
                            Text("Sandbox").tag(2)
                        }
                        .pickerStyle(.segmented)
                        .frame(maxWidth: 420)
                    }
                    card(note: "Lower runs faster and cooler; higher looks sharper.") {
                        HStack {
                            header("Resolution", "aspectratio")
                            Spacer(minLength: 8)
                            Picker("Resolution", selection: $s.v.scale) {
                                ForEach(scaleTags, id: \.self) { Text(scaleLabel($0)).tag($0) }
                            }
                            .pickerStyle(.menu)
                        }
                    }
                    card(note: textureNote) {
                        HStack {
                            header("Textures", "square.stack.3d.up")
                            Spacer(minLength: 8)
                            Picker("Textures", selection: Binding(get: { s.v.textures }, set: { s.v.textures = $0; s.texturesTouched = true })) {
                                ForEach(0..<3) { Text(Settings.textureNames[$0]).tag($0) }
                            }
                            .pickerStyle(.menu)
                        }
                    }
                    card {
                        header("Frame limit", "speedometer")
                        Picker("Frame limit", selection: $s.v.fps) {
                            ForEach(fpsTags, id: \.self) { Text(fpsLabel($0)).tag($0) }
                        }
                        .pickerStyle(.segmented)
                        .frame(maxWidth: 420)
                    }
                    card {
                        HStack {
                            header("Look sensitivity", "hand.draw")
                            Spacer(minLength: 8)
                            Text(verbatim: "\u{00D7}" + Settings.number(s.v.touch))
                                .font(.body.weight(.semibold).monospacedDigit()).foregroundStyle(Theme.accent)
                        }
                        Slider(value: Binding(get: { min(max(s.v.touch, 0.25), 3) }, set: { s.v.touch = $0 }), in: 0.25...3, step: 0.25) {
                            Text("Look sensitivity")
                        } minimumValueLabel: {
                            Text("Slow").font(.caption).foregroundStyle(.secondary)
                        } maximumValueLabel: {
                            Text("Fast").font(.caption).foregroundStyle(.secondary)
                        }
                        .accessibilityValue("\u{00D7}" + Settings.number(s.v.touch))
                    }
                    card {
                        Toggle(isOn: $s.v.mute) { header("Mute audio", "speaker.slash") }
                    }
                    friends
                    VStack(spacing: 4) {
                        Text(verbatim: "V \u{00B7} Build \(buildNumber)")
                        Text("Based on Muguet by Constantin Clerc (c22dev) \u{00B7} GPL-3.0 license")
                    }
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .padding(.top, 6)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
                .frame(maxWidth: 640)
                .frame(maxWidth: .infinity)
            }
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle("V options")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: done).font(.body.weight(.semibold))
                }
            }
        }
        .navigationViewStyle(.stack)
        .preferredColorScheme(.dark)
        .tint(Theme.accent)
        .onAppear { addressError = s.problem() }
        .onChange(of: s.v) { _ in addressError = nil }
    }

    private var friends: some View {
        card(note: s.v.multiplayer ? nil : "Play together with friends on the same network.") {
            Toggle(isOn: $s.v.multiplayer.animation()) { header("Play with friends", "person.2") }
            if s.v.multiplayer {
                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    Text("Your name").font(.subheadline.weight(.semibold))
                    TextField("Your name", text: $s.v.name, prompt: Text("Optional"))
                        .textInputAutocapitalization(.words)
                        .autocorrectionDisabled()
                        .submitLabel(.done)
                        .modifier(FieldStyle())
                    if s.v.name.trimmingCharacters(in: .whitespacesAndNewlines).unicodeScalars.count > 24 {
                        Text("Names are limited to 24 characters; this one will be saved as \u{201C}\(Settings.cleanName(s.v.name))\u{201D}.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Picker("Session", selection: $s.v.host.animation()) {
                    Text("Host").tag(true)
                    Text("Join").tag(false)
                }
                .pickerStyle(.segmented)
                .frame(maxWidth: 420)
                if s.v.host {
                    Label {
                        Text(verbatim: "Friends join with: \(localIPs().joined(separator: " or "))").textSelection(.enabled)
                    } icon: {
                        Image(systemName: "wifi").foregroundStyle(Theme.glow)
                    }
                    .font(.callout)
                    Text("Friends need to be on the same network as this device.").font(.footnote).foregroundStyle(.secondary)
                } else {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Host address").font(.subheadline.weight(.semibold))
                        TextField("Host address", text: $s.v.server, prompt: Text(verbatim: "192.168.1.20"))
                            .keyboardType(.numbersAndPunctuation)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .submitLabel(.done)
                            .modifier(FieldStyle())
                        if let addressError {
                            errorText(addressError)
                        } else {
                            Text("The address shown on the host's screen. Add :port only if they use a different port.")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }

    private var textureNote: String {
        if s.hasTextureKey || s.texturesTouched { return "V applies this to GTA's settings every time the game starts." }
        if let g = s.gtaTextures {
            return "Showing GTA's current setting (\(Settings.textureNames[g])). V only changes it if you pick another."
        }
        return "Not set yet, so GTA uses its own default until you pick one."
    }

    private var scaleTags: [Double] { Settings.scales.contains(s.loaded.scale) ? Settings.scales : Settings.scales + [s.loaded.scale] }

    private func scaleLabel(_ x: Double) -> String {
        if !Settings.scales.contains(x) { return "Current (\(Settings.percent(x)))" }
        return x == 0.35 ? "35 % (fastest)" : x == 1 ? "100 % (sharpest)" : Settings.percent(x)
    }

    private var fpsTags: [Double] { [30, 60].contains(s.loaded.fps) ? [30, 60] : [30, 60, s.loaded.fps] }

    private func fpsLabel(_ x: Double) -> String {
        if x == 30 { return "30 fps (cooler)" }
        if x == 60 { return "60 fps" }
        return x <= 0 ? "Current (no limit)" : "Current (\(Settings.number(x)) fps)"
    }

    private func header(_ title: String, _ icon: String) -> some View {
        Label {
            Text(title).font(.body.weight(.semibold))
        } icon: {
            Image(systemName: icon).foregroundStyle(Theme.accent)
        }
    }

    private func card<C: View>(note: String? = nil, @ViewBuilder _ content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            content()
            if let note {
                Text(note).font(.footnote).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(surface(16, opacity: 0.55))
    }

    private func done() {
        if let problem = s.problem() { addressError = problem; return }
        do {
            try s.save()
            saveError = nil
            dismiss()
        } catch {
            saveError = "Could not save settings: \(error.localizedDescription)"
        }
    }
}

private struct FieldStyle: ViewModifier {
    func body(content: Content) -> some View {
        content
            .textFieldStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.white.opacity(0.07)))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Theme.accent.opacity(0.25)))
    }
}

func localIPs() -> [String] {
    var out: [String] = []
    var ifs: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&ifs) == 0, let first = ifs else { return ["?"] }
    defer { freeifaddrs(ifs) }
    for p in sequence(first: first, next: { $0.pointee.ifa_next }) {
        guard let a = p.pointee.ifa_addr, a.pointee.sa_family == UInt8(AF_INET) else { continue }
        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        if getnameinfo(a, socklen_t(a.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
            let ip = String(cString: host)
            if !ip.hasPrefix("127.") && !out.contains(ip) { out.append(ip) }
        }
    }
    return out.isEmpty ? ["?"] : out
}

@_cdecl("muguet_show_launcher")
public func muguetShowLauncher(_ viewController: UnsafeMutableRawPointer, _ done: @convention(c) () -> Void) {
    let parent = Unmanaged<UIViewController>.fromOpaque(viewController).takeUnretainedValue()
    weak var sheet: UIViewController?
    let host = UIHostingController(rootView: LauncherView(play: {
        sheet?.dismiss(animated: true)
        done()
    }))
    sheet = host
    host.view.backgroundColor = UIColor(Theme.background)
    host.overrideUserInterfaceStyle = .dark
    host.modalPresentationStyle = .fullScreen
    host.isModalInPresentation = true
    parent.present(host, animated: false)
}

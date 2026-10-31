import SwiftUI

/// Opened from the V title before Play. Each control shows this headset's
/// current saved value and saves immediately; Play validates the session.
struct VLauncherOptionsView: View {
    @ObservedObject var runtime: NativeRuntimeModel
    let moreSettings: () -> Void
    @Environment(\.dismiss) private var dismiss
    private let accent = Color(red: 0.62, green: 0.43, blue: 1)

    var body: some View {
        NavigationStack {
            Form {
                Section("Mode") {
                    VModePicker(runtime: runtime)
                    Text("Story Mode uses your story saves. Sandbox starts the original free-roam sandbox.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Section("Graphics") {
                    Picker("Resolution", selection: Binding(
                        get: { runtime.graphicsSettings.resolution },
                        set: { runtime.setGraphicsResolution($0) })) {
                        ForEach(NativeGraphicsSettings.Resolution.allCases) { Text($0.label).tag($0) }
                    }
                    .disabled(!runtime.canChangeGraphics)
                    Picker("Textures", selection: Binding(
                        get: { runtime.effectiveTextureQuality },
                        set: { runtime.setTextureQuality($0) })) {
                        ForEach(0..<3) { Text(NativeTextureChoice.labels[$0]).tag($0) }
                    }
                    .disabled(!runtime.canChangePlaySettings)
                    Text(textureDetail).font(.caption).foregroundStyle(.secondary)
                    Picker("Frame limit", selection: Binding(
                        get: { runtime.graphicsSettings.fpsLimit },
                        set: { runtime.setGraphicsFrameCap($0) })) {
                        Text("30 FPS (cooler)").tag(30)
                        Text("60 FPS").tag(60)
                    }
                    .disabled(!runtime.canChangeGraphics)
                }
                Section("Controls and sound") {
                    LabeledContent("Look sensitivity", value: "\(Int(runtime.playSettings.lookSensitivityPercent.rounded()))%")
                    Slider(value: Binding(get: { runtime.playSettings.lookSensitivityPercent },
                                          set: { runtime.setLookSensitivity($0) }), in: 0...100, step: 1)
                        .accessibilityLabel("Look sensitivity")
                    Toggle("Mute audio", isOn: Binding(get: { runtime.playSettings.mute }, set: { runtime.setMute($0) }))
                    Text("Volume stays at \(Int((runtime.playSettings.masterVolume * 100).rounded()))% for when you unmute.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .disabled(!runtime.canChangePlaySettings)
                multiplayerSection
                    .disabled(!runtime.canChangePlaySettings)
                Section {
                    Button("More V settings…", systemImage: "gearshape") { moreSettings() }
                    if !runtime.launcherOptionsStatus.isEmpty {
                        Text(runtime.launcherOptionsStatus).font(.caption).foregroundStyle(.orange)
                    }
                    if !runtime.playSettingsStatus.isEmpty {
                        Text(runtime.playSettingsStatus).font(.caption).foregroundStyle(.orange)
                    }
                }
            }
            .navigationTitle("V options")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .onAppear { runtime.refreshLauncherOptions() }
        }
        .frame(width: 600, height: 640)
        .tint(accent)
    }

    private var textureDetail: String {
        let gta = runtime.gtaTextureQuality.map { "GTA currently uses \(NativeTextureChoice.labels[$0])." } ?? "GTA has not saved a texture setting yet."
        return runtime.textureChoice == nil
            ? "\(gta) Choosing here applies at each launch, independent of the Quality preset."
            : "Applied at each launch, independent of the Quality preset. \(gta)"
    }

    @ViewBuilder private var multiplayerSection: some View {
        let settings = runtime.multiplayerSettings
        Section {
            Toggle("Play with friends", isOn: Binding(get: { settings.enabled },
                                                      set: { value in runtime.updateMultiplayer { $0.enabled = value } }))
            if settings.enabled {
                TextField("Your name", text: Binding(get: { runtime.multiplayerSettings.playerName },
                                                     set: { value in runtime.updateMultiplayer { $0.playerName = value } }),
                          prompt: Text(NativeMultiplayerSettings.defaultPlayerName))
                    .textInputAutocapitalization(.words)
                Picker("Session", selection: Binding(get: { settings.host },
                                                     set: { value in runtime.updateMultiplayer { $0.host = value } })) {
                    Text("Join").tag(false)
                    Text("Host").tag(true)
                }
                .pickerStyle(.segmented)
                if settings.host {
                    let addresses = localIPv4Addresses()
                    Text(addresses.isEmpty ? "Connect to Wi-Fi so friends can join."
                         : "Friends join with: \(addresses.joined(separator: " or "))")
                        .font(.callout).textSelection(.enabled)
                } else {
                    TextField("Host address", text: Binding(get: { runtime.multiplayerSettings.server },
                                                            set: { value in runtime.updateMultiplayer { $0.server = value } }),
                              prompt: Text("192.168.1.20"))
                        .keyboardType(.numbersAndPunctuation)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
                if let problem = settings.problem {
                    Text(problem).font(.caption).foregroundStyle(.orange)
                }
            }
        } header: {
            Text("Play with friends")
        } footer: {
            Text("Same Wi-Fi network, UDP port \(NativeMultiplayerSettings.defaultPort). Each player needs V with their own game files; the host also plays. Allow Local Network when V asks. This is V's own session, not GTA Online.")
        }
    }
}

/// Story Mode or Sandbox; stored as native-gameplay.json developer_mode.
struct VModePicker: View {
    @ObservedObject var runtime: NativeRuntimeModel
    var body: some View {
        Picker("Mode", selection: Binding(get: { runtime.playSettings.developerMode },
                                          set: { runtime.setDeveloperMode($0) })) {
            Text("Story Mode").tag(false)
            Text("Sandbox").tag(true)
        }
        .pickerStyle(.segmented)
        .disabled(!runtime.canChangePlaySettings)
    }
}

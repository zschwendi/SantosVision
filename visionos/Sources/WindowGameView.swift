import Darwin
import GameController
import QuartzCore
import SwiftUI
import UIKit

private enum SantosTheme {
    static let background = Color(red: 0.035, green: 0.018, blue: 0.065)
    static let surface = Color(red: 0.16, green: 0.08, blue: 0.24)
    static let accent = Color(red: 0.62, green: 0.43, blue: 1)
    static let glow = Color(red: 0.42, green: 0.82, blue: 0.95)
}

/// A native game drawable. wgpu retains its CAMetalLayer; this view owns its
/// attachment and never changes drawableSize behind the configured swapchain.
private final class GameMetalView: UIView {
    var windowAttachmentChanged: ((Bool) -> Void)?
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        metalLayer.isOpaque = true
        metalLayer.contentsScale = 1
        metalLayer.framebufferOnly = true
        // visionOS otherwise translates controller actions into window/pinch
        // interactions instead of delivering them to the game's input poller.
        let controllerEvents = GCEventInteraction()
        controllerEvents.handledEventTypes = .gamepad
        controllerEvents.receivesEventsInView = false
        addInteraction(controllerEvents)
    }
    required init?(coder: NSCoder) { fatalError("GameMetalView uses programmatic initialization") }
    override func willMove(toWindow newWindow: UIWindow?) {
        // Suspend input/presentation before UIKit detaches this drawable.
        if newWindow == nil { windowAttachmentChanged?(false) }
        super.willMove(toWindow: newWindow)
    }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        windowAttachmentChanged?(window != nil)
    }
}

/// The app, rather than a window's view tree, owns this single drawable. Closing
/// and reopening the window reattaches the same layer to the one-shot runtime.
@MainActor
final class WindowGameSurface: ObservableObject {
    let view: UIView = GameMetalView(frame: .zero)
    var metalLayer: CAMetalLayer { view.layer as! CAMetalLayer }
    func observeAttachment(_ callback: @escaping (Bool) -> Void) {
        (view as! GameMetalView).windowAttachmentChanged = callback
    }
}

private struct GameCanvas: UIViewRepresentable {
    let runtime: NativeRuntimeModel
    let surface: WindowGameSurface
    func makeUIView(context: Context) -> UIView {
        surface.observeAttachment { runtime.setWindowAttached($0) }
        // Defer observable-state changes until SwiftUI finishes constructing
        // this view. Attachment still completes before any engine startup.
        Task { @MainActor in
            if !runtime.windowSurfaceReady {
                guard runtime.attachWindowLayer(surface.metalLayer) else { return }
            }
            await runtime.bootstrap()
        }
        return surface.view
    }
    func updateUIView(_ view: UIView, context: Context) {}
}

struct WindowGameView: View {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var surface: WindowGameSurface
    @State private var showingSettings = false
    #if SV_GAME_RUNTIME
    @EnvironmentObject private var world: GameWorldModel
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @Environment(\.dismissWindow) private var dismissWindow
    #endif

    private var shortStatus: String {
        if runtime.engineFailed { return "Game stopped" }
        if runtime.hasStarted { return "Playing · \(runtime.graphicsSettings.summary)" }
        if runtime.isBusy { return "Loading…" }
        return runtime.hasGameData ? "Ready to play" : "Game files required"
    }

    var body: some View {
        ZStack {
            // Keep this view mounted across launcher/play transitions. The
            // native renderer continues to own this exact CAMetalLayer.
            GameCanvas(runtime: runtime, surface: surface)
                .aspectRatio(16 / 9, contentMode: .fit)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .ignoresSafeArea()
            if !runtime.startupAttempted {
                launcher
            } else if !runtime.hasStarted || runtime.engineFailed {
                VStack(spacing: 16) {
                    if runtime.isBusy { ProgressView() }
                    Text(runtime.engineFailed ? "Game stopped" : (runtime.isBusy ? "Starting Los Santos…" : "Game could not start"))
                        .font(.title2)
                    Text(runtime.status).font(.caption).foregroundStyle(.secondary)
                    if !runtime.isBusy {
                        Button("Quit game", systemImage: "power", role: .destructive) { quitGame() }
                    }
                }
                .padding(28)
                .glassBackgroundEffect()
            }
        }
        .frame(minWidth: 640, minHeight: 360)
        .background(.black)
        .background(GameWindowGeometry(isGameplay: runtime.startupAttempted))
        .preferredColorScheme(.dark)
        .tint(SantosTheme.accent)
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setWindowSceneActive(phase == .active)
        }
        .ornament(visibility: runtime.hasStarted ? .visible : .hidden,
                  attachmentAnchor: .scene(.topTrailing)) {
            #if SV_GAME_RUNTIME
            Button { Task { await expandGame() } } label: {
                Image(systemName: "arrow.up.left.and.arrow.down.right")
                    .font(.system(size: 17, weight: .semibold))
                    .frame(width: 44, height: 44)
            }
            .buttonStyle(.plain)
            .glassBackgroundEffect(in: Circle())
            .accessibilityLabel("Enter immersive view")
            .disabled(world.isOpening || world.isImmersiveActive || runtime.engineFailed)
            #endif
        }
        .ornament(visibility: runtime.hasStarted && runtime.gameControlsVisible ? .visible : .hidden,
                  attachmentAnchor: .scene(.bottom), contentAlignment: .top) {
            SantosPauseControls(runtime: runtime,
                windowAction: { runtime.gameControlsVisible = false },
                settingsAction: { showingSettings = true })
                .padding(.top, 12)
        }
        .sheet(isPresented: $showingSettings) { SantosSettingsView(runtime: runtime) }
        #if SV_GAME_RUNTIME
        .onChange(of: world.hasPresentedFrames) { _, presented in
            if presented, world.isImmersiveActive { dismissWindow(id: "main") }
        }
        #endif
    }

    private var launcher: some View {
        VStack(spacing: 24) {
            HStack(spacing: 12) {
                Image(systemName: "sparkles").foregroundStyle(SantosTheme.glow)
                Text("Los Santos").font(.title3.bold())
                Spacer()
                Button { showingSettings = true } label: {
                    Image(systemName: "gearshape").padding(4)
                }
                .accessibilityLabel("Settings")
                Button("Quit game", systemImage: "power", role: .destructive) {
                    quitGame()
                }
            }
            VStack(spacing: 18) {
                Image(systemName: "building.2.crop.circle")
                    .font(.system(size: 58, weight: .light))
                    .foregroundStyle(SantosTheme.glow)
                Text("Los Santos").font(.largeTitle.weight(.semibold))
                Text("GTA V").font(.headline).foregroundStyle(.secondary)
                if runtime.isBusy {
                    ProgressView()
                } else {
                    Button("Play", systemImage: "play.fill") { Task { await runtime.start() } }
                        .buttonStyle(.borderedProminent)
                        .disabled(!runtime.probePassed || !runtime.hasGameData || !runtime.windowSurfaceReady)
                }
                Text(shortStatus).font(.subheadline).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(SantosTheme.surface.opacity(0.45), in: RoundedRectangle(cornerRadius: 22))
            HStack {
                Circle().fill(runtime.hasStarted ? Color.green : SantosTheme.glow).frame(width: 6, height: 6)
                Text(shortStatus).font(.caption).foregroundStyle(.secondary)
                Spacer()
                Label(runtime.controllerSummary, systemImage: "gamecontroller").font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(28)
        .background(SantosTheme.background)
    }

    #if SV_GAME_RUNTIME
    private func expandGame() async {
        guard runtime.hasStarted, !runtime.engineFailed, !world.isOpening, !world.isImmersiveActive else { return }
        world.isOpening = true
        world.status = "Opening immersive Los Santos…"
        switch await openImmersiveSpace(id: GameWorldModel.spaceID) {
        case .opened:
            world.isImmersiveActive = true
            if world.hasPresentedFrames { dismissWindow(id: "main") }
        case .userCancelled:
            world.stopRenderer()
            world.status = "Immersive view cancelled."
        case .error:
            world.stopRenderer()
            world.status = "Immersive view could not open."
        @unknown default:
            world.stopRenderer()
            world.status = "Immersive view could not open."
        }
        world.isOpening = false
    }
    #endif

    private func quitGame() {
        Task { await runtime.quitGame() }
    }
}

/// Match Dark Eco's uniform game-window resizing while keeping the launcher
/// freeform. This changes window geometry, never the renderer's drawable size.
private struct GameWindowGeometry: UIViewRepresentable {
    let isGameplay: Bool
    func makeUIView(context: Context) -> GameWindowGeometryHost {
        let view = GameWindowGeometryHost()
        view.update(isGameplay)
        return view
    }
    func updateUIView(_ view: GameWindowGeometryHost, context: Context) { view.update(isGameplay) }
}

private final class GameWindowGeometryHost: UIView {
    private var isGameplay = false
    private weak var appliedScene: UIWindowScene?
    private var appliedGameplay: Bool?

    init() {
        super.init(frame: .zero)
        isUserInteractionEnabled = false
    }
    required init?(coder: NSCoder) { fatalError("GameWindowGeometryHost uses programmatic initialization") }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        applyPolicy()
    }
    func update(_ isGameplay: Bool) {
        self.isGameplay = isGameplay
        applyPolicy()
    }
    private func applyPolicy() {
        guard let window, let scene = window.windowScene,
              scene !== appliedScene || appliedGameplay != isGameplay else { return }
        let bounds = window.bounds.size
        guard bounds.width > 0, bounds.height > 0 else { return }
        let size: CGSize? = isGameplay ? CGSize(width: bounds.width, height: bounds.width * 9 / 16) : nil
        appliedScene = scene
        appliedGameplay = isGameplay
        scene.requestGeometryUpdate(.Vision(size: size, resizingRestrictions: isGameplay ? .uniform : .freeform)) { error in
            NSLog("SantosVision window geometry: %@", error.localizedDescription)
        }
    }
}

struct SantosSettingsView: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Form {
                Section("Graphics") {
                    Picker("Resolution", selection: Binding(
                        get: { runtime.graphicsSettings.resolution },
                        set: { runtime.setGraphicsResolution($0) })) {
                        ForEach(NativeGraphicsSettings.Resolution.allCases) { resolution in
                            Text(resolution.label).tag(resolution)
                        }
                    }
                    Picker("Frame cap", selection: Binding(
                        get: { runtime.graphicsSettings.fpsLimit },
                        set: { runtime.setGraphicsFrameCap($0) })) {
                        Text("30 FPS").tag(30)
                        Text("60 FPS").tag(60)
                    }
                    Text("GTA graphics choices are preserved.").foregroundStyle(.secondary)
                }
                .disabled(!runtime.canChangeGraphics)
                CloudSaveSettingsView(runtime: runtime, saves: runtime.cloudSaves)
                Section("Sound") {
                    LabeledContent("Master volume", value: "\(Int((runtime.playSettings.masterVolume * 100).rounded()))%")
                    Slider(value: Binding(get: { runtime.playSettings.masterVolume },
                                          set: { runtime.setMasterVolume($0) }), in: 0...1, step: 0.01)
                }
                .disabled(!runtime.canChangePlaySettings)
                Section("Controller") {
                    Text(runtime.controllerSummary)
                    LabeledContent("Look sensitivity", value: "\(Int(runtime.playSettings.lookSensitivityPercent.rounded()))%")
                    Slider(value: Binding(get: { runtime.playSettings.lookSensitivityPercent },
                                          set: { runtime.setLookSensitivity($0) }), in: 0...100, step: 1)
                        .disabled(!runtime.canChangePlaySettings)
                    Text("Adjusts the right stick's look speed. GTA's own mouse sensitivity also applies.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Click the touchpad to cycle cameras. In GTA menus: D-pad to navigate, Cross to confirm, Circle to go back, Square to apply, and L1 for Tab. Start also reveals the Los Santos controls.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Section {
                    Toggle("Rendering optimizations", isOn: Binding(
                        get: { runtime.drawSetupOverheadEnabled },
                        set: { runtime.setDrawSetupOverhead($0) }))
                        .disabled(!runtime.canChangeRenderingOptimizations)
                    if runtime.hasStarted {
                        Text("Quit and reopen to change graphics, sound, or look sensitivity.").font(.caption).foregroundStyle(.secondary)
                    }
                    if !runtime.playSettingsStatus.isEmpty {
                        Text(runtime.playSettingsStatus).font(.caption).foregroundStyle(.secondary)
                    }
                    Button("Save screenshot", systemImage: "camera") { runtime.captureCurrentFrame() }
                        .disabled(!runtime.hasStarted)
                    Button("Quit game", systemImage: "power", role: .destructive) { quitGame() }
                }
                DisclosureGroup("Diagnostics") {
                    Text(runtime.status).font(.caption)
                    Text(runtime.nativeDetails).font(.system(.caption2, design: .monospaced)).textSelection(.enabled)
                }
            }
            .navigationTitle("Settings")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .frame(width: 560, height: 540)
        .tint(SantosTheme.accent)
    }

    private func quitGame() {
        Task { await runtime.quitGame() }
    }
}

struct SantosPauseControls: View {
    @ObservedObject var runtime: NativeRuntimeModel
    let windowAction: () -> Void
    let settingsAction: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 16) {
                Button("Window", systemImage: "arrow.down.right.and.arrow.up.left", action: windowAction)
                Button("Los Santos settings", systemImage: "gearshape", action: settingsAction)
                Button("Quit", systemImage: "power", role: .destructive) {
                    Task { await runtime.quitGame() }
                }
            }
            HStack(spacing: 16) {
                Button("Resume") { runtime.sendMenuAction(1); runtime.gameControlsVisible = false }
                Button("Apply") { runtime.sendMenuAction(3) }
                Button("Yes") { runtime.sendMenuAction(4) }
                Button("No") { runtime.sendMenuAction(5) }
            }
            Text("Cross · Select    Circle · Back    Square · Apply    Triangle · Yes    R1 · No")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(16)
        .glassBackgroundEffect()
        .tint(SantosTheme.accent)
        .disabled(runtime.isQuitting)
    }
}

private struct CloudSaveSettingsView: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @ObservedObject var saves: CloudSaveModel

    var body: some View {
        Section("iCloud saves") {
            Text(saves.status).font(.caption).foregroundStyle(.secondary)
            Button("Back up now", systemImage: "icloud.and.arrow.up") {
                Task { await saves.backupNow() }
            }.disabled(saves.isBusy || runtime.isQuitting)
            DisclosureGroup("Save history") {
                Button("Refresh backups") { Task { await saves.loadHistory() } }
                    .disabled(saves.isBusy)
                if saves.revisions.isEmpty { Text("No archived saves yet.").font(.caption) }
                ForEach(saves.revisions, id: \.id) { revision in
                    Button {
                        Task { await saves.restore(revision.id) }
                    } label: {
                        VStack(alignment: .leading) {
                            Text(revision.createdAt, format: .dateTime.month().day().hour().minute())
                            Text("\(revision.fileCount) files · \(revision.inCloud ? "iCloud history" : "Local backup")")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .disabled(runtime.startupAttempted || runtime.isBusy || saves.isBusy)
                }
                Text("Recovery preserves the current save first. Quit and reopen before restoring.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

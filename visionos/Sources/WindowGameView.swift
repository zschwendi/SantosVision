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
    #if SV_GAME_RUNTIME
    @EnvironmentObject private var world: GameWorldModel
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @Environment(\.dismissWindow) private var dismissWindow
    @State private var attemptedAutomaticImmersion = false
    #endif

    private var shortStatus: String {
        if runtime.engineFailed { return "Game stopped" }
        if runtime.hasStarted { return "Playing · \(runtime.graphicsSettings.summary)" }
        if runtime.isBusy { return "Loading…" }
        return runtime.hasGameData ? "Ready to play" : "Game files required"
    }

    private var senseWindowUtilitiesVisible: Bool {
        #if SV_GAME_RUNTIME
        runtime.senseGameplayUtilitiesVisible && !world.isOpening && !world.isImmersiveActive
        #else
        runtime.senseGameplayUtilitiesVisible
        #endif
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
            #if SV_GAME_RUNTIME
            if runtime.hasStarted, !runtime.engineFailed,
               (world.isOpening || world.isImmersiveActive), !world.hasPresentedFrames {
                VStack(spacing: 14) {
                    ProgressView()
                    Text("Opening immersive view…").font(.headline)
                    Text("Waiting for the first game frame.")
                        .font(.caption).foregroundStyle(.secondary)
                    Button("Return to window", systemImage: "arrow.down.right.and.arrow.up.left") {
                        Task { await returnToWindow() }
                    }
                    .disabled(world.isOpening)
                }
                .padding(24)
                .glassBackgroundEffect()
            }
            #endif
        }
        .frame(minWidth: 640, minHeight: 360)
        .background(.black)
        .background(GameWindowGeometry(isGameplay: runtime.startupAttempted))
        .preferredColorScheme(.dark)
        .tint(SantosTheme.accent)
        .onChange(of: scenePhase, initial: true) { _, phase in
            runtime.setWindowSceneActive(phase == .active)
        }
        .ornament(visibility: senseWindowUtilitiesVisible ? .visible : .hidden,
                  attachmentAnchor: .scene(.bottom), contentAlignment: .top) {
            if senseWindowUtilitiesVisible {
                SenseGameplayUtilities(runtime: runtime)
                    .padding(.top, 12)
            }
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
                windowAction: { runtime.gameControlsVisible = false })
                .padding(.top, 12)
        }
        .sheet(isPresented: $runtime.nativeSettingsPresented) { SantosSettingsView(runtime: runtime) }
        #if SV_GAME_RUNTIME
        .task {
            // Exercise the normal window-to-world transition after real window
            // rendering has started, so cold loading does not spend the world's
            // first-frame timeout. This is opt-in device QA, not launch policy.
            guard ProcessInfo.processInfo.arguments.contains("--sv-auto-immersive-after-window-frames"),
                  !attemptedAutomaticImmersion else { return }
            let deadline = ContinuousClock.now + .seconds(60)
            while !Task.isCancelled, ContinuousClock.now < deadline {
                if runtime.engineFailed { return }
                if runtime.hasStarted, runtime.presentedWindowFrames >= 300 {
                    attemptedAutomaticImmersion = true
                    await expandGame()
                    return
                }
                do { try await Task.sleep(for: .milliseconds(250)) }
                catch { return }
            }
        }
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
                Button { runtime.nativeSettingsPresented = true } label: {
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

    private func returnToWindow() async {
        world.stopRenderer(reason: "window_transition_cancelled")
        await dismissImmersiveSpace()
        world.isImmersiveActive = false
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
                    Text("MSAA is turned off at launch for immersive view. FXAA and your other GTA graphics choices are preserved.")
                        .font(.caption).foregroundStyle(.secondary)
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
                    Text("DualSense: click the touchpad to change camera. Sense: click both sticks together to change camera; hold left stick click and move that stick for D-pad. Tap left stick click for normal L3. The Phone and View buttons sit below the game. Look at a native button and press either Sense trigger to select it. Start opens Resume, Window, Settings and Quit. In GTA menus: Cross selects, Circle goes back, Square applies, Triangle means Yes, and R1 means No.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Section {
                    Toggle("Rendering optimizations", isOn: Binding(
                        get: { runtime.drawSetupOverheadEnabled },
                        set: { runtime.setDrawSetupOverhead($0) }))
                        .disabled(!runtime.canChangeRenderingOptimizations)
                    if runtime.hasStarted {
                        Text("Graphics, sound and look sensitivity apply at the next launch. Quit the game, reopen Los Santos, change them before Play, then launch again. In GTA’s Settings menu, use Square to apply GTA settings.").font(.caption).foregroundStyle(.secondary)
                    }
                    if !runtime.playSettingsStatus.isEmpty {
                        Text(runtime.playSettingsStatus).font(.caption).foregroundStyle(.secondary)
                    }
                    Button("Save screenshot", systemImage: "camera") { runtime.captureCurrentFrame() }
                        .disabled(!runtime.hasStarted)
                    Button("Quit game", systemImage: "power", role: .destructive) { quitGame() }
                }
                Section {
                    Toggle("Developer mode", isOn: Binding(
                        get: { runtime.playSettings.developerMode },
                        set: { runtime.setDeveloperMode($0) }))
                        .disabled(!runtime.canChangePlaySettings)
                    Text("Starts in Sandbox. Turn off for Story Mode and story saves. Quit and reopen to change this setting.")
                        .font(.caption).foregroundStyle(.secondary)
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
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(true))
    }

    private func quitGame() {
        Task { await runtime.quitGame() }
    }
}

struct SantosPauseControls: View {
    @ObservedObject var runtime: NativeRuntimeModel
    let windowAction: () -> Void
    var immersive = false
    @State private var registration = UUID()

    var body: some View {
        VStack(spacing: 12) {
            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 10) {
                ForEach(SantosControlAction.allCases) { action in
                    Button {
                        runtime.activatePauseButton(action)
                    } label: {
                        Label(action.title, systemImage: action.symbol)
                            .frame(maxWidth: .infinity, minHeight: 48)
                    }
                    .buttonStyle(.bordered)
                    .tint(action.rawValue == runtime.nativeControlsSelection ? SantosTheme.glow : SantosTheme.accent)
                    .overlay(RoundedRectangle(cornerRadius: 16).stroke(
                        action.rawValue == runtime.nativeControlsSelection ? SantosTheme.glow : .clear, lineWidth: 2))
                    .accessibilityAddTraits(action.rawValue == runtime.nativeControlsSelection ? .isSelected : [])
                }
            }
            Text("Look and click · or stick + Cross\nCircle / Start · Resume")
                .font(.caption2).foregroundStyle(.secondary).multilineTextAlignment(.center)
            if !runtime.nativeControlsStatus.isEmpty { Text(runtime.nativeControlsStatus).font(.caption) }
        }
        .frame(width: 340)
        .padding(16)
        .glassBackgroundEffect()
        .tint(SantosTheme.accent)
        .disabled(runtime.isQuitting)
        // Keep real system gaze/pinch routing in the native UI. Gameplay's
        // Metal view remains exclusive GC input; modal ownership blocks leaks.
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(true))
        .onAppear {
            runtime.registerPauseWindowAction(token: registration, immersive: immersive, action: windowAction)
        }
        .onDisappear {
            runtime.unregisterPauseWindowAction(token: registration, immersive: immersive)
        }
    }
}

/// Compact gameplay-only alternatives to controls absent from Sense hardware.
/// The flat game hosts this in a native window ornament outside its picture.
struct SenseGameplayUtilities: View {
    @ObservedObject var runtime: NativeRuntimeModel
    var body: some View {
        HStack(alignment: .bottom, spacing: 10) {
            mainButton(.phone)
            if runtime.sensePhoneNavigationVisible {
                SensePhoneArrowOrnament(runtime: runtime)
                    .padding()
            }
            mainButton(.changeView)
        }
        .padding(8)
        .handlesGameControllerEvents(matching: .gamepad, withOptions: .receivesEventsInView(true))
        .onDisappear { runtime.clearFlatSenseUtilityPressed() }
    }

    private func mainButton(_ action: SantosSenseUtilityAction) -> some View {
        SenseUtilityControl(runtime: runtime, action: action)
            .frame(width: 108, height: 48)
            .glassBackgroundEffect(in: Capsule())
            .overlay(alignment: .topTrailing) {
                if action == .phone, runtime.sensePhoneNavigationVisible {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 16, weight: .semibold))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .black)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
    }
}

/// Exact layout/metrics from current PeelApple Shared/Client's
/// KeyboardArrowKeysOverlay + KeyboardMacroKeysOverlay: 164 x 78 points,
/// blank/Left, Up/Down, blank/Right, with individual glass keys.
private struct SensePhoneArrowOrnament: View {
    let runtime: NativeRuntimeModel
    private let spacing: CGFloat = 10
    var body: some View {
        HStack(spacing: spacing) {
            VStack(spacing: spacing) { placeholder; arrow(.left) }
            VStack(spacing: spacing) { arrow(.up); arrow(.down) }
            VStack(spacing: spacing) { placeholder; arrow(.right) }
        }
        .onDisappear { runtime.clearFlatSensePhoneInteractions() }
    }
    private var placeholder: some View {
        Color.clear.frame(width: 48, height: 34)
            .allowsHitTesting(false).accessibilityHidden(true)
    }
    private func arrow(_ action: SantosSenseUtilityAction) -> some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        return SenseUtilityControl(runtime: runtime, action: action)
            .frame(width: 48, height: 34)
            .contentShape(shape)
            .clipShape(shape)
            .glassBackgroundEffect(in: shape)
    }
}

/// A system control is the event target, rather than an image inside a custom
/// SwiftUI ButtonStyle. Its own interaction explicitly permits UIKit's
/// gaze/trigger events while the adjacent Metal canvas retains raw game input.
/// Sense's system indirect select is a trigger; Cross remains a GTA action.
private struct SenseUtilityControl: UIViewRepresentable {
    let runtime: NativeRuntimeModel
    let action: SantosSenseUtilityAction

    func makeCoordinator() -> Coordinator { Coordinator(runtime: runtime, action: action) }

    func makeUIView(context: Context) -> UIButton {
        let button = UIButton(configuration: configuration)
        updateAccessibility(button)
        button.accessibilityHint = "Look at this button and press either Sense trigger, or pinch."
        button.isPointerInteractionEnabled = true
        let controllerEvents = GCEventInteraction()
        controllerEvents.handledEventTypes = .gamepad
        controllerEvents.receivesEventsInView = true
        button.addInteraction(controllerEvents)
        button.addTarget(context.coordinator, action: #selector(Coordinator.beginUtilityPress), for: [.touchDown, .touchDragEnter])
        button.addTarget(context.coordinator, action: #selector(Coordinator.endUtilityPress),
                         for: [.touchUpInside, .touchUpOutside, .touchCancel, .touchDragExit])
        button.addTarget(context.coordinator, action: #selector(Coordinator.activate), for: .primaryActionTriggered)
        return button
    }

    func updateUIView(_ button: UIButton, context: Context) {
        context.coordinator.runtime = runtime
        button.configuration = configuration
        updateAccessibility(button)
    }

    private var configuration: UIButton.Configuration {
        var value = action.isDirection ? UIButton.Configuration.plain() : .tinted()
        value.title = action.isDirection ? nil : action == .changeView ? "View" : "Phone"
        value.image = UIImage(systemName: action.symbol,
                              withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .semibold))
        if action.isDirection {
            value.contentInsets = .zero
            value.baseForegroundColor = .label
        } else {
            value.imagePadding = 8
            value.cornerStyle = .capsule
            value.baseForegroundColor = .white
            value.baseBackgroundColor = UIColor(red: 0.62, green: 0.43, blue: 1, alpha: 1)
        }
        return value
    }

    private func updateAccessibility(_ button: UIButton) {
        button.accessibilityLabel = action == .phone && runtime.sensePhoneNavigationVisible ? "Close phone" : action.title
    }

    static func dismantleUIView(_ button: UIButton, coordinator: Coordinator) {
        coordinator.endUtilityPress()
    }

    @MainActor final class Coordinator: NSObject {
        var runtime: NativeRuntimeModel
        let action: SantosSenseUtilityAction
        init(runtime: NativeRuntimeModel, action: SantosSenseUtilityAction) {
            self.runtime = runtime
            self.action = action
        }
        @objc func beginUtilityPress() { runtime.setFlatSenseUtilityInteractionPressed(action, pressed: true) }
        @objc func endUtilityPress() { runtime.setFlatSenseUtilityInteractionPressed(action, pressed: false) }
        @objc func activate() {
            // Assistive/system activation may emit only primaryActionTriggered.
            // Acquire before enqueueing and release even without a touch-up.
            beginUtilityPress()
            defer { endUtilityPress() }
            runtime.activateFlatSenseUtility(action)
        }
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

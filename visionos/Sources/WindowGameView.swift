import Darwin
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
    override class var layerClass: AnyClass { CAMetalLayer.self }
    var metalLayer: CAMetalLayer { layer as! CAMetalLayer }
    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        metalLayer.isOpaque = true
        metalLayer.contentsScale = 1
        metalLayer.framebufferOnly = true
    }
    required init?(coder: NSCoder) { fatalError("GameMetalView uses programmatic initialization") }
}

private struct GameCanvas: UIViewRepresentable {
    let runtime: NativeRuntimeModel
    func makeUIView(context: Context) -> UIView {
        let view = GameMetalView(frame: .zero)
        // Defer observable-state changes until SwiftUI finishes constructing
        // this view. Attachment still completes before any engine startup.
        Task { @MainActor in
            guard runtime.attachWindowLayer(view.metalLayer) else { return }
            await runtime.bootstrap()
        }
        return view
    }
    func updateUIView(_ view: UIView, context: Context) {}
}

struct WindowGameView: View {
    @ObservedObject var runtime: NativeRuntimeModel
    @State private var showingSettings = false

    private var shortStatus: String {
        if runtime.engineFailed { return "Game stopped" }
        if runtime.hasStarted { return "Playing · \(runtime.graphicsSettings.summary)" }
        if runtime.isBusy { return "Loading…" }
        return runtime.hasGameData ? "Ready to play" : "Game files required"
    }

    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 12) {
                Image(systemName: "sparkles").foregroundStyle(SantosTheme.glow)
                Text("SantosVision").font(.title3.bold())
                Spacer()
                Button { showingSettings = true } label: {
                    Image(systemName: "gearshape").padding(4)
                }
                .accessibilityLabel("Settings")
                Button("Quit game", systemImage: "power", role: .destructive) {
                    quitGame()
                }
            }
            GameCanvas(runtime: runtime)
                .aspectRatio(16 / 9, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 18))
                .overlay {
                    RoundedRectangle(cornerRadius: 18)
                        .stroke(SantosTheme.accent.opacity(0.45), lineWidth: 1)
                }
                .overlay {
                    if !runtime.hasStarted {
                        VStack(spacing: 16) {
                            Text("Los Santos").font(.largeTitle.weight(.semibold))
                            if runtime.isBusy {
                                ProgressView()
                            } else {
                                Button("Play", systemImage: "play.fill") {
                                    Task { await runtime.start() }
                                }
                                .buttonStyle(.borderedProminent)
                                .disabled(!runtime.probePassed || !runtime.hasGameData || !runtime.windowSurfaceReady)
                            }
                            Text(shortStatus).font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                }
            HStack {
                Circle().fill(runtime.hasStarted ? Color.green : SantosTheme.glow).frame(width: 6, height: 6)
                Text(shortStatus).font(.caption).foregroundStyle(.secondary)
                Spacer()
                Label("Controller", systemImage: "gamecontroller").font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(20)
        .background(SantosTheme.background)
        .preferredColorScheme(.dark)
        .tint(SantosTheme.accent)
        .sheet(isPresented: $showingSettings) { settings }
    }

    private var settings: some View {
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
                    Text("Low preset · Audio muted").foregroundStyle(.secondary)
                }
                .disabled(!runtime.canChangeGraphics)
                Section {
                    Toggle("Rendering optimizations", isOn: Binding(
                        get: { runtime.drawSetupOverheadEnabled },
                        set: { runtime.setDrawSetupOverhead($0) }))
                        .disabled(!runtime.canChangeRenderingOptimizations)
                    if runtime.hasStarted {
                        Text("Quit and reopen to change graphics settings.").font(.caption).foregroundStyle(.secondary)
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
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showingSettings = false } } }
        }
        .frame(width: 560, height: 540)
        .tint(SantosTheme.accent)
    }

    private func quitGame() {
        runtime.setActive(false)
        // The engine has process-wide Wasm/GPU threads and one-shot runtime
        // state. Exiting guarantees all threads/audio stop; reopening is fresh.
        Darwin.exit(EXIT_SUCCESS)
    }
}

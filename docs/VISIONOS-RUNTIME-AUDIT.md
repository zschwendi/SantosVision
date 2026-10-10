# Native visionOS and surrounding-world rendering audit

Audited October 10, 2026, against upstream baseline `562ee94975132c46acec2eb6922381c518c7c84f`. This document covers the game runtime and missing integration work. Concurrent additions to the fork may prepare AOT compilation or a tracking calibration app; those preparations do not establish a working GTA V runtime on Vision Pro.

## Required outcome and current result

The requested outcome is GTA V's world surrounding the player, with head tracking and distinct views for each eye. A large window, fullscreen window, mono texture stretched around the player, or cinema in an immersive space does not satisfy that outcome. There is no verified native engine, game-camera control, or headset-pose-to-game path in the audited code.

The user's ordinary retail PC/console copy is not a direct input. Muguet executes an already compiled GTA V WebAssembly port, using matching game data and WGSL shaders. Its [README](../README.md), [ZIP importer](../src/import.rs), and build scripts require `data/manifest.json`, `b/<build>/game.wasm`, and `b/<build>/shaders/index.json`. The repository supplies no retail executable/disc-to-WebAssembly converter, conversion procedure, game source compilation pipeline, or game assets. Renaming or ZIP-compressing a retail installation or console ISO cannot supply the missing `game.wasm`. See [GAME-FILES.md](GAME-FILES.md) for the package contract.

With no compatible package available, the game cannot be started or its internal function signatures inspected. A standalone native stereo tracking calibration target can validate Apple rendering and tracking, but its Play control must remain unavailable until the actual engine, package, and stereo camera path are implemented and tested.

## Native platform requirements

| Area | Evidence in the baseline | Required integration |
| --- | --- | --- |
| Rust target | Native Rust target is `aarch64-apple-visionos`; simulator is `aarch64-apple-visionos-sim`. | Use the XROS/XRSimulator SDK and `XROS_DEPLOYMENT_TARGET`, with matching native app packaging. |
| Swift/build packaging | [build.rs](../build.rs) returns unless `target_os` is `ios`, selects `iphoneos`, and invokes Swift for `arm64-apple-ios15.0`. [ios/project.yml](../ios/project.yml) targets iOS and replaces its executable with an iOS Rust binary. | Native visionOS target, platform metadata, signing, SDK linkage, and SwiftUI scene lifecycle. An iOS IPA remains an iPad compatibility app. |
| Windowing | [Cargo.toml](../Cargo.toml) pins `winit` 0.30.13. Its platform aliases include `ios` and `macos`, but no native `visionos` backend. | Bypass winit on visionOS with a native SwiftUI application and Rust core bridge, or independently qualify a newer UIKit backend for windowed use. A windowing upgrade does not provide immersive rendering. |
| GPU | `wgpu` 30.0.1 selects Metal for Apple vendor targets and models visionOS capabilities. [src/app.rs](../src/app.rs) currently creates its surface from a winit window. | Use the compositor's device, per-eye render targets, timing, and submission, with a verified Metal/wgpu interoperability design. |
| Audio | CPAL 0.18.2 selects CoreAudio for Apple targets and declares `ios`, `tvos`, and `visionos` AVAudioSession dependencies. | Qualify the actual audio session, output format, interruption handling, and spatial behavior on the device. These dependency guards are support indicators, not a runtime PASS. |
| Startup and data | [src/config.rs](../src/config.rs) uses mobile Documents, permissive missing-data startup, and launcher reload only for `ios`. [src/import.rs](../src/import.rs) exports launcher FFI only for `ios`. | Share the appropriate mobile behavior with visionOS while retaining explicit missing-code/data errors. Use native file import and lifecycle. |
| Controller/input | [src/pad.rs](../src/pad.rs), [src/app.rs](../src/app.rs), and [src/mp.rs](../src/mp.rs) contain iOS-only input assumptions. | Native controller handling and a separate predicted headset-pose bridge; touch UI and mouse locking must not become the head-tracking implementation. |

Rust's official [visionOS target documentation](https://doc.rust-lang.org/rustc/platform-support/apple-visionos.html) currently lists these targets as Tier 2 without host tools and distributes them through rustup. Native Rust support does not automatically qualify all dependent crates. Exact dependency evidence: [winit 0.30.13 platform aliases](https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/build.rs), [wgpu 30.0.1 Metal backend selection](https://raw.githubusercontent.com/gfx-rs/wgpu/v30.0.1/wgpu-hal/build.rs), [wgpu Metal capability handling](https://raw.githubusercontent.com/gfx-rs/wgpu/v30.0.1/wgpu-hal/src/metal/adapter.rs), and [CPAL 0.18.2 platform dependencies](https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/Cargo.toml).

## Signed AOT game code

The baseline's mobile solution in [src/main.rs](../src/main.rs) compiles the game on the Mac, inserts the result in the app executable's `__GAME,__code` section, links that segment read/execute, and loads it in place with `Module::deserialize_raw`. Its `SignedCode` custom code-memory implementation avoids dynamically publishing new executable mappings. The section, in-place loader, custom code memory, and smaller heap/guard configuration are guarded by `target_os = "ios"`.

A native visionOS build needs this signed AOT path generalized deliberately. Simply changing the Cargo target would select the non-iOS embedded `Module::deserialize` path and the desktop heap reservation in the baseline. Wasmtime's ordinary Unix publication [changes mapped memory to executable with `mprotect`](https://raw.githubusercontent.com/bytecodealliance/wasmtime/v49.0.2/crates/wasmtime/src/runtime/vm/sys/unix/mmap.rs); that is not the existing mobile signed-code technique.

The host compiler must use the native visionOS triple and the same Wasmtime version, CPU features, heap reservation, guard size, and relevant engine configuration as the runtime. A successful host AOT smoke check proves that target compilation is available. It does not prove that the signed segment can execute on a headset, that a package is compatible, or that the full game fits the device's memory limit. The existing iOS entitlements in [ios/Muguet.entitlements](../ios/Muguet.entitlements) must not be assumed to grant the same native visionOS behavior without provisioning and device verification.

## Existing camera and patch mechanisms

[src/patch.rs](../src/patch.rs) reads the WASM `name` custom section, maps demangled function names to indices, adds host imports, and exports selected discovered functions under `mp:<label>`. Its required hooks are the following exact existing strings:

| Existing symbol lookup | Injected import | Verified code purpose |
| --- | --- | --- |
| `GtaThread::Update(int)` | `muguet_script` | Passes local 0 and `__tls_base` to a host function with generated WASM type `(i64, i64) -> ()`. |
| `CExplosionManager::AddExplosion(CExplosionManager::CExplosionArgs&, CProjectile*, bool)` | `muguet_explosion` | Reports an explosion argument pointer to multiplayer. |
| `CTimeWarper::SetTargetTimeWarp(TW_Dir)` | `muguet_timewarp` | Lets multiplayer replace the time-warp argument. |

The host dispatch in [src/host.rs](../src/host.rs) calls `mp::script_tick` for `muguet_script`. That tick selects a script thread, preserves its active-thread TLS, allocates guest stack scratch storage, runs command exports, restores the guest stack, and restores TLS. It runs independently of compositor frame timing; it is not a qualified late-pose update point.

[src/mp.rs](../src/mp.rs) contains these exact camera lookup strings:

| Export label | Existing function name string | Capability |
| --- | --- | --- |
| `GetGameplayCamCoord` | `camera_commands::CommandGetGameplayCamCoord()` | Read current camera position. |
| `GetGameplayCamRot` | `camera_commands::CommandGetGameplayCamRot(int)` | Read camera Euler rotation with a rotation-order argument. |
| `ShakeGameplayCam` | `camera_commands::CommandShakeGameplayCam(char const*, float)` | Request camera shake. |

`Cmd::v` supplies a guest scratch pointer for returned vectors and reads components at offsets 0, 8, and 16. `call` reads the discovered function's actual Wasmtime type and coerces arguments. Those source conventions are not proof that any absent camera setter has the same WASM ABI. The exact getter function types, actual symbol presence, and vector layout must be checked in the supplied module. Currently optional command lookup failures only log and skip the command; required VR functions must instead fail readiness explicitly.

There is no camera setter, asymmetric projection, view matrix, stereo render, IPD, or headset pose hook in this command list or patcher. Once an authorized compatible `game.wasm` is available, inspect its function names and types for camera control and the renderer's view/projection updates. Names such as `SetCamCoord`, `SetCamRot`, `RenderScriptCams`, or camera-matrix updates are discovery labels only; no demangled signatures, function indices, memory addresses, or layouts for them have been verified here. Do not manufacture them from retail files or conventional GTA script API names.

The feasible extension points are additional exports for functions actually present in the name section, or a new injected import at a verified camera/render update point. Any import must validate the relevant locals/globals and function type before patching. Head pose should cross into the engine through a synchronized snapshot consumed on its own appropriate thread, with tracking validity, predicted time, recentering, axis/unit conversion, and player-space origin. Directly invoking an active script's `Caller` from the compositor thread is not a valid shortcut.

## Surrounding-world stereo requirements

The existing [src/gpu.rs](../src/gpu.rs) `SurfaceHandoff` owns a winit window. `Gpu::present` reads one game texture and draws a fullscreen triangle sampling `texture_2d<f32>`, then presents the window surface. Existing render passes use `multiview_mask: None`. There is no separate left/right world camera, headset transform, per-eye projection, compositor drawable, or compositor depth submission in that path.

Apple's [custom immersive Metal rendering documentation](https://developer.apple.com/documentation/CompositorServices/drawing-fully-immersive-content-using-metal) describes `ImmersiveSpace`, `CompositorLayer`, and `LayerRenderer` as the bridge between SwiftUI lifecycle and the custom render loop. Integration requires current drawable views, their eye transforms and projection, depth, predicted timing, and coordinated presentation. The game engine must render its scene from those views. Two copies of the same final flat game image do not create correct world stereo.

A script camera orientation setter, if eventually discovered, could support an initial head-rotation experiment. It still cannot establish per-eye asymmetric frusta, translation parallax, view-dependent visibility, correct screen-space effects, or compositor-depth consistency. Those need engine camera/render integration or a substantially more complex qualified draw-command interception path. No such game-specific path has been verified.

## Acceptance checkpoints

1. Obtain and validate an authorized compatible compiled package; the retail copy alone cannot pass this checkpoint with the supplied repository tools.
2. Run the actual package on the Mac, record its hash and discovered name/type inventory, and prove required camera/render functions exist before designing a game-specific bridge.
3. Prove native signed AOT execution with a small controlled module on a device, then qualify full game startup, memory, GPU, audio, and controller behavior.
4. Independently verify native compositor stereo and tracked transforms using calibration geometry. Keep game readiness false during this stage.
5. Connect predicted head pose, world origin, and both eye views to the actual game render path; verify orientation, translation, IPD, projection, depth, and pause/recenter behavior in gameplay.
6. Measure real-device frame pacing, motion response, memory and thermal behavior. A completed calibration app or host AOT compilation is not completion of surrounding-world GTA V.

This audit claims code inspection and primary-source verification only. It does not claim a running GTA V package, native full-engine build, headset installation, or stereo gameplay.

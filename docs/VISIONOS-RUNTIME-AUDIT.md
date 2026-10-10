# Native visionOS and surrounding-world rendering audit

Audited October 10, 2026, against upstream baseline `562ee94975132c46acec2eb6922381c518c7c84f`, then updated with read-only inspection of the actual `8b0b5899ed` web module. This document covers the game runtime and missing integration work. AOT compilation, verified module symbols, and a tracking calibration app do not establish a working GTA V runtime on Vision Pro.

## Required outcome and current result

The requested outcome is GTA V's world surrounding the player, with head tracking and distinct views for each eye. A large window, fullscreen window, mono texture stretched around the player, or cinema in an immersive space does not satisfy that outcome. There is no verified native engine, game-camera control, or headset-pose-to-game path in the audited code.

The user's ordinary retail PC/console copy is not a direct input. Muguet executes an already compiled GTA V WebAssembly port, using matching game data and WGSL shaders. Its [README](../README.md), [ZIP importer](../src/import.rs), and build scripts require `data/manifest.json`, `b/<build>/game.wasm`, and `b/<build>/shaders/index.json`. The repository supplies no retail executable/disc-to-WebAssembly converter, conversion procedure, game source compilation pipeline, or game assets. Renaming or ZIP-compressing a retail installation or console ISO cannot supply the missing `game.wasm`. See [GAME-FILES.md](GAME-FILES.md) for the package contract.

A candidate complete package has now been located, and its actual `game.wasm` has been retrieved and inventoried without executing game code. Its required Muguet patch symbols and camera/projection candidates are present, as recorded below. Package loading, game startup, and stereo camera behavior still need validation. A standalone native stereo tracking calibration target can validate Apple rendering and tracking, but its Play control must remain unavailable until the actual engine, package, and stereo camera path are implemented and tested.

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

`Cmd::v` supplies a guest scratch pointer for returned vectors and reads components at offsets 0, 8, and 16. `call` reads the discovered function's actual Wasmtime type and coerces arguments. The actual module confirms the getters' output-pointer WASM signatures and the three float offsets through the body inspection below. Currently optional command lookup failures only log and skip the command; required VR functions must instead fail readiness explicitly.

There is no camera setter, asymmetric projection, view matrix, stereo render, IPD, or headset pose hook in this command list or patcher. The actual module does contain internal camera setters and viewport projection functions. They are potential extension points rather than implemented host capabilities. Their signatures, object-pointer requirements, and exact spelling must come from this module, rather than retail script API names.

The feasible extension points are additional exports for functions actually present in the name section, or a new injected import at a verified camera/render update point. Any import must validate the relevant locals/globals and function type before patching. Head pose should cross into the engine through a synchronized snapshot consumed on its own appropriate thread, with tracking validity, predicted time, recentering, axis/unit conversion, and player-space origin. Directly invoking an active script's `Caller` from the compositor thread is not a valid shortcut.

## Actual module evidence: October 10, 2026

The [Full game v1 release](https://github.com/alihammoud21/playgta5/releases/tag/full-game-v1) publishes fourteen independent ZIP archives. Its [file inventory](https://github.com/alihammoud21/playgta5/releases/download/full-game-v1/file-inventory.json) places the module at `mirror/playgta5.com/b/8b0b5899ed/game.wasm` in `playgta5-part-01-of-14.zip`. The local module is `/Users/zach/gaming/game-assets/playgta5-full-game-v1/game.wasm`: 63,201,802 bytes, SHA-256 `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`, matching the publisher's inventory. Gameplay compatibility remains a separate check.

The fork's [inspect-game tool](../src/bin/inspect-game.rs) validates WebAssembly and reports 91,111 functions, 91,111 function names, and 2,513 camera candidates. Its JSON is `/Users/zach/gaming/game-assets/playgta5-full-game-v1/wasm-inventory.json`. The three required patch targets are present:

| Original function index | Actual name | Actual WASM type |
| --- | --- | --- |
| 63732 | `GtaThread::Update(int)` | `(i64, i32) -> i32` |
| 82028 | `CExplosionManager::AddExplosion(CExplosionManager::CExplosionArgs&, CProjectile*, bool)` | `(i64, i64, i32) -> i32` |
| 36231 | `CTimeWarper::SetTargetTimeWarp(TW_Dir)` | `(i64, i32) -> ()` |

Relevant internal functions have these actual signatures:

| Original function index | Actual name | Actual WASM type |
| --- | --- | --- |
| 49040 | `camera_commands::CommandGetGameplayCamCoord()` | `(i64) -> ()` |
| 49041 | `camera_commands::CommandGetGameplayCamRot(int)` | `(i64, i32) -> ()` |
| 48924 | `camera_commands::CommandCreateCam(char const*, bool)` | `(i64, i32) -> i32` |
| 48944 | `camera_commands::CommandSetCamCoord(int, rage::scrVector const&)` | `(i32, i64) -> ()` |
| 48945 | `camera_commands::CommandSetCamRotation(int, rage::scrVector const&, int)` | `(i32, i64, i32) -> ()` |
| 48931 | `camera_commands::CommandSetCamActive(int, bool)` | `(i32, i32) -> ()` |
| 48922 | `camera_commands::CommandRenderScriptCams(bool, bool, int, bool, bool, int)` | `(i32, i32, i32, i32, i32, i32) -> ()` |
| 48928 | `camera_commands::CommandDestroyCam(int, bool)` | `(i32, i32) -> ()` |
| 49046 | `camera_commands::CommandSetGameplayCamRelativeHeading(float)` | `(f32) -> ()` |
| 49048 | `camera_commands::CommandSetGameplayCamRelativePitch(float, float)` | `(f32, f32) -> ()` |
| 9636 | `rage::grcViewport::SetCameraMtx(rage::Mat34V const&)` | `(i64, i64) -> ()` |
| 9620 | `rage::grcViewport::SetProjection(rage::Mat44V const&)` | `(i64, i64) -> ()` |
| 9614 | `rage::grcViewport::SetCurrent(rage::grcViewport const*, bool)` | `(i64, i32) -> i64` |
| 9601 | `rage::grcViewport::RegenerateMatrices()` | `(i64) -> ()` |
| 9611 | `rage::grcViewport::RegenerateFrustumPlanes()` | `(i64) -> ()` |

`CommandSetCamRotation` is the exact internal setter name; `CommandSetCamRot` is not an interchangeable lookup. These indices refer to the unpatched module. Muguet inserts three function imports, shifting defined-function indices, so patches should resolve and validate names/types rather than reuse raw indices after reencoding. None of these camera functions is exported by the unmodified module. `SetProjection`, `SetCurrent`, and `RegenerateMatrices` were confirmed with an independent read-only parse of the full function-name and type sections. The inventory tool now includes all `grcViewport::` labels; its expanded output is `wasm-inventory-complete.json` beside the original report.

The actual module also passed the fork's `--precompile-visionos` path using Wasmtime 49.0.2 and the mobile memory configuration (5 GiB reservation, 64 KiB guard, ARM LSE enabled). The resulting `game.visionos.cwasm` is 211,065,752 bytes, SHA-256 `ae558582f4febd9aba54404e853cb554d300c96b5286207d968f905e5abdb75e`. The adjacent JSON records the source, compiler and artifact hashes. This establishes full-module host AOT compilation for `aarch64-apple-visionos`; signed device loading and game execution remain untested.

The next safe camera experiment is to add exports only for verified signatures, preserve Muguet's script-thread/TLS/guest-stack discipline, and first measure camera getters and a reversible orientation change while the game runs on the Mac. Script camera creation/destruction can follow after object ownership and vector layout are confirmed. Pose exchange must remain a synchronized host snapshot consumed on the game thread. The script hook executes at entry to `GtaThread::Update`; its selected-thread rank and update order must be measured before assuming that a pose applied there survives camera updates until rendering.

For world stereo, instrument `SetCurrent`, `SetCameraMtx`, and `SetProjection` to establish the active main-world viewport, matrix storage layout, update order, and how shadow/reflection/HUD passes differ. A viewport `this` pointer and matrix pointer must not be guessed or reused across passes. The existing GPU ring provides concrete observation points: opcode 23 binds constant buffers, opcodes 12/47 upload buffers, opcode 26 binds render/depth targets, opcode 30 sets viewports, opcode 32 draws, and opcode 7 presents. Shader metadata and constant-buffer observations can support a verified matrix interception design, but replacing one final texture or blindly modifying every projection would not produce correct per-eye rendering. Qualify a render-only second-eye path and matching depth without advancing simulation twice.

### Static camera and matrix bodies: October 10, 2026

A small parser-only helper linked to the already built `wasmparser` library decoded the seven functions below without instantiating the module or executing game code. Its source and binary are `target/verification/inspect-camera-bodies.rs` and `target/verification/inspect-camera-bodies`; the instruction dump, including absolute WASM byte offsets and resolved call names, is `target/verification/camera-bodies.txt`. These are local verification artifacts. All offsets below apply only to the verified module hash above and are relative to pointers actually passed or resolved by each function.

| Function | Direct instruction evidence |
| --- | --- |
| `CommandSetCamCoord`, 48944 | Reads three `f32` values from the supplied script vector at offsets **0, 8, 16**. Rejects a negative handle, resolves it with `fwBasePool::GetAt`, and requires `IsCameraScriptControllable`. Calls `StopInterpolating`, stores those values at resolved camera offsets **112, 116, 120**, and ORs bit 64 into a 16-bit field at **256**. |
| `CommandSetCamRotation`, 48945 | Reads `f32` values at script-vector offsets **0, 8, 16**, multiplies them by **0.01745329238474369** (the degree-to-radian factor), and passes the packed temporary vector plus the caller's order integer to `CScriptEulers::MatrixFromEulers`. After the same handle/control guards it calls `StopInterpolating`, then `camFrame::SetWorldMatrix` with the camera pointer plus **48**, and ORs bit 128 into its 16-bit field at **256**. |
| `CommandGetGameplayCamCoord`, 49040 | Loads an internal pointer and reads its packed `f32` values at **544, 548, 552**, writing the caller's output vector at **0, 8, 16**. This confirms the output spacing used by `Cmd::v`. |
| `CommandGetGameplayCamRot`, 49041 | Passes an internal pointer plus **496** and the caller's order to `CScriptEulers::MatrixToEulers`; reads the packed temporary at **0, 4, 8**, multiplies by **57.2957763671875** (radian-to-degree factor), and writes output at **0, 8, 16**. |
| `grcViewport::SetProjection`, 9620 | Reads four 16-byte SIMD vectors at input offsets **0, 16, 32, 48** and copies them to viewport offsets **320, 336, 352, 368**. Sets the byte at **916** to 1, then calls `RegenerateFrustumPlanes` followed by `RegenerateMatrices`. |
| `grcViewport::SetCameraMtx`, 9636 | Also reads four 16-byte SIMD vectors at input offsets **0, 16, 32, 48**, despite the `Mat34V` label: its input must provide **64 readable bytes**, rather than an assumed 48-byte packed matrix. Copies three basis vectors to viewport **192, 208, 224** with fourth lanes forced to 0, and the fourth vector to **240** with fourth lane forced to 1. Builds transposed basis vectors at **256, 272, 288** and a negative-translation combination at **304** with fourth lane 1; then calls the same two regeneration functions in that order. |
| `grcViewport::RegenerateMatrices`, 9601 | Reads the world block at **0–63**, the camera-related blocks above, and the projection at **320–383**. Writes derived four-vector blocks at **64–127**, **128–191**, **384–447**, and **512–575**, also using input blocks at **448–511** and **688–751**. A conditional branch compares the viewport pointer with an internal current pointer, makes indirect calls using TLS-derived state, and copies the first **256 bytes** of the viewport to a pointer read from temporary memory after an indirect call. The actual destination resource and indirect targets remain unqualified. |

The projection composition in `RegenerateMatrices` explicitly forms `v.x * P0 + v.y * P1 + v.z * P2 + v.w * P3` from its stored vectors. The camera body's basis transpose and negative-translation combination are consistent with a rigid-transform inverse; that interpretation requires an appropriate orthonormal input. These observations narrow the memory contract but do not establish Apple's matrix serialization, engine axis conventions, Euler-order integer meanings, a valid viewport lifetime, or which viewport controls the main-world pass. The setters' synchronous regeneration also means a later rendering hook must account for derived matrices and frustum updates, rather than changing only a copied matrix block. No direct memory write or debug command has been tried in a running game.

## Surrounding-world stereo requirements

The existing [src/gpu.rs](../src/gpu.rs) `SurfaceHandoff` owns a winit window. `Gpu::present` reads one game texture and draws a fullscreen triangle sampling `texture_2d<f32>`, then presents the window surface. Existing render passes use `multiview_mask: None`. There is no separate left/right world camera, headset transform, per-eye projection, compositor drawable, or compositor depth submission in that path.

Apple's [custom immersive Metal rendering documentation](https://developer.apple.com/documentation/CompositorServices/drawing-fully-immersive-content-using-metal) describes `ImmersiveSpace`, `CompositorLayer`, and `LayerRenderer` as the bridge between SwiftUI lifecycle and the custom render loop. Integration requires current drawable views, their eye transforms and projection, depth, predicted timing, and coordinated presentation. The game engine must render its scene from those views. Two copies of the same final flat game image do not create correct world stereo.

A verified script camera orientation setter could support an initial head-rotation experiment. It still cannot establish per-eye asymmetric frusta, translation parallax, view-dependent visibility, correct screen-space effects, or compositor-depth consistency. Those need engine camera/render integration or a substantially more complex qualified draw-command interception path. No such game-specific execution path has been verified.

## Acceptance checkpoints

1. Obtain and validate an authorized compatible compiled package; the retail copy alone cannot pass this checkpoint with the supplied repository tools.
2. Run the actual package on the Mac, record its hash and discovered name/type inventory, and prove required camera/render functions exist before designing a game-specific bridge.
3. Prove native signed AOT execution with a small controlled module on a device, then qualify full game startup, memory, GPU, audio, and controller behavior.
4. Independently verify native compositor stereo and tracked transforms using calibration geometry. Keep game readiness false during this stage.
5. Connect predicted head pose, world origin, and both eye views to the actual game render path; verify orientation, translation, IPD, projection, depth, and pause/recenter behavior in gameplay.
6. Measure real-device frame pacing, motion response, memory and thermal behavior. A completed calibration app or host AOT compilation is not completion of surrounding-world GTA V.

This audit records code inspection, source-package integrity checks, actual WASM validation and host AOT compilation. A running GTA V package, native full-engine build, headset installation and stereo gameplay remain unverified.

# Native immersive world bridge

The implemented ABI is [RuntimeBridge.h](RuntimeBridge.h) plus [WorldBridge.h](WorldBridge.h). The older `ImmersiveBridge.h` borrowed-command-buffer proposal is retained as historical design material and is not used. A passing calibration build is separate from qualified GTA world publication and compositor completion.

## Camera and frame ownership

Swift owns `ARKitSession`, `WorldTrackingProvider`, `LayerRenderer`, presentation timing, the compositor command queue and final presentation. It queries a tracked device anchor after `optimalInputTime` at the drawable's predicted presentation timestamp, assigns that same anchor to `drawable.deviceAnchor`, and publishes `SVXRHeadPose`. Explicit simulator fixtures leave the device anchor unset and are labeled synthetic.

`SVXRViewRequest` carries the built-in drawable's actual one or two `head_from_eye` transforms and reverse-Z projections. Recording/capture drawables cannot overwrite the headset mailbox with a different view count. The guest camera hook runs on the game thread and preserves the game's current primary camera, including third-person and cutscenes, with recentered headset movement relative to that camera. The player selects camera modes through ordinary game controls. With two requested views it alternates independent eye cameras on natural game updates (AER); it does not replay simulation for the second eye. This entails temporal differences between stereo eyes and requires headset qualification.

The native GPU path must match a qualified active gameplay camera and its baked GPU viewport before publishing any world snapshot. Source color and real scene depth must come from the same qualified game presentation. The native publisher copies both into immutable Metal resources and publishes only after their GPU copy completes. It bounds the pool and at most twelve outstanding leases. Missing or ambiguous world/depth qualification keeps acquisition pending; loading/menu images are not accepted as immersive world proof.

Swift acquires distinct view-index leases, verifies version/size, supported color format, dimensions, projection matrices and Metal device identity. A two-view compositor never receives duplicated mono content. The compositor command keeps the native leases and strong texture references alive until GPU completion. `sv_world_complete_frame` verifies a complete set of distinct eye tokens and current generation before counting an actual world presentation, then Swift releases the leases. Closing immersion and app lifecycle changes invalidate old acknowledgements.

## Coordinate and depth reconstruction

All matrices are column-major floats multiplied by column vectors. ARKit stage coordinates are right-handed meters: +X right, +Y up, -Z forward. `view.transform` is `head_from_eye`; it comes from CompositorServices, with no invented IPD. Each immutable snapshot contains its actual source `projection_from_eye`, `game_from_eye` and `game_from_stage`, including the native recenter and player locomotion basis.

Swift computes the current eye as:

```
game_from_current_eye = game_from_stage * predicted_stage_from_head * view.transform
```

The GPU reconstructs actual scene vertices from the R32Float source NDC depth:

```
source_eye = inverse(source_projection) * source_clip
game_point = source_game_from_eye * source_eye
current_clip = compositor_projection * inverse(game_from_current_eye) * game_point
```

Swift combines `inverse(game_from_current_eye) * source_game_from_eye` in Double before uploading a relative Float matrix, avoiding subtraction of large GTA world coordinates in the vertex shader. Source texture coordinates are interpolated projectively (`uv * source_clip_w`, `source_clip_w`, then divided in the fragment), which preserves the source image when the two camera transforms match even across changing vertex depths.

It rasterizes this depth mesh into the current full viewport rather than drawing a screen plane. Shared texture passes clear each texture/slice once and preserve the other eye viewport; layered passes use each assigned array slice. Foveation is disabled. Destination depth is reverse-Z, cleared to zero, compared greater-or-equal and stored for compositor reprojection. Source depth uses its actual projection convention, without assuming it is reverse-Z.

Depth edges reject triangles that would stretch foreground pixels across background surfaces. The verified simulator images show black silhouette outlines, disocclusion holes and uncovered regions; visual cleanup and headset qualification remain open. The compositor color format is negotiated from its supported formats. Already encoded GTA unorm final color is decoded before an sRGB destination; native sRGB source formats are re-encoded only for a plain unorm destination. This avoids applying the transfer function twice.

Near geometry uses its reconstructed current-eye distance: visibility is zero at 0.15 m, one at 0.45 m, and follows `smoothstep` between them. Stable 4×4 ordered coverage discards fragments while preserving the actual depth of surviving geometry. This is a camera-distance comfort fade, not tracked body collision. Hidden background is absent from the source depth image, so fading can expose empty pixels; it cannot reveal geometry that the game did not render.

## Acceptance evidence

On October 10, 2026, the **v5i simulator run passed the actual immersive world prototype**. The signed AOT game published qualified 1280×720 native color and real scene depth. Readback from the normal production compositor command recorded the actual source textures and its 3840×2160 output, with 7,781,704 of 8,294,400 output pixels nonblack, finite output depth and a successful native compositor acknowledgement. The simulator screenshot visibly showed the third-person game world surrounding the app panel under synthetic yaw, pitch, roll and translation. This supersedes the earlier v5e checkpoint where depth qualification stalled and actual compositor pixels were still unproven.

`target/verification/native-shell-world-v5i-acceptance.json` records source game frame 600, request frame 1711, head frame 1714, the nonidentity eye transform and real depth checks. `live-world-v5i-source.png`, `live-world-v5i-output.png` and `live-world-v5i-simulator.png` preserve source, production output and simulator appearance. The nearest sampled geometry in this capture was 2.78 m away, so its prominent black edge holes were outside the 15–45 cm comfort-fade range. Missing source pixels remain a reprojection limitation.

This proof uses **one simulator compositor view and explicitly synthetic poses**. It does not establish stereo, physical tracking or headset performance. The finite run later ended with OS termination following a simulator Metal-host failure; it does not establish sustained stability. Later low-graphics measurements below remained connected through their finite observations, but 60 FPS has not been achieved.

An offscreen Apple M4 run of the actual world shader also passed unchanged-camera identity with varying source depths (3,600 pixels byte-exact), near-fade coverage with no mismatches, and preserved surviving depth within 8×10⁻⁸. Its receipt is `target/verification/world-shader-check.json`. Swift game and diagnostic typechecks and the linked simulator build passed. The app's connected flag requires successful compositor completion plus fresh native publication, fresh compositor completion and the active lifecycle generation; historical counters remain separate evidence and do not keep a stalled renderer marked connected.

## Low graphics launch configuration

The launcher now has native graphics controls and persists the exact configuration to `<save_root>/native-graphics.json` before starting the Rust engine:

```json
{"width":960,"height":540,"fps_limit":60,"graphics_preset":"low"}
```

Only the Low preset is supported. Allowed game source resolutions are 640×360, 960×540 and 1280×720; frame caps are 30 or 60. The default is Low at 960×540 with a 60 FPS cap. Rust validates these values before engine startup, applies the launch dimensions/frame limit, and reports the accepted configuration, saved graphics XML action and backup path through `status.graphics`. Existing saves are preserved. Unsupported or malformed settings fail startup explicitly. Controls lock once startup is accepted; quit and reopen the app to change graphics, since the engine cannot restart in-process.

The exact automated low launch arguments are `--sv-probe-runtime --sv-start-runtime --sv-open-game --sv-resolution 960x540 --sv-fps-limit 60 --sv-graphics-preset low`. Game builds use Swift `-O` by default; `MUGUET_SWIFT_OPTIMIZATION=-Onone` enables Swift debugging. Simulator and device link receipts are `target/verification/native-shell-low-simulator-link.json` and `native-shell-low-device-link.json`, including all 15 native symbols and `__GAME` read/execute protection. The device result is an unsigned cross-build, not a physical-headset execution result.

Smooth 60 FPS is the first performance target. A configured cap does not demonstrate the achieved frame rate. The completed Mac mini simulator observations measured **18.45 game source FPS at 960×540** over 22 warm intervals in about six minutes, and **29.81 game source FPS at 640×360** over seven warm intervals in about three minutes. Their receipts are `target/verification/native-low540-v2-performance.json` and `native-low360-v1-performance.json`.

Both runs used the Low preset and 60 FPS cap, omitted full world debug readback and concurrent builds, and retained other host applications and simulators. The 540p CPU profiling interval was excluded. Both remained connected with fresh one-view immersive frames and no native/world error through their final samples; root intentionally terminated them for the next comparison. The earlier assertion did not recur during these finite observations, which do not prove a crash fix or sustained headset stability. These measured game source rates are separate from compositor completion/reprojection rates and remain below the 60 FPS target. Upload timing instrumentation is the next profiling checkpoint; it is not a rendering optimization by itself.

Primary references: [Apple's Metal immersive rendering guide](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), [predicted device pose](https://developer.apple.com/documentation/arkit/worldtrackingprovider/querydeviceanchor(attimestamp:)), [per-view projection](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/computeprojection(convention:viewindex:)), and [GTA V REAL mod's alternating-eye rendering](https://github.com/LukeRoss00/gta5-real-mod). API names and `view.transform` semantics were also checked against the installed Xcode 27 SDK interfaces and headers.

## Historical native Rust baseline — October 10, 2026

The findings below describe the earlier baseline before the current shared-library and Swift runtime-bridge implementation. Their failing dependency check remains useful evidence for why the refactor was necessary; it is not the current simulator compilation result.

The real `game.wasm` has now been retrieved and independently inventoried: 63,201,802 bytes, SHA256 `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`. The full release-part acquisition and assembled ZIP validation are separate work. The local inventory has 91,111 named functions, the three hooks required by Muguet's current patcher, and 21 exported functions. None of those exports is a native stereo/headset camera API. Internal `rage::grcViewport::SetCameraMtx` (function 9636, two i64 parameters) is a research lead, not a verified camera-control contract or memory layout.

The installed Rust 1.98.1 toolchain includes both `aarch64-apple-visionos` and `aarch64-apple-visionos-sim`. Cargo metadata currently exposes only binaries and a build script; it has no library target. A native static library is a practical next checkpoint, but this repository has not yet compiled one.

One baseline native check was run with locked dependencies, `--no-default-features`, `XROS_DEPLOYMENT_TARGET=26.0`, two build jobs and isolated `target/visionos-feasibility`. It exits 101 in `wayland-backend` 0.3.17: missing `poll_fd`, `ret`, `SendFlags::NOSIGNAL`, `RecvFlags::CMSG_CLOEXEC` and `dispatch_all_clients`. This confirms that winit's selected desktop dependency path cannot compile for native visionOS. It fails before checking the application's own Rust source; it is not a native-runtime or signed-AOT execution result. Full log: `target/verification/rust-visionos-check-online.log`; receipt: `target/verification/rust-visionos-feasibility-receipt.json`.

The smallest next refactor, without claiming game immersion, is:

1. Add a `staticlib`/`rlib` target for shared runtime modules and a small explicit C FFI probe. Keep the CLI launcher and `app.rs` in the desktop/iOS executable path. Exclude the **dependency** on winit for `target_os = "visionos"`, as well as its module uses. Winit 0.30.13's aliases recognize Apple platforms only as iOS/macOS; visionOS reports `unix` and consequently selects its X11/Wayland dependency path. Disabling its default features alone supplies no native visionOS backend.
2. Remove winit types from the visionOS compilation of `SurfaceHandoff`, `Gpu.surface` and the window-surface branch of `Gpu::present`. The existing `Gpu::new(..., None)` already creates a windowless Metal instance. This permits a core compilation checkpoint; its existing single-texture presentation is still insufficient for stereo immersion.
3. Deliberately extend only the signed-code and mobile-memory guards in `main.rs` to `any(target_os = "ios", target_os = "visionos")`. Preserve `__GAME,__code`, 16 KiB alignment, `Module::deserialize_raw`, `SignedCode`, memory64/shared threads, fixed memory reservation and 64 KiB guard. Match the exact Wasmtime version, patch version/hooks, target triple, `has_lse` setting and compile-time `MUGUET_IOS_HEAP_GB` between host AOT compiler and runtime. The current host compiler defaults to 5 GiB; changing that constant requires recompiling both sides. The final **Swift app link** must apply `-Wl,-segprot,__GAME,rx,rx`; build.rs's current `rustc-link-arg-bins` does not configure a Swift executable linking a static archive. Do not extend the iOS launcher/touch build blindly.
4. Give the native FFI explicit data/save paths, returned errors and lifecycle ownership. The existing desktop `osascript` alert, multiplayer `afplay` helper and host `std::process::exit` cannot be the native library's error/lifecycle policy. A first probe can validate the embedded AOT module and imports without starting game threads or claiming playable rendering. Simulator execution needs an AOT artifact compiled for the simulator triple, separate from the device artifact.

Metal interoperability APIs exist in the locked dependencies: wgpu 30 provides `Device::create_texture_from_hal` and `CommandEncoder::as_hal_mut`; its Metal HAL provides `texture_from_raw`, `device_from_raw`, raw-command-buffer access and shared-event queue synchronization. These are useful integration surfaces, **not a completed implementation** of the proposed borrowed-command-buffer ABI. Texture import requires the compositor and game to use the same underlying Metal device, exact texture descriptors and valid resource lifetimes. Existing wgpu queue submission owns its command buffers, so the host-only commit rule above needs a deliberate synchronization/ownership implementation before it can be used.

The recommended next acceptance result is a native archive and a signed-loader probe linked into the Swift checkpoint, with a matching simulator AOT smoke check. Actual game cameras, immutable frame replay, compositor color/depth import and physical-headset qualification follow separately. This feasibility pass changes documentation only; Rust, Cargo and Swift source remain unchanged.

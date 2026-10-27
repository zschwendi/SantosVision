# V native visionOS integration

The SwiftUI/CompositorServices shell has two build modes. `diagnostic` renders a native 3D calibration world with Metal using each compositor view's camera. `game` also links the Rust engine, its signed in-place AOT module, camera mailboxes, and an immersive renderer for qualified native game color/depth snapshots. Module loading, engine startup, qualified world publication, and actual compositor completion are separate acceptance checks.

## Build

Requires full Xcode with visionOS SDK and XcodeGen; deployment target is visionOS 26.0. From the repository root:

```sh
./make-visionos.sh diagnostic simulator
./make-visionos.sh diagnostic device
./make-visionos.sh game simulator
./make-visionos.sh game device
```

Game mode calls `scripts/build-vision-runtime.sh` to build the matching native Rust static library and target-specific AOT module. The default source is the verified module under the sibling `game-assets/playgta5-full-game-v1/` folder; override it with `MUGUET_GAME_WASM`. Override the complete native archive with `MUGUET_RUNTIME_ARCHIVE` to skip the helper. The archive must contain AOT code for the same platform; a device artifact cannot be used in the simulator. The final Swift link marks the `__GAME` segment executable with `-Wl,-segprot,__GAME,rx,rx`, and force-loads the archive so its embedded code and ABI exports remain available. The compiler condition `SV_GAME_RUNTIME` and C bridging header are enabled only in game mode.

Build products live under `target/visionos-xcode/DerivedData-<mode>-<destination>/`. Builds are unsigned and do not boot a simulator or install an app. Simulator game mode targets arm64, matching the Rust archive.

Game builds use Swift `-O` by default, even though the Xcode configuration is named Debug. Set `MUGUET_SWIFT_OPTIMIZATION=-Onone` for Swift debugging. Earlier simulator and device low-graphics builds passed their then-current 15-symbol final link and read/execute protection on `__GAME`; receipts are `target/verification/native-shell-low-simulator-link.json` and `native-shell-low-device-link.json`. The new draw-setup setter requires a fresh link check. The device result is a cross-build, with no physical-headset run established.

The launcher persists three pre-start Rendering optimizations choices. **Adaptive depth mesh and sky fan** defaults OFF while its absolute precision gate remains unresolved. **Frozen-depth handoff** defaults ON in the app and reuses the qualified pre-UI snapshot; the native non-app request remains OFF unless set before startup. **Reduce draw setup overhead (experimental)** defaults OFF and requests render preparation, uniform staging, and bind-group preparation together through the typed `sv_runtime_set_draw_setup_overhead` startup setter. The pending bind-group module must call `crate::draw_setup::bind_group_prepare_enabled()` when integrated; this checkout does not include that module. Controls lock after a native startup attempt; quit and reopen the app to change them. Explicit diagnostic `MUGUET_COMPOSITOR_ADAPTIVE_MESH=0/1` and `MUGUET_COMPOSITOR_ADAPTIVE_SKY_FAN=0/1` each override the saved adaptive choice. `MUGUET_TRANSFER_FROZEN_DEPTH=0/1` overrides the typed pre-start native request. Exact `MUGUET_RENDER_PREPARE_OPT=0/1`, `MUGUET_UNIFORM_STAGE_PACK_OPT=0/1`, and `MUGUET_BIND_GROUP_PREPARE_OPT=0/1` independently override the saved draw-setup choice; unrelated values fall back to that choice. The native non-app draw-setup request starts OFF. Native status reports the app request and all three resolved values; the launcher report records its requested and effective choices. These flags are experimental and the existing different-scene simulator intervals do not establish an FPS improvement. Source-anchor copy and the analytic probe remain diagnostic-only.

## Native engine startup

The **Check native game module** button loads the actual embedded module and displays the C ABI's JSON result. **Start native engine** validates game data and starts the native runtime, then displays its state and any reported error. A successful startup request alone does not demonstrate gameplay.

Install the extracted compatible package in the app's `Documents/game/` folder. It must contain `data/manifest.json` and `b/<build>/shaders/`. Saves default to `Documents/saves/`. The source ZIP is external to the app bundle and is not committed to the repository. A retail PC/console install or ISO cannot supply this fork's compiled web-port engine.

## Graphics settings and the 60 FPS target

The game launcher includes **Graphics** controls before engine startup. Low is the supported preset; resolution choices are **640×360**, **960×540**, **1280×720**, **1920×1080**, **2560×1440** and **3840×2160**, with **30** or **60 FPS** caps. Defaults are Low at 960×540 with a 60 FPS cap. **Restore low defaults** resets these values. Once startup is accepted, the controls lock: quit and reopen spatial-V before changing graphics. Closing and reopening immersion does not restart the engine.

Swift validates and atomically saves `native-graphics.json` in the selected save root before calling the native runtime. With the default save root, the file is `Documents/saves/native-graphics.json`:

```json
{"width":960,"height":540,"fps_limit":60,"graphics_preset":"low"}
```

Rust validates the same settings, applies the launch resolution/frame limit, and updates the game's saved graphics XML while preserving saves and backing up an existing `settings.xml` before replacement. Low reduces texture, shadow, reflection, grass, water, particle and post-processing quality, retains the supported shader quality floor, and disables optional effects such as antialiasing and SSAO. Malformed or unsupported settings produce an explicit startup error; choose a supported value or restore the defaults before starting. The app report separates requested settings from the native `status.graphics` payload, which records the accepted configuration and XML update result.

Smooth **60 FPS is the first performance target**, not a guarantee from the cap. The completed low-graphics simulator observations measured these weighted game source frame rates after warmup:

| Low preset source resolution | Observation | Warm intervals | Game source FPS |
| --- | --- | --- | --- |
| 960×540 | About 6 minutes | 22 | **18.45**; interval range 16.2–20.7 |
| 640×360 | About 3 minutes | 7 | **29.81**; interval range 24.4–35.2 |

Receipts are `target/verification/native-low540-v2-performance.json` and `native-low360-v1-performance.json`. These one-view simulator tests used a 60 FPS cap, no concurrent builds and no full world debug export; other host applications and simulators remained running. The 540p CPU profiling interval was excluded. Both apps remained connected with fresh immersive frames through their final samples, then were intentionally terminated for the next comparison. These finite observations did not repeat the earlier assertion; they do not establish a crash fix, physical-headset performance or achieved 60 FPS. Game source frames are counted separately from compositor reprojection. Optional full source/output debug readback should be omitted from performance runs because its copying and file export add work.

Host rendering optimizations are compiled into V and automatically govern compatible imported game data. They do not modify the source ZIP, WASM or cached game AOT. The public staging-pool experiment remains opt-in: same-binary Low 640×360 observations measured 38.74 source FPS for queue uploads, 21.71 for the 1 MiB-page pool, and 31.28 for the queue repeat. Host load and scene motion varied, but the pool was slower in both comparisons despite lower upload-call CPU time. Evidence is `target/verification/native-upload-pool-queue-bracket-comparison.json`; rates use observer monotonic intervals, with whole-second report timestamps retained only as diagnostics.

Developer comparisons can set `MUGUET_BUFFER_UPLOAD_MODE=queue|pool` and `MUGUET_BUFFER_UPLOAD_PAGE_BYTES=65536|1048576`. Queue is the default. The pool caps retained pages at 16 MiB; one-shot transient sources are separate and can exceed that amount. `MUGUET_METAL_SKIP_EMPTY_TRANSIT=1` enables the separately tested conservative Metal optimization; missing/other values leave it off. Required initialization clears, implicit initialization/discard tracking, queries and presentation are preserved. Actual Metal branch tests and all 46 integrated tests pass (`native-upload-tuned-metal-branch-tests.json`). New GPU-thread counters separate upload manager work, waits, producer idle and publication; inclusive replay/present spans contain other segments and must not be added to them. World publication can also flush uploads, so its time overlaps manager submission/uniform work. Gameplay acceptance requires a frozen build and finite comparisons, beyond the focused command-buffer reduction tests.

The tuned same-binary Metal-toggle comparison measured **31.75 FPS off**, **34.47 FPS on**, then **37.70 FPS off** in the repeat. The initial gain did not survive the repeat, so this option remains off by default. Scene motion and host load varied; this is not a fixed-scene comparison. Evidence is `target/verification/native-upload-tuned-transit-bracket-comparison.json`. A separate 64 KiB public-pool attempt trapped in guest texture/resource placement allocation before any qualified immersive-world sample. Its continued menu presents are not world FPS. Cause and pooling causation remain unproven; see `native-tuned-pool64-trap-diagnosis.json`. All observed apps were intentionally terminated after their finite runs.

The internal allocation experiment is `MUGUET_METAL_INTERNAL_UPLOAD_POOL=1`, also off by default. It reuses exact-size, four-byte-aligned Metal upload allocations up to 64 KiB inside the original PendingWrites encoder. Retained allocations are limited to 16 MiB and 1,024 objects across all states; one-shot fallback allocations are separate. Reuse requires an actually completed, successfully tracked GPU submission and disposal of its old encoder, followed by publication outside core locks and a fresh map on checkout. It adds no polling, waiting or submission triggers. Failed writes cannot supply reusable sources. Texture transfers and other backends keep their existing allocation path.

For this source, all 46 app tests and eight focused Metal checks pass in both option modes with ordinary locks. Ten backend tests pass with ranked locks, including blocked GPU work, delayed publication, size/cap bounds, destruction, injected failures and reentrant loss cleanup. Full ranked public-submit testing remains blocked by the existing upstream rank TODO #5572; no cyclic rank edge was added to hide it. The ordinary-lock no-std Metal/WGSL check also passes. Current Metal HAL fence completion does not distinguish asynchronous command-buffer execution errors, so this is not a claim that every hidden GPU error is detected. Frozen source aggregate: `97ecc342a20a859beb08cccdb63de911d52828298d109da8f72b66913964fbc5`.

Both updated native apps build and independently pass archive/platform/ABI/embedded-AOT checks (`internal-upload-builds-independent-review.json`). The device app remains an unsigned cross-build. The simulator's fixed-input off/on/off sequence measured **17.17 / 37.52 / 32.38 source FPS** at Low 640×360 with cap60; compositor rates were **54.46 / 57.47 / 59.64** completions/s, separately. Each run used 60 seconds after the first qualified world sample for warmup, then a finite 120-second observation. Strict input, fresh-world, runtime-mode, memory-cap and error guards pass. The enabled backend window recorded 92,343 actual reuses in 90.065 seconds, with no allocation/remap/retirement/quarantine errors and retained peaks of 4,995,052 bytes/1,024 objects. The object cap forced fallback for many other writes. These counters use their own backend window, not the source-FPS denominator.

The on result exceeds the baseline repeat by 15.9%, but the baseline varied greatly and scenes, upload calls and pass/submission workloads differed. A causal FPS gain is not accepted and default reuse remains off. Upload API wall was 14.99/7.09/11.48 ms per source frame; these are overlapping CPU spans, not GPU execution time. All three actual-world screenshots were inspected after timing; severe existing depth-edge/disocclusion holes remain. Root intentionally ended each run. Evidence is `native-internal-upload-v1-comparison.json` and `native-internal-upload-bracket-comparison.json` under `target/verification`. This does not establish smooth60, hardware stereo/tracking, or a fix for the historical simulator-host assertion.

The draw-preparation experiment is opt-in with `MUGUET_RENDER_PREPARE_OPT=1` (default off). It uses program UID/stage/group keys for binding layouts, borrows immutable binding descriptors, stores a fixed 37-word render-pass key and reads scalar pass metadata without cloning its format list. Uniform packing, resource effects, submits and waits keep their existing order. Five focused fixtures, including actual Metal color/depth/stencil and packed-uniform readbacks, and all 52 regression tests pass with the flag off and on. Current simulator and unsigned device apps compile and preserve the verified AOT bytes; source/test/build receipts use the `native-render-prepare` and `native-shell-render-prepare` prefixes in `target/verification`.

The same-binary preparation off/on/off sequence measured **35.21 / 32.64 / 42.90 game-source FPS** at Low 640×360 with a 60 FPS cap. All three finite windows pass configuration, liveness, fresh-world and timing guards, and the enabled branch's site counters are exercised. Actual scene-start positions and draws/passes/submits per frame differ materially; these are controlled configuration observations, not identical-scene replay or an accepted causal speed comparison. The enabled run is below both baselines, so default preparation remains off. See `native-render-prepare-v1-comparison.json`, `native-render-prepare-bracket-comparison-v1.json` and `render-prepare-bracket-independent-review-v1.md` under `target/verification`.

A subsequent preparation-on **3840×2160** run verified actual native game-source dimensions throughout its warm measurement window and averaged **18.71 game-source FPS** over 120 monotonic seconds; compositor completions averaged **59.56/s** separately. A repeated **1920×1080** run verified source dimensions and averaged **32.36 game-source FPS**, with **59.94/s** compositor completions. These are one-view simulator results, not physical-headset stereo or achieved 4K/60. The inspected images still have black uncovered regions, depth-edge holes and edge color separation. The first 1920×1080 launch stopped before a qualified world with a Wasm out-of-bounds trap in `CApp::UpdateState`; its continued loading/present counters are not gameplay FPS. Evidence is `native-render-prepare-resolution-sweep-v2.json` and `render-prepare-1080-failure-independent-review-v1.md`. Finite healthy runs do not establish a startup or simulator-host crash fix. The app was intentionally stopped after the observations and Low 960×540 cap60 was restored for the next startup.

`MUGUET_COMPOSITOR_GPU_TIMING=1` optionally records completed Metal compositor command-buffer intervals in `immersive_compositor_gpu_timing` in the runtime report. It measures depth reprojection, optional debug blits and encodePresent; guest/wgpu rendering and system display are excluded. Qualified acknowledged completions use separate windows of at least ten seconds with exact monotonic bounds and reset counters. Missing simulator timestamps remain unavailable with null durations. These GPU intervals do not measure game-source FPS. Native 4K source color plus real depth can retain about 379.69 MiB across six world slots before other game resources; a combined 4K source/output debug export exceeds the existing 96 MiB debug budget.

For an automated low-graphics launch, pass these arguments to the app:

```text
--sv-probe-runtime --sv-start-runtime --sv-open-game --sv-resolution 960x540 --sv-fps-limit 60 --sv-graphics-preset low
```

The launcher accepts these explicit automation arguments:

| Argument | Effect |
| --- | --- |
| `--sv-probe-runtime` | Load the embedded game module and record its actual result. |
| `--sv-start-runtime` | Start the engine after a successful probe. |
| `--sv-open-game` | Open the native immersive game space after accepted engine startup. |
| `--sv-game-root <path>` | Override the extracted `data/` + `b/` root. |
| `--sv-save-root <path>` | Override the writable saves directory. |
| `--sv-report <path>` | Override `Documents/native-runtime-report.json`. |
| `--sv-resolution <640x360\|960x540\|1280x720\|1920x1080\|2560x1440\|3840x2160>` | Choose the game source resolution before startup; default `960x540`. |
| `--sv-fps-limit <30\|60>` | Choose the game frame cap before startup; default `60`. |
| `--sv-graphics-preset low` | Apply the supported Low graphics preset; default `low`. |
| `--sv-capture <path.ppm>` | Queue readback of the next actual game GPU presentation after startup. |
| `--sv-capture-delay <seconds>` | Delay the first capture request after startup; default 0. |
| `--sv-capture-count <1...20>` | Queue a bounded sequence of captures; default 1. |
| `--sv-capture-interval <seconds>` | Time between capture requests; positive, default 10. |
| `--sv-world-debug-capture <prefix>` | Once per renderer: read actual leased color/depth and the production compositor output after game frame 600, save source/output PPMs and numeric/matrix JSON. Relative prefixes resolve in Documents; readback is bounded to 96 MiB. |
| `--sv-world-debug-min-game-frame <N>` | Override the live shader diagnostic's warmup threshold; default 600. |
| `--sv-open-tracking` | Open the calibration immersive space automatically. |
| `--sv-simulate-head-pose <yaw,pitch>` | Simulator only: supply smooth synthetic pose amplitudes in degrees. |
| `--sv-simulate-head-position <x,y,z>` | Optional simulator-only translation amplitudes in meters, each bounded to ±2 m. Requires the pose fixture. |
| `--sv-simulate-head-roll <degrees>` | Optional simulator-only roll amplitude, bounded to ±180°. Requires the pose fixture. |

The report preserves actual probe/start/status return codes and payloads and explicitly records whether the game immersive renderer is connected. Runtime, tracking-status and first compositor-frame logs use subsystem `com.zephyrstudios.santosvision`.

Multiple captures append numbered filenames such as `frame-001.ppm`; existing files are preserved. **Capture next game frame** requests another image without restarting the engine. The report records every requested path and native return code; the native GPU status identifies when a queued capture was actually written.

The app observes aggregate scene phase across its window and immersive space. Inactive/background phases cancel synthetic pose input, clear the tracked mailbox, and suspend game GPU/audio/input through `sv_runtime_set_active`; active phase resumes. Closing immersion also clears pose publication, with a gate that rejects late render-thread poses. The ordinary window can still show engine status or capture game frames after immersion closes.

## Headset pose and per-view calibration

**Test headset tracking** opens a floor grid and colored 3D markers around the initial headset position. The renderer queries predicted ARKit pose at each drawable's presentation timestamp after the compositor's optimal input time, assigns the same device anchor to the compositor, and derives each eye's view/projection from the supplied transforms. Each eye renders into its assigned texture, slice and viewport with reverse-Z depth. The first completed compositor frame logs its actual view count, eye offset, projection offset and GPU completion status.

Game builds also publish predicted head matrices to `sv_xr_submit_pose`. Swift only updates native mailboxes; the game script thread consumes snapshots and executes qualified guest camera functions. **Recenter native head tracking** establishes a new reference on the next tracked pose. See [RuntimeBridge.h](RuntimeBridge.h) for the implemented ABI and [IMMERSIVE-BRIDGE.md](IMMERSIVE-BRIDGE.md) for game world ownership and depth reconstruction.

## Native immersive game world

**Enter immersive GTA world** opens a separate compositor layer. It submits the actual built-in drawable's per-eye transforms and projections after optimal input time. Capture drawables cannot overwrite the headset request. The native camera path preserves the game's current primary camera, including third-person and cutscenes, and applies head orientation/translation relative to it. Choose camera modes using the game's standard controls. Two-eye requests use alternating natural game frames with independent eye cameras, without advancing simulation twice.

`sv_world_acquire_frame` returns bounded leases on immutable, GPU-complete native Metal color and R32Float scene-depth copies. Swift validates ABI, view count/index, supported color format, dimensions, matrices and Metal device identity. The GPU reconstructs a depth mesh from the source projection, transforms it through `game_from_eye` into the current compositor eye, and rasterizes the actual world with reverse-Z depth. It rejects depth discontinuities instead of stretching foreground geometry across the background. No screen plane or fixed scene depth is substituted. Reprojection can reveal missing pixels; this is not a promise of artifact-free headset rendering.

Geometry is fully visible at 45 cm or farther from the current camera, smoothly fades through stable ordered coverage, and disappears at 15 cm or nearer. The distance comes from reconstructed camera-space geometry; surviving fragments keep their actual depth. This camera-distance comfort effect does not track the player's body. Fading cannot recover background pixels hidden in the source image, so exposed regions may be empty.

Each lease remains live until the compositor command completes. `sv_world_complete_frame` verifies the complete set of distinct eye tokens before acknowledging a world presentation; Swift then releases the leases. The runtime report marks the immersive game renderer connected only after successful GPU completion and records the actual view count and source game/request/head frames. Missing qualified frames produce a black cleared view and an explicit waiting status. Opening/closing or backgrounding invalidates old frame acknowledgements. A simulator one-view completion does not establish physical tracking, headset performance, or stereo.

The simulator fixture starts at identity, then smoothly varies yaw/pitch. It feeds the native mailbox after engine startup and can supply the calibration scene's view transform when ARKit is unavailable. It leaves `drawable.deviceAnchor` unset and is explicitly labeled synthetic in logs/reports. Physical-device builds ignore the fixture. This exercises camera integration and compositor rendering without claiming real headset tracking or GTA stereo rendering.

The October 10 calibration run supplied one compositor view, one color texture and one depth texture at 3840×2160, with zero eye/projection offset. The later **v5i game run passed actual world presentation**: qualified 1280×720 GTA source color and real depth reached the production compositor shader, and its 3840×2160 output plus the simulator screenshot visibly showed the game world surrounding the app panel. The source/output readback recorded 7,781,704 nonblack output pixels, finite real depth and successful compositor completion. Evidence is in `target/verification/native-shell-world-v5i-acceptance.json` and `live-world-v5i-{source,output,simulator}.png`.

Black silhouette outlines, disocclusion holes and uncovered regions remain visible. That finite run later ended after a simulator Metal-host failure and OS termination, so it is not sustained-stability proof. Reports and UI label the pose fixture synthetic, and the simulator supplies one view. Physical head tracking, stereo/IPD and headset performance remain unverified. Later low-graphics runs stayed connected during the finite observations reported above; further profiling is needed to approach the 60 FPS game source target.

Apple references: [CompositorServices](https://developer.apple.com/documentation/compositorservices), [Metal immersive rendering](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), [predicted device anchor](https://developer.apple.com/documentation/arkit/worldtrackingprovider/querydeviceanchor(attimestamp:)), [per-eye projection](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/computeprojection(convention:viewindex:)).

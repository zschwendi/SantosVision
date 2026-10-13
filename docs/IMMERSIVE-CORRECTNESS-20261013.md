# Overnight physical immersive correctness

Working checkpoint: `los-santos-immersive-checkpoint-20261013` (`d4b78b8`). User reports immersive game visible. Preserve this tag and its signed build5 in `target/verification/render-hotpath-20261011/los-santos-calibration-device-v5`.

Next candidate: `los-santos-aim-capture-candidate-v6-20261013` (`7951aa8`). Signed build6-r2 is in `target/verification/render-hotpath-20261011/los-santos-aim-capture-device-v6-r2`. It is not installed or visually accepted: installation attempted with r1 failed before launch with CoreDevice4016, and the physical AVP subsequently listed unavailable. No simulator use, AOT regeneration, assets changes, banked resets or push.

The user's authorized recording is copied into `target/verification/immersive-recording-20261013/ScreenRecording_10-13-2026 03-06-35_1.MP4`. Its manifest includes SHA256. Duration82.93s, 1920×1080 at15 recordedFPS; that is not a game-frame-rate measurement. Source build is not established. Frames at5s intervals show stepped interior sky holes as well as peripheral gaps. Guard-band overscan alone cannot explain the interior holes.

## Implemented candidate

The original gameplay camera ray is sampled in the selected guest script context. Original typed getters and the exact original/patched function-table command hide only HUD reticle14. An exact original-AOT certificate, gameplay/pause gates and a replacement-pipeline readiness handshake are mandatory before hiding. The native snapshot ABI is104bytes, scene-epoch checked and no older than100ms. No camera angle/mode or bullet physics setter is called.

Swift projects one shared snapshot into each eye using the full physical projection and current game-to-eye transform. It uses a finite40m aim-direction point for binocular convergence, not a claimed raycast/impact point; spread, recoil, assisted aim and muzzle parallax remain game-owned. Depth writes are disabled. Old renderer teardown cannot disable a new renderer. Readiness is refreshed after accepted tracked view requests because tracking loss/recenter advances the native epoch. Camera acquire and stop-state check are atomic under the existing stop lock.

Raw source and pre-present output depths are now exported as tight little-endianFloat32 rows, registered to the existing PPM color images. Allocation remains bounded by96MiB. A separate opt-in trace records up to256 draw states, including shader hashes, RT/SRV bindings, blend/write masks, viewport/scissor and exact camera markers. Native status summary is capped at24KiB; full trace uses a new file and never overwrites an existing one. This is a draw-state trace, not a complete compute/copy dependency trace. It does not change rendering or choose a HUD boundary.

## Next hardware capture

Install the r2 app on physical UDID `00008112-001408160C78A01E` when available. Preserve game data/saves; never uninstall. Resolve its existing data-container Documents path from the own-app report. Launch with `--sv-start-runtime --sv-auto-immersive-after-window-frames --sv-world-debug-capture sky-v6 --sv-world-debug-min-game-frame 300`. Use environment `MUGUET_XR_COLOR_PHASE_TRACE=1` and a unique absolute `MUGUET_XR_COLOR_PHASE_TRACE_PATH` inside that app's Documents. The QA flag takes the normal window-to-world transition after300 actual window frames, preserving its CAMetalLayer and avoiding the cold-loading8s timeout.

Retrieve `Documents/native-runtime-report.json`, `sky-v6-report.json`, `sky-v6-source.ppm`, `sky-v6-source-depth.f32`, and supported `sky-v6-output.ppm`/`sky-v6-output-depth.f32`, plus the color trace. Capture prefix chooses a new suffix if necessary. Verify valid view submissions and qualified world publication/acquisition/GPU completion before claiming hardware success. Find whether sky holes are already in source color, appear in the production Metal output, or arise only in system composition.

Exact-clear sky currently warps as an infinite ray but outputs depth0 while the compositor has a finite1000m far range. This has no late-warp margin and is a concrete hypothesis, not proof of the sky-hole cause. Do not broad-classify near-clear values or clamp all depth without the paired capture. Apple documents valid perspective depth and finite range clipping. The old depth-clear hook does not prove that color is final post-FX or that following draws are HUD: weapon/viewmodel phases must be distinguished.

Full HUD isolation requires a certified final-target, post-tonemap boundary and correct premultiplied coverage. Do not infer HUD alpha from final-minus-world, redirect every unqualified draw, or put world depth behind HUD pixels. A lease-keyed optional layer accessor can extend host texture metadata without changing WorldFrame ABI1 or guest AOT.

Current stereo still alternates eyes on natural game frames. This produces temporal mismatch under motion and is not a coherent same-simulation pair. Overscan can help limited head-motion border exposure, but a1.2 factor sacrifices center resolution and gives only roughly3.5° minimum horizontal margin. No simultaneous stereo,4K60 or visual-quality acceptance claim is made.

## Verification

Five integrated native aim tests pass, including exact private original/patched WASM table mapping and signature; fixture test is explicitly ignored unless invoked with the required asset environment. Five color-trace host tests pass. Swift ray projection regression covers asymmetric eyes/IPD, large origins, invalid rays and behind-camera targets. Production ownership-class regression covers recenter readiness without camera reset and stale-owner refresh/teardown. Swift6 strict-concurrency xros typecheck, complete native release archive, Metal/app compilation, signing and original-AOT embedding checks pass. Logs and receipts are retained beside the recording and signed build.

Parent overnight work `w-c6e3be74eb8743d1ab27b2ebffa59406`, coordinator `a-b2c2efe4331b4eee97c95dfe03cc52eb`. Settings/cloud-save/input parent `w-0b4b141b67e5488292106768ba98af39` remains separately unfinished. Scheduled thread follow-ups run until8a.m. America/Denver,2026-10-13. Prefer meaningful offline source qualification while hardware is unavailable; do not repeatedly request wake/unlock or retry Device Hub's unsupported physical display.

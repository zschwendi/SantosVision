# Frozen game draw stereo geometry probe — 2026-10-11

Parent task `w-ab947ade438e427bb263135cabb1ac55`. The production capture is OFF by default in `src/gpu.rs`, with the matching request-retention hook in `src/xr_view.rs`. Set `MUGUET_GEOMETRY_STEREO_CAPTURE=1` only for a parent-serialized proof run from the repository root. It attempts one qualified `default::VS_TransformD` + `default::PS_DeferredTextured` indexed opaque world draw, reads at most 2 MiB of real GPU position VB/IB data plus 256 KiB of bound constant buffers, and exclusively creates `target/verification/reprojection-implementation-v1/frozen-game-draw.json`. An earlier capture is never overwritten. The capture includes command-correlated source camera/projection, shader IDs and exact source/translated WGSL, full draw/layout/signature metadata, target IDs, CB bytes/versions, and the real bound position VB and IB bytes. Its declared scope is `cpu_position_vertex_buffer_and_index_only`: other vertex attributes and the pixel shader are not replayed. It does not render a second eye or publish `CertifiedPairEpoch`.

The tracked CPU runner is `scripts/geometry_stereo_proof.py`. It requires the real capture and **the exact 288-byte little-endian `SVXRViewRequest` consumed by the captured camera update**. In opt-in mode, `src/xr_view.rs` retains at most 96 successful stereo camera consumptions under its existing State mutex, clears them on session invalidation, and looks up the request by the exact command `WorldView` (epoch, request frame, camera sequence, pose frame, presentation time, eye index and matrices). The GPU capture embeds the matching request record and rejects the draw if it is unavailable. It never substitutes the latest submitted request. A separate record may be passed with `--request-record` for independent review. Its JSON shape is:

```json
{
  "schema": 1,
  "source": "sv_xr_submit_views",
  "request_bytes_hex": "<576 lowercase hex characters>",
  "request_sha256": "<optional for embedded host receipt; required for separate record>",
  "provenance_epoch": "<integer matching captured world_view.provenance_epoch>",
  "head_pose_frame": "<integer matching captured world_view.head_pose_frame>"
}
```

The embedded 288 bytes are serialized directly from the validated `SVXRViewRequest` copy actually used at the successful camera update; the ABI has no padding between these fields. The source label and optional embedded digest are integrity checks on the bytes, while the host's bounded consumed-request lookup supplies the binding to the source command. An external request record needs a matching digest; a record without raw bytes is rejected. The script parses ABI version, size, frame, time, view count, and two eye transforms/projections from those bytes. It matches the captured source eye's frame, epoch, pose frame, projection and camera; derives the other game-eye camera from the same source head transform and raw `head_from_eye` values; checks rigid transforms and 1 mm–25 cm separation. It does not accept two arbitrary camera arrays with matching counters.

For the active exact packaged shader pair `6a6a0898a51cf403` (`VS_TransformD`) and `75cf2d53bf9497c9`, the script checks source SHA256 values `fc914d159a51b8bd8e6d35633dd385a6cef50fb1ed740d5ad3d86893a03acb61` and `1e3d02aa02c1219e721c351c2fbcebc45154b2f9b1a8f2bdc8f1f6a96ff0daed`, then hashes the captured translated WGSL and checks its position formula. The older `VS_Transform` hash `2e9584811e870546` remains a synthetic regression variant; its CB0 clip-plane output must be identically disabled. Active `VS_TransformD` has no user clip-distance output. The script decodes actual VB/IB positions through the recorded DXGI layout/signature and extracts the source local-to-world and local-to-clip matrices from bound CB1. The source clip matrix must agree with the command-view camera/projection. Every indexed vertex must lie strictly inside source and both eye clip volumes, so side, eye, near and far clipping cannot be skipped. The two eye clip transforms are compared against independent direct projections. CPU scratch coverage and reverse-Z depth are compared per eye at the validated captured full viewport, with minimum four covered pixels, a valid-triangle gate and at least two pixels of eye disparity. The viewport must be finite, start at (0,0), span depth 0–1, have positive integer dimensions at most 1024 each, and contain at most one million pixels; oversized captures fail closed.

Use a fresh output directory that does not yet exist:

```sh
cd /Users/zach/gaming/SantosVision
python3 scripts/geometry_stereo_proof.py \
  --capture target/verification/reprojection-implementation-v1/host-capture-runs/root-real-draw-6/frozen-game-draw-captured.json \
  --shader-zip /Users/zach/gaming/game-assets/playgta5-full-game-v1/playgta5-part-01-of-14.zip \
  --output target/verification/reprojection-implementation-v1/geometry-proof-real-run-003
```

A passing real run with an embedded consumed request reports `single-draw-cpu-geometry-verified` and writes two `eye-*.ppm` scratch targets and `report.json` in that new directory. It proves only this draw's CPU geometry/depth projection math. It does **not** execute the game pixel shader, reproduce game color/GBuffer/deferred/transparent/post passes, prove stereo-safe portal/occlusion culling, or establish a frozen full scene. It must never mint `CertifiedPairEpoch` or unblock paired frame publication.

CPU plumbing regression, with no app/GPU execution:

```sh
python3 -m py_compile scripts/geometry_stereo_proof.py scripts/geometry_stereo_synthetic_test.py
python3 scripts/geometry_stereo_synthetic_test.py
```

The test uses synthetic vertices and marks its result `synthetic-plumbing-pass`. It tests both shader variants, independent projection and disparity plus stale camera, duplicate eyes, shader drift, active clip plane, side/eye/far clipping and stale epoch, request digest, and missing receipt rejection. It also rejects fractional/nonfinite/offset/partial-depth/oversize viewports and protects existing output. A synthetic pass is not a real draw result.

Root qualification after the request-receipt change, with unchanged frozen Rust and proof scripts:

- Default-host-feature `cargo check --lib --tests --offline --locked -j 2` passed. Log SHA256 `5e317e38befa5d51d4a445790c80b6a35b78dfd41d58973ae0a151bb34d55ecf` in `target/verification/reprojection-implementation-v1/capture-rust-regression-v1/`.
- The serial consumed-request history unit test passed (1/1). It covers later request advancement, epoch/time/pose/sequence mismatch, capacity eviction, invalidation and raw ABI bytes. Log SHA256 `3c2a631c4693737ff2aff1a66eef807e667e54727ad692228da66e9ccadc89f9` in `capture-rust-regression-v2/`.
- The earlier canonical synthetic geometry test passed the then-current rejection gates and exclusive output protection. Log SHA256 `22e3b198f47ae1cbec68ec0d55b06652747b516a762df01ac791de193c362cef` in `capture-rust-regression-v2/`. The current synthetic test additionally passed active-D shader and strict viewport rejection gates after the real-capture changes.

Two unsupported no-JIT macOS compiler/test invocations failed on the existing `load_module` configuration requirement; they remain recorded and are not passes. The successful checks used the default host JIT feature. Those earlier qualification runs did not execute the game. The later offscreen Mac run did capture a real game draw, as documented below. No full-scene stereo producer is certified.

## Optional real-draw Mac driver

`src/bin/geometry-capture.rs` starts the existing native game runtime offscreen and submits an explicitly synthetic, static 64 mm two-eye request through the public XR ABI. Game geometry, shaders, buffers, and the consumed request receipt still come from the real game. This is a single-draw experiment, not two rendered game streams. It requires repository-root working directory, `MUGUET_GEOMETRY_STEREO_CAPTURE=1`, an absolute extracted game root, an absent capture file, and a fresh save root. It seeds Low 640×360 at 30 FPS, forces master gain zero and disables host audio, and never changes existing game assets or saves.

After building the host binary with the default JIT feature, invoke:

```sh
MUGUET_GEOMETRY_STEREO_CAPTURE=1 <geometry-capture-binary> \
  <absolute-game-root> <fresh-save-root> [deadline-seconds] [--cache-seed <absolute-file>]
```

The deadline defaults to 120 seconds; an explicit value up to 600 permits initial host compilation. Existing capture evidence is never overwritten. Exit zero requires complete JSON with `status: captured`; rejection and timeout return failure. Runtime status is logged every five seconds. The first root attempt timed out during compilation, before GPU startup. The second 600-second attempt compiled the game and reached 2,467 offscreen presents, 2,302 overridden main-camera updates, and over three million draws, but timed out without a qualifying capture. Immutable logs are in `target/verification/reprojection-implementation-v1/host-capture-runs/root-real-draw-{1,2}/`. These are debug Mac experiments, not Vision Pro performance measurements or coherent stereo results.

Opt-in diagnostics now publish cumulative surviving capture-gate counts, the first exact shader-pair state, and up to 32 real qualified world-depth shader pairs. They appear in native status as `geometry_capture_diagnostics`; no draw qualification is relaxed. The driver writes an exclusive `final-native-status.json` in its fresh save root on capture, rejection, or deadline expiry. Its source-bound Rust 1.98.1 build passed in `host-capture-runs/root-real-draw-3/build-result.json`.

The optional `--cache-seed <absolute-file>` copies one explicitly selected regular Mac JIT cache into the fresh run, with exclusive destination creation. It trusts a locally verified serialized artifact: root must check its digest, the source WASM digest/size/mtime, engine configuration, patch inputs, dependency lock and toolchain provenance before using it. Wasmtime still checks engine compatibility. The third through sixth runs used only this locally verified Mac cache; they never used the visionOS AOT artifact as a Mac cache. Omitting the flag preserves a fresh cache. Supplying two eye transforms does not certify a full-scene producer.

## Preserved real-draw result

The third run timed out after confirming the active Low-scene `VS_TransformD` + deferred-textured pair, absent from the original `VS_Transform` selector. The fourth run reached that pair but rejected the reverse-Z cloned view because the consumed-request lookup recognized only the primary forward-depth view. The fifth run matched the exact derived-view receipt but rejected an unrelated, unbound optional layout VB. Those rejection receipts remain immutable under `host-capture-runs/root-real-draw-{4,5}/`. The capture now validates the derived reverse-Z view against the exact consumed primary request and reads only the real shader-location-0 position VB, never the renderer's dummy zero buffer.

Root's sixth offscreen run exited successfully after 13.4 seconds and preserved the real game capture at `target/verification/reprojection-implementation-v1/host-capture-runs/root-real-draw-6/frozen-game-draw-captured.json` (SHA256 `0f7d0b2a6e0ee251e3456cf8c91e9e7fdf9b051a62e8414a6cfbbe320d2289fc`). The draw used actual `VS_TransformD`/`PS_DeferredTextured`, 1,506 indices forming 502 triangles, a real 16,056-byte position VB and 3,012-byte 16-bit IB, bound CB bytes, and a 640×360 full-depth reverse-Z command viewport. Its embedded exact consumed request records both views from the synthetic 64 mm host input; the source camera and shader CB clip matrix agreed within `1.24e-6`. Run and build receipts are in the same `root-real-draw-6/` directory.

The initial proof used its older fixed 128×128 scratch and rejected the draw because only one source pixel was covered; `root-real-draw-6/initial-proof-result.json` preserves that failure. The corrected CPU proof uses the validated 640×360 source viewport without upsampling or lowering the four-pixel threshold. `target/verification/reprojection-implementation-v1/geometry-proof-real-run-002/report.json` (SHA256 `cda632f650cd853f5f3a9d2d144ad306ca4b2b6db16fc227e8e9c7d5016df7a0`) reports `single-draw-cpu-geometry-verified`: 502 valid triangles in each eye, 9 and 10 covered pixels, 11 differing coverage pixels, and independent per-eye clip/depth agreement within `3.56e-15` and `3.91e-18` respectively. The two `eye-*.ppm` files are CPU scratch masks. This verifies one draw's position/depth projection math with actual game geometry and an exact request receipt; it does not run the game fragment shader into two GPU targets, replay the remaining scene, prove culling or deferred/post behavior, or authorize a production certified stereo pair.

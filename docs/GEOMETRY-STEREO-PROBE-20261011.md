# Frozen game draw stereo geometry probe — 2026-10-11

Parent task `w-ab947ade438e427bb263135cabb1ac55`. The production capture is OFF by default in `src/gpu.rs` (current SHA256 `7e7f2d6350dd52cde68017aaa42ee5daa04af08e72cd67ea4b2608d96f564b00`) and request-retention hook `src/xr_view.rs` (current SHA256 `ed0201874f5a775758938f693dd2ce9707eb98e4af813e46294273900059bd4d`), pending parent integration. Set `MUGUET_GEOMETRY_STEREO_CAPTURE=1` only for a parent-serialized proof run from the repository root. It attempts one qualified `default::VS_Transform` + `default::PS_DeferredTextured` indexed opaque world draw, reads at most 2 MiB of real GPU VB/IB data plus 256 KiB of bound constant buffers, and exclusively creates `target/verification/reprojection-implementation-v1/frozen-game-draw.json`. An earlier capture is never overwritten. The capture includes command-correlated source camera/projection, shader IDs and exact source/translated WGSL, draw/layout/signature, target IDs, CB bytes/versions, and real geometry bytes. It does not render a second eye or publish `CertifiedPairEpoch`.

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

For the exact packaged shaders `2e9584811e870546` and `75cf2d53bf9497c9`, the script checks source SHA256 values `696bab4312d89127bcbec89d20e5158ee45e1af6f5d726892e02867f62e6572d` and `1e3d02aa02c1219e721c351c2fbcebc45154b2f9b1a8f2bdc8f1f6a96ff0daed`, then hashes the captured translated WGSL. It decodes actual VB/IB positions through the recorded DXGI layout/signature and extracts the source local-to-world and local-to-clip matrices from bound CB1. The source clip matrix must agree with the command-view camera/projection. The shader emits four clip distances; this bounded route requires its CB0 plane to be identically zero. Every indexed vertex must lie strictly inside source, left, and right clip volumes, so side, eye, near and far clipping cannot be skipped. The two native-eye clip transforms are compared against independent direct projections. CPU scratch color coverage and reverse-Z depth are compared per eye, with a minimum visible-geometry and disparity gate.

Use a fresh output directory that does not yet exist:

```sh
cd /Users/zach/gaming/SantosVision
python3 scripts/geometry_stereo_proof.py \
  --capture target/verification/reprojection-implementation-v1/frozen-game-draw.json \
  --shader-zip /Users/zach/gaming/game-assets/playgta5-full-game-v1/playgta5-part-01-of-14.zip \
  --output target/verification/reprojection-implementation-v1/geometry-proof-run-001
```

A passing real run with an embedded consumed request reports `single-draw-cpu-geometry-verified` and writes two `eye-*.ppm` scratch targets and `report.json` in that new directory. It proves only this draw's CPU geometry/depth projection math. It does **not** execute the game pixel shader, reproduce game color/GBuffer/deferred/transparent/post passes, prove stereo-safe portal/occlusion culling, or establish a frozen full scene. It must never mint `CertifiedPairEpoch` or unblock paired frame publication.

CPU plumbing regression, with no app/GPU execution:

```sh
python3 -m py_compile scripts/geometry_stereo_proof.py scripts/geometry_stereo_synthetic_test.py
python3 scripts/geometry_stereo_synthetic_test.py
```

The test uses synthetic vertices and marks its result `synthetic-plumbing-pass`. It tests independent projection and disparity plus stale camera, duplicate eyes, shader drift, active clip plane, side/eye/far clipping and stale epoch, request digest, and missing receipt rejection, plus exclusive output protection. A synthetic pass is not a real draw result.

Root qualification after the request-receipt change, with unchanged frozen Rust and proof scripts:

- Default-host-feature `cargo check --lib --tests --offline --locked -j 2` passed. Log SHA256 `5e317e38befa5d51d4a445790c80b6a35b78dfd41d58973ae0a151bb34d55ecf` in `target/verification/reprojection-implementation-v1/capture-rust-regression-v1/`.
- The serial consumed-request history unit test passed (1/1). It covers later request advancement, epoch/time/pose/sequence mismatch, capacity eviction, invalidation and raw ABI bytes. Log SHA256 `3c2a631c4693737ff2aff1a66eef807e667e54727ad692228da66e9ccadc89f9` in `capture-rust-regression-v2/`.
- The canonical synthetic geometry test passed all ten rejection gates and exclusive output protection. Log SHA256 `22e3b198f47ae1cbec68ec0d55b06652747b516a762df01ac791de193c362cef` in `capture-rust-regression-v2/`.

Two unsupported no-JIT macOS compiler/test invocations failed on the existing `load_module` configuration requirement; they remain recorded and are not passes. The successful checks used the default host JIT feature. No game, GPU capture or headset run was performed, and no full-scene stereo producer is certified.

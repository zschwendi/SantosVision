# Native reprojection prototype qualification — 2026-10-11

This is the root integration record for parent work `w-ab947ade438e427bb263135cabb1ac55`. The implementation narrative in `REPROJECTION-IMPLEMENTATION-20261011.md` describes earlier frozen stages. All new production paths remain OFF by default. No physical headset, full-scene coherent stereo, or 4K60 result is claimed.

## Initial framework checkpoint

Implemented retained-pose/source-anchor copy gates, an independently logged analytic two-eye probe, conservative 8×8 adaptive tiles, and consumption of the guarded atomic pair ABI. The default renderer is the reference path. The pair bridge has no production full-scene producer; natural alternating eye frames cannot certify one scene epoch.

Root strict Swift 6 checks passed for simulator runtime, device runtime, and non-runtime builds; both Metal sources compiled for macOS, visionOS simulator, and device SDKs. Latest complete static receipt: `target/verification/reprojection-implementation-v1/native-final-static-v3/summary.json` (source unchanged). The root's subsequent tracked-only verdict repair rebuilt the standalone harness successfully. Actual Claude Desktop Fable 5.1 / Extra / Fast OFF implemented the native framework and substantive harness repairs; root integrates and verifies, with Sol reviewing resource lifetime and proof plumbing.

The reproducible tracked harness is `scripts/reprojection-harness.swift`, built by `scripts/build-reprojection-harness.sh`; the original Metal source comes from Git commit `e74b9d2` with SHA256 `62dc4b92fa41c7af2592b166b41175b12626a8b7e627400360c229c25f90c146`. `scripts/run-reprojection-gpu-batch.sh` requires a new run ID and skips timing after a parity failure. Earlier failed runs are preserved.

`runs/root-final-batch-v3/parity.json` contains 31 synthetic fixtures. The default/OFF renderer is bit-identical to the original throughout. Common regression guards and the declared comparison against baseline pass for candidates. Overall absolute qualification FAILS: the square-pixel 37×23 slope reaches 128.047852 µm depth error in the original, legacy and adaptive paths, exceeding the unchanged 100 µm budget. The original non-square border remains a separate fixture. The relative metric rule permits up to 10% above the original error and is labelled explicitly; it is not a claim of exact depth or no regression. Written invalid depth and unavailable adaptive tile-class readback fail closed.

A separate explicitly diagnostic timing run `runs/root-diagnostic-timing-v1/` retained 20 paired GPU samples per scene and every image check. It is not a successful qualification run: the 4K street copy differs by 5 depth ULP away from edges against the 4-ULP comparison gate, despite exact color and coverage. Raw median command times on this Mac M4, one synthetic image at a time:

| Source / scene | Original | Legacy OFF | Adaptive | Fullscreen copy |
|---|---:|---:|---:|---:|
| 3840×2160 street | 57.145 ms | 57.566 ms | 42.490 ms | 1.696 ms |
| 3840×2160 plane | 60.715 ms | 60.548 ms | 34.235 ms | 2.470 ms |
| 3840×2160 sky | 55.028 ms | 55.931 ms | 59.148 ms | 1.663 ms |
| 3840×2160 noise | 65.819 ms | 65.997 ms | 70.592 ms | 2.205 ms |

The street accepted 84,608 / 129,600 tiles; the plane accepted 128,104; sky and noise accepted none. Classification overhead hurts all-fallback scenes. Copy requires a matching lattice, projection, eye calibration and retained anchor; game copy additionally needs a certified pair for a two-view drawable. Its cheap command cost therefore cannot be extrapolated to this game's current stereo pipeline. None of these timings include guest rendering, display composition, headset reprojection or two streams.

## Outstanding work

The next reversible experiment is an indexed fan with the same 32 triangles and 33 unique vertex definitions instead of 96 repeated definitions. It must preserve exact corner values, triangle order, fallback strips, classification, leases and gates; it needs fresh qualification.

The real-draw experiment is separate. Mac startup and two-eye camera consumption work with sound muted. The initially selected shader is unused in the actual Low scene; a pinned `VS_TransformD` variant is active. The exact request link through the certified forward-to-reverse camera clone was added and its focused CPU regression passed. A bounded actual-position-buffer capture and CPU two-eye proof are still being qualified. They cannot publish `CertifiedPairEpoch` or substitute for stereo-safe culling, GBuffer, deferred shading and post passes.

Unsigned app builds and the synthetic simulator probe remain pending this checkpoint. Headset display-time adjustment needs physical-device evidence; simulator matrices have no DeviceAnchor.

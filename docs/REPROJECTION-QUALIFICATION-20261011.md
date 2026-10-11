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

## Indexed fan checkpoint

The accepted fan now evaluates 33 unique vertices and draws the exact original 32 triangles through 96 static UInt16 indices. Fallback strips, classifier, geometry and gates are unchanged. The first indexed static check rejected a complex Swift initializer; replacing it with an explicit append loop fixed compilation. `native-indexed-static-v2/summary.json` records all strict Swift, Metal and harness checks passing with unchanged source (output SHA256 `bf662ca262288323d3c96ba8885ab989de40cfa9b45762c188d5e02dece33b1b`).

`runs/root-indexed-batch-v1/root-validation.json` compares every color, depth and coverage image SHA256 with the unindexed `root-final-batch-v3`: all 31 fixtures are bit-identical. Default/OFF remains bit-identical to the original. The inherited square-border absolute failure remains, so overall qualification is still FAIL. The candidate relative guards pass. Parity JSON SHA256 is `78bdd10b4df9d0f797486558c93e85e27914d3b2cde3e4b6796ff766caeaf7c5`.

A separate diagnostic run `runs/root-indexed-diagnostic-timing-v1/` retains the same 20 samples and image gates. It again fails only the street-copy 5-versus-4-ULP comparison. Its one-image M4 medians are:

| 3840×2160 scene | Original | Indexed adaptive | Prior unindexed adaptive | Copy |
|---|---:|---:|---:|---:|
| Street | 57.929 ms | 35.645 ms | 42.490 ms | 1.983 ms |
| Plane | 60.795 ms | 23.277 ms | 34.235 ms | 1.456 ms |
| Sky | 55.065 ms | 57.286 ms | 59.148 ms | 1.597 ms |
| Noise | 64.937 ms | 71.772 ms | 70.592 ms | 2.423 ms |

Prior adaptive values came from the earlier separate diagnostic batch, not a simultaneous indexed/unindexed A/B measurement. Current original/indexed values are sampled together. Vertex reuse helps accepted planes but cannot meet a 16.67 ms two-stream budget on these commands; all-fallback classification remains overhead. These synthetic Mac results do not establish actual game or headset FPS. All production flags remain OFF.

## Real geometry checkpoint and remaining work

Muted offscreen Mac run6 captured a real `VS_TransformD` indexed game draw. The exact consumed request survives its certified forward-to-reverse camera clone, and the focused regression passed. `geometry-proof-real-run-002/report.json` passes at the captured 640×360 viewport using 502 triangles, 9/10 covered pixels and 11 distinct eye-coverage pixels. This is one draw's CPU position/depth math, without GPU fragment shading or full-scene rendering. Details and earlier failed attempts remain in `GEOMETRY-STEREO-PROBE-20261011.md`. Commit `406f2a7` preserves the capture/proof work separately from the indexed native change.

The proof cannot publish `CertifiedPairEpoch` or substitute for stereo-safe culling, GBuffer, deferred shading and post passes. Full default-host native suite: 104 passed, zero failed/ignored/filtered, including Metal fixtures. Frozen-source receipt `runs/final-native-tests-2/native-test-success.json` has SHA256 `27c4ae21357c77626c1f91b5b6d47c199372498d3717d3cf9cfbb127d273ed27`. The first test invocation ran all 104 successfully but the runner failed afterward while concatenating a null error value; its raw evidence remains. The helper was repaired and a fresh source/tool freeze and success receipt were generated; runtime source did not change.

Simulator and device release libraries compiled, followed by unsigned Swift `-O` apps. The original build stopped at XcodeGen because its requested output parent was absent; a fresh recovery run reused the exact successful source-bound archives and created that directory. `runs/final-unsigned-apps-recovery-1/build-manifest.json` SHA256 `f76d58860f58628e376f79b513d261022ecfb8e2668eaca0f64a8ba81dede27f` records both successful apps, required bridge symbols, the correct ARM64 platform and exact read/execute game AOT bytes. The apps are unsigned and have not run on a physical headset.

`runs/root-indexed-probe-smoke-v3/result.json` passes the bounded analytic simulator smoke: 23.6 seconds after activation, observed mesh/copy/mesh switches, 3840×2160 source, two retained texture sets totaling 199,065,600 bytes (under the 384 MiB cap), zero GPU/render/allocation failures and zero native pair acquisition or game acknowledgement. The simulator supplied one synthetic view. Unified OSLog SHA256 `3734755fe1ccd571579319e3132a6b2d2ccf2c2ba7458d5d2dacc3bde40596e3` preserves completed probe commands; timing windows mix mesh/copy and guest concurrency and are not a clean path benchmark. The owned app was terminated and exact prior graphics/XML/audio settings restored, including master volume zero.

Earlier simulator preflights are preserved: v1 timed out in the SDK shell launcher (its version-check sort, not the responding CoreSimulator service); v2 stopped when Simulator rotated the data-container UUID during reinstall. Game data and setting bytes migrated intact; root verified the unchanged WASM SHA and restored settings at the new path. v3 used Apple's underlying simctl, checked framework1171.7, validated migrated bytes and succeeded. Headset display-time adjustment needs physical-device evidence; simulator matrices have no DeviceAnchor. Actual-game adaptive smoke is the next separate check; full-scene stereo remains open.

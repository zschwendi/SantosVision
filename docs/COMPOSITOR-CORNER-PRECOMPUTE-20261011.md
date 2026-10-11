# Exact corner-depth precompute experiment

The immersive renderer draws a four-vertex footprint for every source texel. At 3840 × 2160 that is 8,294,400 instances, 33,177,600 nominal vertices and 16,588,800 nominal triangles per eye. The original vertex shader also fits the same shared corner depth separately for each owning texel. This experiment evaluates that fit once per geometric corner in a compute pass, then reuses its result during the existing draw.

Set `MUGUET_COMPOSITOR_CORNER_PRECOMPUTE=1` to request the experiment. It is **off by default**. The source resolution, geometry count, fragment shader, color conversion, clipping, background handling and near-camera fade remain the same. Invalid or discontinuous neighborhoods retain their original owner-specific depth.

Each eye's compute pass runs before its draw on the same command buffer. Scratch textures are private R32Float, bounded to six textures and 64 MiB per texture, and remain leased until command completion. Missing resources or unavailable scratch fall back to the single-pass pipeline. There is no reuse across source frames or texture-identity cache.

Claude Desktop Opus 5.5, Extra effort, implemented the patch; Sol independently reviewed and tested it. Root checked the source, resource lifetime and reviewed input/result hashes before this checkpoint.

## Validation

- Metal compiled offline for macOS, visionOS simulator and visionOS device.
- Swift 6 simulator typechecking with strict concurrency passed.
- On Apple M4, all 53 GPU fixtures produced bit-identical color/depth for the frozen original, refactored single-pass and precompute paths. Qualified corner values matched exactly.
- Fixtures cover depth discontinuities, invalid samples, clear/background depth, both depth conventions, edges, small sources, camera changes, clipping, fade and color transfer.
- All 36 consecutive commands over six distinct source/projection/size cases matched the reference, with no failed buffers, pool leaks or fallbacks. Pool bounds, reuse and resizing checks passed.

This validates the tested fixtures, not all possible inputs or the visionOS app runtime. The previously qualified app binaries are preserved. No game app or physical headset ran for this experiment.

## Paired synthetic GPU timing

Twenty samples per variant followed four warmups, alternating variant order. Source and output were both 3840 × 2160 in these cases. Values are median whole-command GPU milliseconds on Apple M4 using a standalone macOS Metal harness.

| Synthetic scene | Original | Precompute | Precompute draw alone |
| --- | ---: | ---: | ---: |
| Street | 51.335 | 50.035 | 47.670 |
| Plane | 52.657 | 51.202 | 48.766 |
| Sky | 46.124 | 45.980 | 43.793 |
| Noise | 56.926 | 54.055 | 52.551 |

The small improvement is provisional: timing distributions overlap and the harness did not retain raw per-pair durations. This is neither game FPS nor a headset/stereo benchmark. The remaining draw includes vertex processing, primitive setup, rasterization and fragment work; these measurements do not identify which stage dominates. The fit duplication is a modest part of this synthetic workload. The next investigation should attribute the remaining draw cost before changing geometry or adding a cross-frame cache.

## Preserved evidence

All paths below are relative to `/Users/zach/gaming/SantosVision/target/verification/compositor-performance-v1/`.

| Artifact | SHA-256 |
| --- | --- |
| `freeze.json` | `a99247e57d1124718b461bfcdc958bab2c87ab01bfe5f3e72e4f5f6333a8b339` |
| `review-sol.txt` | `203c2edf42365ddbf570c1dfeb4fdf7f414dea75aa47e6ff5843c7d5a9f6feb6` |
| `runs/sol-batch-1/parity.json` | `84f789cf8ad3dcf2a72eb7807583567f1ddb5125ac054cbb37d9ee595ab74fa6` |
| `runs/sol-batch-1/timing.json` | `6f64200259115589fd08976fb3cbfc58021346321c81b62a395404028029abf1` |

The directory also retains original/candidate shader snapshots, the standalone Swift harness, pool extraction, SDK compile/typecheck logs and run input hashes. Its batch runner requires the parent `runs/` directory to exist; the independent tester created that artifact directory after the initial setup failure. No source change was needed for recovery.

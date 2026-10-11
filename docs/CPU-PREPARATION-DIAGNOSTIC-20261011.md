# Actual-game draw preparation diagnostic — October 11, 2026

Three sequential, source-bound simulator runs passed with the same unsigned app and graphics/pose settings. They exercised the existing render-preparation and direct uniform-packing paths independently. The settings were Low 960×540 with a 60 FPS cap and master volume zero, one synthetic head-tracked simulator view output to 3840×2160, adaptive mesh/sky fan enabled, and the default frozen-depth handoff enabled. Each run retained more than 30 seconds after game presentation, a nonblank frame-1500 capture, finite output depth, zero reported validation errors, and exact restoration of the three owned settings files.

| Flags: render preparation / direct packing | New guest presents/s | Compositor completions/s | Loaded draws/frame |
| --- | ---: | ---: | ---: |
| 0 / 0 | 36.95 | 58.77 | 916–1,034 |
| 1 / 0 | 33.21 | 59.17 | 1,358–1,606 |
| 0 / 1 | 26.99 | 59.69 | 1,486–1,636 |

The game scene/save advanced between runs. Draw, pass and submission counts differed substantially; the uniform run used about 5.1 submissions/frame versus 3.1 in the others. These observations establish neither a causal FPS improvement nor a slowdown. Compositor completions include repeated/reprojected sources and are not distinct game frames, binocular stereo, native 4K rendering or physical-headset performance.

The cumulative source-site counters verify that the selected optimizations executed. Render preparation recorded 5,075,593 numeric layout-key lookups, 5,080,034 descriptor-vector clone sites avoided, 2,550,095 `PassInfo` and target-format-vector clone sites avoided, and 2,552,668 stack pass keys. Direct packing recorded 4,255,844 temporary-allocation sites and 5,643,826,192 logical second-copy bytes avoided. These are executed source sites and logical bytes, not measured allocator savings or device GPU time. Inclusive replay CPU-thread wall still occupied roughly 7.06–9.56 seconds in the steady ten-second windows. Upload API-call wall is nested CPU/driver time and cannot be added to replay time as a separate GPU cost.

Root inspected all three output images: recognizable game geometry remains, with soft source upscaling, silhouette gaps and uncovered borders. The scenes differ, so the screenshots are not a parity comparison. A fixed scene and command workload is needed for a causal performance comparison.

Evidence is retained under `target/verification/reprojection-implementation-v1/frame-transfer/`:

- `perf-comparisons/perf-comparison-v1.json`, SHA256 `d10efab5dff974d9473ebd4a76e319cd8ea835b8bc89e140eb2a907f91ace3d3`.
- `perf-comparisons/qualification-v1.md`, SHA256 `697799ea5ab578ccc0f25234655c213dc3ff8a9da48d5d56195e423867d07050`, with raw-log counter and interval audit.
- `perf-comparisons/root-image-inspection-v1.json`, SHA256 `4dadac8f442789365a06f5b4b0159a7a2f036b6b59b929f5629a3ca674f7dd12`.
- `runs/perf-baseline-v1`, `runs/perf-render-prepare-v1` and `runs/perf-uniform-pack-v1`, including launch arguments, source/tool/app bindings, every native report, raw logs and capture readbacks.

All three bind app executable SHA256 `6370c54fca8627bcce39e27ecacf5a6c37992139a570a7a3293942bcc0e2c4e4`, Metal library SHA256 `4b0093a3110ba67d952c35301a29c10b4778eb4b5e78d0d493ff0ecc32e5807b`, and complete source freeze SHA256 `df56ccaafab72a8b18cb92bb1e6c76f1516194d170ee18140da26d19edfc278d`. The app qualification is described in [the main qualification record](REPROJECTION-QUALIFICATION-20261011.md).

# Exact-clear sky fan prototype

`MUGUET_COMPOSITOR_ADAPTIVE_SKY_FAN=1` enables coarse sky fans when adaptive reprojection is enabled. The default remains zero. An interior 8×8 tile qualifies only when its complete 10×10 halo contains exact clear depth and all 81 corner rays remain finite and in front of the current eye. Mixed geometry, invalid depth, clipped rays, border tiles, and partial tiles use the established fine fallback. Class 3 denotes sky; class 1 retains the plane classifier. Both share the existing indexed fan, color sampler, fragment shader, and sky rotation rule. Sky rays discard translation and bypass body fade.

The params ABI now has size 20, stride 24, alignment 8, and reserved offset 16. Swift and Metal declarations agree; the CPU selfcheck asserts this layout. Clear-depth atomics are gated by the opt-in bit. The exact-clear reject signature is 6 because the halo and legacy corner qualification each contribute a bit.

Root qualification on the Mac mini M4:

- Static validation: all 11 Swift/Metal/harness steps passed for macOS, visionOS simulator, and visionOS device; CPU selfcheck passed with 36 fixtures and no GPU use.
- GPU parity: all 36 candidate-relative checks passed. Full-sky class counts match their predeclared masks, mixed/invalid tiles retain fine fallback, and the four full-sky opt-out redraws are bit-identical to the original. The independent Double sky oracle found zero coverage, depth, or color violations, including moved and asymmetric projections.
- Overall absolute parity remains **failed** on `borders_37x23_slope_square`: original, legacy, and adaptive all have the inherited 128.047852 µm metric depth error against the unchanged 100 µm limit. This change adds no new failure and does not waive that limit.

A separate diagnostic batch retained 20 paired samples per scene. These are single-image compositor GPU medians on the Mac M4, excluding the game renderer and headset compositor:

| 3840×2160 source and output | Original | Adaptive with sky fans |
| --- | ---: | ---: |
| Street | 57.675 ms | 19.644 ms |
| Plane | 61.263 ms | 23.664 ms |
| Clear sky | 54.912 ms | 9.860 ms |
| Noisy depth | 65.039 ms | 69.580 ms |

The timing batch exited 1 on the existing street source-copy 5-ULP image difference against its 4-ULP gate. Timing does not override either correctness failure. These numbers do not establish native 4K60, two-eye performance, or physical Vision Pro performance. The noisy-depth case remains a fallback cost problem.

The isolated GPU evidence remains under `/Users/zach/gaming/SantosVision-sky-fan-20261011/target/verification/reprojection-implementation-v1/`: static `sky-fan-static-v2/result.json` SHA-256 `db6a33e291f73f6073e2e294797ab7efdd732b0f039404f61ac721663b5b6508`; parity `runs/root-sky-fan-parity-v1/parity.json` SHA-256 `aeba4377dd03177c8bd3355b5d5e204e1ee756587ae1cc2ba83eec0fd76d7872`; diagnostic timing `runs/root-sky-fan-diagnostic-timing-v1/timing.json` SHA-256 `21f978f93c3a3b608a3bf42343d7f23d533ca3063584991dd5df954e6c259d83`. Source hashes remained unchanged during qualification.

The main checkout has new unsigned simulator and device app build receipts and a passing actual-game simulator smoke. See `REPROJECTION-QUALIFICATION-20261011.md` for their source freezes and hashes. The smoke observed 2,048 completed compositor frames without GPU errors, with 54.44% combined planar/sky fan tiles, Low 960×540 game input and one synthetic view at 3840×2160 output. It does not isolate sky class counts, provide a controlled FPS comparison, or qualify physical stereo. Keep the feature opt-in pending headset validation.

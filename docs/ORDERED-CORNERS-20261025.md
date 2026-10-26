# Exact ordered corner sharing

`MUGUET_COMPOSITOR_ORDERED_CORNERS=1` opts into an additional finite-sky world path. Normal launch remains unchanged. It shares only qualified geometric corner vertices and keeps both original triangles for every source texel in the original row-major order. It changes no source camera, guard, lighting, color transfer, sampler, HUD or geometry coverage. The failed tile-fan approximation is not used.

The normal affine/bilinear path is qualified. Active smooth/vertical reconstruction retains its original path. HUD availability alone does not disable ordered sharing. Pipeline, resource or encoder failure falls back to complete original precompute/fine rendering. Captures label the actual first-eye draw, and resource status distinguishes configuration from physical acceptance.

Scratch allocation is bounded to six sets and 384 MiB in total, including driver padding. Each set contains `(width+1)×(height+1)` R32Float corners and six UInt32 indices per source texel. Checked arithmetic and the device buffer limit reject unsupported sizes. The exact command owns a unique checkout until completion; abandoned or partly encoded commands release it safely. There is no index cache across source frames.

Independent Mac Metal review passed 62 cases with bit-identical color, winning source-pixel ownership, final depth and UV derivatives. The cases include real registered 1080/1440/2304 sources and the equal-far-depth overlap that rejected the tile-fan experiment. Full finite-sky repair and gap fill are included. Warming and alternating six uninstrumented samples per path gave these one-eye Apple M4 medians:

| Identical retained source | Original | Ordered |
| --- | ---: | ---: |
| 1920×1080 | 11.807 ms | 8.704 ms |
| 2560×1440 | 16.997 ms | 10.547 ms |
| 2560×2304 | 27.557 ms | 16.273 ms |

These are separate scenes, each compared against itself. They are not game FPS, headset M2 timings, stereo throughput or perceived sharpness measurements. The new source resolution remains experimental. Do not enable this normally solely from the Mac measurements.

The actual integrated helper passes 68 budget checks and 44 Metal pool checks, including blocked in-flight work, exhaustion, exact indices, first/second encoder refusal and abandoned commands. Two padded tall sets use 332,824,576 bytes; the third is refused. Eighteen assembled finite-sky renders pass 90 checks for correct selection, complete byte-identical fallbacks and retirement. The actual physical-target app build passes. Signing, installation and physical quality/performance acceptance are separate.

Run `scripts/test-ordered-corners-pool.sh NEW_OUTPUT_DIR`. Existing finite-sky/fill validation scripts also link the helper. Private captured inputs and reports remain outside Git under `target/verification/device-session-20261025/third-person27/ordered-corners-experiment/` and `ordered-corners-pool/` in the canonical checkout.

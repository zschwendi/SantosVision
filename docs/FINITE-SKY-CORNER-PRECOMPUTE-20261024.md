# Finite sky corner precompute integration

The `MUGUET_COMPOSITOR_CORNER_PRECOMPUTE=1` diagnostic now shares the existing
qualified corner-depth kernel with the normal final-owner sky path. The kernel,
vertex arithmetic, fragment shader, ownership MRT, ordered depth-only sphere
repair and depth comparison are unchanged. If the optional pipeline, bounded
scratch resource or compute encoder is unavailable, the original vertex path
still draws the full source. Each corner texture is held until its command
buffer completes. The existing pool caps use at six textures, each at 64 MiB.
No source images or per-frame ownership are cached across submissions.

Normal play continues to use the original vertex path. Warmed alternating Mac
Metal measurements rejected making precompute the default on current evidence:

| Apple M4 registered source test | Original median | Precompute median |
| --- | ---: | ---: |
| Original compute-group sizing, six warmed samples per path | 12.27 ms | 13.34 ms |
| Bounded 256-thread group experiment, six warmed samples per path | 12.01 ms | 14.83 ms |

These timings cover one registered 2026-10-20 source on Mac Metal, including the
same final-owner readback blit. They do not establish headset performance or
causality for the user's black edges. The original group sizing is retained.
Do not enable this diagnostic for ordinary play based on reduced sample counts
alone; it must first win a relevant physical A/B.

`sh scripts/test-finite-sky-support.sh OUTPUT_DIRECTORY` compiles the production
uniform and bounded corner-pool declarations together with the actual helper
and Metal shaders. It verifies byte-identical source color and ownership and
bit-identical final depth between the original and shared-corner sky paths.
Cases include the registered game source, real black finite geometry, invalid
depth, all color transfer modes, sky overlapping 900 m geometry in both draw
orders and an exhausted six-texture corner pool. The latter must fall back
without changing output or releasing another submission's resources.

Evidence: `target/verification/device-session-20261024/shared-corner-sky19-warm/result.json`
and `shared-corner-sky19-group256/result.json`. The final-source run is in
`shared-corner-sky19-final/result.json`. Seven cases must pass. This work does
not change temporal stereo, source camera freshness or source guard-band policy.

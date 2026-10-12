# Metal skips in the experimental rendering choice

The existing saved **Reduce rendering overhead (experimental)** choice now requests both conservative Metal skips alongside the five existing CPU rendering optimizations. A fresh installation starts with that choice off. The existing typed pre-start native setter forwards the same boolean to a public `wgpu::wgc::rendering_overhead` control; no new native ABI, Cargo dependency, AOT input or process environment mutation is required.

The vendored core stores a default-false atomic app request. Each device captures the per-pass choice at creation; each queue captures its Transit choice at creation. Both remain Metal-only and retain their existing work predicates, tracker transitions, memory initialization, query handling, indirect validation, counters and fallback paths. A later request cannot change an already-created device or queue.

| Diagnostic environment | Exact `0` | Exact `1` | Unset or any other value |
| --- | --- | --- | --- |
| `MUGUET_METAL_SKIP_EMPTY_TRANSIT` | Disable Transit skip | Request Transit skip | Saved experimental choice |
| `MUGUET_METAL_SKIP_EMPTY_PREPASS` | Disable per-pass skip | Request per-pass skip | Saved experimental choice |

Overrides are independent. Native draw-setup status and Swift effective-setting reports include `metal_skip_empty_transit` and `metal_skip_empty_prepass`. Those configuration fields describe resolved requests; actual created Metal controls remain confirmed by the existing `metal transit` / `metal prepass` backend lines and counters. The launcher explains the effect in plain language and still requires a quit/reopen to change settings after startup.

Op47 upload dedup remains environment-only and default off. The default upload route remains queue; internal staging-pool routing and all other experiments keep their existing controls.

Pure fixtures cover exact override/default behavior, independence from the other flag and upload experiments, and native/core resolver parity. They do not mutate global control state or the process environment. The application-level parity fixture calls the actual public core resolver, so the root library suite can exercise it without running dependency tests separately.

Prepared from `f48c19a8b8ca0359fc92ae4adf8bf9da41f59e2a` in `/Users/zach/gaming/SantosVision-metal-launcher-controls-20261012`, branch `codex/metal-launcher-controls-20261012`. Source inspection and `git diff --check` completed. No tests, builds, GPU work, apps, installs, preferences or resets were run by this worker. Root owns fresh qualification and integration after its frozen trials. Parent work: `w-ab947ade438e427bb263135cabb1ac55`; worker scope: `w-ed0ed0798a594800a31534a08a5575db`.

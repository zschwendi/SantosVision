# Metal empty Pre Pass command-buffer skip, 2026-10-12

Status: **source and fixtures only. Not compiled, not run, not linked, not measured.** Worker `w-c56f51861a3a4290b1f6867a89f252db` under parent `w-ab947ade438e427bb263135cabb1ac55`, branch `codex/metal-prepass-skip-20261012` from `bd509bd`. Root owns builds, GPU tests and live qualification. The source proof with line references is in the review note of worker `w-3d6c5046a3d74e39b71461ddc2af817e`.

## What it changes

`MUGUET_METAL_SKIP_EMPTY_PREPASS=1`, read once when a device is created, turns it on. Anything else leaves every path as before. It is separate from `MUGUET_METAL_SKIP_EMPTY_TRANSIT`, which is about one command buffer per submitted command buffer; this one is about one per render or compute pass.

Vendored wgpu-core encodes each pass into its own HAL command buffer and then always creates a second one, labelled "(wgpu internal) Pre Pass", inserted ahead of it. That buffer carries the work that has to run just before the pass:

| Work | Emits anything only when |
| --- | --- |
| clears of surfaces discarded earlier in the same encoder | the pass reads such a surface |
| query resets (render) | the pass begins a query |
| the indirect-draw validation pass (render) | the device validates indirect draws and the pass batched one |
| resource barriers | never on Metal: its HAL transition methods are empty |

When none of it exists the buffer is empty, but it is still created, committed and freed. The profiled pooled trial had about 120–130 HAL command buffers per source frame for about 40 passes, and creating and freeing them was about 15% of the GPU thread's samples in the simulator.

## The predicate in this commit

A pass goes without a pre-pass command buffer only when all of these hold:

- the backend is Metal and the switch is on;
- no discarded-surface fixups are pending for the pass;
- render only: the pass's query reset map is empty;
- render only: the device has no indirect validation at all.

The last condition is the conservative one root asked for first. It does not look at whether the pass batched an indirect draw. Compute pre-passes never contain query resets or indirect validation, so only the first two conditions apply to them.

On the skip path the tracker is merged exactly as `insert_barriers_from_scope` (render) or `insert_barriers_from_tracker` (compute) does it, pending transitions are drained and resolved against their resources the same way, and nothing is handed to the HAL. No encoder is opened or swapped; the pass's own command buffer is already last in the list and the encoder stays closed. Every other case runs the unchanged block.

Not affected: buffer and texture initialization actions (recorded on the command buffer, executed at submit), deferred query resolve insertion points (indices into the list as it stood when recorded; nothing is ever inserted before them), query results, discard state.

**In the game this predicate keeps every render pre-pass.** `Gpu::new` creates its instance with default flags, and the default includes `VALIDATION_INDIRECT_CALL` in debug and release builds, so the device always validates indirect draws. With this commit alone only compute pre-passes are skipped there. The counters below show how many render pre-passes were kept for that reason with nothing to validate.

## Counters

`wgpu::Instance::generate_report()` now carries `metal_prepass`, summed over the instance's devices, and the queue prints the same cumulative numbers as `[wgpu] metal prepass: {...}` whenever it prints its transit line (first submit, then at most every ten seconds):

- `enabled`
- `render_skipped`, `compute_skipped`: passes that got no pre-pass buffer. One each is one HAL command buffer not created.
- `retained_disabled`: passes encoded with the switch off.
- `retained_discard_fixups`, `retained_query_resets`, `retained_indirect_validation`: passes that kept it, by the first reason that applied, in that order.
- `retained_indirect_validation_empty_batch`: of the last, passes that batched no indirect draw.

They are cumulative since device creation, are CPU-side decisions, and say nothing about GPU time. On a Metal device every encoded pass lands in exactly one of the skipped or retained counters.

## Fixtures

`src/metal_prepass_tests.rs`, five tests on a real Metal device through the public wgpu API. The switch is per device creation, so run the file in two processes, off and on. Each test requires the same stated bytes either way and then checks the counters against what the process's switch implies.

- Two render passes and two dependent compute passes with a copy after each, in one encoder: the bytes fix the order of pass and copy command buffers; all four pre-passes are skipped on a device without indirect validation.
- Discard: a pass stores `Discard`, the next loads the same surface. That pass keeps its pre-pass and the surface reads as zero.
- Query: a pass with an occlusion query keeps its pre-pass. A deferred resolve recorded between a filler copy and a later pass and copy lands exactly there, with a skipped pre-pass after it.
- Indirect: a default-flag device keeps both render pre-passes, one with a real validated `draw_indirect`, and the empty-batch counter tells them apart; the compute pass in the same encoder is skipped.
- Init: a never-written texture loaded by a pass and a never-written storage buffer read by a compute pass both read as zero with their pre-passes skipped.

Suggested commands for root:

```sh
cargo test --lib metal_prepass_tests -- --test-threads=1
MUGUET_METAL_SKIP_EMPTY_PREPASS=1 cargo test --lib metal_prepass_tests -- --test-threads=1
cargo test --lib -- --test-threads=1
MUGUET_METAL_SKIP_EMPTY_PREPASS=1 cargo test --lib -- --test-threads=1
```

## Limits

- Not compiled. No Rust toolchain was available to the worker; the code was desk-checked against the vendored and locked sources only. The fixtures use WGSL and wgpu 30 calls copied in shape from existing tests in this repository.
- The estimate above comes from one profiled simulator run. `MTLResourceListPoolCreateResourceList` dominates command-buffer creation there; a device may differ. No FPS or GPU-time claim.
- The skip relies on two facts of the locked `wgpu-hal 30.0.1` Metal backend: `transition_buffers` and `transition_textures` have empty bodies. A wgpu-hal update has to re-check them.
- `GlobalReport` gains a public field. Nothing else in the tree constructs that type.
- No Cargo, native ABI or AOT input changes. `src/lib.rs` gains the test module declaration only.

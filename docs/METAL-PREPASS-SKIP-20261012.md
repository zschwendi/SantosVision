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

## The predicate

A pass goes without a pre-pass command buffer only when all of these hold:

- the backend is Metal and the switch is on;
- no discarded-surface fixups are pending for the pass;
- render only: the pass's query reset map is empty;
- render only: the device has no indirect validation, or this pass batched no indirect draw.

Compute pre-passes never contain query resets or indirect validation, so only the first two conditions apply to them.

The indirect condition landed in two commits. The first (`f701c55`) required the device to have no indirect validation at all. The second allows a validating device when the pass's batch map is empty. That is the test `Draw::inject_validation_pass` makes itself, first thing, at `indirect_validation/draw.rs:258`: it takes the batch map and returns `Ok(())` before encoding anything or touching its resources. Validation stays enabled, and every pass with a real indirect draw keeps its pre-pass and its validation pass.

On the skip path the tracker is merged exactly as `insert_barriers_from_scope` (render) or `insert_barriers_from_tracker` (compute) does it, pending transitions are drained and resolved against their resources the same way, and nothing is handed to the HAL. No encoder is opened or swapped; the pass's own command buffer is already last in the list and the encoder stays closed. Every other case runs the unchanged block.

Not affected: buffer and texture initialization actions (recorded on the command buffer, executed at submit), deferred query resolve insertion points (indices into the list as it stood when recorded; nothing is ever inserted before them), query results, discard state.

The second commit is what makes this reach the game's render passes. `Gpu::new` creates its instance with default flags, and the default includes `VALIDATION_INDIRECT_CALL` in debug and release builds, so the game's device always validates indirect draws. With the first commit alone only compute pre-passes were skipped there.

## Counters

`wgpu::Instance::generate_report()` now carries `metal_prepass`, summed over the instance's devices, and the queue prints the same cumulative numbers as `[wgpu] metal prepass: {...}` whenever it prints its transit line (first submit, then at most every ten seconds):

- `enabled`
- `render_skipped`, `compute_skipped`: passes that got no pre-pass buffer. One each is one HAL command buffer not created.
- `retained_disabled`: passes encoded with the switch off.
- `retained_discard_fixups`, `retained_query_resets`, `retained_indirect_validation`: passes that kept it, by the first reason that applied, in that order. The last counts passes with a real indirect draw on a validating device.
- `render_skipped_empty_indirect_batch`: of `render_skipped`, passes on a device that validates indirect draws which batched none.

They are cumulative since device creation, are CPU-side decisions, and say nothing about GPU time. On a Metal device every encoded pass lands in exactly one of the skipped or retained counters.

## Fixtures

`src/metal_prepass_tests.rs`, five tests on a real Metal device through the public wgpu API. The switch is per device creation, so run the file in two processes, off and on. Each test requires the same stated bytes either way and then checks the counters against what the process's switch implies.

- Two render passes and two dependent compute passes with a copy after each, in one encoder: the bytes fix the order of pass and copy command buffers; all four pre-passes are skipped on a device without indirect validation.
- Discard: a pass stores `Discard`, the next loads the same surface. That pass keeps its pre-pass and the surface reads as zero.
- Query: a pass with an occlusion query keeps its pre-pass. A deferred resolve recorded between a filler copy and a later pass and copy lands exactly there, with a skipped pre-pass after it.
- Indirect: on a default-flag device, as in the game, a direct draw goes without a pre-pass while a real validated `draw_indirect` keeps it and renders the expected bytes; the compute pass in the same encoder is skipped.
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
- The skip relies on two facts of the locked `wgpu-hal 30.0.1` Metal backend: `transition_buffers` and `transition_textures` have empty bodies. A wgpu-hal update has to re-check them, and a wgpu-core update has to re-check the early return in `inject_validation_pass`.
- `GlobalReport` gains a public field. Nothing else in the tree constructs that type.
- No Cargo, native ABI or AOT input changes. `src/lib.rs` gains the test module declaration only.

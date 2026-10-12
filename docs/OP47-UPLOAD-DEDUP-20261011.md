# Redundant op47 upload guard, 2026-10-11

Status: **source and fixtures only. Not compiled, not run, not linked into any app, not measured.** Worker `w-9cd445e396df4b5db0daf84851f6d8a6` under parent `w-ab947ade438e427bb263135cabb1ac55`, branch `codex/op47-dedup-20261011` from `828bcc2`. Root owns builds, GPU tests and live qualification. Design source: final section of `target/verification/practical-frame-time-20261011/uniform-version-review-v1/review.md` in the main checkout.

## What it does

`MUGUET_OP47_UPLOAD_DEDUP=1` enables the guard; anything else leaves it off (an unrecognised value logs one warning). It does not follow the app's saved draw-setup choice. Off, `upload_buffer` and every other path behave as before; the only visible difference is one startup log line, `op47 upload dedup: {"enabled":false,...}`.

Ordinary GPU vertex/index buffers have no CPU mirror, so the host cannot ask whether an upload changes anything. The guard keeps one bounded record per eligible buffer object: the exact offset and bytes of its last op47 (no-overwrite) upload. A later op47 on the same object lifetime with the same offset, the same length and the same bytes is redundant, and only its queue write is skipped. Nothing else is skipped: op47 never flushed, marked usage or touched a CPU shadow on these buffers, and still does not.

A buffer is eligible at creation when it has GPU storage, a VERTEX or INDEX bind, no SRV/UAV bind (so no STORAGE usage), no CONSTANT bind (so no CPU shadow) and no draw-indirect misc flag. The indirect exclusion is narrower than the review asked for; it is not known to be required.

A call is compared only when it is nonempty, 4-aligned in offset and size, ends inside the GPU buffer and fits one record. Empty, unaligned, padded-tail, out-of-range and larger calls take the legacy route byte for byte and drop that buffer's record first, because their write may land inside the recorded range.

## When a record stops being trusted

- **Every op12** drops the buffer's record before its flush and write, including identical and empty ones.
- **A changed op47** drops the old record before its write and stores the new bytes only after the ordinary route has run. The guest snapshot `Vec` is moved into the record, not copied again.
- **A GPU-side write** — `copy_region`/`copy_resource` destination, `clear_uav` buffer destination (both the encoder clear and the value write), `copy_counter` destination, or a writable storage binding in `stage_bind_group_inner` — drops the record and disqualifies the object for the rest of its lifetime, before that write is recorded.
- **Destroy and replacement** (`destroy`, `set_obj` over a live buffer) drop the record. A new object gets a new host identity, so a recycled guest id cannot reach an older record.

The GPU-write rule is sticky on purpose. Buffer uploads execute in a queue prelude ahead of every encoder command of their submission, so a copy recorded before a later op47 still lands after it. Dropping the record at the copy and re-recording at that op47 would call bytes authoritative that the copy then replaces; the next identical op47 would be skipped and the copy's bytes would stay. Readbacks and vertex/index/indirect consumers only read and need no rule. Skipping an exact repeat leaves earlier-recorded consumers reading the same bytes as before, and a changed op47 still precedes them through the prelude exactly as without the guard.

## Bounds

128 records, 4096 bytes per record, 512 KiB of record bytes in all, each enforced separately. Insertion evicts least-recently-used records until both the count and the byte cap admit the new one; eviction only forgets, so the evicted buffer's next op47 is an ordinary miss. Per-object state is one enum in `Buf` (untracked, lifetime identity, disqualified). Identities come from a counter that is never reissued: if it ever ran out, later buffers would stay untracked. The recency counter empties the table if it runs out. Neither can produce a stale hit.

The guard adds no waits, submits, polls or readbacks, and creates no GPU objects.

## Metrics

With the guard on, each ten-second report adds `op47 upload dedup: {...}` after `buffer upload pool:`. `stats_interval` holds `op47_calls`, `op12_calls`, `checked`, `hits`, `misses`, `bypasses`, `saved_calls`, `saved_bytes`, `check_ns`, `check_max_ns`, `store_ns`, `store_max_ns`, plus `miss_reasons`, `bypass_reasons`, `invalidations`, `disqualified` and `table` (stored, rejected, evictions, peaks, tracked buffers). `op47_calls = checked + bypasses` and `checked = hits + misses`, counted for op47 on GPU-backed buffers only. A hit skips exactly one queue write; `saved_bytes` are the bytes of those writes.

Skipped calls never reach `measured_write_buffer`, so the existing `buffer upload timing: source op47` line drops by exactly `saved_calls` and `saved_bytes`. Compare ON and OFF windows with that in mind. `check_ns` is CPU classification and byte comparison for every op47 call; `store_ns` is record replacement and eviction after a miss. Neither is GPU time, and neither contains the guest snapshot or the upload route.

## Fixtures

Sixteen pure CPU tests in `src/upload_dedup.rs`: flag parsing and the inert off state; the eligibility matrix; repeat, changed bytes, changed range, changed length and A→B→A; op12 invalidation; every uncompared shape; sticky disqualification per GPU-write kind; destroy/recreate/replacement identities; identity and recency counter exhaustion; count and aggregate caps with LRU order; default caps under churn; metrics and interval reset. Four of them use a small model of the host's ordering (queue prelude, then encoder, per submission) and replay one operation stream with the guard off and on: a 48-seed randomized stream that must agree on every buffer byte, and three fixed streams, two of which also show that the model catches a missing hook and a non-sticky hook.

Eight Metal tests in `src/upload_dedup_tests.rs`, in the style of `guest_write_tests.rs`. Each drives the production `create_buffer`, `upload_buffer`, `copy_region`, `clear_uav`, `set_uav_counter`, `copy_counter`, `destroy` and `flush` on a real device twice, guard off then on, reads the buffers back, and requires identical bytes, the stated bytes, exact guard counters, and that the op47 route saw exactly `saved_calls`/`saved_bytes` fewer calls and bytes. They cover repeat/change, op12 interleave, padded tail, destroy/recreate/replacement, copy/clear/counter destinations, a copy recorded before an op47 of the same submission, consumers recorded before a repeat or a change, and the aggregate cap.

Suggested commands for root (the upload route follows `MUGUET_BUFFER_UPLOAD_MODE`, so the Metal set is worth one run per route):

```sh
cargo test --lib upload_dedup::tests -- --test-threads=1
cargo test --lib gpu::upload_dedup_tests -- --test-threads=1
MUGUET_BUFFER_UPLOAD_MODE=pool cargo test --lib gpu::upload_dedup_tests -- --test-threads=1
cargo test --lib -- --test-threads=1
MUGUET_OP47_UPLOAD_DEDUP=1 cargo test --lib -- --test-threads=1
```

The Metal fixtures set the guard explicitly on each device they create, so they pass or fail the same way whatever the process environment says.

## Limits and open points

- Nothing here has been compiled. No Rust toolchain was used; the code was desk-checked only. Expect to fix ordinary compile errors before the first test run.
- Hit rate in the game is unknown and may be low. No FPS, upload-time or allocation claim is made.
- `Buf` gains a `dedup` field, so the two `Buf` literals in `uniform_pack.rs` tests gain one line each. The uniform-snapshot-cache worker edits the same struct and literals; that merge is mechanical but will conflict.
- The writable-storage hook has no Metal fixture. An eligible buffer has no STORAGE usage, so it cannot be bound writable on a real device; the hook is defence in depth and is covered at guard level only.
- The model tests check the guard's rules against a model of the ordering, not against wgpu. The Metal fixtures are the evidence for the real ordering.
- One record per buffer: a buffer written at two alternating ranges never hits. Writes larger than 4 KiB are never compared.
- Two new source files and three edited ones change the native input set; build manifests and source-bound receipts must be regenerated. Shader ABI, the packed uniform path, queries, flush behaviour and AOT inputs are untouched.

# Redundant op47 upload guard, 2026-10-11

Status of this revision (multi-range): **source and fixtures only. Not compiled, not run, not linked, not measured.** Worker `w-280cd7770e7146a7bed7e61248c6222a` under parent `w-ab947ade438e427bb263135cabb1ac55`, on branch `codex/op47-dedup-20261011` after `974ba28`. Root owns builds, GPU tests and live qualification.

The first revision (`974ba28`, worker `w-9cd445e396df4b5db0daf84851f6d8a6`) kept one record per buffer. Root integrated and qualified it: 151 native and Metal tests passed with the guard off and 151 with it on, and the fixed airport run passed. That run had **0 hits in 205,550 compared writes** over 70.095 s (230,041 op47 calls). A typical 10 s interval showed 25,421 range misses and 2,940 no-record misses on 8 buffers, plus 3,360 over-4-KiB bypasses. One last range per buffer cannot catch ranges that alternate, which is what this revision adds.

## What it does

`MUGUET_OP47_UPLOAD_DEDUP=1` enables the guard; anything else leaves it off (an unrecognised value logs one warning). It does not follow the app's saved draw-setup choice. Off, every path behaves as before, nothing is reserved, and the only visible difference is one startup log line, `op47 upload dedup: {"enabled":false,...}`.

Ordinary GPU vertex/index buffers have no CPU mirror, so the host cannot ask whether an upload changes anything. The guard keeps a bounded table of exact records: the offset and bytes of an op47 (no-overwrite) upload to an eligible buffer object. A buffer may hold several records as long as their half-open ranges `[offset, offset + len)` are pairwise disjoint; ranges that only touch are disjoint. A later op47 on the same object lifetime with the same offset, the same length and the same bytes is redundant, and only its queue write is skipped. Bytes are compared in full. Nothing is hashed. Equal bytes at another offset, or a sub-range of a longer record, are not a repeat.

Eligibility is unchanged: GPU storage, a VERTEX or INDEX bind, no SRV/UAV bind, no CONSTANT bind, no draw-indirect misc flag. A call is compared only when it is nonempty, 4-aligned in offset and size, ends inside the GPU buffer and fits one record; such a call is one queue write of exactly its named range.

`gpu.rs` is not touched by this revision. The guard's entry points keep their signatures, so the hooks from `974ba28` (and root's integrated `upload_buffer`) apply as they are. Two comments there still say "record" in the singular.

## When records stop being trusted

- **A compared op47 that is not an exact repeat** first drops every record of that buffer whose range intersects its own, whole, and keeps the disjoint ones. Its own bytes are stored only after the ordinary route has run. If the table cannot admit it, the intersecting records stay dropped.
- **Every op12** drops all of the buffer's records before its flush and write, wherever it lands and even when it is empty or identical.
- **A valid oversized op47** (nonempty, 4-aligned offset/length, checked in-bounds end, too large for a record) takes the legacy route and drops only intersecting records before writing. Disjoint records stay trusted. Its exact bytes are never compared or stored, even when its length exceeds the entire table byte budget.
- **Every other uncompared op47 shape** (empty, unaligned, padded tail or out of bounds, including oversized requests failing those checks) takes the legacy route and drops all of the buffer's records first. A padded tail reaches past the range the guest named, and those writes remain conservative full-buffer purges.
- **A GPU-side write** — `copy_region`/`copy_resource` destination, `clear_uav` buffer destination, `copy_counter` destination, writable storage binding — drops all of the buffer's records and disqualifies the object for the rest of its lifetime, before that write is recorded. This is never range-local.
- **Destroy and replacement** drop all of the object's records. A new object gets a new host identity, so a recycled guest id cannot reach an older record.

The GPU-write rule is sticky on purpose. Buffer uploads execute in a queue prelude ahead of every encoder command of their submission, so a copy recorded before a later op47 still lands after it. Re-recording that op47 would call bytes authoritative that the copy then replaces. Copy sources, readbacks and vertex/index/indirect consumers only read and need no rule.

## Bounds

128 records, 4096 bytes per record and 512 KiB of record bytes across the whole table, as before, now counting individual ranges rather than buffers. One buffer may hold at most 64 of the 128. Insertion evicts least-recently-used records: within the buffer for its own cap, then across the table until both global caps admit the new record. Eviction only forgets, so the evicted range's next op47 is an ordinary miss.

The per-buffer cap is not from a measurement of range cardinality; there is none yet. Sixty-four keeps the policy close to the global LRU the table had, while guaranteeing that one busy buffer cannot push every other buffer out. `largest_buffer_records` and `buffer_records_peak` report how close a run comes to it.

The table is one vector reserved once when the guard is on and never longer than the record cap. An op47 makes one pass over it, and a second when it has records to drop; a stored miss makes a few more for the caps. No per-buffer map exists. A hit allocates nothing; a stored miss moves the existing guest snapshot into the record. Identities are never reissued, and the recency counter empties the table and its byte count if it ever runs out. The guard adds no waits, submits, polls or readbacks and creates no GPU objects.

## Metrics

`schema` is now 2. Everything root's extractor reads is unchanged in name and meaning: `enabled`, `interval_s`, `cap_records`, `cap_record_bytes`, `cap_total_bytes`, `records`, `record_bytes`, and in `stats_interval` `op47_calls`, `op12_calls`, `checked`, `hits`, `misses`, `bypasses`, `saved_calls`, `saved_bytes`, `check_ns`, `store_ns`.

New or changed:

- `miss_reasons.no_record` now means no record of the buffer intersects the range; `range` means records intersect it but none has that exact range; `bytes` is unchanged.
- `records_dropped` counts records by cause: `changed_op47`, `op12`, `unrecorded_op47` (with `unrecorded_op47_by_shape`), `gpu_write`, `destroyed_or_replaced`, `evicted`, `recency_reset`. `stored` minus their sum is the change in `records`. `invalidations` still counts calls that dropped at least one record.
- `stats_interval.oversized_range_bypass`: `calls` counts valid oversized bypasses, `invalidated_calls` counts those dropping at least one record, `dropped_records` counts their intersecting records, and `retained_disjoint_records` sums the surviving same-buffer record count after each such call (not unique records). Existing extractor field names and schema 2 are preserved.
- `table.evictions_buffer_cap` and `table.buffer_records_peak`; top-level `cap_buffer_records`, `record_buffers` and `largest_buffer_records`.

Skipped calls never reach `measured_write_buffer`, so the `buffer upload timing: source op47` line drops by exactly `saved_calls` and `saved_bytes`. `check_ns` now includes the bounded table scan; neither timer is GPU time.

## What the first run says about this one

Read from `runs/scene-all-v1/stderr.log` of the airport run, 18 nonzero intervals:

- `invalidations.op12` is 0 in every interval. The buffers that hold records never receive an op12.
- `invalidations.unrecorded_op47` equals `miss_reasons.no_record` in every interval. Every no-record miss followed an over-4-KiB op47 that dropped that buffer's record.

So on these buffers a full-buffer purge happens about once per nine compared writes, and it comes from oversized op47 calls, not op12. The dd5d27e revision retained that full-buffer purge, limiting records to the stretch between two oversized writes. This incremental refinement now retains disjoint records across a valid oversized write; their actual recurrence within the caps still requires root measurement. `oversized_range_bypass` reports the surviving records, while existing `records_dropped.unrecorded_op47_by_shape.over_record_cap` reports the ones actually intersected.

This refinement relies only on SHAPE_OVERSIZE: bypass_shape has already checked nonempty aligned offset/length and checked the end against GPU storage. The unchanged legacy upload route therefore writes the entire data slice once, with no tail. No gpu.rs hook, eligibility/cap/disqualification rule, or stored data extent changes. Independent root review and qualification remain pending.

## Fixtures

Twenty pure CPU tests in `src/upload_dedup.rs`: flag parsing and the inert off state; the eligibility matrix; exact repeat and changed bytes; alternating disjoint ranges with hits; ten overlap, sub-range, super-range and adjacency cases; op12 purge; twelve invalid/uncompared shapes (including invalid oversized requests), plus five valid oversized cases covering one/multiple crossings, adjacency and GPU-end writes larger than the total cache budget; sticky disqualification per GPU-write kind; destroy/recreate/replacement; identity and recency counter exhaustion; global, aggregate and per-buffer caps with LRU order and a write the table cannot admit; default caps under churn; metrics and interval reset. Five use a small model of the host's ordering (queue prelude, then encoder, per submission) and replay one stream with the guard off and on: a 48-seed randomized stream that must agree on every byte and keeps the table consistent after every step, and four fixed streams, two of which show the model catching a missing hook and a non-sticky hook.

Thirteen Metal tests in `src/upload_dedup_tests.rs`. Each drives the production entry points on a real device twice, guard off then on, reads the buffers back, and requires identical bytes, the stated bytes, exact guard counters, and that the op47 route saw exactly `saved_calls`/`saved_bytes` fewer calls and bytes. They cover exact repeats; alternating disjoint ranges; changed writes over, inside and beside known ranges; changes and repeats inside one submission; op12 anywhere in the buffer; padded tail; oversized writes crossing one/multiple or missing known ranges, exact GPU end and larger-than-table-budget writes, plus an oversized padded tail that still purges all; destroy/recreate/replacement; copy/clear/counter destinations; a copy recorded before op47 calls of its submission; consumers recorded before a repeat or a change; and the global and per-buffer caps.

Suggested commands for root (the upload route follows `MUGUET_BUFFER_UPLOAD_MODE`):

```sh
cargo test --lib upload_dedup::tests -- --test-threads=1
cargo test --lib gpu::upload_dedup_tests -- --test-threads=1
MUGUET_BUFFER_UPLOAD_MODE=pool cargo test --lib gpu::upload_dedup_tests -- --test-threads=1
cargo test --lib -- --test-threads=1
MUGUET_OP47_UPLOAD_DEDUP=1 cargo test --lib -- --test-threads=1
```

The Metal fixtures set the guard explicitly on each device they create, so they behave the same whatever the process environment says.

## Limits and open points

- This revision has not been compiled. No Rust toolchain was available to the worker; the code was desk-checked only.
- Whether ranges recur at all within the caps is unknown. No FPS, upload-time or hit-rate claim is made.
- The writable-storage hook has no Metal fixture: an eligible buffer has no STORAGE usage and cannot be bound writable on a real device. It is covered at guard level only.
- The model tests check the guard's rules against a model of the ordering, not against wgpu. The Metal fixtures are the evidence for the real ordering.
- A changed write drops whole records. A record partly overwritten is not split, and a sub-range of a record never hits.
- Writes larger than 4 KiB are never compared or stored. Only valid exact-extent oversized writes retain disjoint records; all other bypasses still purge the buffer.
- Shader ABI, the packed uniform path, queries, flush behaviour and AOT inputs are untouched. Two source files change; source-bound receipts must be regenerated.

## Incremental oversized refinement qualification

Only upload_dedup.rs, upload_dedup_tests.rs and this document changed. Static diff checks passed; the refining worker did not compile or execute any fixture. Added one CPU matrix and one Metal stream; adapted existing oversized/cap expectations. Metal off_then_on still requires complete byte parity and actual op47 route calls/bytes reduced by exactly saved_calls/saved_bytes. Root remains the sole compiler/GPU/runtime owner. No measured hit-rate, upload-time or FPS benefit is claimed.

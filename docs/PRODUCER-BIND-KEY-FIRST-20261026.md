# Default-off producer bind-group lookup prototype

Base: `d2e15a9347068b4fb24155b1072e577ff4ba725c`. This is a native producer
CPU optimization. It changes no source game, AOT, camera, image sampling,
controller, quality setting or default rendering path.

`MUGUET_BIND_GROUP_KEY_FIRST=1` explicitly requests the prototype. The existing
bind-group preparation feature must also be enabled. The variable is read once
when the native renderer is constructed; only the exact value `1` opts in.

The original resource walk clones native buffer/view/sampler handles before
checking the bind-group cache, even when the cache hits. The new path collects
copy-only plans and the same ordered key, performs the original resource effects
and fresh uniform snapshot, and resolves owned native handles only on a miss.
This is intentionally a qualified warm subset:

- Uniforms and existing real, comparison-compatible samplers.
- Already cached ordinary native SRVs. Both depth/float proxy branches and cold
  or missing views retain the original path, including proxy refresh work.
- Existing native storage buffers with zero or valid aligned view offsets.
  Counter bindings, storage textures, dummies and malformed offsets retain the
  original path and its diagnostics.

The keys, binding order and cache invalidation are unchanged. Texture pending
clears, `use_tex` / `use_buf`, `in_bg`, storage-write guards and `pack_copy` still
run in their original order before every lookup. Uniform plans retain the exact
ring slot and chunk that produced the dynamic offset. Existing object
replacement/destruction invalidation remains authoritative. The deferred-depth
clear chain can allocate separate proxy/foreground snapshot images, but does
not replace guest objects, invalidate ordinary SRVs or advance the uniform ring
slot during this exclusive call.

Fixed-size integer counters extend the existing bind-group status: eligible
lookups, hits, fallbacks, avoided owned resource items, and binding-item mixes
(uniform/sampler/texture/storage-buffer) across eligible lookups and hits. There
is no new per-call clock, string formatting, logging, allocation or I/O for these
counters. Eligible calls which fail before lookup are not counted as lookups.

For device sessions without console logs, the existing native
`draw_setup.bind_group_key_first` status contains a **periodic metadata snapshot**.
The renderer copies fixed POD counters under a short, separate metadata mutex
only at construction and its existing ten-second status tick. The status reader
copies that POD and releases the mutex before creating JSON or other callbacks;
it never reads the GPU object or acquires a GPU mutex. Publisher IDs retire old
renderer writers, and publication ordinals identify updates. Ordinal zero is the
initial empty snapshot. This is not current-frame state, can lag ten seconds
during play, and may remain stale while idle. The default-off payload is only
`{"enabled":false}`. There is no new per-call publication work or bridge ABI.

## Evidence

Private receipts and exact invocations are under
`/Users/zach/gaming/SantosVision/target/verification/device-session-20261026/bind-key-first`.

The final optimized release benchmark uses the actual production
`stage_bind_group` function with real Mac M4 Metal handles, enabled existing
preparation/uniform features, synthetic warm bindings, alternating mode order,
nine rounds of 5,000 calls per mode, and 64 initial warmups. All eight texture
bindings intentionally refer to the same ordinary SRV. The bounded CPU-only ring
is rewound between batches, which have no GPU consumer. Each batch asserts zero
GPU submissions and no open command encoder. `cpu-bench-final.json` records all
raw trials and source hashes are in its recipe.

| Synthetic binding case | Reference median | Key-first median | Saved per call |
| --- | ---: | ---: | ---: |
| Uniform + storage buffer, stable UBO | 272.24 ns | 258.01 ns | 14.23 ns |
| Eight ordinary SRVs + sampler, stable UBO | 822.43 ns | 422.91 ns | 399.52 ns |
| Eight ordinary SRVs + sampler, changed UBO each call | 1043.21 ns | 637.99 ns | 405.22 ns |

The uniform-only saving is negligible. The textured cases show a repeatable
sub-microsecond source-site saving. Actual eligible calls per game frame and
aggregate frame savings remain unmeasured; these are not GPU timings, M2
measurements, game FPS or perceived-quality evidence. Earlier pre-mix-counter
baseline timings are separately retained in `cpu-bench-release.json`.

The initial three actual offscreen Mac Metal tests passed serially with an
exclusive host GPU slot (`gpu-differential-r2.log`). The final six-test
qualification, including review-driven miss/offset coverage, is recorded in
`gpu-differential-final.log`:

1. Both production paths produce identical seven ordered keys, dynamic offsets,
   uniform-snapshot semantic counters, write-guard accounting and actual GPU
   output words. The sequence includes a cached hit with a pending color clear,
   same-ID parent texture/buffer/sampler replacement, a subsequent warm hit, and
   a real new native uniform-ring chunk. The write-guard hit hook uses an
   explicitly injected tracked fixture lifetime; the normal UAV is ineligible
   for upload deduplication.
2. Borrowed preflight rejects cold views, proxies, missing samplers, unaligned
   offsets and counter bindings without advancing the ring or encoding work.
3. An ordinary sampled-depth hit consumes a pending depth clear while a real
   foreground pre-snapshot session is open. The nested production post-snapshot
   and depth-proxy path closes that session and retains exact expected sampled
   depth values on both paths. Explicit eligible-lookup/hit/fallback assertions
   establish that this actually exercised the prototype.
4. The same pending-depth-clear/foreground chain runs on a qualified cache miss
   caused by a real new uniform-ring chunk. Ordered keys, dynamic offsets,
   snapshot semantic counters and output words match the reference.
5. A nonzero 256-byte-aligned storage view, with an explicit size, produces the
   same native key, dynamic offsets and actual GPU output on both miss and hit.
6. The eight repeated texture binding case from the benchmark is also exercised
   by the actual shader-output/lifetime differential, rather than CPU timing
   alone.

The original first differential run compared two nondeterministic CPU clock
fields and failed, despite identical keys/offsets/output. Its log is preserved
as `gpu-differential.log`. Only those clock fields were removed from the
semantic equality assertion. No production correction was required.

Ten adjacent texture lifetime/reused-target/deferred-clear Metal regressions
and the existing uniform-snapshot/reused-view-ID Metal regression also passed
(`adjacent-gpu-receipt.json`). Pure exact static SRV key and separate opt-in flag
tests pass (`pure-final-receipt.json`). Production default remains off.
Two CPU metadata/concurrency tests additionally check consistent POD copies,
unlock-before-JSON, bounded payload and rejection of a retired publisher
(`status-cpu.log`). This status-only addition required no new GPU execution.

Actual first-party Opus 5.5 source review is recorded in `opus-review` and the
final frozen-source follow-up in `opus-final-review`. Requested Fast mode is
recorded separately from the provider's reported execution speed; it must not be
assumed applied. The independent metadata-only delta review is recorded in
`status-delta-review`.

The next useful qualification is one bounded native gameplay run with the new
flag explicitly enabled, collecting actual eligible/hit/fallback binding mixes
and frame CPU cost against the same scene. Root owns that scheduling, native
archive/app integration and any physical device work. No app has been built or
installed from this worker scope.

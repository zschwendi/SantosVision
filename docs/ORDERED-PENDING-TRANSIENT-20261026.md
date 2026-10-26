# Independent preparation while a source writer is pending

Base: `9d85d88e142f8f15fe77320432050dea95118b9e`. Source-cache reuse remains
default off. Native code, source identity validation, shaders, source resolution,
image settings, ordinary app behavior and the existing rollback are unchanged.

Previously, requesting the exact source key while its canonical writer was
unfinished immediately returned `nil`, causing the assembled renderer to use
its slower fine path even with spare ordered-corner capacity. The actual Mac
Metal baseline reproduction commits the canonical command behind a shared-event
wait. It confirms the duplicate returns `nil` while one set is busy and both
set/byte capacity remain available.

The pool now separates the requested key from its cache-admission key. The
requested key still governs source/epoch retirement, completed-source lookup
and idle-unkeyed preference. Normal new canonical sources retain pressure LRU.
A pending duplicate gets a separate
fresh preparation with a **nil admission key**. It can only use an idle or newly
allocated set, never the canonical writer's allocation or a live reader's set.
Its completion can never publish a ready cache or replace the canonical entry.
Completed transient sets remain idle/unkeyed and can be recycled safely.

The limits remain six sets, 384 MiB including actual driver allocation padding,
and twelve outstanding checkouts. If no safe bounded set is available the
complete fine fallback is preserved. Existing completion-only leases,
abandonment cleanup and partial-encoder handling apply to each transient
independently. The pool retains no source texture or native source lease.
Transients never evict keyed cached data, even if it is idle: byte/set pressure
declines the transient before canonical LRU. A later actual-driver-padding
budget refusal likewise preserves keyed entries. This deliberately limits the
new behavior to spare-capacity assistance.

The actual per-eye preparation label is `pending_source_fresh_transient`.
`pending_transient_admissions` is a bounded cumulative count of resource
admissions, including admissions whose later encoder creation fails; it does
not claim completed renders or cache hits.

Private evidence lives at
`/Users/zach/gaming/SantosVision/target/verification/device-session-20261026/ordered-pending-transient`.
The original helper source and executable are retained under `baseline`, with
the actual blocked-writer receipt `baseline/blocked-writer.json`. Candidate
final receipts are under `candidate-pressure-safe` (`candidate` records the
pre-review LRU policy), and the repeatable command is
`scripts/test-ordered-pending-transient.sh NEW_OUTPUT_DIR`.

Qualification passed on actual offscreen Mac M4 Metal:

- 34 CPU identity/qualifier/budget checks.
- 114 Metal resource/content/lifetime checks, including distinct committed
  transients queued behind a GPU-event-blocked writer; idle other-eye cache
  preservation with spare capacity; six fully busy sets declining; a transient
  completing before the uncommitted canonical; safe idle transient recycling;
  first/second-encoder refusal, completion and abandonment; canonical-only cache
  publication; and the existing source-reuse, reader, epoch and memory bounds.
  Targeted tall-source pressure preserves an idle other-eye cache; ordinary new
  canonical LRU remains active. A real driver-padding boundary, found within a
  bounded 33-probe search, demonstrates that nominal byte admission can pass
  while actual padded allocation is refused without evicting keyed entries.
- 125 assembled image checks, 30 completed finite-sky-plus-gap-fill renders.
  Seven distinct display poses complete using fresh transients while the
  canonical source remains uncommitted. Full RGBA bytes, Float32 depth bits and
  final ownership bytes match the cacheless ordered baseline exactly. The
  committed/blocked resource test separately covers simultaneous outstanding
  transient leases; the image cadence intentionally completes each transient
  before recycling its idle set.

These are immutable synthetic inputs and offscreen tests. This does not prove
M2/AVP frame savings, real-game quality, stereo coherence or perceived latency.
No worker app was built or installed, and no performance timing was collected.
Actual first-party Opus 5.5 read-only review is preserved under `opus-review`;
the provider reported standard speed despite requested Fast mode.
Its flagged under-pressure cache-eviction tradeoff was explicitly rejected by
the parent, and the conservative source delta is separately reviewed under
`opus-pressure-delta`.

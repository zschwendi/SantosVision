# Uniform snapshot insertion token

This incremental change removes the second large-key lookup in cache insertion.
The only production caller is `Gpu::pack_copy`: probe, synchronously copy a fresh
immutable ring snapshot, then insert on a miss. That sequence has one GPU-worker
owner and no callback, guest command, wait, flush or intervening cache access.

`probe` returns an opaque owned `MissToken` only on a miss. Its fields are private
and it implements neither `Copy` nor `Clone`; `insert` consumes it. Hits and
bypasses return no token. An enabled probe advances a checked serial, as do
present/version-wrap clears. Insert rejects a stale serial before changing the
map or FIFO. Repeated probes, including hits, therefore invalidate older pending
misses. Rejected tokens do not evict entries or publish their supplied offsets.
Callers must pair the token with the freshly copied snapshot from the same cache
owner. This is an internal insertion authorization, not an offset validator.

Serial exhaustion never recycles an authorization: it clears entries and latches
fresh-packing fallback. The existing requested-enabled status stays visible;
`serial_exhausted` reports the fallback, and `stale_insertions` counts rejected
tokens. Subsequent requests count as bypasses. There is no new flag, allocation,
linear search, eviction-policy change, transient 129th entry, ring reuse change,
shader ABI change or AOT change. The existing pre-eviction keeps the cap at 128.

The measured 3a4c4b7 ALL workload had 2,212,141 misses in 70.095 seconds. This
removes that many redundant `contains_key` lookups and stops returning complete
keys on hits. Each enabled probe gains one checked serial increment/store and
each insertion one integer freshness check; benefit remains unmeasured. Queue
uploads and encoder finishing are larger observed costs.

Four added CPU fixtures cover repeated probes, an intervening hit at a full
cache, present/version-wrap invalidation, and terminal exhaustion. Reusing a
consumed token is prevented by Rust ownership. Root owns compilation, existing
Metal snapshot fixtures and full OFF/ON qualification; this worker ran only
static diff checks.

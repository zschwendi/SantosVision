//! Redundant op47 upload guard. Default OFF; `MUGUET_OP47_UPLOAD_DEDUP=1`.
//!
//! Ordinary GPU vertex/index buffers have no CPU mirror, so "would this upload
//! change anything?" has no general answer. This keeps a bounded table of exact
//! records: the range and bytes of an op47 (no-overwrite) upload to an eligible
//! buffer object. One buffer may hold several records as long as their
//! half-open ranges [offset, offset + len) are pairwise disjoint. A later op47
//! on the same object lifetime with the same offset, the same length and the
//! same bytes is redundant, and only its queue write is skipped. Every other
//! call takes the legacy route unchanged. Bytes are compared in full; nothing
//! is hashed, and equal bytes at another offset or inside a longer record are
//! not a repeat.
//!
//! A record is trusted only while nothing else can have changed its range:
//! - a compared op47 that is not an exact repeat first drops every record of
//!   that buffer whose range intersects its own, and keeps the disjoint ones;
//! - every op12, and every op47 the guard does not compare, drops ALL of the
//!   buffer's records, since a padded tail reaches past the range the guest
//!   named and the guard does not model those writes;
//! - a GPU-side write (copy/clear/counter destination, writable storage
//!   binding) drops them all and takes the object out of the guard for the
//!   rest of its lifetime.
//!
//! The last rule is deliberately sticky and never range-local. Uploads run in
//! a queue prelude ahead of every encoder command of their submission, so a
//! copy recorded BEFORE a later op47 still lands AFTER it; recording that op47
//! would call bytes authoritative that the copy is about to replace.
//!
//! Pure CPU bookkeeping: no waits, submits, readbacks or GPU objects. The table
//! is one vector reserved once and never longer than its record cap, so every
//! scan here is bounded by that cap.
use std::time::Instant;

pub(crate) const ENVIRONMENT: &str = "MUGUET_OP47_UPLOAD_DEDUP";

/// Table bounds. `records` and `total_bytes` are global across all buffers;
/// `buffer_records` keeps one buffer from pushing every other buffer out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Caps {
    pub(crate) records: usize,
    pub(crate) buffer_records: usize,
    pub(crate) record_bytes: usize,
    pub(crate) total_bytes: usize,
}

impl Caps {
    pub(crate) const DEFAULT: Caps = Caps { records: 128, buffer_records: 64, record_bytes: 4096, total_bytes: 512 * 1024 };
}

// Guest D3D11 flags exactly as `Gpu::create_buffer` receives them. SRV and UAV
// buffers get STORAGE usage there; CONSTANT buffers get a CPU shadow.
const BIND_VERTEX: u32 = 0x1;
const BIND_INDEX: u32 = 0x2;
const BIND_CONSTANT: u32 = 0x4;
const BIND_SHADER_RESOURCE: u32 = 0x8;
const BIND_UNORDERED_ACCESS: u32 = 0x80;
const MISC_DRAW_INDIRECT: u32 = 0x10;

// Indexes into `Interval::bypass_shape`: op47 calls that are never compared.
const SHAPE_EMPTY: usize = 0;
const SHAPE_UNALIGNED: usize = 1;
const SHAPE_OUT_OF_BOUNDS: usize = 2;
const SHAPE_OVERSIZE: usize = 3;

/// Per-object guard state. It lives in the buffer object itself, so it ends
/// exactly when that object is destroyed or replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Track {
    /// Guard off, or a kind of buffer the guard never follows.
    Untracked,
    /// Eligible. The identity is issued once per object and never reused, so a
    /// guest id recycled by destroy/recreate cannot reach an older record.
    Lifetime(u64),
    /// A GPU-side write has targeted this object. Permanent.
    Disqualified,
}

/// GPU-side writes into a guest buffer. Any of them ends tracking for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuWrite { CopyRegion, ClearUav, CopyCounter, StorageBinding }

impl GpuWrite {
    fn index(self) -> usize {
        match self { Self::CopyRegion => 0, Self::ClearUav => 1, Self::CopyCounter => 2, Self::StorageBinding => 3 }
    }
}

/// What `upload_buffer` does with one op47 call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op47 {
    /// Legacy route; nothing is recorded.
    Legacy,
    /// Exact repeat of a recorded upload: its queue write is redundant.
    Skip,
    /// Legacy route, then `store` the uploaded bytes under this identity.
    Record(u64),
}

struct Record { lifetime: u64, offset: u64, touched: u64, bytes: Box<[u8]> }

impl Record {
    // `insert` admits only ranges whose end was computed without overflow.
    fn end(&self) -> u64 { self.offset + self.bytes.len() as u64 }
    // Half-open [offset, end): ranges that merely touch do not intersect.
    fn intersects(&self, offset: u64, end: u64) -> bool { self.offset < end && offset < self.end() }
}

#[derive(Default)]
struct Interval {
    op12_calls: u64, op47_calls: u64, checked: u64, hits: u64,
    miss_no_record: u64, miss_range: u64, miss_bytes: u64,
    bypass_untracked: u64, bypass_disqualified: u64, bypass_shape: [u64; 4],
    saved_calls: u64, saved_bytes: u64,
    stored: u64, store_rejected: u64, evictions: u64, evictions_buffer_cap: u64,
    // Calls that dropped at least one record.
    invalidated_op12: u64, invalidated_unrecorded_op47: u64, invalidated_gpu_write: u64, forgotten: u64,
    // Records dropped, by cause. Evictions are counted above.
    dropped_changed_op47: u64, dropped_op12: u64, dropped_bypass: [u64; 4], dropped_gpu_write: u64,
    dropped_forgotten: u64, dropped_recency_reset: u64,
    disqualified: [u64; 4], tracked_buffers: u64, lifetime_exhausted: u64,
    check_ns: u64, check_max_ns: u64, store_ns: u64, store_max_ns: u64,
    records_peak: u64, record_bytes_peak: u64, buffer_records_peak: u64,
}

impl Interval {
    fn misses(&self) -> u64 { self.miss_no_record + self.miss_range + self.miss_bytes }
    fn bypasses(&self) -> u64 { self.bypass_untracked + self.bypass_disqualified + self.bypass_shape.iter().sum::<u64>() }
    #[cfg(test)]
    fn purged(&self) -> u64 {
        self.dropped_op12 + self.dropped_bypass.iter().sum::<u64>() + self.dropped_gpu_write + self.dropped_forgotten
    }
}

/// Counters a fixture can compare directly. `dropped_changed` counts records
/// dropped because a compared, changed op47 intersected them; `purged` counts
/// records dropped by op12, uncompared op47, GPU writes and lifetime end.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub(crate) checked: u64, pub(crate) hits: u64, pub(crate) misses: u64, pub(crate) bypasses: u64,
    pub(crate) saved_calls: u64, pub(crate) saved_bytes: u64, pub(crate) evictions: u64,
    pub(crate) dropped_changed: u64, pub(crate) purged: u64,
    pub(crate) disqualified: u64, pub(crate) records: usize, pub(crate) record_bytes: usize,
}

pub(crate) struct UploadDedup {
    enabled: bool,
    caps: Caps,
    // Every record of every buffer, in no particular order. Records of one
    // lifetime never intersect. Never longer than `caps.records`.
    records: Vec<Record>,
    total_bytes: usize,
    next_lifetime: u64,
    clock: u64,
    stats: Interval,
}

// Only the exact value 1 turns the guard on. It does not follow the app's
// saved draw-setup choice: this experiment is enabled per process, by hand.
fn parse(value: Option<&str>) -> Result<bool, ()> {
    match value { None | Some("0") => Ok(false), Some("1") => Ok(true), _ => Err(()) }
}

/// Creation-time eligibility: a GPU-backed vertex or index buffer with no
/// storage binding, no CPU shadow and no indirect-argument use.
fn eligible_kind(bind: u32, misc: u32, has_gpu: bool, has_shadow: bool) -> bool {
    has_gpu && !has_shadow
        && bind & (BIND_VERTEX | BIND_INDEX) != 0
        && bind & (BIND_CONSTANT | BIND_SHADER_RESOURCE | BIND_UNORDERED_ACCESS) == 0
        && misc & MISC_DRAW_INDIRECT == 0
}

// Only nonempty, 4-aligned, in-bounds writes that fit one record are compared.
// Such a write is one queue write of exactly [offset, offset + size). Empty,
// unaligned, padded-tail, out-of-range and larger calls keep their legacy
// handling byte for byte.
fn bypass_shape(offset: u64, size: u64, gpu_bytes: u64, max_record_bytes: usize) -> Option<usize> {
    if size == 0 { Some(SHAPE_EMPTY) }
    else if !offset.is_multiple_of(4) || !size.is_multiple_of(4) { Some(SHAPE_UNALIGNED) }
    else if offset.checked_add(size).is_none_or(|end| end > gpu_bytes) { Some(SHAPE_OUT_OF_BOUNDS) }
    else if size > max_record_bytes as u64 { Some(SHAPE_OVERSIZE) }
    else { None }
}

#[inline]
fn bump(counter: &mut u64) { *counter = counter.saturating_add(1); }
#[inline]
fn add(counter: &mut u64, amount: u64) { *counter = counter.saturating_add(amount); }

fn elapsed_ns(started: Instant) -> u64 { started.elapsed().as_nanos().min(u64::MAX as u128) as u64 }

impl UploadDedup {
    pub(crate) fn from_environment() -> Self {
        let value = std::env::var(ENVIRONMENT).ok();
        let enabled = parse(value.as_deref()).unwrap_or_else(|_| {
            eprintln!("[gpu] invalid {ENVIRONMENT}; expected 0 or 1, leaving the op47 upload guard off");
            false
        });
        Self::new(enabled, Caps::DEFAULT)
    }

    pub(crate) fn new(enabled: bool, caps: Caps) -> Self {
        // Reserved once: the table never grows past its cap. Off reserves nothing.
        let records = if enabled { Vec::with_capacity(caps.records) } else { Vec::new() };
        Self { enabled, caps, records, total_bytes: 0, next_lifetime: 1, clock: 0, stats: Interval::default() }
    }

    pub(crate) fn enabled(&self) -> bool { self.enabled }

    /// Guard state for a buffer that is being created.
    pub(crate) fn track_new(&mut self, bind: u32, misc: u32, has_gpu: bool, has_shadow: bool) -> Track {
        if !self.enabled || !eligible_kind(bind, misc, has_gpu, has_shadow) { return Track::Untracked; }
        // Identities are never reissued. Should they ever run out, later
        // buffers stay untracked instead of wrapping onto a live record's key.
        let Some(next) = self.next_lifetime.checked_add(1) else {
            bump(&mut self.stats.lifetime_exhausted);
            return Track::Untracked;
        };
        let lifetime = self.next_lifetime;
        self.next_lifetime = next;
        bump(&mut self.stats.tracked_buffers);
        Track::Lifetime(lifetime)
    }

    /// Every op12 drops all of the buffer's records before it flushes or
    /// writes, wherever it lands and even when it is empty or changes nothing.
    pub(crate) fn op12(&mut self, track: Track) {
        if !self.enabled { return; }
        bump(&mut self.stats.op12_calls);
        if let Track::Lifetime(lifetime) = track {
            let dropped = self.purge(lifetime);
            if dropped > 0 {
                bump(&mut self.stats.invalidated_op12);
                add(&mut self.stats.dropped_op12, dropped);
            }
        }
    }

    /// Classifies one op47 BEFORE its legacy route runs. `data` is the host's
    /// snapshot of the guest bytes, exactly what the route would write;
    /// `gpu_bytes` is the destination's GPU size.
    pub(crate) fn op47(&mut self, track: Track, offset: u64, data: &[u8], gpu_bytes: u64) -> Op47 {
        if !self.enabled { return Op47::Legacy; }
        let started = Instant::now();
        let decision = self.classify(track, offset, data, gpu_bytes);
        let ns = elapsed_ns(started);
        add(&mut self.stats.check_ns, ns);
        self.stats.check_max_ns = self.stats.check_max_ns.max(ns);
        decision
    }

    fn classify(&mut self, track: Track, offset: u64, data: &[u8], gpu_bytes: u64) -> Op47 {
        bump(&mut self.stats.op47_calls);
        let lifetime = match track {
            Track::Lifetime(lifetime) => lifetime,
            Track::Untracked => { bump(&mut self.stats.bypass_untracked); return Op47::Legacy; }
            Track::Disqualified => { bump(&mut self.stats.bypass_disqualified); return Op47::Legacy; }
        };
        let size = data.len() as u64;
        if let Some(shape) = bypass_shape(offset, size, gpu_bytes, self.caps.record_bytes) {
            bump(&mut self.stats.bypass_shape[shape]);
            // The guard does not model where that legacy write lands (a padded
            // tail reaches past the named range), so the whole buffer is
            // purged, not just the records the named range intersects.
            let dropped = self.purge(lifetime);
            if dropped > 0 {
                bump(&mut self.stats.invalidated_unrecorded_op47);
                add(&mut self.stats.dropped_bypass[shape], dropped);
            }
            return Op47::Legacy;
        }
        bump(&mut self.stats.checked);
        // `bypass_shape` proved this end lies inside the buffer: no overflow.
        let end = offset + size;
        let (mut exact, mut intersecting) = (None, 0u64);
        for (index, record) in self.records.iter().enumerate() {
            if record.lifetime != lifetime || !record.intersects(offset, end) { continue; }
            intersecting += 1;
            if record.offset == offset && record.bytes.len() == data.len() { exact = Some(index); }
        }
        // One buffer's records are disjoint, so an exact-range record is the
        // only one this range can intersect. Anything else is treated as a
        // change rather than trusted.
        if let (Some(index), 1) = (exact, intersecting) {
            if &self.records[index].bytes[..] == data {
                let now = self.tick();
                if let Some(record) = self.records.get_mut(index) { record.touched = now; }
                bump(&mut self.stats.hits);
                bump(&mut self.stats.saved_calls);
                add(&mut self.stats.saved_bytes, size);
                return Op47::Skip;
            }
        }
        bump(if intersecting == 0 { &mut self.stats.miss_no_record } else if exact.is_none() { &mut self.stats.miss_range } else { &mut self.stats.miss_bytes });
        // A changed upload: every record its range reaches into goes before
        // the write happens, and the records it does not touch stay. The new
        // record arrives through `store` only after the legacy route has run.
        if intersecting > 0 {
            let dropped = self.drop_matching(|record| record.lifetime == lifetime && record.intersects(offset, end));
            add(&mut self.stats.dropped_changed_op47, dropped);
        }
        Op47::Record(lifetime)
    }

    /// Records the bytes an op47 just sent down the legacy route. Called only
    /// for `Op47::Record`, after routing, so a record never precedes its write.
    pub(crate) fn store(&mut self, lifetime: u64, offset: u64, data: Vec<u8>) {
        if !self.enabled { return; }
        let started = Instant::now();
        self.insert(lifetime, offset, data);
        let ns = elapsed_ns(started);
        add(&mut self.stats.store_ns, ns);
        self.stats.store_max_ns = self.stats.store_max_ns.max(ns);
    }

    fn insert(&mut self, lifetime: u64, offset: u64, data: Vec<u8>) {
        let (size, caps) = (data.len(), self.caps);
        let end = offset.checked_add(size as u64).filter(|_| size > 0);
        // `op47` already dropped what this range intersects. Doing it again
        // here keeps one buffer's records disjoint whatever the caller did.
        if let Some(end) = end {
            let dropped = self.drop_matching(|record| record.lifetime == lifetime && record.intersects(offset, end));
            add(&mut self.stats.dropped_changed_op47, dropped);
        }
        // Not admitted: the records dropped above stay dropped.
        if end.is_none() || size > caps.record_bytes || size > caps.total_bytes || caps.records == 0 || caps.buffer_records == 0 {
            bump(&mut self.stats.store_rejected);
            return;
        }
        // Make room, least recently used first: within this buffer for its own
        // cap, then across the table for the global count and byte caps.
        // Eviction only forgets; the evicted range's next op47 is a plain miss.
        while self.buffer_records(lifetime) >= caps.buffer_records {
            let Some(at) = self.oldest(Some(lifetime)) else { break };
            self.evict(at);
            bump(&mut self.stats.evictions_buffer_cap);
        }
        while self.records.len() >= caps.records || self.total_bytes.saturating_add(size) > caps.total_bytes {
            let Some(at) = self.oldest(None) else { break };
            self.evict(at);
        }
        let touched = self.tick();
        self.total_bytes += size;
        self.records.push(Record { lifetime, offset, touched, bytes: data.into_boxed_slice() });
        bump(&mut self.stats.stored);
        self.stats.records_peak = self.stats.records_peak.max(self.records.len() as u64);
        self.stats.record_bytes_peak = self.stats.record_bytes_peak.max(self.total_bytes as u64);
        self.stats.buffer_records_peak = self.stats.buffer_records_peak.max(self.buffer_records(lifetime) as u64);
    }

    /// A GPU-side write is about to target this buffer. Called BEFORE that
    /// write is recorded: every record goes and the object never comes back.
    pub(crate) fn gpu_write(&mut self, track: &mut Track, kind: GpuWrite) {
        if !self.enabled { return; }
        let Track::Lifetime(lifetime) = *track else { return };
        let dropped = self.purge(lifetime);
        if dropped > 0 {
            bump(&mut self.stats.invalidated_gpu_write);
            add(&mut self.stats.dropped_gpu_write, dropped);
        }
        *track = Track::Disqualified;
        bump(&mut self.stats.disqualified[kind.index()]);
    }

    /// The object was destroyed or replaced; its records end with it.
    pub(crate) fn forget(&mut self, track: Track) {
        if let Track::Lifetime(lifetime) = track {
            let dropped = self.purge(lifetime);
            if dropped > 0 {
                bump(&mut self.stats.forgotten);
                add(&mut self.stats.dropped_forgotten, dropped);
            }
        }
    }

    // Drops every record of one buffer. Returns how many there were.
    fn purge(&mut self, lifetime: u64) -> u64 {
        self.drop_matching(|record| record.lifetime == lifetime)
    }

    // One pass over the table. Returns how many records were dropped.
    fn drop_matching(&mut self, mut matches: impl FnMut(&Record) -> bool) -> u64 {
        let (mut dropped, mut bytes) = (0u64, 0usize);
        self.records.retain(|record| {
            let gone = matches(record);
            if gone { dropped += 1; bytes += record.bytes.len(); }
            !gone
        });
        self.total_bytes -= bytes;
        dropped
    }

    fn buffer_records(&self, lifetime: u64) -> usize {
        self.records.iter().filter(|record| record.lifetime == lifetime).count()
    }

    // Index of the least recently used record, of one buffer or of the table.
    fn oldest(&self, lifetime: Option<u64>) -> Option<usize> {
        self.records.iter().enumerate()
            .filter(|(_, record)| lifetime.is_none_or(|wanted| record.lifetime == wanted))
            .min_by_key(|(_, record)| record.touched)
            .map(|(index, _)| index)
    }

    fn evict(&mut self, at: usize) {
        let record = self.records.swap_remove(at);
        self.total_bytes -= record.bytes.len();
        bump(&mut self.stats.evictions);
    }

    // Recency for eviction only. Exhausting the counter empties the table and
    // its byte count: that can cost hits, never correctness.
    fn tick(&mut self) -> u64 {
        match self.clock.checked_add(1) {
            Some(next) => self.clock = next,
            None => {
                add(&mut self.stats.dropped_recency_reset, self.records.len() as u64);
                self.records.clear();
                self.total_bytes = 0;
                self.clock = 1;
            }
        }
        self.clock
    }

    // (buffers holding records, most records held by one buffer). Used when
    // reporting, never per upload.
    fn occupancy(&self) -> (usize, usize) {
        let mut lifetimes: Vec<u64> = self.records.iter().map(|record| record.lifetime).collect();
        lifetimes.sort_unstable();
        let (mut buffers, mut most, mut run) = (0usize, 0usize, 0usize);
        for (index, lifetime) in lifetimes.iter().enumerate() {
            if index == 0 || lifetimes[index - 1] != *lifetime { buffers += 1; run = 0; }
            run += 1;
            most = most.max(run);
        }
        (buffers, most)
    }

    pub(crate) fn metrics(&self, interval_s: f64) -> serde_json::Value {
        let s = &self.stats;
        let (record_buffers, largest_buffer_records) = self.occupancy();
        // Built in pieces: one large literal would strain json!'s recursion.
        let misses = serde_json::json!({"no_record": s.miss_no_record, "range": s.miss_range, "bytes": s.miss_bytes});
        let bypasses = serde_json::json!({"untracked_buffer": s.bypass_untracked, "disqualified_buffer": s.bypass_disqualified,
            "empty": s.bypass_shape[SHAPE_EMPTY], "unaligned_or_padded": s.bypass_shape[SHAPE_UNALIGNED],
            "out_of_bounds": s.bypass_shape[SHAPE_OUT_OF_BOUNDS], "over_record_cap": s.bypass_shape[SHAPE_OVERSIZE]});
        let invalidations = serde_json::json!({"op12": s.invalidated_op12, "unrecorded_op47": s.invalidated_unrecorded_op47,
            "gpu_write": s.invalidated_gpu_write, "destroyed_or_replaced": s.forgotten});
        let dropped_by_shape = serde_json::json!({"empty": s.dropped_bypass[SHAPE_EMPTY], "unaligned_or_padded": s.dropped_bypass[SHAPE_UNALIGNED],
            "out_of_bounds": s.dropped_bypass[SHAPE_OUT_OF_BOUNDS], "over_record_cap": s.dropped_bypass[SHAPE_OVERSIZE]});
        let dropped = serde_json::json!({"changed_op47": s.dropped_changed_op47, "op12": s.dropped_op12,
            "unrecorded_op47": s.dropped_bypass.iter().sum::<u64>(), "unrecorded_op47_by_shape": dropped_by_shape,
            "gpu_write": s.dropped_gpu_write, "destroyed_or_replaced": s.dropped_forgotten,
            "evicted": s.evictions, "recency_reset": s.dropped_recency_reset});
        let disqualified = serde_json::json!({"copy_region": s.disqualified[0], "clear_uav": s.disqualified[1],
            "copy_counter": s.disqualified[2], "storage_binding": s.disqualified[3]});
        let table = serde_json::json!({"stored": s.stored, "store_rejected": s.store_rejected,
            "evictions": s.evictions, "evictions_buffer_cap": s.evictions_buffer_cap,
            "records_peak": s.records_peak, "record_bytes_peak": s.record_bytes_peak, "buffer_records_peak": s.buffer_records_peak,
            "tracked_buffers": s.tracked_buffers, "lifetime_exhausted": s.lifetime_exhausted});
        let interval = serde_json::json!({"op47_calls": s.op47_calls, "op12_calls": s.op12_calls,
            "checked": s.checked, "hits": s.hits, "misses": s.misses(), "bypasses": s.bypasses(),
            "saved_calls": s.saved_calls, "saved_bytes": s.saved_bytes,
            "check_ns": s.check_ns, "check_max_ns": s.check_max_ns, "store_ns": s.store_ns, "store_max_ns": s.store_max_ns,
            "miss_reasons": misses, "bypass_reasons": bypasses, "invalidations": invalidations,
            "records_dropped": dropped, "disqualified": disqualified, "table": table});
        serde_json::json!({"schema": 2, "enabled": self.enabled, "environment": ENVIRONMENT, "interval_s": interval_s,
            "cap_records": self.caps.records, "cap_buffer_records": self.caps.buffer_records,
            "cap_record_bytes": self.caps.record_bytes, "cap_total_bytes": self.caps.total_bytes,
            "records": self.records.len(), "record_bytes": self.total_bytes,
            "record_buffers": record_buffers, "largest_buffer_records": largest_buffer_records, "stats_interval": interval,
            "counting_scope": "op47_calls = checked + bypasses and checked = hits + misses, for op47 on GPU-backed buffers only. A hit skips exactly one queue write; saved_bytes are the bytes of those writes; hits are absent from the op47 line of buffer upload timing. miss_reasons: no_record = no record of this buffer intersects the range, range = records intersect it but none has this exact range, bytes = exact range with other bytes. invalidations count calls that dropped at least one record; records_dropped counts records, and stored minus its sum is the change in records.",
            "timing_scope": "CPU only, not GPU time: check_ns is the classification, bounded table scan and byte comparison of every op47 call; store_ns is record insertion and eviction after a miss. The guest snapshot and the upload route are outside both."})
    }

    pub(crate) fn reset_interval(&mut self) {
        self.stats = Interval { records_peak: self.records.len() as u64, record_bytes_peak: self.total_bytes as u64,
            buffer_records_peak: self.occupancy().1 as u64, ..Default::default() };
    }

    #[cfg(test)]
    pub(crate) fn totals(&self) -> Totals {
        let s = &self.stats;
        Totals { checked: s.checked, hits: s.hits, misses: s.misses(), bypasses: s.bypasses(),
            saved_calls: s.saved_calls, saved_bytes: s.saved_bytes, evictions: s.evictions,
            dropped_changed: s.dropped_changed_op47, purged: s.purged(),
            disqualified: s.disqualified.iter().sum(), records: self.records.len(), record_bytes: self.total_bytes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPU_BYTES: u64 = 256;

    fn guard() -> UploadDedup { UploadDedup::new(true, Caps::DEFAULT) }

    fn vertex_buffer(guard: &mut UploadDedup) -> Track {
        let track = guard.track_new(BIND_VERTEX, 0, true, false);
        assert!(matches!(track, Track::Lifetime(_)), "a plain vertex buffer must be tracked, got {track:?}");
        track
    }

    /// One op47 through the protocol `upload_buffer` follows. Returns whether
    /// the legacy write ran.
    fn op47(guard: &mut UploadDedup, track: Track, offset: u64, data: &[u8]) -> bool {
        match guard.op47(track, offset, data, GPU_BYTES) {
            Op47::Skip => false,
            Op47::Legacy => true,
            Op47::Record(lifetime) => { guard.store(lifetime, offset, data.to_vec()); true }
        }
    }

    /// The table's own bookkeeping: caps, byte accounting, and that no two
    /// records of one buffer intersect.
    fn assert_consistent(guard: &UploadDedup) {
        let caps = guard.caps;
        assert!(guard.records.len() <= caps.records, "{} records", guard.records.len());
        assert_eq!(guard.total_bytes, guard.records.iter().map(|record| record.bytes.len()).sum::<usize>());
        assert!(guard.total_bytes <= caps.total_bytes, "{} bytes", guard.total_bytes);
        for (index, a) in guard.records.iter().enumerate() {
            assert!(!a.bytes.is_empty() && a.bytes.len() <= caps.record_bytes);
            assert!(guard.buffer_records(a.lifetime) <= caps.buffer_records);
            for b in &guard.records[index + 1..] {
                assert!(a.lifetime != b.lifetime || !a.intersects(b.offset, b.end()),
                    "two records of buffer {} intersect: [{}, {}) and [{}, {})", a.lifetime, a.offset, a.end(), b.offset, b.end());
            }
        }
    }

    /// The (offset, length) of every record one buffer holds, in offset order.
    fn ranges(guard: &UploadDedup, track: Track) -> Vec<(u64, usize)> {
        assert_consistent(guard);
        let Track::Lifetime(lifetime) = track else { return Vec::new() };
        let mut ranges: Vec<(u64, usize)> = guard.records.iter().filter(|record| record.lifetime == lifetime)
            .map(|record| (record.offset, record.bytes.len())).collect();
        ranges.sort();
        ranges
    }

    #[test]
    fn only_the_exact_value_one_enables_the_guard() {
        assert_eq!(ENVIRONMENT, "MUGUET_OP47_UPLOAD_DEDUP");
        assert_eq!(parse(None), Ok(false));
        assert_eq!(parse(Some("0")), Ok(false));
        assert_eq!(parse(Some("1")), Ok(true));
        for value in ["", "true", "on", "2", " 1", "1 ", "01"] { assert_eq!(parse(Some(value)), Err(())); }
        assert_eq!(Caps::DEFAULT, Caps { records: 128, buffer_records: 64, record_bytes: 4096, total_bytes: 524288 });

        // Off: nothing is reserved, tracked, compared, recorded or counted.
        let mut off = UploadDedup::new(false, Caps::DEFAULT);
        assert!(!off.enabled());
        assert_eq!(off.records.capacity(), 0);
        let mut track = off.track_new(BIND_VERTEX, 0, true, false);
        assert_eq!(track, Track::Untracked);
        for _ in 0..3 { assert_eq!(off.op47(track, 0, &[7; 16], GPU_BYTES), Op47::Legacy); }
        off.store(1, 0, vec![7; 16]);
        assert_eq!(off.op47(Track::Lifetime(1), 0, &[7; 16], GPU_BYTES), Op47::Legacy);
        off.op12(track);
        off.gpu_write(&mut track, GpuWrite::CopyRegion);
        assert_eq!(track, Track::Untracked);
        assert_eq!(off.totals(), Totals::default());
        assert_eq!(off.records.capacity(), 0);
        assert_eq!(off.metrics(0.0)["enabled"], false);
        // On: the whole table is reserved once, up front.
        assert!(guard().records.capacity() >= Caps::DEFAULT.records);
    }

    #[test]
    fn only_plain_gpu_vertex_and_index_buffers_are_tracked() {
        assert!(eligible_kind(BIND_VERTEX, 0, true, false));
        assert!(eligible_kind(BIND_INDEX, 0, true, false));
        assert!(eligible_kind(BIND_VERTEX | BIND_INDEX, 0, true, false));
        // No GPU storage, a CPU shadow, a storage binding or indirect use.
        assert!(!eligible_kind(BIND_VERTEX, 0, false, false));
        assert!(!eligible_kind(BIND_VERTEX, 0, true, true));
        assert!(!eligible_kind(0, 0, true, false));
        for extra in [BIND_CONSTANT, BIND_SHADER_RESOURCE, BIND_UNORDERED_ACCESS] {
            assert!(!eligible_kind(extra, 0, true, false));
            assert!(!eligible_kind(BIND_VERTEX | extra, 0, true, false));
            assert!(!eligible_kind(BIND_INDEX | extra, 0, true, false));
        }
        assert!(!eligible_kind(BIND_VERTEX, MISC_DRAW_INDIRECT, true, false));

        let mut guard = guard();
        assert_eq!(guard.track_new(BIND_UNORDERED_ACCESS, 0, true, false), Track::Untracked);
        assert_eq!(guard.track_new(BIND_VERTEX | BIND_CONSTANT, 0, true, true), Track::Untracked);
        let (first, second) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        assert_ne!(first, second, "two objects never share an identity");
        // An untracked buffer is never compared, however often it repeats.
        for _ in 0..3 { assert!(op47(&mut guard, Track::Untracked, 0, &[5; 8])); }
        assert_eq!(guard.totals(), Totals { bypasses: 3, ..Default::default() });
    }

    #[test]
    fn exact_repeat_is_skipped_and_changed_bytes_are_uploaded() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        let x: Vec<u8> = (1..=16).collect();
        assert!(op47(&mut guard, a, 8, &x), "nothing is known about a first upload");
        assert!(!op47(&mut guard, a, 8, &x));
        assert!(!op47(&mut guard, a, 8, &x));
        // The same range and bytes on another buffer are that buffer's own record.
        assert!(op47(&mut guard, b, 8, &x));
        assert!(!op47(&mut guard, a, 8, &x));
        assert!(!op47(&mut guard, b, 8, &x));

        // One differing byte at either end, then the new bytes repeat.
        for at in [0, 15] {
            let mut changed = x.clone();
            changed[at] ^= 0x80;
            assert!(op47(&mut guard, a, 8, &changed), "byte {at} differs");
            assert!(!op47(&mut guard, a, 8, &changed));
            // Back to the earlier bytes is a change too: A, B, A all upload.
            assert!(op47(&mut guard, a, 8, &x));
            assert!(!op47(&mut guard, a, 8, &x));
        }
        assert_eq!((ranges(&guard, a), ranges(&guard, b)), (vec![(8, 16)], vec![(8, 16)]));

        let totals = guard.totals();
        assert_eq!(totals.checked, totals.hits + totals.misses);
        assert_eq!((totals.checked, totals.hits, totals.misses, totals.bypasses), (14, 8, 6, 0));
        assert_eq!((totals.saved_calls, totals.saved_bytes), (8, 8 * 16));
        assert_eq!((totals.records, totals.record_bytes, totals.dropped_changed), (2, 32, 4));
        let stats = &guard.stats;
        assert_eq!((stats.miss_no_record, stats.miss_range, stats.miss_bytes), (2, 0, 4));
        assert_eq!(stats.op47_calls, 14);
    }

    #[test]
    fn alternating_disjoint_ranges_of_one_buffer_all_hit() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        let (first, second, between) = ([0xa1u8; 16], [0xb2u8; 16], [0xc3u8; 16]);
        assert!(op47(&mut guard, a, 0, &first));
        assert!(op47(&mut guard, a, 32, &second), "a disjoint range is its own first upload");
        for round in 0..3 {
            assert!(!op47(&mut guard, a, 0, &first), "round {round}");
            assert!(!op47(&mut guard, a, 32, &second), "round {round}");
        }
        // [16, 32) touches both neighbours without intersecting either.
        assert!(op47(&mut guard, a, 16, &between));
        assert_eq!(ranges(&guard, a), vec![(0, 16), (16, 16), (32, 16)]);
        assert!(!op47(&mut guard, a, 32, &second));
        assert!(!op47(&mut guard, a, 16, &between));
        assert!(!op47(&mut guard, a, 0, &first));
        // Equal bytes at another offset are another record, never a hit.
        assert!(op47(&mut guard, a, 64, &first));
        assert!(!op47(&mut guard, a, 64, &first));
        // And another buffer shares nothing with this one.
        assert!(op47(&mut guard, b, 0, &first));
        assert!(!op47(&mut guard, a, 0, &first));
        assert_eq!(ranges(&guard, a), vec![(0, 16), (16, 16), (32, 16), (64, 16)]);
        assert_eq!(ranges(&guard, b), vec![(0, 16)]);

        let totals = guard.totals();
        assert_eq!((totals.checked, totals.hits, totals.misses), (16, 11, 5));
        assert_eq!((totals.saved_calls, totals.saved_bytes), (11, 11 * 16));
        assert_eq!((totals.records, totals.record_bytes, totals.dropped_changed, totals.purged), (5, 80, 0, 0));
        let stats = &guard.stats;
        assert_eq!((stats.miss_no_record, stats.miss_range, stats.miss_bytes), (5, 0, 0));
        assert_eq!(stats.buffer_records_peak, 4);
    }

    #[test]
    fn a_changed_write_drops_every_record_it_intersects_and_keeps_the_rest() {
        // Three records on one buffer: [0, 16), [32, 48) and [64, 80).
        let (low, mid, high) = ([0x11u8; 16], [0x22u8; 16], [0x33u8; 16]);
        let seeded = || {
            let mut guard = guard();
            let track = vertex_buffer(&mut guard);
            assert!(op47(&mut guard, track, 0, &low));
            assert!(op47(&mut guard, track, 32, &mid));
            assert!(op47(&mut guard, track, 64, &high));
            (guard, track)
        };
        // (offset, length, ranges afterwards, records dropped). Every write
        // carries 0x11 bytes, so several of them equal part of `low` and
        // still are not repeats.
        let cases: [(u64, usize, Vec<(u64, usize)>, u64); 10] = [
            (8, 32, vec![(8, 32), (64, 16)], 2),                    // reaches into low and mid
            (8, 4, vec![(8, 4), (32, 16), (64, 16)], 1),            // inside low, same bytes
            (16, 16, vec![(0, 16), (16, 16), (32, 16), (64, 16)], 0), // touches low and mid only
            (0, 8, vec![(0, 8), (32, 16), (64, 16)], 1),            // same offset, shorter, same bytes
            (0, 24, vec![(0, 24), (32, 16), (64, 16)], 1),          // same offset, longer
            (0, 80, vec![(0, 80)], 3),                              // covers all three
            (12, 56, vec![(12, 56)], 3),                            // part of low, all of mid, part of high
            (40, 8, vec![(0, 16), (40, 8), (64, 16)], 1),           // tail of mid
            (48, 16, vec![(0, 16), (32, 16), (48, 16), (64, 16)], 0), // touches mid and high only
            (44, 24, vec![(0, 16), (44, 24)], 2),                   // reaches into mid and high
        ];
        for (offset, length, left, dropped) in cases {
            let (mut guard, track) = seeded();
            let data = vec![0x11u8; length];
            assert!(op47(&mut guard, track, offset, &data), "[{offset}, +{length}) is not an exact repeat");
            assert_eq!(ranges(&guard, track), left, "after [{offset}, +{length})");
            assert_eq!(guard.stats.dropped_changed_op47, dropped, "[{offset}, +{length})");
            let reasons = (guard.stats.miss_no_record, guard.stats.miss_range, guard.stats.miss_bytes);
            assert_eq!(reasons, if dropped > 0 { (3, 1, 0) } else { (4, 0, 0) }, "[{offset}, +{length})");
            // The new range repeats as a hit.
            assert!(!op47(&mut guard, track, offset, &data), "[{offset}, +{length}) repeated");
            // Every old record that survived still hits; every dropped one uploads.
            for (old_offset, old) in [(0u64, &low), (32, &mid), (64, &high)] {
                let survived = left.contains(&(old_offset, 16usize));
                assert_eq!(!op47(&mut guard, track, old_offset, old), survived, "old [{old_offset}, +16) after [{offset}, +{length})");
            }
            assert_consistent(&guard);
        }

        // The same range with other bytes replaces exactly that record.
        let (mut guard, track) = seeded();
        assert!(op47(&mut guard, track, 32, &[0x99; 16]));
        assert_eq!((guard.stats.miss_bytes, guard.stats.dropped_changed_op47), (1, 1));
        assert_eq!(ranges(&guard, track), vec![(0, 16), (32, 16), (64, 16)]);
        assert!(op47(&mut guard, track, 32, &mid), "the earlier bytes are a change again");
        assert!(!op47(&mut guard, track, 0, &low));
        assert!(!op47(&mut guard, track, 64, &high));
    }

    #[test]
    fn every_op12_drops_all_of_the_buffers_ranges_even_when_nothing_changed() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        assert!(op47(&mut guard, a, 0, &[3; 16]));
        assert!(op47(&mut guard, a, 32, &[4; 16]));
        assert!(op47(&mut guard, b, 0, &[5; 16]));
        // The guard is not told what an op12 wrote or where: every range goes.
        guard.op12(a);
        assert_eq!((ranges(&guard, a), ranges(&guard, b)), (vec![], vec![(0, 16)]));
        assert!(op47(&mut guard, a, 0, &[3; 16]), "an op12 came between the repeats");
        assert!(op47(&mut guard, a, 32, &[4; 16]), "and it dropped the other range too");
        assert!(!op47(&mut guard, a, 0, &[3; 16]));
        assert!(!op47(&mut guard, a, 32, &[4; 16]));
        assert!(!op47(&mut guard, b, 0, &[5; 16]), "an op12 on one buffer leaves the others alone");
        // Nothing to drop is fine, and untracked buffers are only counted.
        guard.op12(a);
        guard.op12(a);
        guard.op12(Track::Untracked);
        guard.op12(Track::Disqualified);
        assert_eq!((guard.stats.op12_calls, guard.stats.invalidated_op12, guard.stats.dropped_op12), (5, 2, 4));
        assert_eq!(guard.totals().purged, 4);
        assert!(op47(&mut guard, a, 0, &[3; 16]));
    }

    #[test]
    fn uncompared_op47_shapes_stay_legacy_and_drop_every_range() {
        let big = vec![9u8; Caps::DEFAULT.record_bytes + 4];
        // (offset, data, gpu size, counter). All but the last two would not
        // even reach the recorded ranges [0, 16) and [32, 48); the whole
        // buffer is purged regardless.
        let cases: [(u64, &[u8], u64, usize); 10] = [
            (128, &[], GPU_BYTES, SHAPE_EMPTY),
            (130, &[1; 8], GPU_BYTES, SHAPE_UNALIGNED),
            (128, &[1; 6], GPU_BYTES, SHAPE_UNALIGNED),
            (132, &[1; 3], GPU_BYTES, SHAPE_UNALIGNED),
            (252, &[1; 8], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (256, &[1; 4], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (u64::MAX - 3, &[1; 4], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (8192, &big, 16384, SHAPE_OVERSIZE),
            (12, &[1; 6], GPU_BYTES, SHAPE_UNALIGNED),
            (0, &big, 16384, SHAPE_OVERSIZE),
        ];
        for (index, (offset, data, gpu_bytes, shape)) in cases.into_iter().enumerate() {
            let mut guard = guard();
            let (track, other) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
            // With nothing recorded it is just the legacy route.
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index}");
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index} repeated");
            assert_eq!(guard.stats.bypass_shape[shape], 2, "case {index}");
            assert_eq!(guard.totals(), Totals { bypasses: 2, ..Default::default() }, "case {index}");
            assert!(op47(&mut guard, track, 0, &[6; 16]));
            assert!(op47(&mut guard, track, 32, &[7; 16]));
            assert!(op47(&mut guard, other, 0, &[8; 16]));
            assert!(!op47(&mut guard, track, 0, &[6; 16]));
            assert!(!op47(&mut guard, track, 32, &[7; 16]));
            // Both ranges go, wherever the uncompared write was aimed.
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index}");
            assert_eq!((ranges(&guard, track), ranges(&guard, other)), (vec![], vec![(0, 16)]), "case {index}");
            assert_eq!((guard.stats.invalidated_unrecorded_op47, guard.stats.dropped_bypass[shape], guard.totals().purged), (1, 2, 2), "case {index}");
            assert!(op47(&mut guard, track, 0, &[6; 16]), "case {index}: the first range must upload again");
            assert!(op47(&mut guard, track, 32, &[7; 16]), "case {index}: and so must the second");
            assert!(!op47(&mut guard, other, 0, &[8; 16]), "case {index}: another buffer keeps its record");
        }
        // The largest record and the exact end of the buffer are still compared.
        let mut guard = guard();
        let track = vertex_buffer(&mut guard);
        let full = vec![2u8; Caps::DEFAULT.record_bytes];
        assert_eq!(guard.op47(track, 4096, &full, 8192), Op47::Record(1));
        guard.store(1, 4096, full.clone());
        assert_eq!(guard.op47(track, 4096, &full, 8192), Op47::Skip);
        assert_eq!(guard.totals().record_bytes, Caps::DEFAULT.record_bytes);
    }

    #[test]
    fn gpu_write_forgets_every_range_and_disqualifies_the_object_for_life() {
        for kind in [GpuWrite::CopyRegion, GpuWrite::ClearUav, GpuWrite::CopyCounter, GpuWrite::StorageBinding] {
            let mut guard = guard();
            let mut track = vertex_buffer(&mut guard);
            let mut other = vertex_buffer(&mut guard);
            assert!(op47(&mut guard, track, 0, &[1; 16]));
            assert!(op47(&mut guard, track, 32, &[2; 16]));
            assert!(!op47(&mut guard, track, 0, &[1; 16]));
            assert!(!op47(&mut guard, track, 32, &[2; 16]));
            assert!(op47(&mut guard, other, 0, &[1; 16]));
            guard.gpu_write(&mut track, kind);
            assert_eq!(track, Track::Disqualified, "{kind:?}");
            assert_eq!(guard.totals().records, 1, "{kind:?}: all of its ranges went before the GPU write");
            // Nothing brings it back: not repeats, not op12, not another GPU write.
            for round in 0..4 {
                assert_eq!(guard.op47(track, 0, &[1; 16], GPU_BYTES), Op47::Legacy, "{kind:?} round {round}");
                assert_eq!(guard.op47(track, 32, &[2; 16], GPU_BYTES), Op47::Legacy, "{kind:?} round {round}");
                guard.op12(track);
                guard.gpu_write(&mut track, GpuWrite::CopyRegion);
                assert_eq!(track, Track::Disqualified);
            }
            assert_eq!(guard.stats.disqualified[kind.index()], 1);
            assert_eq!(guard.totals().disqualified, 1, "{kind:?}: counted once per object");
            assert_eq!((guard.stats.bypass_disqualified, guard.stats.invalidated_gpu_write, guard.stats.dropped_gpu_write), (8, 1, 2));
            assert!(!op47(&mut guard, other, 0, &[1; 16]), "{kind:?}: other objects keep their records");

            // A GPU write seen before any upload disqualifies just the same.
            guard.gpu_write(&mut other, kind);
            let mut fresh = vertex_buffer(&mut guard);
            guard.gpu_write(&mut fresh, kind);
            assert_eq!((other, fresh), (Track::Disqualified, Track::Disqualified));
            for _ in 0..2 { assert!(op47(&mut guard, fresh, 0, &[1; 16])); }
            assert_eq!(guard.totals().records, 0);
        }
        let mut guard = guard();
        let mut untracked = Track::Untracked;
        guard.gpu_write(&mut untracked, GpuWrite::ClearUav);
        assert_eq!(untracked, Track::Untracked);
        assert_eq!(guard.totals().disqualified, 0);
    }

    #[test]
    fn destroy_recreate_and_replacement_get_a_fresh_identity() {
        let mut guard = guard();
        let old = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, old, 0, &[8; 16]));
        assert!(op47(&mut guard, old, 32, &[9; 16]));
        assert!(!op47(&mut guard, old, 0, &[8; 16]));
        assert!(!op47(&mut guard, old, 32, &[9; 16]));
        // Destroy or replacement: every record ends with the object.
        guard.forget(old);
        assert_eq!((guard.totals().records, guard.totals().record_bytes), (0, 0));
        assert_eq!((guard.stats.forgotten, guard.stats.dropped_forgotten), (1, 2));
        let new = vertex_buffer(&mut guard);
        assert_ne!(old, new);
        assert!(op47(&mut guard, new, 0, &[8; 16]), "a recreated buffer starts with no records");
        assert!(op47(&mut guard, new, 32, &[9; 16]));
        assert!(!op47(&mut guard, new, 0, &[8; 16]));
        assert!(!op47(&mut guard, new, 32, &[9; 16]));

        // Even a record that outlived its object is unreachable: every later
        // buffer has another identity, so none of them can hit it.
        let Track::Lifetime(stale) = old else { unreachable!() };
        guard.store(stale, 0, vec![8; 16]);
        for _ in 0..8 {
            let next = vertex_buffer(&mut guard);
            assert_ne!(next, old);
            assert!(op47(&mut guard, next, 0, &[8; 16]));
            guard.forget(next);
        }
        // A disqualified object leaves nothing behind, and its successor at
        // the same guest id is eligible again.
        let mut written = vertex_buffer(&mut guard);
        guard.gpu_write(&mut written, GpuWrite::CopyCounter);
        guard.forget(written);
        let successor = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, successor, 0, &[8; 16]));
        assert!(!op47(&mut guard, successor, 0, &[8; 16]));
        guard.forget(Track::Untracked);
        guard.forget(Track::Disqualified);
        assert_consistent(&guard);
    }

    #[test]
    fn identities_are_never_reissued_when_the_counter_runs_out() {
        let mut guard = guard();
        guard.next_lifetime = u64::MAX - 2;
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        assert_eq!((a, b), (Track::Lifetime(u64::MAX - 2), Track::Lifetime(u64::MAX - 1)));
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, b, 0, &[2; 8]));
        // Exhausted: no wrap onto 0, 1 or any identity still in the table.
        for _ in 0..5 {
            let late = guard.track_new(BIND_VERTEX, 0, true, false);
            assert_eq!(late, Track::Untracked);
            assert!(op47(&mut guard, late, 0, &[1; 8]));
        }
        assert_eq!((guard.stats.lifetime_exhausted, guard.stats.tracked_buffers), (5, 2));
        assert_eq!(guard.next_lifetime, u64::MAX);
        // The identities already issued keep working.
        assert!(!op47(&mut guard, a, 0, &[1; 8]));
        assert!(!op47(&mut guard, b, 0, &[2; 8]));
    }

    #[test]
    fn recency_counter_exhaustion_only_forgets() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        guard.clock = u64::MAX - 3;
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, a, 32, &[2; 8]));
        assert!(op47(&mut guard, b, 0, &[3; 8]));
        assert_eq!((guard.clock, guard.totals().records, guard.totals().record_bytes), (u64::MAX, 3, 24));
        // This hit is decided against a valid record; refreshing its recency
        // wraps the counter, which empties the whole table and its byte count
        // instead of misordering it.
        assert!(!op47(&mut guard, a, 0, &[1; 8]));
        assert_eq!((guard.clock, guard.totals().records, guard.totals().record_bytes), (1, 0, 0));
        assert_eq!(guard.stats.dropped_recency_reset, 3);
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, a, 32, &[2; 8]));
        assert!(op47(&mut guard, b, 0, &[3; 8]));
        assert!(!op47(&mut guard, a, 0, &[1; 8]));
        assert!(!op47(&mut guard, a, 32, &[2; 8]));
        assert!(!op47(&mut guard, b, 0, &[3; 8]));
        assert_eq!(guard.totals().record_bytes, 24);
        // The same on the insertion side.
        guard.clock = u64::MAX;
        let c = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, c, 0, &[4; 8]));
        assert_eq!((guard.clock, guard.totals().records, guard.totals().record_bytes), (1, 1, 8));
        assert_eq!(guard.stats.dropped_recency_reset, 6);
        assert!(!op47(&mut guard, c, 0, &[4; 8]));
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert_consistent(&guard);
    }

    #[test]
    fn caps_bound_the_table_and_evict_least_recently_used() {
        // Four records in all, two per buffer, 16 bytes each at most, 48 bytes in all.
        let caps = Caps { records: 4, buffer_records: 2, record_bytes: 16, total_bytes: 48 };
        let mut guard = UploadDedup::new(true, caps);
        let (a, b, c) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        let evictions = |guard: &UploadDedup| (guard.stats.evictions, guard.stats.evictions_buffer_cap);

        // One buffer's third range pushes out that buffer's own oldest one.
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, a, 16, &[2; 8]));
        assert!(op47(&mut guard, a, 32, &[3; 8]));
        assert_eq!((ranges(&guard, a), evictions(&guard)), (vec![(16, 8), (32, 8)], (1, 1)));
        // A hit refreshes recency, so [32, 40) is this buffer's oldest now.
        assert!(!op47(&mut guard, a, 16, &[2; 8]));
        assert!(op47(&mut guard, a, 48, &[4; 8]));
        assert_eq!((ranges(&guard, a), evictions(&guard)), (vec![(16, 8), (48, 8)], (2, 2)));

        // Two 16-byte records of another buffer fill both global caps exactly.
        assert!(op47(&mut guard, b, 0, &[5; 16]));
        assert!(op47(&mut guard, b, 16, &[6; 16]));
        assert_eq!((guard.totals().records, guard.totals().record_bytes, evictions(&guard)), (4, 48, (2, 2)));
        // A fifth record: the least recently used of the whole table goes.
        assert!(op47(&mut guard, c, 0, &[7; 8]));
        assert_eq!((ranges(&guard, a), ranges(&guard, b), ranges(&guard, c)), (vec![(48, 8)], vec![(0, 16), (16, 16)], vec![(0, 8)]));
        assert_eq!((guard.totals().records, guard.totals().record_bytes, evictions(&guard)), (4, 48, (3, 2)));
        // One 16-byte record can cost two evictions: the count, then the bytes.
        assert!(op47(&mut guard, c, 16, &[8; 16]));
        assert_eq!((ranges(&guard, a), ranges(&guard, b), ranges(&guard, c)), (vec![], vec![(16, 16)], vec![(0, 8), (16, 16)]));
        assert_eq!((guard.totals().records, guard.totals().record_bytes, evictions(&guard)), (3, 40, (5, 2)));
        // An evicted range simply uploads again, then repeats skip as usual.
        assert!(op47(&mut guard, a, 48, &[4; 8]));
        assert!(!op47(&mut guard, a, 48, &[4; 8]));
        assert!(!op47(&mut guard, b, 16, &[6; 16]));
        assert!(!op47(&mut guard, c, 0, &[7; 8]));
        assert!(!op47(&mut guard, c, 16, &[8; 16]));
        assert_eq!((guard.totals().records, guard.totals().record_bytes, evictions(&guard)), (4, 48, (5, 2)));
        // Over the per-record cap: legacy route, never stored, buffer purged.
        assert_eq!(guard.op47(b, 32, &[7; 20], GPU_BYTES), Op47::Legacy);
        assert_eq!((guard.stats.bypass_shape[SHAPE_OVERSIZE], guard.stats.dropped_bypass[SHAPE_OVERSIZE], ranges(&guard, b)), (1, 1, vec![]));
        assert_eq!((guard.stats.records_peak, guard.stats.record_bytes_peak, guard.stats.buffer_records_peak), (4, 48, 2));

        // A store nothing asked for cannot break the caps either.
        guard.store(77, 0, vec![1; 17]);
        guard.store(78, 0, Vec::new());
        guard.store(79, u64::MAX - 3, vec![1; 8]);
        assert_eq!(guard.stats.store_rejected, 3);
        assert_consistent(&guard);

        // A compared write the table cannot admit still drops what it intersects.
        let mut tight = UploadDedup::new(true, Caps { records: 4, buffer_records: 2, record_bytes: 32, total_bytes: 24 });
        let track = vertex_buffer(&mut tight);
        assert!(op47(&mut tight, track, 0, &[1; 16]));
        assert!(op47(&mut tight, track, 64, &[2; 8]));
        assert!(op47(&mut tight, track, 8, &[3; 32]), "32 bytes pass the record cap but not the 24-byte table");
        assert_eq!((ranges(&tight, track), tight.stats.store_rejected, tight.stats.dropped_changed_op47), (vec![(64, 8)], 1, 1));
        assert!(op47(&mut tight, track, 8, &[3; 32]), "never stored, so never skipped");
        assert!(op47(&mut tight, track, 0, &[1; 16]), "the record it reached into is gone");
        assert!(!op47(&mut tight, track, 64, &[2; 8]), "the disjoint one is not");

        // Caps of zero keep everything on the legacy route.
        for zero in [Caps { records: 0, ..caps }, Caps { buffer_records: 0, ..caps }, Caps { total_bytes: 0, ..caps }, Caps { record_bytes: 0, ..caps }] {
            let mut none = UploadDedup::new(true, zero);
            let track = vertex_buffer(&mut none);
            for _ in 0..3 { assert!(op47(&mut none, track, 0, &[1; 4])); }
            assert_eq!((none.totals().records, none.totals().record_bytes, none.totals().hits), (0, 0, 0));
        }
    }

    #[test]
    fn default_caps_hold_under_churn() {
        // Many buffers with one range each: the global caps decide.
        let mut guard = guard();
        let buffers: Vec<Track> = (0..1000).map(|_| vertex_buffer(&mut guard)).collect();
        for round in 0..3u8 {
            for (index, track) in buffers.iter().enumerate() {
                let size = if index % 3 == 0 { Caps::DEFAULT.record_bytes } else { 64 + 4 * (index % 200) };
                let data = vec![round.wrapping_add(index as u8); size];
                assert_eq!(guard.op47(*track, 0, &data, 8192), Op47::Record(index as u64 + 1));
                guard.store(index as u64 + 1, 0, data);
                let totals = guard.totals();
                assert!(totals.records <= Caps::DEFAULT.records && totals.record_bytes <= Caps::DEFAULT.total_bytes);
            }
            assert_consistent(&guard);
        }
        assert_eq!(guard.totals().records, Caps::DEFAULT.records);
        assert_eq!((guard.totals().evictions, guard.stats.evictions_buffer_cap), (3000 - 128, 0));
        assert_eq!((guard.stats.records_peak, guard.stats.buffer_records_peak), (128, 1));
        assert!(guard.stats.record_bytes_peak <= Caps::DEFAULT.total_bytes as u64);
        // The most recent buffer is still recorded; the oldest is long gone.
        let last = vec![2u8.wrapping_add(999u16 as u8); Caps::DEFAULT.record_bytes];
        assert_eq!(guard.op47(buffers[999], 0, &last, 8192), Op47::Skip);
        assert_eq!(guard.op47(buffers[0], 0, &[2u8; 4096], 8192), Op47::Record(1));

        // One buffer with many ranges: its own cap decides, the newest stay,
        // and it cannot push another buffer's record out.
        let mut guard = UploadDedup::new(true, Caps::DEFAULT);
        let (hot, quiet) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        assert!(op47(&mut guard, quiet, 0, &[9; 4]));
        for index in 0..1000u64 {
            assert_eq!(guard.op47(hot, index * 4, &[index as u8; 4], 8192), Op47::Record(1));
            guard.store(1, index * 4, vec![index as u8; 4]);
            assert!(ranges(&guard, hot).len() <= Caps::DEFAULT.buffer_records);
        }
        assert_eq!(ranges(&guard, hot), (936..1000u64).map(|index| (index * 4, 4usize)).collect::<Vec<_>>());
        assert_eq!((guard.totals().evictions, guard.stats.evictions_buffer_cap, guard.stats.buffer_records_peak), (936, 936, 64));
        assert!(!op47(&mut guard, quiet, 0, &[9; 4]));
        for index in 936..1000u64 { assert_eq!(guard.op47(hot, index * 4, &[index as u8; 4], 8192), Op47::Skip); }
        assert_eq!(guard.op47(hot, 0, &[0; 4], 8192), Op47::Record(1));
    }

    #[test]
    fn interval_reset_keeps_records_and_metrics_report_every_counter() {
        let mut guard = guard();
        let mut a = vertex_buffer(&mut guard);
        let b = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, a, 0, &[1; 16]));
        assert!(!op47(&mut guard, a, 0, &[1; 16]));
        assert!(op47(&mut guard, a, 0, &[2; 16]));
        assert!(op47(&mut guard, a, 32, &[5; 8]));
        assert!(op47(&mut guard, b, 4, &[3; 8]));
        assert!(op47(&mut guard, a, 8, &[9; 32]));
        assert!(op47(&mut guard, b, 0, &[3; 6]));
        guard.op12(b);
        assert!(op47(&mut guard, Track::Untracked, 0, &[1; 16]));
        guard.gpu_write(&mut a, GpuWrite::ClearUav);
        assert!(op47(&mut guard, a, 0, &[2; 16]));

        let metrics = guard.metrics(10.0);
        assert_eq!(metrics["schema"], 2);
        assert_eq!(metrics["enabled"], true);
        assert_eq!(metrics["environment"], "MUGUET_OP47_UPLOAD_DEDUP");
        let top = |name: &str| metrics[name].as_u64().unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!((top("cap_records"), top("cap_buffer_records"), top("cap_record_bytes"), top("cap_total_bytes")), (128, 64, 4096, 524288));
        assert_eq!((top("records"), top("record_bytes"), top("record_buffers"), top("largest_buffer_records")), (0, 0, 0, 0));
        let interval = &metrics["stats_interval"];
        let count = |name: &str| interval[name].as_u64().unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!((count("op47_calls"), count("op12_calls")), (9, 1));
        assert_eq!((count("checked"), count("hits"), count("misses"), count("bypasses")), (6, 1, 5, 3));
        assert_eq!(count("op47_calls"), count("checked") + count("bypasses"));
        assert_eq!((count("saved_calls"), count("saved_bytes")), (1, 16));
        for name in ["check_ns", "check_max_ns", "store_ns", "store_max_ns"] { count(name); }
        assert_eq!(interval["miss_reasons"], serde_json::json!({"no_record": 3, "range": 1, "bytes": 1}));
        assert_eq!(interval["bypass_reasons"], serde_json::json!({"untracked_buffer": 1, "disqualified_buffer": 1,
            "empty": 0, "unaligned_or_padded": 1, "out_of_bounds": 0, "over_record_cap": 0}));
        assert_eq!(interval["invalidations"], serde_json::json!({"op12": 0, "unrecorded_op47": 1, "gpu_write": 1, "destroyed_or_replaced": 0}));
        assert_eq!(interval["records_dropped"], serde_json::json!({"changed_op47": 3, "op12": 0, "unrecorded_op47": 1,
            "unrecorded_op47_by_shape": {"empty": 0, "unaligned_or_padded": 1, "out_of_bounds": 0, "over_record_cap": 0},
            "gpu_write": 1, "destroyed_or_replaced": 0, "evicted": 0, "recency_reset": 0}));
        assert_eq!(interval["disqualified"], serde_json::json!({"copy_region": 0, "clear_uav": 1, "copy_counter": 0, "storage_binding": 0}));
        assert_eq!(interval["table"], serde_json::json!({"stored": 5, "store_rejected": 0, "evictions": 0, "evictions_buffer_cap": 0,
            "records_peak": 3, "record_bytes_peak": 40, "buffer_records_peak": 2, "tracked_buffers": 2, "lifetime_exhausted": 0}));

        // A new interval starts from zero but keeps what is recorded.
        let c = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, c, 0, &[4; 16]));
        assert!(op47(&mut guard, c, 32, &[4; 16]));
        guard.reset_interval();
        assert_eq!(guard.totals(), Totals { records: 2, record_bytes: 32, ..Default::default() });
        assert_eq!((guard.stats.records_peak, guard.stats.record_bytes_peak, guard.stats.buffer_records_peak, guard.stats.check_ns), (2, 32, 2, 0));
        let metrics = guard.metrics(10.0);
        assert_eq!((metrics["records"].as_u64(), metrics["record_buffers"].as_u64(), metrics["largest_buffer_records"].as_u64()), (Some(2), Some(1), Some(2)));
        assert!(!op47(&mut guard, c, 0, &[4; 16]), "the records survive an interval reset");
        assert!(!op47(&mut guard, c, 32, &[4; 16]));
    }

    // ---- Ordering model -------------------------------------------------
    //
    // The host's ordering, reduced to bytes: uploads go to a queue prelude,
    // GPU-side writes go to the encoder, and a submission runs the whole
    // prelude before the whole encoder. Two models replay one operation
    // stream, guard off and guard on, and must agree on every buffer byte.

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Hook {
        /// What the host does: disqualify for life before the GPU write.
        Sticky,
        /// Drop the records at the GPU write but keep tracking afterwards.
        InvalidateOnly,
        /// No hook at all.
        Missing,
    }

    struct Live { serial: u64, track: Track, bytes: Vec<u8>, used: bool }
    struct Write { serial: u64, offset: usize, data: Vec<u8> }

    struct Model {
        guard: UploadDedup,
        hook: Hook,
        live: Vec<Option<Live>>,
        next_serial: u64,
        prelude: Vec<Write>,
        encoder: Vec<Write>,
        queue_writes: u64,
    }

    impl Model {
        fn new(guard: UploadDedup, hook: Hook, ids: usize) -> Self {
            Self { guard, hook, live: (0..ids).map(|_| None).collect(), next_serial: 0,
                prelude: Vec::new(), encoder: Vec::new(), queue_writes: 0 }
        }

        fn create(&mut self, id: usize, size: usize, bind: u32) {
            // set_obj: a replaced buffer's records are forgotten with it.
            if let Some(old) = self.live[id].take() { self.guard.forget(old.track); }
            let track = self.guard.track_new(bind, 0, true, bind & BIND_CONSTANT != 0);
            self.next_serial += 1;
            self.live[id] = Some(Live { serial: self.next_serial, track, bytes: vec![0; size], used: false });
        }

        fn destroy(&mut self, id: usize) {
            if self.live[id].as_ref().is_some_and(|buffer| buffer.used) { self.flush(); }
            if let Some(old) = self.live[id].take() { self.guard.forget(old.track); }
        }

        // The legacy route: the 4-aligned body, then a zero-padded tail word.
        // Like the real queue, an unaligned or out-of-range write is rejected.
        fn queue(&mut self, serial: u64, gpu_bytes: usize, offset: usize, data: &[u8]) {
            let body = data.len() & !3;
            let mut parts: Vec<(usize, Vec<u8>)> = Vec::new();
            if body > 0 { parts.push((offset, data[..body].to_vec())); }
            if data.len() & 3 != 0 {
                let mut tail = [0u8; 4];
                tail[..data.len() - body].copy_from_slice(&data[body..]);
                parts.push((offset + body, tail.to_vec()));
            }
            for (offset, data) in parts {
                self.queue_writes += 1;
                if offset % 4 == 0 && offset + data.len() <= gpu_bytes { self.prelude.push(Write { serial, offset, data }); }
            }
        }

        fn upload(&mut self, id: usize, offset: usize, data: &[u8], no_overwrite: bool) {
            let Some(buffer) = self.live[id].as_ref() else { return };
            let (serial, track, gpu_bytes, used) = (buffer.serial, buffer.track, buffer.bytes.len(), buffer.used);
            if !no_overwrite {
                self.guard.op12(track);
                if used { self.flush(); }
                self.queue(serial, gpu_bytes, offset, data);
                return;
            }
            match self.guard.op47(track, offset as u64, data, gpu_bytes as u64) {
                Op47::Skip => {}
                Op47::Legacy => self.queue(serial, gpu_bytes, offset, data),
                Op47::Record(lifetime) => {
                    self.queue(serial, gpu_bytes, offset, data);
                    self.guard.store(lifetime, offset as u64, data.to_vec());
                }
            }
        }

        // copy_region / clear_uav / copy_counter: recorded in the encoder now,
        // executed after every prelude upload of the same submission.
        fn gpu_write(&mut self, id: usize, offset: usize, data: &[u8], kind: GpuWrite) {
            let Some(buffer) = self.live[id].as_mut() else { return };
            match self.hook {
                Hook::Sticky => self.guard.gpu_write(&mut buffer.track, kind),
                Hook::InvalidateOnly => self.guard.forget(buffer.track),
                Hook::Missing => {}
            }
            buffer.used = true;
            self.encoder.push(Write { serial: buffer.serial, offset, data: data.to_vec() });
        }

        fn flush(&mut self) {
            let writes: Vec<Write> = self.prelude.drain(..).chain(self.encoder.drain(..)).collect();
            for write in writes {
                // A write to an object destroyed meanwhile has nothing to change.
                let target = self.live.iter_mut().flatten().find(|buffer| buffer.serial == write.serial);
                if let Some(buffer) = target { buffer.bytes[write.offset..write.offset + write.data.len()].copy_from_slice(&write.data); }
            }
            for buffer in self.live.iter_mut().flatten() { buffer.used = false; }
        }

        fn image(&self) -> Vec<Option<Vec<u8>>> {
            self.live.iter().map(|buffer| buffer.as_ref().map(|buffer| buffer.bytes.clone())).collect()
        }
    }

    struct Rng(u64);
    impl Rng {
        fn draw(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, bound: u64) -> u64 { self.draw() % bound }
        fn pick<T: Copy>(&mut self, from: &[T]) -> T { from[self.below(from.len() as u64) as usize] }
    }

    #[test]
    fn skipping_never_changes_buffer_bytes_in_a_submission_model() {
        const IDS: usize = 6;
        const SIZE: usize = 64;
        // Small caps so both evictions and the per-record cap are exercised too.
        let caps = Caps { records: 6, buffer_records: 3, record_bytes: 32, total_bytes: 96 };
        let (mut hits, mut evictions, mut disqualified, mut shape_bypasses, mut dropped_changed) = (0, 0, 0, 0, 0);
        for seed in 1..=48u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut legacy = Model::new(UploadDedup::new(false, caps), Hook::Sticky, IDS);
            let mut guarded = Model::new(UploadDedup::new(true, caps), Hook::Sticky, IDS);
            // The last few distinct op47 uploads of each buffer, for replays.
            let mut recent: Vec<Vec<(usize, Vec<u8>)>> = vec![Vec::new(); IDS];
            for model in [&mut legacy, &mut guarded] {
                for id in 0..IDS { model.create(id, SIZE, BIND_VERTEX); }
                // Two disjoint ranges repeated once each, so every seed has
                // skips on a buffer that holds more than one record.
                model.upload(0, 0, &[0x5a; 16], true);
                model.upload(0, 32, &[0x6b; 16], true);
                model.upload(0, 0, &[0x5a; 16], true);
                model.upload(0, 32, &[0x6b; 16], true);
            }
            for step in 0..3000 {
                let id = rng.below(IDS as u64) as usize;
                let roll = rng.below(100);
                let mut flushed = false;
                if roll < 60 {
                    // Most op47 calls replay one of this buffer's last few
                    // uploads, so alternating ranges and exact repeats are common.
                    let replay = !recent[id].is_empty() && rng.below(5) < 3;
                    let (offset, data) = if replay {
                        let which = rng.below(recent[id].len() as u64) as usize;
                        recent[id][which].clone()
                    } else {
                        let offset = rng.pick(&[0usize, 4, 8, 12, 16, 32, 48, 2, 60]);
                        let length = rng.pick(&[4usize, 8, 8, 16, 16, 32, 6, 0, 40, 80]);
                        (offset, vec![rng.pick(&[0x11u8, 0x22, 0x33]); length])
                    };
                    for model in [&mut legacy, &mut guarded] { model.upload(id, offset, &data, true); }
                    if !replay {
                        recent[id].push((offset, data));
                        if recent[id].len() > 4 { recent[id].remove(0); }
                    }
                } else if roll < 68 {
                    let offset = rng.pick(&[0usize, 8, 32]);
                    let data = vec![rng.pick(&[0x11u8, 0x44]); rng.pick(&[8usize, 16, 32])];
                    for model in [&mut legacy, &mut guarded] { model.upload(id, offset, &data, false); }
                } else if roll < 73 {
                    let kind = rng.pick(&[GpuWrite::CopyRegion, GpuWrite::ClearUav, GpuWrite::CopyCounter]);
                    let offset = rng.pick(&[0usize, 8, 32]);
                    let data = vec![rng.pick(&[0u8, 0x77]); rng.pick(&[4usize, 16, 32])];
                    for model in [&mut legacy, &mut guarded] { model.gpu_write(id, offset, &data, kind); }
                } else if roll < 79 {
                    // Mostly eligible kinds; storage and shadowed ones stay untracked.
                    let bind = rng.pick(&[BIND_VERTEX, BIND_VERTEX, BIND_INDEX, BIND_VERTEX | BIND_UNORDERED_ACCESS, BIND_CONSTANT | BIND_VERTEX]);
                    for model in [&mut legacy, &mut guarded] { model.create(id, SIZE, bind); }
                    recent[id].clear();
                } else if roll < 82 {
                    for model in [&mut legacy, &mut guarded] { model.destroy(id); }
                    recent[id].clear();
                } else if roll < 92 {
                    for model in [&mut legacy, &mut guarded] { model.flush(); }
                    flushed = true;
                } else if legacy.live[id].is_none() {
                    // A fresh tracked buffer in a free slot keeps the mix lively.
                    for model in [&mut legacy, &mut guarded] { model.create(id, SIZE, BIND_VERTEX); }
                }
                if flushed { assert_eq!(legacy.image(), guarded.image(), "seed {seed}, step {step}"); }
                assert_consistent(&guarded.guard);
            }
            legacy.flush();
            guarded.flush();
            assert_eq!(legacy.image(), guarded.image(), "seed {seed}, final image");
            let totals = guarded.guard.totals();
            assert_eq!(legacy.guard.totals(), Totals::default(), "seed {seed}: the disabled guard stayed inert");
            assert_eq!(totals.checked, totals.hits + totals.misses, "seed {seed}");
            assert_eq!(guarded.guard.stats.op47_calls, totals.checked + totals.bypasses, "seed {seed}");
            // Every skip is one queue write the legacy model made and this one did not.
            assert_eq!(legacy.queue_writes, guarded.queue_writes + totals.saved_calls, "seed {seed}");
            assert!(totals.hits >= 2 && guarded.guard.stats.buffer_records_peak >= 2, "seed {seed}");
            hits += totals.hits;
            evictions += totals.evictions;
            disqualified += totals.disqualified;
            shape_bypasses += guarded.guard.stats.bypass_shape.iter().sum::<u64>();
            dropped_changed += totals.dropped_changed;
        }
        assert!(hits > 0 && evictions > 0 && disqualified > 0 && shape_bypasses > 0 && dropped_changed > 0,
            "the stream must exercise skips, eviction, GPU writes, legacy shapes and overlaps: {hits} {evictions} {disqualified} {shape_bypasses} {dropped_changed}");
    }

    /// Runs one fixed stream on a legacy model and on a guarded one with the
    /// given hook, and reports (legacy bytes, guarded bytes, guarded hits).
    fn replay(hook: Hook, stream: fn(&mut Model)) -> (Vec<u8>, Vec<u8>, u64) {
        let mut legacy = Model::new(UploadDedup::new(false, Caps::DEFAULT), Hook::Sticky, 1);
        let mut guarded = Model::new(guard(), hook, 1);
        for model in [&mut legacy, &mut guarded] { stream(model); model.flush(); }
        let bytes = |model: &Model| model.live[0].as_ref().map(|buffer| buffer.bytes.clone()).unwrap();
        assert_consistent(&guarded.guard);
        (bytes(&legacy), bytes(&guarded), guarded.guard.totals().hits)
    }

    #[test]
    fn a_gpu_write_after_the_records_must_drop_them_all() {
        fn stream(model: &mut Model) {
            model.create(0, 64, BIND_VERTEX);
            model.upload(0, 0, &[0xaa; 8], true);
            model.upload(0, 32, &[0xab; 8], true);
            // The copy lands in the second range only.
            model.gpu_write(0, 32, &[0xbb; 8], GpuWrite::CopyRegion);
            model.flush();
            // Both ranges are forgotten, so both of these upload again.
            model.upload(0, 0, &[0xaa; 8], true);
            model.upload(0, 32, &[0xab; 8], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!((&legacy[..8], &legacy[32..40]), (&[0xaa; 8][..], &[0xab; 8][..]));
        assert_eq!((guarded, hits), (legacy.clone(), 0));
        // The model does notice a host that forgets the hook.
        let (_, broken, hits) = replay(Hook::Missing, stream);
        assert_eq!((&broken[32..40], hits), (&[0xbb; 8][..], 2));
        assert_ne!(broken, legacy);
    }

    #[test]
    fn a_gpu_write_recorded_before_an_op47_of_its_submission_needs_the_sticky_rule() {
        fn stream(model: &mut Model) {
            model.create(0, 64, BIND_VERTEX);
            // Recorded first, executed last: the prelude uploads below run
            // BEFORE this copy, so the submission ends with the copy's bytes.
            model.gpu_write(0, 0, &[0xbb; 8], GpuWrite::CopyRegion);
            model.upload(0, 0, &[0xaa; 8], true);
            model.upload(0, 32, &[0xab; 8], true);
            model.flush();
            model.upload(0, 0, &[0xaa; 8], true);
            model.upload(0, 32, &[0xab; 8], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!((&legacy[..8], &legacy[32..40]), (&[0xaa; 8][..], &[0xab; 8][..]));
        assert_eq!((guarded, hits), (legacy.clone(), 0));
        // Dropping the records at the copy is not enough: the op47 after it
        // would be recorded as authoritative and its next repeat skipped.
        let (_, trapped, hits) = replay(Hook::InvalidateOnly, stream);
        assert_eq!((&trapped[..8], hits), (&[0xbb; 8][..], 2));
        assert_ne!(trapped, legacy);
    }

    #[test]
    fn a_changed_write_across_two_ranges_is_followed_by_real_uploads() {
        fn stream(model: &mut Model) {
            model.create(0, 64, BIND_VERTEX);
            model.upload(0, 0, &[0xa1; 16], true);
            model.upload(0, 32, &[0xb2; 16], true);
            model.flush();
            // Two skips, in the alternating order a single record never caught.
            model.upload(0, 0, &[0xa1; 16], true);
            model.upload(0, 32, &[0xb2; 16], true);
            // [8, 40) reaches into both ranges, in the same submission.
            model.upload(0, 8, &[0xc3; 32], true);
            model.upload(0, 0, &[0xa1; 16], true);
            model.flush();
            model.upload(0, 32, &[0xb2; 16], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!((&legacy[..16], &legacy[16..32], &legacy[32..48]), (&[0xa1; 16][..], &[0xc3; 16][..], &[0xb2; 16][..]));
        assert_eq!((guarded, hits), (legacy, 2));
    }

    #[test]
    fn an_earlier_consumer_sees_the_same_bytes_whether_or_not_repeats_are_skipped() {
        // Consumers are read-only, so the model needs no extra operation: a
        // consumer recorded before a no-overwrite upload reads the bytes the
        // prelude leaves, which skipped exact repeats do not change.
        fn stream(model: &mut Model) {
            model.create(0, 64, BIND_VERTEX);
            model.upload(0, 4, &[0xc1; 8], true);
            model.upload(0, 32, &[0xd2; 8], true);
            model.flush();
            for _ in 0..2 {
                model.upload(0, 4, &[0xc1; 8], true);
                model.upload(0, 32, &[0xd2; 8], true);
            }
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!((&legacy[4..12], &legacy[32..40]), (&[0xc1; 8][..], &[0xd2; 8][..]));
        assert_eq!((guarded, hits), (legacy, 4));
    }
}

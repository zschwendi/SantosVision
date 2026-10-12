//! Redundant op47 upload guard. Default OFF; `MUGUET_OP47_UPLOAD_DEDUP=1`.
//!
//! Ordinary GPU vertex/index buffers have no CPU mirror, so "would this upload
//! change anything?" has no general answer. This keeps one bounded record per
//! eligible buffer object: the exact range and bytes of its last op47
//! (no-overwrite) upload. A later op47 on the same object lifetime with the
//! same offset, the same length and the same bytes is redundant, and only its
//! queue write is skipped. Every other call takes the legacy route unchanged.
//!
//! A record is trusted only while nothing else can have changed that range:
//! every op12 drops it, an op47 the guard does not record drops it, and a
//! GPU-side write (copy/clear/counter destination, writable storage binding)
//! takes the object out of the guard for the rest of its lifetime. That last
//! rule is deliberately sticky. Uploads run in a queue prelude ahead of every
//! encoder command of their submission, so a copy recorded BEFORE a later op47
//! still lands AFTER it; recording that op47 would call bytes authoritative
//! that the copy is about to replace.
//!
//! Pure CPU bookkeeping: no waits, submits, readbacks or GPU objects.
use std::collections::HashMap;
use std::time::Instant;

pub(crate) const ENVIRONMENT: &str = "MUGUET_OP47_UPLOAD_DEDUP";
pub(crate) const MAX_RECORDS: usize = 128;
pub(crate) const MAX_RECORD_BYTES: usize = 4096;
pub(crate) const MAX_TOTAL_BYTES: usize = 512 * 1024;

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
    /// Exact repeat of the recorded upload: its queue write is redundant.
    Skip,
    /// Legacy route, then `store` the uploaded bytes under this identity.
    Record(u64),
}

struct Record { offset: u64, bytes: Box<[u8]>, touched: u64 }

#[derive(Default)]
struct Interval {
    op12_calls: u64, op47_calls: u64, checked: u64, hits: u64,
    miss_no_record: u64, miss_range: u64, miss_bytes: u64,
    bypass_untracked: u64, bypass_disqualified: u64, bypass_shape: [u64; 4],
    saved_calls: u64, saved_bytes: u64,
    stored: u64, store_rejected: u64, evictions: u64,
    invalidated_op12: u64, invalidated_unrecorded_op47: u64, invalidated_gpu_write: u64, forgotten: u64,
    disqualified: [u64; 4], tracked_buffers: u64, lifetime_exhausted: u64,
    check_ns: u64, check_max_ns: u64, store_ns: u64, store_max_ns: u64,
    records_peak: u64, record_bytes_peak: u64,
}

impl Interval {
    fn misses(&self) -> u64 { self.miss_no_record + self.miss_range + self.miss_bytes }
    fn bypasses(&self) -> u64 { self.bypass_untracked + self.bypass_disqualified + self.bypass_shape.iter().sum::<u64>() }
}

/// Counters a fixture can compare directly.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub(crate) checked: u64, pub(crate) hits: u64, pub(crate) misses: u64, pub(crate) bypasses: u64,
    pub(crate) saved_calls: u64, pub(crate) saved_bytes: u64, pub(crate) evictions: u64,
    pub(crate) disqualified: u64, pub(crate) records: usize, pub(crate) record_bytes: usize,
}

pub(crate) struct UploadDedup {
    enabled: bool,
    max_records: usize,
    max_record_bytes: usize,
    max_total_bytes: usize,
    records: HashMap<u64, Record>,
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
// Empty, unaligned, padded-tail, out-of-range and larger calls keep their
// legacy handling byte for byte.
fn bypass_shape(offset: u64, size: u64, gpu_bytes: u64, max_record_bytes: usize) -> Option<usize> {
    if size == 0 { Some(SHAPE_EMPTY) }
    else if !offset.is_multiple_of(4) || !size.is_multiple_of(4) { Some(SHAPE_UNALIGNED) }
    else if offset.checked_add(size).is_none_or(|end| end > gpu_bytes) { Some(SHAPE_OUT_OF_BOUNDS) }
    else if size > max_record_bytes as u64 { Some(SHAPE_OVERSIZE) }
    else { None }
}

#[inline]
fn bump(counter: &mut u64) { *counter = counter.saturating_add(1); }

fn elapsed_ns(started: Instant) -> u64 { started.elapsed().as_nanos().min(u64::MAX as u128) as u64 }

impl UploadDedup {
    pub(crate) fn from_environment() -> Self {
        let value = std::env::var(ENVIRONMENT).ok();
        let enabled = parse(value.as_deref()).unwrap_or_else(|_| {
            eprintln!("[gpu] invalid {ENVIRONMENT}; expected 0 or 1, leaving the op47 upload guard off");
            false
        });
        Self::new(enabled, MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES)
    }

    pub(crate) fn new(enabled: bool, max_records: usize, max_record_bytes: usize, max_total_bytes: usize) -> Self {
        Self { enabled, max_records, max_record_bytes, max_total_bytes, records: HashMap::new(),
            total_bytes: 0, next_lifetime: 1, clock: 0, stats: Interval::default() }
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

    /// Every op12 drops the buffer's record before it flushes or writes, even
    /// when its bytes would compare equal or it is empty.
    pub(crate) fn op12(&mut self, track: Track) {
        if !self.enabled { return; }
        bump(&mut self.stats.op12_calls);
        if let Track::Lifetime(lifetime) = track {
            if self.remove(lifetime) { bump(&mut self.stats.invalidated_op12); }
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
        self.stats.check_ns = self.stats.check_ns.saturating_add(ns);
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
        if let Some(shape) = bypass_shape(offset, size, gpu_bytes, self.max_record_bytes) {
            bump(&mut self.stats.bypass_shape[shape]);
            // That legacy write may still change bytes inside the recorded range.
            if self.remove(lifetime) { bump(&mut self.stats.invalidated_unrecorded_op47); }
            return Op47::Legacy;
        }
        bump(&mut self.stats.checked);
        let (present, same_range, same_bytes) = match self.records.get(&lifetime) {
            None => (false, false, false),
            Some(record) => {
                let same_range = record.offset == offset && record.bytes.len() == data.len();
                (true, same_range, same_range && &record.bytes[..] == data)
            }
        };
        if same_bytes {
            let now = self.tick();
            if let Some(record) = self.records.get_mut(&lifetime) { record.touched = now; }
            bump(&mut self.stats.hits);
            bump(&mut self.stats.saved_calls);
            self.stats.saved_bytes = self.stats.saved_bytes.saturating_add(size);
            return Op47::Skip;
        }
        bump(if !present { &mut self.stats.miss_no_record } else if !same_range { &mut self.stats.miss_range } else { &mut self.stats.miss_bytes });
        // A changed upload: the old record goes before the write happens; the
        // new one arrives through `store` only after the legacy route has run.
        self.remove(lifetime);
        Op47::Record(lifetime)
    }

    /// Records the bytes an op47 just sent down the legacy route. Called only
    /// for `Op47::Record`, after routing, so a record never precedes its write.
    pub(crate) fn store(&mut self, lifetime: u64, offset: u64, data: Vec<u8>) {
        if !self.enabled { return; }
        let started = Instant::now();
        self.insert(lifetime, offset, data);
        let ns = elapsed_ns(started);
        self.stats.store_ns = self.stats.store_ns.saturating_add(ns);
        self.stats.store_max_ns = self.stats.store_max_ns.max(ns);
    }

    fn insert(&mut self, lifetime: u64, offset: u64, data: Vec<u8>) {
        self.remove(lifetime);
        let size = data.len();
        if size == 0 || size > self.max_record_bytes || size > self.max_total_bytes || self.max_records == 0 {
            bump(&mut self.stats.store_rejected);
            return;
        }
        // Make room under BOTH caps, least recently used first. Eviction only
        // forgets: the evicted buffer's next op47 is an ordinary miss.
        while self.records.len() >= self.max_records || self.total_bytes.saturating_add(size) > self.max_total_bytes {
            let Some(oldest) = self.records.iter().min_by_key(|(_, record)| record.touched).map(|(key, _)| *key) else { break };
            self.remove(oldest);
            bump(&mut self.stats.evictions);
        }
        let touched = self.tick();
        self.total_bytes += size;
        self.records.insert(lifetime, Record { offset, bytes: data.into_boxed_slice(), touched });
        bump(&mut self.stats.stored);
        self.stats.records_peak = self.stats.records_peak.max(self.records.len() as u64);
        self.stats.record_bytes_peak = self.stats.record_bytes_peak.max(self.total_bytes as u64);
    }

    /// A GPU-side write is about to target this buffer. Called BEFORE that
    /// write is recorded: the record goes and the object never comes back.
    pub(crate) fn gpu_write(&mut self, track: &mut Track, kind: GpuWrite) {
        if !self.enabled { return; }
        let Track::Lifetime(lifetime) = *track else { return };
        if self.remove(lifetime) { bump(&mut self.stats.invalidated_gpu_write); }
        *track = Track::Disqualified;
        bump(&mut self.stats.disqualified[kind.index()]);
    }

    /// The object was destroyed or replaced; its record ends with it.
    pub(crate) fn forget(&mut self, track: Track) {
        if let Track::Lifetime(lifetime) = track {
            if self.remove(lifetime) { bump(&mut self.stats.forgotten); }
        }
    }

    fn remove(&mut self, lifetime: u64) -> bool {
        let Some(record) = self.records.remove(&lifetime) else { return false };
        self.total_bytes -= record.bytes.len();
        true
    }

    // Recency for eviction only. Exhausting the counter empties the table:
    // that can cost hits, never correctness.
    fn tick(&mut self) -> u64 {
        match self.clock.checked_add(1) {
            Some(next) => self.clock = next,
            None => { self.records.clear(); self.total_bytes = 0; self.clock = 1; }
        }
        self.clock
    }

    pub(crate) fn metrics(&self, interval_s: f64) -> serde_json::Value {
        let s = &self.stats;
        // Built in pieces: one large literal would strain json!'s recursion.
        let misses = serde_json::json!({"no_record": s.miss_no_record, "range": s.miss_range, "bytes": s.miss_bytes});
        let bypasses = serde_json::json!({"untracked_buffer": s.bypass_untracked, "disqualified_buffer": s.bypass_disqualified,
            "empty": s.bypass_shape[SHAPE_EMPTY], "unaligned_or_padded": s.bypass_shape[SHAPE_UNALIGNED],
            "out_of_bounds": s.bypass_shape[SHAPE_OUT_OF_BOUNDS], "over_record_cap": s.bypass_shape[SHAPE_OVERSIZE]});
        let invalidations = serde_json::json!({"op12": s.invalidated_op12, "unrecorded_op47": s.invalidated_unrecorded_op47,
            "gpu_write": s.invalidated_gpu_write, "destroyed_or_replaced": s.forgotten});
        let disqualified = serde_json::json!({"copy_region": s.disqualified[0], "clear_uav": s.disqualified[1],
            "copy_counter": s.disqualified[2], "storage_binding": s.disqualified[3]});
        let table = serde_json::json!({"stored": s.stored, "store_rejected": s.store_rejected, "evictions": s.evictions,
            "records_peak": s.records_peak, "record_bytes_peak": s.record_bytes_peak,
            "tracked_buffers": s.tracked_buffers, "lifetime_exhausted": s.lifetime_exhausted});
        let interval = serde_json::json!({"op47_calls": s.op47_calls, "op12_calls": s.op12_calls,
            "checked": s.checked, "hits": s.hits, "misses": s.misses(), "bypasses": s.bypasses(),
            "saved_calls": s.saved_calls, "saved_bytes": s.saved_bytes,
            "check_ns": s.check_ns, "check_max_ns": s.check_max_ns, "store_ns": s.store_ns, "store_max_ns": s.store_max_ns,
            "miss_reasons": misses, "bypass_reasons": bypasses, "invalidations": invalidations,
            "disqualified": disqualified, "table": table});
        serde_json::json!({"schema": 1, "enabled": self.enabled, "environment": ENVIRONMENT, "interval_s": interval_s,
            "cap_records": self.max_records, "cap_record_bytes": self.max_record_bytes, "cap_total_bytes": self.max_total_bytes,
            "records": self.records.len(), "record_bytes": self.total_bytes, "stats_interval": interval,
            "counting_scope": "op47_calls = checked + bypasses and checked = hits + misses, for op47 on GPU-backed buffers only. A hit skips exactly one queue write; saved_bytes are the bytes of those writes. Hits are absent from the op47 line of buffer upload timing.",
            "timing_scope": "CPU only, not GPU time: check_ns is the classification and byte comparison of every op47 call, store_ns is record replacement and eviction after a miss. The guest snapshot and the upload route are outside both."})
    }

    pub(crate) fn reset_interval(&mut self) {
        self.stats = Interval { records_peak: self.records.len() as u64, record_bytes_peak: self.total_bytes as u64, ..Default::default() };
    }

    #[cfg(test)]
    pub(crate) fn totals(&self) -> Totals {
        let s = &self.stats;
        Totals { checked: s.checked, hits: s.hits, misses: s.misses(), bypasses: s.bypasses(),
            saved_calls: s.saved_calls, saved_bytes: s.saved_bytes, evictions: s.evictions,
            disqualified: s.disqualified.iter().sum(), records: self.records.len(), record_bytes: self.total_bytes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPU_BYTES: u64 = 64;

    fn guard() -> UploadDedup { UploadDedup::new(true, MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES) }

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

    #[test]
    fn only_the_exact_value_one_enables_the_guard() {
        assert_eq!(ENVIRONMENT, "MUGUET_OP47_UPLOAD_DEDUP");
        assert_eq!(parse(None), Ok(false));
        assert_eq!(parse(Some("0")), Ok(false));
        assert_eq!(parse(Some("1")), Ok(true));
        for value in ["", "true", "on", "2", " 1", "1 ", "01"] { assert_eq!(parse(Some(value)), Err(())); }
        assert_eq!((MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES), (128, 4096, 524288));

        // Off: nothing is tracked, compared, recorded or counted.
        let mut off = UploadDedup::new(false, MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES);
        assert!(!off.enabled());
        let mut track = off.track_new(BIND_VERTEX, 0, true, false);
        assert_eq!(track, Track::Untracked);
        for _ in 0..3 { assert_eq!(off.op47(track, 0, &[7; 16], GPU_BYTES), Op47::Legacy); }
        off.store(1, 0, vec![7; 16]);
        assert_eq!(off.op47(Track::Lifetime(1), 0, &[7; 16], GPU_BYTES), Op47::Legacy);
        off.op12(track);
        off.gpu_write(&mut track, GpuWrite::CopyRegion);
        assert_eq!(track, Track::Untracked);
        assert_eq!(off.totals(), Totals::default());
        assert_eq!(off.metrics(0.0)["enabled"], false);
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
    fn exact_repeat_is_skipped_and_any_difference_is_uploaded() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        let x: Vec<u8> = (1..=16).collect();
        assert!(op47(&mut guard, a, 8, &x), "nothing is known about a first upload");
        assert!(!op47(&mut guard, a, 8, &x));
        assert!(!op47(&mut guard, a, 8, &x));
        // Another buffer's uploads neither hit nor disturb this record.
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
        // Same bytes at another offset, then a prefix and a longer run of them.
        assert!(op47(&mut guard, a, 12, &x));
        assert!(op47(&mut guard, a, 12, &x[..12]));
        assert!(!op47(&mut guard, a, 12, &x[..12]));
        let longer: Vec<u8> = x.iter().copied().chain([17, 18, 19, 20]).collect();
        assert!(op47(&mut guard, a, 12, &longer));
        // The one record per buffer now describes offset 12 only.
        assert!(op47(&mut guard, a, 8, &x));

        let totals = guard.totals();
        assert_eq!(totals.checked, totals.hits + totals.misses);
        assert_eq!((totals.checked, totals.hits, totals.misses, totals.bypasses), (19, 9, 10, 0));
        assert_eq!((totals.saved_calls, totals.saved_bytes), (9, 8 * 16 + 12));
        assert_eq!((totals.records, totals.record_bytes), (2, 32));
        let stats = &guard.stats;
        assert_eq!((stats.miss_no_record, stats.miss_range, stats.miss_bytes), (2, 4, 4));
        assert_eq!(stats.op47_calls, 19);
    }

    #[test]
    fn every_op12_drops_the_record_even_when_nothing_changed() {
        let mut guard = guard();
        let (a, b) = (vertex_buffer(&mut guard), vertex_buffer(&mut guard));
        assert!(op47(&mut guard, a, 0, &[3; 16]));
        assert!(op47(&mut guard, b, 0, &[4; 16]));
        // The guard is not told what an op12 wrote: identical, empty or not.
        guard.op12(a);
        assert_eq!(guard.totals().records, 1);
        assert!(op47(&mut guard, a, 0, &[3; 16]), "an op12 came between the repeats");
        assert!(!op47(&mut guard, a, 0, &[3; 16]));
        assert!(!op47(&mut guard, b, 0, &[4; 16]), "an op12 on one buffer leaves the others alone");
        // Nothing to drop is fine, and untracked buffers are only counted.
        guard.op12(a);
        guard.op12(a);
        guard.op12(Track::Untracked);
        guard.op12(Track::Disqualified);
        assert_eq!((guard.stats.op12_calls, guard.stats.invalidated_op12), (5, 2));
        assert!(op47(&mut guard, a, 0, &[3; 16]));
    }

    #[test]
    fn uncompared_op47_shapes_stay_legacy_and_drop_the_record() {
        let big = vec![9u8; MAX_RECORD_BYTES + 4];
        // (offset, data, gpu size, counter): each keeps the legacy route.
        let cases: [(u64, &[u8], u64, usize); 8] = [
            (0, &[], GPU_BYTES, SHAPE_EMPTY),
            (2, &[1; 8], GPU_BYTES, SHAPE_UNALIGNED),
            (0, &[1; 6], GPU_BYTES, SHAPE_UNALIGNED),
            (4, &[1; 3], GPU_BYTES, SHAPE_UNALIGNED),
            (60, &[1; 8], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (64, &[1; 4], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (u64::MAX - 3, &[1; 4], GPU_BYTES, SHAPE_OUT_OF_BOUNDS),
            (0, &big, 8192, SHAPE_OVERSIZE),
        ];
        for (index, (offset, data, gpu_bytes, shape)) in cases.into_iter().enumerate() {
            let mut guard = guard();
            let track = vertex_buffer(&mut guard);
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index}");
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index} repeated");
            assert_eq!(guard.stats.bypass_shape[shape], 2, "case {index}");
            assert_eq!(guard.totals(), Totals { bypasses: 2, ..Default::default() }, "case {index}");
            // Such a write may overlap a recorded range, so the record goes.
            assert!(op47(&mut guard, track, 0, &[6; 8]));
            assert!(!op47(&mut guard, track, 0, &[6; 8]));
            assert_eq!(guard.op47(track, offset, data, gpu_bytes), Op47::Legacy, "case {index}");
            assert_eq!(guard.totals().records, 0, "case {index}");
            assert_eq!(guard.stats.invalidated_unrecorded_op47, 1, "case {index}");
            assert!(op47(&mut guard, track, 0, &[6; 8]), "case {index}: the repeat must upload again");
        }
        // The largest record and the exact end of the buffer are still compared.
        let mut guard = guard();
        let track = vertex_buffer(&mut guard);
        let full = vec![2u8; MAX_RECORD_BYTES];
        assert_eq!(guard.op47(track, 4096, &full, 8192), Op47::Record(1));
        guard.store(1, 4096, full.clone());
        assert_eq!(guard.op47(track, 4096, &full, 8192), Op47::Skip);
        assert_eq!(guard.totals().record_bytes, MAX_RECORD_BYTES);
    }

    #[test]
    fn gpu_write_disqualifies_the_object_for_life() {
        for kind in [GpuWrite::CopyRegion, GpuWrite::ClearUav, GpuWrite::CopyCounter, GpuWrite::StorageBinding] {
            let mut guard = guard();
            let mut track = vertex_buffer(&mut guard);
            let mut other = vertex_buffer(&mut guard);
            assert!(op47(&mut guard, track, 0, &[1; 16]));
            assert!(!op47(&mut guard, track, 0, &[1; 16]));
            assert!(op47(&mut guard, other, 0, &[1; 16]));
            guard.gpu_write(&mut track, kind);
            assert_eq!(track, Track::Disqualified, "{kind:?}");
            assert_eq!(guard.totals().records, 1, "{kind:?}: its record went before the GPU write");
            // Nothing brings it back: not repeats, not op12, not another GPU write.
            for round in 0..4 {
                assert_eq!(guard.op47(track, 0, &[1; 16], GPU_BYTES), Op47::Legacy, "{kind:?} round {round}");
                guard.op12(track);
                guard.gpu_write(&mut track, GpuWrite::CopyRegion);
                assert_eq!(track, Track::Disqualified);
            }
            assert_eq!(guard.stats.disqualified[kind.index()], 1);
            assert_eq!(guard.totals().disqualified, 1, "{kind:?}: counted once per object");
            assert_eq!((guard.stats.bypass_disqualified, guard.stats.invalidated_gpu_write), (4, 1));
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
        assert!(!op47(&mut guard, old, 0, &[8; 16]));
        // Destroy or replacement: the record ends with the object.
        guard.forget(old);
        assert_eq!((guard.totals().records, guard.totals().record_bytes, guard.stats.forgotten), (0, 0, 1));
        let new = vertex_buffer(&mut guard);
        assert_ne!(old, new);
        assert!(op47(&mut guard, new, 0, &[8; 16]), "a recreated buffer starts with no record");
        assert!(!op47(&mut guard, new, 0, &[8; 16]));

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
        guard.clock = u64::MAX - 2;
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, b, 0, &[2; 8]));
        assert_eq!((guard.clock, guard.totals().records), (u64::MAX, 2));
        // This hit is decided against a valid record; refreshing its recency
        // wraps the counter, which empties the table instead of misordering it.
        assert!(!op47(&mut guard, a, 0, &[1; 8]));
        assert_eq!((guard.clock, guard.totals().records, guard.totals().record_bytes), (1, 0, 0));
        assert!(op47(&mut guard, a, 0, &[1; 8]));
        assert!(op47(&mut guard, b, 0, &[2; 8]));
        assert!(!op47(&mut guard, a, 0, &[1; 8]));
        assert!(!op47(&mut guard, b, 0, &[2; 8]));
        assert_eq!(guard.totals().record_bytes, 16);
        // The same on the insertion side.
        guard.clock = u64::MAX;
        let c = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, c, 0, &[3; 8]));
        assert_eq!((guard.clock, guard.totals().records, guard.totals().record_bytes), (1, 1, 8));
        assert!(!op47(&mut guard, c, 0, &[3; 8]));
        assert!(op47(&mut guard, a, 0, &[1; 8]));
    }

    #[test]
    fn caps_bound_records_and_bytes_and_evict_least_recently_used() {
        // Three records, 16 bytes each at most, 40 bytes in all.
        let mut guard = UploadDedup::new(true, 3, 16, 40);
        let buffers: Vec<Track> = (0..5).map(|_| vertex_buffer(&mut guard)).collect();
        let data = |index: usize| vec![index as u8 + 1; 16];
        let held = |guard: &UploadDedup| -> Vec<usize> {
            (0..5).filter(|&index| {
                let Track::Lifetime(lifetime) = buffers[index] else { unreachable!() };
                guard.records.contains_key(&lifetime)
            }).collect()
        };
        assert!(op47(&mut guard, buffers[0], 0, &data(0)));
        assert!(op47(&mut guard, buffers[1], 0, &data(1)));
        assert_eq!((held(&guard), guard.totals().record_bytes), (vec![0, 1], 32));
        // 48 bytes would pass the aggregate cap: the oldest record goes.
        assert!(op47(&mut guard, buffers[2], 0, &data(2)));
        assert_eq!((held(&guard), guard.totals().record_bytes, guard.totals().evictions), (vec![1, 2], 32, 1));
        // A hit refreshes recency, so buffer 2 becomes the eviction candidate.
        assert!(!op47(&mut guard, buffers[1], 0, &data(1)));
        assert!(op47(&mut guard, buffers[3], 0, &data(3)));
        assert_eq!((held(&guard), guard.totals().evictions), (vec![1, 3], 2));
        // A small third record fills both caps exactly.
        assert!(op47(&mut guard, buffers[4], 0, &[5; 8]));
        assert_eq!((held(&guard), guard.totals().record_bytes, guard.totals().evictions), (vec![1, 3, 4], 40, 2));
        // With that small record now the oldest, one 16-byte record costs two
        // evictions: the record count first, then the aggregate bytes.
        assert!(!op47(&mut guard, buffers[1], 0, &data(1)));
        assert!(!op47(&mut guard, buffers[3], 0, &data(3)));
        assert!(op47(&mut guard, buffers[2], 0, &data(2)));
        assert_eq!((held(&guard), guard.totals().record_bytes, guard.totals().evictions), (vec![2, 3], 32, 4));
        // An evicted buffer simply uploads again, then repeats skip as usual.
        assert!(op47(&mut guard, buffers[1], 0, &data(1)));
        assert!(!op47(&mut guard, buffers[1], 0, &data(1)));
        assert_eq!((held(&guard), guard.totals().record_bytes, guard.totals().evictions), (vec![1, 2], 32, 5));
        // Over the per-record cap: legacy route, never stored.
        assert_eq!(guard.op47(buffers[0], 0, &[7; 20], GPU_BYTES), Op47::Legacy);
        assert_eq!(guard.stats.bypass_shape[SHAPE_OVERSIZE], 1);
        assert_eq!((held(&guard), guard.totals().record_bytes), (vec![1, 2], 32));
        assert_eq!((guard.stats.records_peak, guard.stats.record_bytes_peak), (3, 40));

        // A store nothing asked for cannot break the caps either.
        guard.store(77, 0, vec![1; 17]);
        guard.store(78, 0, Vec::new());
        assert_eq!(guard.stats.store_rejected, 2);
        assert!(guard.totals().records <= 3 && guard.totals().record_bytes <= 40);
        // Caps of zero keep everything on the legacy route.
        for mut none in [UploadDedup::new(true, 0, 16, 40), UploadDedup::new(true, 3, 16, 0), UploadDedup::new(true, 3, 0, 40)] {
            let track = vertex_buffer(&mut none);
            for _ in 0..3 { assert!(op47(&mut none, track, 0, &[1; 4])); }
            assert_eq!((none.totals().records, none.totals().record_bytes, none.totals().hits), (0, 0, 0));
        }
    }

    #[test]
    fn default_caps_hold_under_churn() {
        let mut guard = guard();
        let buffers: Vec<Track> = (0..1000).map(|_| vertex_buffer(&mut guard)).collect();
        for round in 0..3u8 {
            for (index, track) in buffers.iter().enumerate() {
                let size = if index % 3 == 0 { MAX_RECORD_BYTES } else { 64 + 4 * (index % 200) };
                let data = vec![round.wrapping_add(index as u8); size];
                assert_eq!(guard.op47(*track, 0, &data, 8192), Op47::Record(index as u64 + 1));
                guard.store(index as u64 + 1, 0, data);
                let totals = guard.totals();
                assert!(totals.records <= MAX_RECORDS && totals.record_bytes <= MAX_TOTAL_BYTES);
                assert_eq!(totals.record_bytes, guard.records.values().map(|record| record.bytes.len()).sum::<usize>());
            }
        }
        assert_eq!(guard.totals().records, MAX_RECORDS);
        assert_eq!(guard.totals().evictions, 3000 - MAX_RECORDS as u64);
        assert_eq!(guard.stats.records_peak, MAX_RECORDS as u64);
        assert!(guard.stats.record_bytes_peak <= MAX_TOTAL_BYTES as u64);
        // The most recent buffer is still recorded; the oldest is long gone.
        let last = vec![2u8.wrapping_add(999u16 as u8); MAX_RECORD_BYTES];
        assert_eq!(guard.op47(buffers[999], 0, &last, 8192), Op47::Skip);
        assert_eq!(guard.op47(buffers[0], 0, &[2u8; MAX_RECORD_BYTES], 8192), Op47::Record(1));
    }

    #[test]
    fn interval_reset_keeps_records_and_metrics_report_every_counter() {
        let mut guard = guard();
        let mut a = vertex_buffer(&mut guard);
        let b = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, a, 0, &[1; 16]));
        assert!(!op47(&mut guard, a, 0, &[1; 16]));
        assert!(op47(&mut guard, a, 0, &[2; 16]));
        assert!(op47(&mut guard, b, 4, &[3; 8]));
        assert!(op47(&mut guard, b, 0, &[3; 6]));
        guard.op12(b);
        assert!(op47(&mut guard, Track::Untracked, 0, &[1; 16]));
        guard.gpu_write(&mut a, GpuWrite::ClearUav);
        assert!(op47(&mut guard, a, 0, &[2; 16]));

        let metrics = guard.metrics(10.0);
        assert_eq!(metrics["enabled"], true);
        assert_eq!(metrics["environment"], "MUGUET_OP47_UPLOAD_DEDUP");
        assert_eq!((metrics["cap_records"].as_u64(), metrics["cap_record_bytes"].as_u64(), metrics["cap_total_bytes"].as_u64()),
            (Some(128), Some(4096), Some(524288)));
        assert_eq!((metrics["records"].as_u64(), metrics["record_bytes"].as_u64()), (Some(0), Some(0)));
        let interval = &metrics["stats_interval"];
        let count = |name: &str| interval[name].as_u64().unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!((count("op47_calls"), count("op12_calls")), (7, 1));
        assert_eq!((count("checked"), count("hits"), count("misses"), count("bypasses")), (4, 1, 3, 3));
        assert_eq!(count("op47_calls"), count("checked") + count("bypasses"));
        assert_eq!((count("saved_calls"), count("saved_bytes")), (1, 16));
        for name in ["check_ns", "check_max_ns", "store_ns", "store_max_ns"] { count(name); }
        assert_eq!(interval["miss_reasons"], serde_json::json!({"no_record": 2, "range": 0, "bytes": 1}));
        assert_eq!(interval["bypass_reasons"], serde_json::json!({"untracked_buffer": 1, "disqualified_buffer": 1,
            "empty": 0, "unaligned_or_padded": 1, "out_of_bounds": 0, "over_record_cap": 0}));
        assert_eq!(interval["invalidations"], serde_json::json!({"op12": 0, "unrecorded_op47": 1, "gpu_write": 1, "destroyed_or_replaced": 0}));
        assert_eq!(interval["disqualified"], serde_json::json!({"copy_region": 0, "clear_uav": 1, "copy_counter": 0, "storage_binding": 0}));
        assert_eq!(interval["table"], serde_json::json!({"stored": 3, "store_rejected": 0, "evictions": 0,
            "records_peak": 2, "record_bytes_peak": 24, "tracked_buffers": 2, "lifetime_exhausted": 0}));

        // A new interval starts from zero but keeps what is recorded.
        let c = vertex_buffer(&mut guard);
        assert!(op47(&mut guard, c, 0, &[4; 16]));
        guard.reset_interval();
        assert_eq!(guard.totals(), Totals { records: 1, record_bytes: 16, ..Default::default() });
        assert_eq!((guard.stats.records_peak, guard.stats.record_bytes_peak, guard.stats.check_ns), (1, 16, 0));
        assert!(!op47(&mut guard, c, 0, &[4; 16]), "the record survives an interval reset");
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
        /// Drop the record at the GPU write but keep tracking afterwards.
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
            // set_obj: a replaced buffer's record is forgotten with it.
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
        let (mut hits, mut evictions, mut disqualified, mut shape_bypasses) = (0, 0, 0, 0);
        for seed in 1..=48u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            // Small caps so eviction and the per-record cap are exercised too.
            let mut legacy = Model::new(UploadDedup::new(false, 4, 32, 64), Hook::Sticky, IDS);
            let mut guarded = Model::new(UploadDedup::new(true, 4, 32, 64), Hook::Sticky, IDS);
            let mut last: Vec<Option<(usize, Vec<u8>)>> = vec![None; IDS];
            for model in [&mut legacy, &mut guarded] {
                for id in 0..IDS { model.create(id, SIZE, BIND_VERTEX); }
                // One certain repeat, so every seed has at least one skip.
                model.upload(0, 0, &[0x5a; 16], true);
                model.upload(0, 0, &[0x5a; 16], true);
            }
            for step in 0..3000 {
                let id = rng.below(IDS as u64) as usize;
                let roll = rng.below(100);
                let mut flushed = false;
                if roll < 60 {
                    // Half of the op47 calls repeat this buffer's previous one.
                    let repeat = last[id].clone().filter(|_| rng.below(2) == 0);
                    let (offset, data) = repeat.unwrap_or_else(|| {
                        let offset = rng.pick(&[0usize, 4, 8, 12, 32, 2, 60]);
                        let length = rng.pick(&[4usize, 8, 16, 16, 32, 6, 0, 40, 80]);
                        (offset, vec![rng.pick(&[0x11u8, 0x22, 0x33]); length])
                    });
                    for model in [&mut legacy, &mut guarded] { model.upload(id, offset, &data, true); }
                    last[id] = Some((offset, data));
                } else if roll < 70 {
                    let offset = rng.pick(&[0usize, 8, 32]);
                    let data = vec![rng.pick(&[0x11u8, 0x44]); rng.pick(&[8usize, 16, 32])];
                    for model in [&mut legacy, &mut guarded] { model.upload(id, offset, &data, false); }
                } else if roll < 76 {
                    let kind = rng.pick(&[GpuWrite::CopyRegion, GpuWrite::ClearUav, GpuWrite::CopyCounter]);
                    let offset = rng.pick(&[0usize, 8, 32]);
                    let data = vec![rng.pick(&[0u8, 0x77]); rng.pick(&[4usize, 16, 32])];
                    for model in [&mut legacy, &mut guarded] { model.gpu_write(id, offset, &data, kind); }
                } else if roll < 81 {
                    // Mostly eligible kinds; storage and shadowed ones stay untracked.
                    let bind = rng.pick(&[BIND_VERTEX, BIND_VERTEX, BIND_INDEX, BIND_VERTEX | BIND_UNORDERED_ACCESS, BIND_CONSTANT | BIND_VERTEX]);
                    for model in [&mut legacy, &mut guarded] { model.create(id, SIZE, bind); }
                    last[id] = None;
                } else if roll < 84 {
                    for model in [&mut legacy, &mut guarded] { model.destroy(id); }
                    last[id] = None;
                } else if roll < 92 {
                    for model in [&mut legacy, &mut guarded] { model.flush(); }
                    flushed = true;
                } else {
                    // A fresh tracked buffer in a free slot keeps the mix lively.
                    if legacy.live[id].is_none() { for model in [&mut legacy, &mut guarded] { model.create(id, SIZE, BIND_VERTEX); } }
                }
                if flushed { assert_eq!(legacy.image(), guarded.image(), "seed {seed}, step {step}"); }
                let totals = guarded.guard.totals();
                assert!(totals.records <= 4 && totals.record_bytes <= 64, "seed {seed}, step {step}: caps exceeded");
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
            assert!(totals.hits >= 1, "seed {seed}");
            hits += totals.hits;
            evictions += totals.evictions;
            disqualified += totals.disqualified;
            shape_bypasses += guarded.guard.stats.bypass_shape.iter().sum::<u64>();
        }
        assert!(hits > 0 && evictions > 0 && disqualified > 0 && shape_bypasses > 0,
            "the stream must exercise skips, eviction, GPU writes and legacy shapes: {hits} {evictions} {disqualified} {shape_bypasses}");
    }

    /// Runs one fixed stream on a legacy model and on a guarded one with the
    /// given hook, and reports (legacy bytes, guarded bytes, guarded hits).
    fn replay(hook: Hook, stream: fn(&mut Model)) -> (Vec<u8>, Vec<u8>, u64) {
        let mut legacy = Model::new(UploadDedup::new(false, MAX_RECORDS, MAX_RECORD_BYTES, MAX_TOTAL_BYTES), Hook::Sticky, 1);
        let mut guarded = Model::new(guard(), hook, 1);
        for model in [&mut legacy, &mut guarded] { stream(model); model.flush(); }
        let bytes = |model: &Model| model.live[0].as_ref().map(|buffer| buffer.bytes.clone()).unwrap();
        (bytes(&legacy), bytes(&guarded), guarded.guard.totals().hits)
    }

    #[test]
    fn a_gpu_write_after_the_record_must_drop_it() {
        fn stream(model: &mut Model) {
            model.create(0, 16, BIND_VERTEX);
            model.upload(0, 0, &[0xaa; 8], true);
            model.gpu_write(0, 0, &[0xbb; 8], GpuWrite::CopyRegion);
            model.flush();
            // The copy replaced those bytes; the same op47 has to upload again.
            model.upload(0, 0, &[0xaa; 8], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!(&legacy[..8], &[0xaa; 8]);
        assert_eq!((guarded, hits), (legacy.clone(), 0));
        // The model does notice a host that forgets the hook.
        let (_, broken, hits) = replay(Hook::Missing, stream);
        assert_eq!((&broken[..8], hits), (&[0xbb; 8][..], 1));
        assert_ne!(broken, legacy);
    }

    #[test]
    fn a_gpu_write_recorded_before_an_op47_of_its_submission_needs_the_sticky_rule() {
        fn stream(model: &mut Model) {
            model.create(0, 16, BIND_VERTEX);
            // Recorded first, executed last: the prelude upload below runs
            // BEFORE this copy, so the submission ends with the copy's bytes.
            model.gpu_write(0, 0, &[0xbb; 8], GpuWrite::CopyRegion);
            model.upload(0, 0, &[0xaa; 8], true);
            model.flush();
            model.upload(0, 0, &[0xaa; 8], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!(&legacy[..8], &[0xaa; 8]);
        assert_eq!((guarded, hits), (legacy.clone(), 0));
        // Dropping the record at the copy is not enough: the op47 after it
        // would be recorded as authoritative and the next repeat skipped.
        let (_, trapped, hits) = replay(Hook::InvalidateOnly, stream);
        assert_eq!((&trapped[..8], hits), (&[0xbb; 8][..], 1));
        assert_ne!(trapped, legacy);
    }

    #[test]
    fn an_earlier_consumer_sees_the_same_bytes_whether_or_not_a_repeat_is_skipped() {
        // Consumers are read-only, so the model needs no extra operation: a
        // consumer recorded before a no-overwrite upload reads the bytes the
        // prelude leaves, which a skipped exact repeat does not change.
        fn stream(model: &mut Model) {
            model.create(0, 16, BIND_VERTEX);
            model.upload(0, 4, &[0xc1; 8], true);
            model.flush();
            model.upload(0, 4, &[0xc1; 8], true);
            model.upload(0, 4, &[0xc1; 8], true);
        }
        let (legacy, guarded, hits) = replay(Hook::Sticky, stream);
        assert_eq!(&legacy[4..12], &[0xc1; 8]);
        assert_eq!((guarded, hits), (legacy, 2));
    }
}

//! Default-off reuse of immutable packed CB snapshots within one present.
//! Metadata eviction never reclaims ring storage. Ordinary flushes retain keys;
//! present clears them before the ring slot can be recycled.
use super::{Cb, Obj, Pack};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::Instant;

const LIMIT: usize = 128;
const MEMBERS: usize = 16;

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
struct MemberKey {
    binding: u32,
    offset: u64,
    size: u64,
    buf: u32,
    first: u32,
    identity: u64,
    version: u64,
    buffer_size: u64,
    shadow_len: usize,
    source: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    program: u64,
    stage: usize,
    size: u64,
    len: usize,
    members: [MemberKey; MEMBERS],
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.program.hash(state);
        self.stage.hash(state);
        self.size.hash(state);
        self.members[..self.len].hash(state);
    }
}

fn key(program: u64, stage: usize, pack: &Pack, objects: &[Option<Obj>], bindings: &[Cb]) -> Option<Key> {
    if pack.size == 0 || pack.members.len() > MEMBERS { return None; }
    let mut key = Key { program, stage, size: pack.size, len: pack.members.len(), members: [MemberKey::default(); MEMBERS] };
    for (at, member) in pack.members.iter().enumerate() {
        let cb = bindings.get(member.binding as usize).copied().unwrap_or_default();
        let mut m = MemberKey { binding: member.binding, offset: member.offset, size: member.size,
            buf: cb.buf, first: cb.first, ..Default::default() };
        if let Some(Obj::Buf(buffer)) = objects.get(cb.buf as usize).and_then(Option::as_ref) {
            // GPU writes/copies do not maintain CB shadows in the current
            // renderer. Never infer coherence or skip packing for mixed CBs.
            if buffer.gpu.is_some() { return None; }
            m.identity = buffer.identity;
            m.version = buffer.version;
            m.buffer_size = buffer.size;
            m.source = 1;
            if let Some(shadow) = &buffer.shadow {
                m.source = 2;
                m.shadow_len = shadow.len();
                let from = cb.first as u64 * 16;
                let len = member.size.min(buffer.size.saturating_sub(from));
                if len > 0 {
                    // Invalid synthetic ranges must take the reference path,
                    // including its existing failure/allocator behavior.
                    if member.offset.checked_add(len)? > pack.size || from.checked_add(len)? > shadow.len() as u64 { return None; }
                }
            }
        }
        key.members[at] = m;
    }
    Some(key)
}

#[derive(Default)]
struct Counts {
    requests: u64,
    hits: u64,
    misses: u64,
    bypasses: u64,
    saved_bytes: u64,
    saved_aligned_bytes: u64,
    key_check_cpu_ns: u64,
    key_check_cpu_max_ns: u64,
    evictions: u64,
    present_clears: u64,
    version_wrap_clears: u64,
}

pub(super) struct UniformSnapshot {
    pub(super) enabled: bool,
    entries: HashMap<Key, (usize, u64)>,
    order: VecDeque<Key>,
    counts: Counts,
}

impl UniformSnapshot {
    pub(super) fn from_environment() -> Self {
        Self::new(std::env::var("MUGUET_UNIFORM_SNAPSHOT_CACHE").as_deref() == Ok("1"))
    }

    pub(super) fn new(enabled: bool) -> Self {
        Self { enabled, entries: HashMap::new(), order: VecDeque::new(), counts: Counts::default() }
    }

    pub(super) fn probe(&mut self, program: u64, stage: usize, pack: &Pack, objects: &[Option<Obj>], bindings: &[Cb]) -> (Option<Key>, Option<(usize, u64)>) {
        if !self.enabled { return (None, None); }
        let started = Instant::now();
        self.counts.requests += 1;
        let key = key(program, stage, pack, objects, bindings);
        let location = key.as_ref().and_then(|key| self.entries.get(key)).copied();
        if let Some(key) = &key {
            if location.is_some() {
                self.counts.hits += 1;
                self.counts.saved_bytes += key.size;
                self.counts.saved_aligned_bytes += (key.size + 255) & !255;
            } else { self.counts.misses += 1; }
        } else { self.counts.bypasses += 1; }
        let ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.counts.key_check_cpu_ns = self.counts.key_check_cpu_ns.saturating_add(ns);
        self.counts.key_check_cpu_max_ns = self.counts.key_check_cpu_max_ns.max(ns);
        (key, location)
    }

    pub(super) fn insert(&mut self, key: Option<Key>, location: (usize, u64)) {
        let Some(key) = key else { return };
        if self.entries.contains_key(&key) { return; }
        if self.entries.len() == LIMIT {
            if let Some(old) = self.order.pop_front() { self.entries.remove(&old); }
            self.counts.evictions += 1;
        }
        self.entries.insert(key, location);
        self.order.push_back(key);
    }

    fn clear(&mut self) { self.entries.clear(); self.order.clear(); }

    pub(super) fn present(&mut self) {
        self.clear();
        if self.enabled { self.counts.present_clears += 1; }
    }

    pub(super) fn version_wrapped(&mut self) {
        self.clear();
        if self.enabled { self.counts.version_wrap_clears += 1; }
    }

    pub(super) fn metrics(&self) -> serde_json::Value {
        let c = &self.counts;
        serde_json::json!({"schema":1, "enabled":self.enabled, "cumulative":true,
            "requests":c.requests, "hits":c.hits, "misses":c.misses, "bypasses":c.bypasses,
            "saved_bytes":c.saved_bytes, "saved_aligned_bytes":c.saved_aligned_bytes,
            "key_check_cpu_ns":c.key_check_cpu_ns, "key_check_cpu_max_ns":c.key_check_cpu_max_ns,
            "evictions":c.evictions, "present_clears":c.present_clears, "version_wrap_clears":c.version_wrap_clears,
            "entries":self.entries.len(), "entry_limit":LIMIT, "member_limit":MEMBERS,
            "scope":"enabled pack_copy probes; saved logical/aligned snapshot bytes, not actual queue-write savings or GPU time"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Buf, PackMember};

    fn object(identity: u64, version: u64, bytes: &[u8]) -> Option<Obj> {
        Some(Obj::Buf(Buf { identity, size: bytes.len() as u64, stride: 0, gpu: None, bytes: 0,
            shadow: Some(bytes.to_vec()), version, enc: 0, in_bg: false, counter: None }))
    }

    fn fixture_pack() -> Pack { Pack { size: 32, members: vec![PackMember { binding: 0, size: 32, offset: 0 }] } }

    #[test]
    fn cpu_snapshot_reuse_preserves_old_bytes_and_separates_every_dependency() {
        let mut cache = UniformSnapshot::new(true);
        let mut objects = vec![None, object(11, 0, &[7; 48])];
        let mut bindings = [Cb { buf: 1, first: 1 }];
        let mut pack = fixture_pack();
        let mut stage = vec![0x91; 512];
        let (a, hit) = cache.probe(20, 2, &pack, &objects, &bindings); assert!(hit.is_none());
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 0, &mut stage);
        cache.insert(a, (0, 0));
        let saved = stage[..32].to_vec();
        assert_eq!(cache.probe(20, 2, &pack, &objects, &bindings).1, Some((0, 0)));
        objects[1] = object(11, 1, &[9; 48]);
        let (b, hit) = cache.probe(20, 2, &pack, &objects, &bindings); assert!(hit.is_none());
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 256, &mut stage);
        cache.insert(b, (0, 256));
        assert_eq!(&stage[..32], saved.as_slice()); assert_eq!(&stage[256..288], &[9; 32]);
        assert!(cache.probe(21, 2, &pack, &objects, &bindings).1.is_none());
        assert!(cache.probe(20, 1, &pack, &objects, &bindings).1.is_none());
        bindings[0].first = 2; assert!(cache.probe(20, 2, &pack, &objects, &bindings).1.is_none());
        bindings[0].first = 1;
        objects[1] = object(12, 1, &[9; 48]); assert!(cache.probe(20, 2, &pack, &objects, &bindings).1.is_none());
        objects[1] = object(11, 1, &[9; 48]);
        pack.members[0].size = 16; assert!(cache.probe(20, 2, &pack, &objects, &bindings).1.is_none());
        pack.members[0].offset = 16; assert!(cache.probe(20, 2, &pack, &objects, &bindings).1.is_none());
        let metrics = cache.metrics(); assert_eq!(metrics["hits"], 1); assert_eq!(metrics["saved_bytes"], 32);
        assert_eq!(metrics["saved_aligned_bytes"], 256);
    }

    #[test]
    fn cpu_missing_truncated_and_no_shadow_sources_cannot_reuse_stale_members() {
        let mut cache = UniformSnapshot::new(true); let pack = fixture_pack(); let bindings = [Cb { buf: 1, first: 1 }];
        let mut objects = vec![None, None]; let mut stage = vec![0xab; 512];
        let (missing, _) = cache.probe(1, 0, &pack, &objects, &bindings);
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 0, &mut stage); cache.insert(missing, (0, 0));
        assert_eq!(&stage[..32], &[0; 32]); assert_eq!(cache.probe(1, 0, &pack, &objects, &bindings).1, Some((0, 0)));
        objects[1] = object(3, 0, &[5; 20]); let (short, hit) = cache.probe(1, 0, &pack, &objects, &bindings); assert!(hit.is_none());
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 256, &mut stage); cache.insert(short, (0, 256));
        assert_eq!(&stage[256..260], &[5; 4]); assert_eq!(&stage[260..288], &[0; 28]);
        if let Some(Obj::Buf(b)) = &mut objects[1] { b.shadow = None; }
        assert!(cache.probe(1, 0, &pack, &objects, &bindings).1.is_none());
        objects[1] = None; assert_eq!(cache.probe(1, 0, &pack, &objects, &bindings).1, Some((0, 0)));
    }

    #[test]
    fn cpu_ordered_overlapping_members_keep_last_write_wins_snapshots() {
        let mut cache = UniformSnapshot::new(true);
        let objects = vec![None, object(1, 0, &[11; 16]), object(2, 0, &[22; 16])];
        let bindings = [Cb { buf: 1, first: 0 }, Cb { buf: 2, first: 0 }];
        let mut pack = Pack { size: 16, members: vec![PackMember { binding: 0, size: 16, offset: 0 }, PackMember { binding: 1, size: 16, offset: 0 }] };
        let mut stage = vec![0; 512];
        let (key, _) = cache.probe(1, 0, &pack, &objects, &bindings);
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 0, &mut stage); cache.insert(key, (0, 0));
        pack.members.reverse();
        let (key, location) = cache.probe(1, 0, &pack, &objects, &bindings); assert!(location.is_none());
        super::super::uniform_pack::direct_copy(&pack, &objects, &bindings, 256, &mut stage); cache.insert(key, (0, 256));
        assert_eq!(&stage[..16], &[22; 16]); assert_eq!(&stage[256..272], &[11; 16]);
        pack.members.reverse(); assert_eq!(cache.probe(1, 0, &pack, &objects, &bindings).1, Some((0, 0)));
    }

    #[test]
    fn cpu_metadata_is_bounded_and_present_and_version_wrap_forget_offsets() {
        let mut cache = UniformSnapshot::new(true); let pack = fixture_pack(); let objects = vec![None, object(1, 0, &[1; 32])];
        let bindings = [Cb { buf: 1, first: 0 }];
        for program in 0..=LIMIT as u64 { let (key, _) = cache.probe(program, 0, &pack, &objects, &bindings); cache.insert(key, (0, program * 256)); }
        assert_eq!(cache.entries.len(), LIMIT); assert_eq!(cache.order.len(), LIMIT);
        assert!(cache.probe(0, 0, &pack, &objects, &bindings).1.is_none());
        assert_eq!(cache.probe(1, 0, &pack, &objects, &bindings).1, Some((0, 256)));
        cache.present(); assert!(cache.probe(1, 0, &pack, &objects, &bindings).1.is_none());
        let (key, _) = cache.probe(1, 0, &pack, &objects, &bindings); cache.insert(key, (0, 0));
        cache.version_wrapped(); assert!(cache.probe(1, 0, &pack, &objects, &bindings).1.is_none());
        let mut wide = fixture_pack(); wide.members.resize(MEMBERS + 1, wide.members[0].clone());
        assert!(cache.probe(1, 0, &wide, &objects, &bindings).0.is_none());
        let mut bad = fixture_pack(); bad.members[0].offset = 1;
        assert!(cache.probe(1, 0, &bad, &objects, &bindings).0.is_none());
        let mut off = UniformSnapshot::new(false); assert!(off.probe(1, 0, &pack, &objects, &bindings).0.is_none());
        assert_eq!(off.metrics()["requests"], 0);
    }
}

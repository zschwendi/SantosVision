//! Opt-in, bounded temporary storage for bind-group lookup preparation.
//! The resource walk and fresh uniform snapshot remain in gpu.rs on every call.
use std::time::Duration;

fn key_first_requested(existing_prepare:bool,value:Option<&str>)->bool {
    existing_prepare && value==Some("1")
}

pub(super) const KEY_INLINE: usize = 32;
pub(super) const ENTRIES_INLINE: usize = 12;

#[derive(Clone, Copy)]
pub(super) enum NonBindingKind { Query, Layout, Shader, State }

impl NonBindingKind {
    fn index(self) -> usize {
        match self { Self::Query => 0, Self::Layout => 1, Self::Shader => 2, Self::State => 3 }
    }
}

pub(super) struct KeyScratch {
    inline: [u64; KEY_INLINE],
    len: usize,
    heap: Option<Vec<u64>>,
    legacy: bool,
}

impl KeyScratch {
    pub(super) fn new(fast: bool, prefix: &[u64]) -> Self {
        let mut result = Self { inline: [0; KEY_INLINE], len: 0,
            heap: if fast { None } else { Some(Vec::from(prefix)) }, legacy: !fast };
        if fast { for &word in prefix { result.push(word); } }
        result
    }

    pub(super) fn push(&mut self, word: u64) {
        if let Some(heap) = self.heap.as_mut() { heap.push(word); }
        else if self.len < KEY_INLINE {
            self.inline[self.len] = word;
            self.len += 1;
        } else {
            let mut heap = Vec::with_capacity(KEY_INLINE * 2);
            heap.extend_from_slice(&self.inline);
            heap.push(word);
            self.heap = Some(heap);
        }
    }

    pub(super) fn as_slice(&self) -> &[u64] {
        self.heap.as_deref().unwrap_or(&self.inline[..self.len])
    }

    pub(super) fn spilled(&self) -> bool { !self.legacy && self.heap.is_some() }

    pub(super) fn into_vec(self) -> Vec<u64> {
        match self.heap {
            Some(heap) => heap,
            None => self.inline[..self.len].to_vec(),
        }
    }
}

pub(super) struct EntryScratch<T> {
    inline: [Option<T>; ENTRIES_INLINE],
    len: usize,
    heap: Option<Vec<T>>,
    legacy: bool,
}

impl<T> EntryScratch<T> {
    pub(super) fn new(fast: bool) -> Self {
        Self { inline: std::array::from_fn(|_| None), len: 0,
            heap: if fast { None } else { Some(Vec::new()) }, legacy: !fast }
    }

    pub(super) fn push(&mut self, value: T) {
        if let Some(heap) = self.heap.as_mut() { heap.push(value); }
        else if self.len < ENTRIES_INLINE {
            self.inline[self.len] = Some(value);
            self.len += 1;
        } else {
            let mut heap = Vec::with_capacity(ENTRIES_INLINE * 2);
            for entry in self.inline.iter_mut().take(self.len) {
                heap.push(entry.take().expect("inline entry initialized"));
            }
            heap.push(value);
            self.heap = Some(heap);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.heap.as_ref().map_or(self.len, Vec::len)
    }

    pub(super) fn spilled(&self) -> bool { !self.legacy && self.heap.is_some() }

    pub(super) fn into_vec(mut self) -> Vec<T> {
        if let Some(heap) = self.heap.take() { return heap; }
        let mut heap = Vec::with_capacity(self.len);
        for entry in self.inline.iter_mut().take(self.len) {
            heap.push(entry.take().expect("inline entry initialized"));
        }
        heap
    }
}

pub(super) struct BindGroupPrepare {
    pub(super) enabled: bool,
    pub(super) key_first: bool,
    key_first_publisher:u64,
    key_first_calls: u64,
    key_first_hits: u64,
    key_first_fallbacks: u64,
    key_first_owned_items_avoided: u64,
    key_first_binding_items: [u64;4],
    key_first_hit_binding_items: [u64;4],
    calls: u64,
    lookups: u64,
    hits: u64,
    misses: u64,
    inline_key_lookups: u64,
    key_spills: u64,
    entry_spills: u64,
    owned_key_materializations: u64,
    owned_entry_materializations: u64,
    entry_items_on_miss: u64,
    cache_invalidations: u64,
    skipped_set_obj_invalidations: [u64; 4],
    skipped_destroy_invalidations: [u64; 4],
    sampled_calls: u64,
    sampled_wall_ns: u64,
    sampled_max_ns: u64,
}

impl BindGroupPrepare {
    pub(super) fn from_environment() -> Self {
        let mut result=Self::new(crate::draw_setup::bind_group_prepare_enabled());
        result.key_first=key_first_requested(result.enabled,std::env::var("MUGUET_BIND_GROUP_KEY_FIRST").ok().as_deref());
        result.key_first_publisher=super::bind_group_key_first_status::begin(result.key_first);
        result
    }

    pub(super) fn new(enabled: bool) -> Self {
        Self { enabled, key_first:false,key_first_publisher:0, key_first_calls:0,key_first_hits:0,key_first_fallbacks:0,
            key_first_owned_items_avoided:0,key_first_binding_items:[0;4],key_first_hit_binding_items:[0;4],
            calls: 0, lookups: 0, hits: 0, misses: 0,
            inline_key_lookups: 0, key_spills: 0, entry_spills: 0,
            owned_key_materializations: 0, owned_entry_materializations: 0,
            entry_items_on_miss: 0, cache_invalidations: 0,
            skipped_set_obj_invalidations: [0; 4], skipped_destroy_invalidations: [0; 4],
            sampled_calls: 0, sampled_wall_ns: 0, sampled_max_ns: 0 }
    }

    pub(super) fn publish_key_first_status(&self) {
        super::bind_group_key_first_status::publish(self.key_first_publisher,super::bind_group_key_first_status::Counters{
            calls:self.calls,lookups:self.key_first_calls,hits:self.key_first_hits,fallbacks:self.key_first_fallbacks,
            avoided:self.key_first_owned_items_avoided,mix:self.key_first_binding_items,hit_mix:self.key_first_hit_binding_items});
    }

    pub(super) fn key_first_fallback(&mut self) { self.key_first_fallbacks=self.key_first_fallbacks.saturating_add(1); }
    pub(super) fn key_first_lookup(&mut self,hit:bool,items:usize,mix:[u64;4]) {
        self.key_first_calls=self.key_first_calls.saturating_add(1);
        for (total,count) in self.key_first_binding_items.iter_mut().zip(mix) {*total=total.saturating_add(count);}
        if hit {
            self.key_first_hits=self.key_first_hits.saturating_add(1);
            self.key_first_owned_items_avoided=self.key_first_owned_items_avoided.saturating_add(items as u64);
            for (total,count) in self.key_first_hit_binding_items.iter_mut().zip(mix) {*total=total.saturating_add(count);}
        }
    }

    // One sample in 64 calls keeps timing overhead out of most draw calls.
    pub(super) fn begin_call(&mut self) -> bool {
        if !self.enabled { return false; }
        self.calls = self.calls.saturating_add(1);
        self.calls.is_multiple_of(64)
    }

    pub(super) fn record_wall(&mut self, elapsed: Duration) {
        self.sampled_calls = self.sampled_calls.saturating_add(1);
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.sampled_wall_ns = self.sampled_wall_ns.saturating_add(ns);
        self.sampled_max_ns = self.sampled_max_ns.max(ns);
    }

    pub(super) fn lookup(&mut self, hit: bool, key_spilled: bool, entry_spilled: bool) {
        if !self.enabled { return; }
        self.lookups = self.lookups.saturating_add(1);
        self.hits += u64::from(hit);
        self.misses += u64::from(!hit);
        self.inline_key_lookups += u64::from(!key_spilled);
        self.key_spills += u64::from(key_spilled);
        self.entry_spills += u64::from(entry_spilled);
    }

    pub(super) fn materialized_miss(&mut self, entry_count: usize) {
        if !self.enabled { return; }
        self.owned_key_materializations += 1;
        self.owned_entry_materializations += 1;
        self.entry_items_on_miss = self.entry_items_on_miss.saturating_add(entry_count as u64);
    }

    pub(super) fn invalidated(&mut self) {
        if self.enabled { self.cache_invalidations = self.cache_invalidations.saturating_add(1); }
    }

    pub(super) fn skipped_set_obj(&mut self, kind: NonBindingKind) {
        if self.enabled {
            let count = &mut self.skipped_set_obj_invalidations[kind.index()];
            *count = (*count).saturating_add(1);
        }
    }

    pub(super) fn skipped_destroy(&mut self, kind: NonBindingKind) {
        if self.enabled {
            let count = &mut self.skipped_destroy_invalidations[kind.index()];
            *count = (*count).saturating_add(1);
        }
    }

    pub(super) fn metrics(&self) -> serde_json::Value {
        serde_json::json!({"schema":1,"enabled":self.enabled,
            "key_first_enabled":self.key_first,"key_first_eligible_lookups":self.key_first_calls,
            "key_first_hits":self.key_first_hits,"key_first_fallbacks":self.key_first_fallbacks,
            "key_first_owned_resource_items_avoided":self.key_first_owned_items_avoided,
            "key_first_binding_items":{"uniform":self.key_first_binding_items[0],"sampler":self.key_first_binding_items[1],
                "texture":self.key_first_binding_items[2],"storage_buffer":self.key_first_binding_items[3]},
            "key_first_hit_binding_items":{"uniform":self.key_first_hit_binding_items[0],"sampler":self.key_first_hit_binding_items[1],
                "texture":self.key_first_hit_binding_items[2],"storage_buffer":self.key_first_hit_binding_items[3]},
            "calls":self.calls,"lookups":self.lookups,"hits":self.hits,"misses":self.misses,
            "inline_key_lookups":self.inline_key_lookups,"key_spills":self.key_spills,
            "entry_spills":self.entry_spills,
            "owned_key_materializations":self.owned_key_materializations,
            "owned_entry_materializations":self.owned_entry_materializations,
            "entry_items_on_miss":self.entry_items_on_miss,
            "cache_invalidations":self.cache_invalidations,
            "skipped_nonbinding_set_obj":{"query":self.skipped_set_obj_invalidations[0],
                "layout":self.skipped_set_obj_invalidations[1],
                "shader":self.skipped_set_obj_invalidations[2],
                "non_sampler_state":self.skipped_set_obj_invalidations[3]},
            "skipped_nonbinding_destroy":{"query":self.skipped_destroy_invalidations[0],
                "layout":self.skipped_destroy_invalidations[1],
                "shader":self.skipped_destroy_invalidations[2],
                "non_sampler_state":self.skipped_destroy_invalidations[3]},
            "sample_stride":64,"sampled_calls":self.sampled_calls,
            "sampled_cpu_wall_ns":self.sampled_wall_ns,"sampled_cpu_max_ns":self.sampled_max_ns,
            "scope":"source-site lookup and sampled CPU wall; no GPU timing or FPS claim"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn key_first_is_separately_opt_in_and_requires_existing_prepare() {
        for existing in [false,true] {
            for value in [None,Some(""),Some("0"),Some("true"),Some("2"),Some("1"),Some(" 1"),Some("1 ")] {
                assert_eq!(key_first_requested(existing,value),existing &&value==Some("1"));
            }
        }
        assert!(!BindGroupPrepare::new(true).key_first);
        assert!(!BindGroupPrepare::new(false).key_first);
    }

    #[test]
    fn pure_invalidation_relevance_covers_nonbinding_replacements_and_missing_resources() {
        use super::super::*;
        let query = Obj::Query(QueryObj { qtype: 1, life: QueryLife::new(0), slots: vec![] });
        let layout = Obj::Layout(Rc::new(LayoutObj { els: vec![], slots: vec![] }));
        let shader = Obj::Shader(ShaderObj { stage: 0, hash: String::new(), sig: vec![], program: None });
        let blend_state = Obj::State(Rc::new(StateObj { words: vec![], sampler: None }));
        let view = Obj::Uav(ViewObj { res: 9, is_buf: true, fmt: 28, vdim: 4,
            a: 0, b: 0, c: 0, d: 0, views: HashMap::new() });
        for nonbinding in [&query, &layout, &shader, &blend_state] {
            assert!(!nonbinding.bind_group_relevant());
            assert!(!binding_cache_needs_invalidation(None, Some(nonbinding)));
            assert!(!binding_cache_needs_invalidation(Some(nonbinding), None));
            assert!(!binding_cache_needs_invalidation(Some(&query), Some(nonbinding)));
            assert!(nonbinding.nonbinding_kind().is_some());
            // Reusing a guest ID previously holding a binding must still clear.
            assert!(binding_cache_needs_invalidation(Some(&view), Some(nonbinding)));
        }
        assert!(binding_cache_needs_invalidation(None, Some(&view)));
        assert!(binding_cache_needs_invalidation(Some(&view), None));
        assert!(binding_cache_needs_invalidation(Some(&view), Some(&view)));
        let mut metrics = BindGroupPrepare::new(true);
        metrics.skipped_set_obj(NonBindingKind::Layout);
        metrics.skipped_destroy(NonBindingKind::Query);
        assert_eq!(metrics.metrics()["skipped_nonbinding_set_obj"]["layout"], 1);
        assert_eq!(metrics.metrics()["skipped_nonbinding_destroy"]["query"], 1);
    }

    #[test]
    fn stack_key_matches_vec_lookup_and_preserves_full_ordered_identity() {
        let mut map = HashMap::<Vec<u64>, u32>::new();
        let old = vec![7, 13, 0, 23, 0, 4];
        map.insert(old.clone(), 42);
        let mut key = KeyScratch::new(true, &old[..3]);
        for &word in &old[3..] { key.push(word); }
        assert_eq!(key.as_slice(), old);
        assert_eq!(map.get(key.as_slice()), Some(&42));
        assert!(!key.spilled());
        let mut changed = KeyScratch::new(true, &old[..3]);
        for &word in &[23, 1, 4] { changed.push(word); }
        assert!(map.get(changed.as_slice()).is_none());
        assert_eq!(changed.into_vec(), vec![7, 13, 0, 23, 1, 4]);
    }

    #[test]
    fn long_key_and_entries_fall_back_without_truncation() {
        let mut key = KeyScratch::new(true, &[]);
        for word in 0..(KEY_INLINE as u64 + 9) { key.push(word); }
        assert!(key.spilled());
        assert_eq!(key.as_slice().len(), KEY_INLINE + 9);
        assert_eq!(key.into_vec(), (0..KEY_INLINE as u64 + 9).collect::<Vec<_>>());
        let mut entries = EntryScratch::<u64>::new(true);
        for word in 0..(ENTRIES_INLINE as u64 + 3) { entries.push(word); }
        assert!(entries.spilled());
        assert_eq!(entries.into_vec(), (0..ENTRIES_INLINE as u64 + 3).collect::<Vec<_>>());
    }

    #[test]
    fn entry_storage_only_converts_to_owned_vec_on_miss() {
        let mut entries = EntryScratch::<String>::new(true);
        entries.push("a".into()); entries.push("b".into());
        assert_eq!(entries.len(), 2);
        assert!(!entries.spilled());
        assert_eq!(entries.into_vec(), vec!["a", "b"]);
        let mut legacy = KeyScratch::new(false, &[1, 2]);
        legacy.push(3);
        assert_eq!(legacy.into_vec(), vec![1, 2, 3]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_cache_hit_keeps_fresh_uniform_snapshot_and_reused_view_id_misses() {
        use super::super::*;
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true);
        config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let mem = SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).unwrap();
        let mut gpu = Gpu::new(mem, 0, 65536, 0, None).unwrap();
        gpu.bind_group_prepare = BindGroupPrepare::new(true);
        let scope = gpu.device.push_error_scope(ErrorFilter::Validation);

        gpu.create_buffer(1, 16, BIND_UAV, 0, 4);
        gpu.create_buffer(4, 16, BIND_UAV, 0, 4);
        gpu.create_buffer(3, 16, 4, 0, 0);
        gpu.set_obj(2, Obj::Uav(ViewObj { res: 1, is_buf: true, fmt: 28, vdim: 4,
            a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
        gpu.s.uav[2][0] = 2;
        gpu.s.cb[2][0] = Cb { buf: 3, first: 0 };
        let source = "@group(0) @binding(0) var<uniform> c: vec4u; @group(0) @binding(160) var<storage, read_write> out: array<u32>; @compute @workgroup_size(1) fn main() { out[c.y] = c.x; }";
        let module = gpu.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("bind group key fixture"), source: ShaderSource::Wgsl(source.into()) });
        let info = parse_wgsl(source);
        let mut group_bindings: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
        for binding in &info.bindings {
            group_bindings.entry(binding.group).or_default().push(binding.clone());
        }
        let program = Program { hud_kind:None, module, info, consts: None,
            pack: Some(Pack { members: vec![PackMember { binding: 0, size: 16, offset: 0 }], size: 16 }),
            text: source.into(), flat: vec![], group_bindings, variants: RefCell::new(HashMap::new()),
            label: "bind group key fixture".into(), uid: gpu.uid(), vid: false, iid: false,
            out_locs: vec![], in_locs: vec![], clip: false };
        let layout = gpu.bgl_for(&program, 2, 0);
        let pipeline_layout = gpu.device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None, bind_group_layouts: &[Some(&layout.0)], immediate_size: 0 });
        let pipeline = gpu.device.create_compute_pipeline(&ComputePipelineDescriptor {
            label: None, layout: Some(&pipeline_layout), module: &program.module,
            entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let dispatch = |gpu: &mut Gpu, bg: &BgResult| {
            let mut pass = gpu.encoder().begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg.bg, bg.dyn_off.as_ref().map(std::slice::from_ref).unwrap_or(&[]));
            pass.dispatch_workgroups(1, 1, 1);
        };
        gpu.buf_mut(3).unwrap().version += 1;
        gpu.buf_mut(3).unwrap().shadow.as_mut().unwrap()[..8].copy_from_slice(&[7, 0, 0, 0, 0, 0, 0, 0]);
        let first = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        dispatch(&mut gpu, &first);
        gpu.buf_mut(3).unwrap().version += 1;
        gpu.buf_mut(3).unwrap().shadow.as_mut().unwrap()[..8].copy_from_slice(&[11, 0, 0, 0, 1, 0, 0, 0]);
        let hit = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        dispatch(&mut gpu, &hit);
        assert_eq!(first.uid, hit.uid);
        assert_ne!(first.dyn_off, hit.dyn_off);
        let stage = &gpu.ubo_slots[gpu.ubo_slot][0].stage;
        assert_eq!(&stage[first.dyn_off.unwrap() as usize..first.dyn_off.unwrap() as usize + 4], &7u32.to_le_bytes());
        assert_eq!(&stage[hit.dyn_off.unwrap() as usize..hit.dyn_off.unwrap() as usize + 4], &11u32.to_le_bytes());

        // Creating and replacing an unrelated nonbinding object, then
        // destroying it, cannot change the cached native bind group.
        gpu.set_obj(90, Obj::Layout(Rc::new(LayoutObj { els: vec![], slots: vec![] })));
        assert_eq!(gpu.stage_bind_group(2, &program, 0, &layout).unwrap().uid, hit.uid);
        gpu.set_obj(90, Obj::Query(QueryObj { qtype: 1, life: QueryLife::new(0), slots: vec![] }));
        assert_eq!(gpu.stage_bind_group(2, &program, 0, &layout).unwrap().uid, hit.uid);
        gpu.destroy(90);
        assert_eq!(gpu.stage_bind_group(2, &program, 0, &layout).unwrap().uid, hit.uid);

        // The opposite replacement direction matters too: a nonbinding
        // object taking a live view's ID must discard its old native binding.
        gpu.set_obj(2, Obj::Layout(Rc::new(LayoutObj { els: vec![], slots: vec![] })));
        let missing = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        assert_ne!(missing.uid, hit.uid);

        // Rebind the same guest view ID to a different native resource. The
        // borrowed lookup key is intentionally identical, but the cache must
        // have been invalidated before binding it.
        gpu.set_obj(2, Obj::Uav(ViewObj { res: 4, is_buf: true, fmt: 28, vdim: 4,
            a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
        gpu.buf_mut(3).unwrap().version += 1;
        gpu.buf_mut(3).unwrap().shadow.as_mut().unwrap()[..8].copy_from_slice(&[13, 0, 0, 0, 0, 0, 0, 0]);
        let replaced = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        dispatch(&mut gpu, &replaced);
        assert_ne!(hit.uid, replaced.uid);

        // A live view can initially point at an absent resource. Its guest
        // view ID stays fixed when the real buffer is created later.
        gpu.set_obj(5, Obj::Uav(ViewObj { res: 6, is_buf: true, fmt: 28, vdim: 4,
            a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
        gpu.s.uav[2][0] = 5;
        let dummy = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        gpu.create_buffer(6, 16, BIND_UAV, 0, 4);
        gpu.buf_mut(3).unwrap().version += 1;
        gpu.buf_mut(3).unwrap().shadow.as_mut().unwrap()[..8].copy_from_slice(&[17, 0, 0, 0, 0, 0, 0, 0]);
        let resolved = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        assert_ne!(dummy.uid, resolved.uid);
        dispatch(&mut gpu, &resolved);

        // Counter bindings have the same absent-resource transition, but
        // materialize a separate native counter buffer when the buffer arrives.
        gpu.set_obj(7, Obj::Uav(ViewObj { res: 8, is_buf: true, fmt: 28, vdim: 4,
            a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
        gpu.s.uav[2][0] = 7;
        let counter_source = "@group(0) @binding(176) var<storage, read_write> count: array<u32>; @compute @workgroup_size(1) fn main() { count[0] = 19u; }";
        let counter_module = gpu.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("bind group counter fixture"), source: ShaderSource::Wgsl(counter_source.into()) });
        let counter_info = parse_wgsl(counter_source);
        let mut counter_groups: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
        for binding in &counter_info.bindings {
            counter_groups.entry(binding.group).or_default().push(binding.clone());
        }
        let counter_program = Program { hud_kind:None, module: counter_module, info: counter_info, consts: None,
            pack: None, text: counter_source.into(), flat: vec![], group_bindings: counter_groups,
            variants: RefCell::new(HashMap::new()), label: "bind group counter fixture".into(),
            uid: gpu.uid(), vid: false, iid: false, out_locs: vec![], in_locs: vec![], clip: false };
        let counter_layout = gpu.bgl_for(&counter_program, 2, 0);
        let counter_dummy = gpu.stage_bind_group(2, &counter_program, 0, &counter_layout).unwrap();
        gpu.create_buffer(8, 16, BIND_UAV, 0, 4);
        let counter_real = gpu.stage_bind_group(2, &counter_program, 0, &counter_layout).unwrap();
        assert_ne!(counter_dummy.uid, counter_real.uid);
        let counter_pipeline_layout = gpu.device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None, bind_group_layouts: &[Some(&counter_layout.0)], immediate_size: 0 });
        let counter_pipeline = gpu.device.create_compute_pipeline(&ComputePipelineDescriptor {
            label: None, layout: Some(&counter_pipeline_layout), module: &counter_program.module,
            entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        { let mut pass = gpu.encoder().begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&counter_pipeline);
            pass.set_bind_group(0, &counter_real.bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        let source1 = gpu.buf(1).unwrap().gpu.clone().unwrap();
        let source4 = gpu.buf(4).unwrap().gpu.clone().unwrap();
        let source6 = gpu.buf(6).unwrap().gpu.clone().unwrap();
        let counter8 = gpu.buf(8).unwrap().counter.clone().unwrap();
        let output = gpu.device.create_buffer(&BufferDescriptor { label: None, size: 64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        gpu.encoder().copy_buffer_to_buffer(&source1, 0, &output, 0, 16);
        gpu.encoder().copy_buffer_to_buffer(&source4, 0, &output, 16, 16);
        gpu.encoder().copy_buffer_to_buffer(&source6, 0, &output, 32, 16);
        gpu.encoder().copy_buffer_to_buffer(&counter8, 0, &output, 48, 16);
        let (tx, rx) = std::sync::mpsc::channel();
        gpu.encoder().map_buffer_on_submit(&output, MapMode::Read, .., move |r| { let _ = tx.send(r); });
        gpu.flush();
        gpu.device.poll(PollType::Wait { submission_index: gpu.last_submit.clone(),
            timeout: Some(Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        let bytes = output.slice(..).get_mapped_range().unwrap();
        let words: Vec<u32> = bytes.chunks_exact(4).map(|v| u32::from_le_bytes(v.try_into().unwrap())).collect();
        assert_eq!(&words[..4], &[7, 11, 0, 0]);
        assert_eq!(&words[4..8], &[13, 0, 0, 0]);
        assert_eq!(&words[8..12], &[17, 0, 0, 0]);
        assert_eq!(words[12], 19);
        drop(bytes); output.unmap();
        let metrics = gpu.bind_group_prepare.metrics();
        assert_eq!(metrics["hits"], 4);
        assert_eq!(metrics["misses"], 7);
        assert_eq!(metrics["owned_entry_materializations"], 7);
        assert!(metrics["cache_invalidations"].as_u64().unwrap() >= 1);
        assert_eq!(metrics["skipped_nonbinding_set_obj"]["layout"], 1);
        assert_eq!(metrics["skipped_nonbinding_set_obj"]["query"], 1);
        assert_eq!(metrics["skipped_nonbinding_destroy"]["query"], 1);
        assert_eq!(metrics["key_spills"], 0);
        assert_eq!(metrics["entry_spills"], 0);
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "{error:?}");
    }
}

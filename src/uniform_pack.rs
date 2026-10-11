//! Opt-in direct packing into persistent uniform staging memory.
//! Every binding call still takes a fresh snapshot of host-owned CB shadows.
//! This switch removes only the temporary zeroed block and its second copy.
use super::{Buf, Cb, Obj, Pack};
use std::time::Instant;

#[derive(Default)]
struct Counts {
    calls: u64,
    pack_bytes: u64,
    member_slots: u64,
    copied_member_bytes: u64,
    zero_initialized_bytes: u64,
    legacy_calls: u64,
    direct_calls: u64,
    legacy_temp_alloc_sites: u64,
    avoided_temp_alloc_sites: u64,
    legacy_second_copy_bytes: u64,
    avoided_second_copy_bytes: u64,
    direct_preflight_member_visits: u64,
    stage_grows: u64,
    rejected_oversize: u64,
}

pub(super) struct UniformPack {
    pub(super) enabled: bool,
    epoch: Instant,
    counts: Counts,
}

impl UniformPack {
    pub(super) fn from_environment() -> Self {
        Self::new(crate::draw_setup::uniform_stage_pack_enabled())
    }

    fn new(enabled: bool) -> Self {
        Self { enabled, epoch: Instant::now(), counts: Counts::default() }
    }

    pub(super) fn rejected_oversize(&mut self) {
        self.counts.rejected_oversize = self.counts.rejected_oversize.saturating_add(1);
    }

    pub(super) fn record(&mut self, pack: &Pack, copied: u64, grew: bool) {
        let c = &mut self.counts;
        c.calls = c.calls.saturating_add(1);
        c.pack_bytes = c.pack_bytes.saturating_add(pack.size);
        c.member_slots = c.member_slots.saturating_add(pack.members.len() as u64);
        c.copied_member_bytes = c.copied_member_bytes.saturating_add(copied);
        c.zero_initialized_bytes = c.zero_initialized_bytes.saturating_add(pack.size);
        c.stage_grows = c.stage_grows.saturating_add(u64::from(grew));
        if self.enabled {
            c.direct_calls = c.direct_calls.saturating_add(1);
            c.avoided_temp_alloc_sites = c.avoided_temp_alloc_sites.saturating_add(u64::from(pack.size != 0));
            c.avoided_second_copy_bytes = c.avoided_second_copy_bytes.saturating_add(pack.size);
            c.direct_preflight_member_visits = c.direct_preflight_member_visits.saturating_add(pack.members.len() as u64);
        } else {
            c.legacy_calls = c.legacy_calls.saturating_add(1);
            c.legacy_temp_alloc_sites = c.legacy_temp_alloc_sites.saturating_add(u64::from(pack.size != 0));
            c.legacy_second_copy_bytes = c.legacy_second_copy_bytes.saturating_add(pack.size);
        }
    }

    pub(super) fn metrics(&self, presents: u64, draws: u64, submits: u64, errors: u64) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        out.insert("schema".into(), 1.into());
        out.insert("enabled".into(), self.enabled.into());
        out.insert("cumulative".into(), true.into());
        out.insert("elapsed_s".into(), self.epoch.elapsed().as_secs_f64().into());
        out.insert("counter_scope".into(), "successful pack_copy source sites and logical bytes; not allocator samples or GPU timing".into());
        for (name, value) in [
            ("presents", presents), ("draws", draws), ("submits", submits), ("errors", errors),
            ("calls", self.counts.calls), ("pack_bytes", self.counts.pack_bytes),
            ("member_slots", self.counts.member_slots), ("copied_member_bytes", self.counts.copied_member_bytes),
            ("zero_initialized_bytes", self.counts.zero_initialized_bytes),
            ("legacy_calls", self.counts.legacy_calls), ("direct_calls", self.counts.direct_calls),
            ("legacy_temp_alloc_sites", self.counts.legacy_temp_alloc_sites),
            ("avoided_temp_alloc_sites", self.counts.avoided_temp_alloc_sites),
            ("legacy_second_copy_bytes", self.counts.legacy_second_copy_bytes),
            ("avoided_second_copy_bytes", self.counts.avoided_second_copy_bytes),
            ("direct_preflight_member_visits", self.counts.direct_preflight_member_visits),
            ("stage_grows", self.counts.stage_grows), ("rejected_oversize", self.counts.rejected_oversize),
        ] { out.insert(name.into(), value.into()); }
        serde_json::Value::Object(out)
    }
}

// A separate preflight is intentionally conservative. The reference writes a
// temporary block, so malformed member/shadow bounds fail before stage growth
// or mutation. Check the same effective ranges in member order before touching
// the persistent stage. No object can change between these two synchronous
// loops: they borrow the GPU worker's immutable host metadata and shadows.
// This extra lookup pass is a CPU tradeoff to measure, not a claimed speedup.
pub(super) fn direct_copy(pack: &Pack, objects: &[Option<Obj>], bindings: &[Cb], off: u64, stage: &mut Vec<u8>) -> (u64, bool) {
    let mut copied = 0u64;
    for m in &pack.members {
        let (buf, first) = bindings.get(m.binding as usize).map(|cb| (cb.buf, cb.first)).unwrap_or_default();
        if let Some(Obj::Buf(Buf { shadow: Some(sh), size, .. })) = objects.get(buf as usize).and_then(Option::as_ref) {
            let from = first as u64 * 16;
            let len = m.size.min(size.saturating_sub(from));
            if len > 0 {
                let start = m.offset as usize;
                let end = (m.offset + len) as usize;
                assert!(start <= end && end <= pack.size as usize, "packed member exceeds packed block");
                let _ = &sh[from as usize..(from + len) as usize];
                copied = copied.saturating_add(len);
            }
        }
    }

    let end = (off + pack.size) as usize;
    let grew = stage.len() < end;
    if grew { stage.resize((end.max(262144)).next_power_of_two().min(super::RING_CHUNK as usize), 0); }
    let block = &mut stage[off as usize..end];
    block.fill(0);
    for m in &pack.members {
        let (buf, first) = bindings.get(m.binding as usize).map(|cb| (cb.buf, cb.first)).unwrap_or_default();
        if let Some(Obj::Buf(Buf { shadow: Some(sh), size, .. })) = objects.get(buf as usize).and_then(Option::as_ref) {
            let from = first as u64 * 16;
            let len = m.size.min(size.saturating_sub(from));
            if len > 0 {
                block[m.offset as usize..(m.offset + len) as usize].copy_from_slice(&sh[from as usize..(from + len) as usize]);
            }
        }
    }
    (copied, grew)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::*;

    fn shadow(bytes: &[u8]) -> Obj {
        Obj::Buf(Buf { size: bytes.len() as u64, stride: 0, gpu: None, bytes: 0, shadow: Some(bytes.to_vec()),
            version: 0, enc: 0, in_bg: false, counter: None })
    }

    #[test]
    fn direct_pack_zeroes_exact_range_and_keeps_member_order_and_tails() {
        let objects = vec![None, Some(shadow(&[9; 20])), Some(shadow(&[7; 18])),
            Some(Obj::Query(QueryObj { qtype: 0, life: QueryLife::new(0), slots: vec![] }))];
        let bindings = [Cb { buf: 1, first: 1 }, Cb { buf: 2, first: 1 }, Cb { buf: 3, first: 0 }, Cb { buf: 1, first: 9 }];
        let pack = Pack { size: 32, members: vec![
            PackMember { binding: 0, size: 16, offset: 0 },
            PackMember { binding: 1, size: 16, offset: 8 },
            PackMember { binding: 2, size: 4, offset: 24 },
            PackMember { binding: 3, size: 4, offset: 28 },
            PackMember { binding: 99, size: 4, offset: 28 },
            // Ordered overlaps must remain last-write-wins, as in the reference.
            PackMember { binding: 1, size: 2, offset: 2 },
        ] };
        let mut stage = vec![0xd7; 96];
        assert_eq!(direct_copy(&pack, &objects, &bindings, 16, &mut stage), (8, false));
        assert_eq!(&stage[..16], &[0xd7; 16]); assert_eq!(&stage[48..], &[0xd7; 48]);
        let mut golden = [0u8; 32]; golden[..4].copy_from_slice(&[9, 9, 7, 7]); golden[8..10].copy_from_slice(&[7, 7]);
        assert_eq!(&stage[16..48], &golden);
        let empty = Pack { size: 0, members: vec![] };
        assert_eq!(direct_copy(&empty, &objects, &bindings, 16, &mut stage), (0, false));
        assert_eq!(&stage[16..48], &golden);
    }

    #[test]
    fn direct_pack_preflight_keeps_stage_unchanged_on_bad_ranges() {
        let objects = vec![None, Some(shadow(&[1; 32]))];
        let bindings = [Cb { buf: 1, first: 0 }];
        let pack = Pack { size: 8, members: vec![PackMember { binding: 0, size: 16, offset: 0 }] };
        let mut stage = vec![0xaf; 32]; let original = stage.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| direct_copy(&pack, &objects, &bindings, 256, &mut stage))).is_err());
        assert_eq!(stage, original);
        // Deliberately inconsistent synthetic shadow is a failure-path check,
        // not a legal game resource. The reference's source slice also panics.
        let bad = vec![None, Some(Obj::Buf(Buf { size: 32, stride: 0, gpu: None, bytes: 0,
            shadow: Some(vec![1; 4]), version: 0, enc: 0, in_bg: false, counter: None }))];
        let legal_destination = Pack { size: 16, members: vec![PackMember { binding: 0, size: 16, offset: 0 }] };
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| direct_copy(&legal_destination, &bad, &bindings, 256, &mut stage))).is_err());
        assert_eq!(stage, original);
    }

    #[test]
    fn direct_pack_growth_and_smaller_reused_snapshot_zero_only_new_pack() {
        let objects = vec![None, Some(shadow(&[5; 32]))];
        let bindings = [Cb { buf: 1, first: 0 }];
        let big = Pack { size: 64, members: vec![PackMember { binding: 0, size: 32, offset: 0 }] };
        let mut stage = vec![];
        assert_eq!(direct_copy(&big, &objects, &bindings, 0, &mut stage), (32, true));
        assert_eq!(stage.len(), 262144); assert_eq!(&stage[..32], &[5; 32]); assert_eq!(&stage[32..64], &[0; 32]);
        stage[16..64].fill(0xb4);
        let small = Pack { size: 16, members: vec![] };
        assert_eq!(direct_copy(&small, &objects, &bindings, 0, &mut stage), (0, false));
        assert_eq!(&stage[..16], &[0; 16]); assert_eq!(&stage[16..64], &[0xb4; 48]);
        let mut off = UniformPack::new(false); let mut on = UniformPack::new(true);
        off.record(&big, 32, true); on.record(&big, 32, true);
        off.record(&small, 0, false); on.record(&small, 0, false);
        let a = off.metrics(0, 0, 0, 0); let b = on.metrics(0, 0, 0, 0);
        assert_eq!(a["pack_bytes"], b["pack_bytes"]); assert_eq!(a["copied_member_bytes"], b["copied_member_bytes"]);
        assert_eq!(a["legacy_temp_alloc_sites"], 2); assert_eq!(b["avoided_temp_alloc_sites"], 2);
        assert_eq!(a["legacy_second_copy_bytes"], 80); assert_eq!(b["avoided_second_copy_bytes"], 80);
    }

    #[cfg(target_os = "macos")]
    fn gpu() -> Gpu {
        let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let mem = SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).unwrap();
        let g = Gpu::new(mem, 0, 65536, 0, None).unwrap();
        assert_eq!(g.uniform_pack.enabled, std::env::var("MUGUET_UNIFORM_STAGE_PACK_OPT").as_deref() == Ok("1"));
        g
    }

    // Tests are run serially. Explicitly invalidate any XR provenance left by
    // an earlier test before exercising production present(), and restore its
    // global counter even if a fixture assertion unwinds.
    #[cfg(target_os = "macos")]
    struct HeadlessPresentIsolation(u64);

    #[cfg(target_os = "macos")]
    impl HeadlessPresentIsolation {
        fn new() -> Self {
            assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(0), 0);
            Self(PRESENTS.swap(0, Ordering::Relaxed))
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for HeadlessPresentIsolation {
        fn drop(&mut self) {
            crate::xr_view::sv_xr_world_camera_set_enabled(0);
            PRESENTS.store(self.0, Ordering::Relaxed);
        }
    }

    #[cfg(target_os = "macos")]
    const SOURCE: &str = r#"
struct Data { words: array<vec4<u32>, 2>, }
struct Control { words: array<vec4<u32>, 1>, }
@group(0) @binding(0) var<uniform> a: Data;
@group(0) @binding(1) var<uniform> b: Data;
@group(0) @binding(2) var<uniform> control: Control;
@group(0) @binding(160) var<storage, read_write> output: array<vec4<u32>>;
@compute @workgroup_size(1)
fn main() {
    let at = control.words[0].x * 5u;
    output[at + 0u] = a.words[0]; output[at + 1u] = a.words[1];
    output[at + 2u] = b.words[0]; output[at + 3u] = b.words[1];
    output[at + 4u] = control.words[0];
}
"#;

    #[cfg(target_os = "macos")]
    fn fixture(g: &mut Gpu, rows: u32) -> (Program, (BindGroupLayout, u64), ComputePipeline) {
        g.create_buffer(1, rows * 80, BIND_UAV, 0, 16);
        g.set_obj(2, Obj::Uav(ViewObj { res: 1, is_buf: true, fmt: 28, vdim: 4, a: 0, b: rows * 5, c: 0, d: 0, views: HashMap::new() }));
        g.s.uav[2][0] = 2;
        let output = g.buf(1).unwrap().gpu.clone().unwrap();
        g.queue.write_buffer(&output, 0, &vec![0xde, 0xad, 0xbe, 0xef].repeat(rows as usize * 20));
        let (text, pack) = pack_uniforms(SOURCE);
        let pack = pack.unwrap(); assert_eq!(pack.size, 80);
        assert_eq!(pack.members.iter().map(|m| (m.binding, m.size, m.offset)).collect::<Vec<_>>(), [(0, 32, 0), (1, 32, 32), (2, 16, 64)]);
        let module = g.device.create_shader_module(ShaderModuleDescriptor { label: Some("uniform stage pack fixture"), source: ShaderSource::Wgsl(text.clone().into()) });
        let info = parse_wgsl(&text); let mut group_bindings: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
        for binding in &info.bindings { group_bindings.entry(binding.group).or_default().push(binding.clone()); }
        for bindings in group_bindings.values_mut() { bindings.sort_by_key(|binding| binding.binding); }
        let p = Program { module, info, consts: None, pack: Some(pack), text, flat: vec![], group_bindings,
            variants: RefCell::new(HashMap::new()), label: "uniform stage pack fixture".into(), uid: g.uid(),
            vid: false, iid: false, out_locs: vec![], in_locs: vec![], clip: false };
        let bgl = g.bgl_for(&p, 2, 0);
        let layout = g.device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl.0)], immediate_size: 0 });
        let pipeline = g.device.create_compute_pipeline(&ComputePipelineDescriptor { label: None, layout: Some(&layout), module: &p.module,
            entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        (p, bgl, pipeline)
    }

    #[cfg(target_os = "macos")]
    fn put(g: &mut Gpu, id: u32, size: u32, first: u32, values: &[u32]) {
        g.create_buffer(id, size, 4, 0, 0);
        overwrite(g, id, first, values);
    }

    #[cfg(target_os = "macos")]
    fn overwrite(g: &mut Gpu, id: u32, first: u32, values: &[u32]) {
        let shadow = g.buf_mut(id).unwrap().shadow.as_mut().unwrap(); shadow.fill(0x9b);
        for (at, word) in values.iter().enumerate() { let at = first as usize * 16 + at * 4; shadow[at..at + 4].copy_from_slice(&word.to_le_bytes()); }
    }

    #[cfg(target_os = "macos")]
    fn control(g: &mut Gpu, row: u32, a: u32, b: u32, c: u32) {
        put(g, 9, 16, 0, &[row, a, b, c]); g.s.cb[2][2] = Cb { buf: 9, first: 0 };
    }

    #[cfg(target_os = "macos")]
    fn dispatch(g: &mut Gpu, p: &Program, bgl: &(BindGroupLayout, u64), pipeline: &ComputePipeline) -> BgResult {
        let bg = g.stage_bind_group(2, p, 0, bgl).unwrap();
        { let mut pass = g.encoder().begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(pipeline); pass.set_bind_group(0, &bg.bg, bg.dyn_off.as_ref().map(std::slice::from_ref).unwrap_or(&[]));
            pass.dispatch_workgroups(1, 1, 1); }
        bg
    }

    #[cfg(target_os = "macos")]
    fn read_output(g: &mut Gpu, bytes: u64) -> Vec<u32> {
        let src = g.buf(1).unwrap().gpu.clone().unwrap();
        let dst = g.device.create_buffer(&BufferDescriptor { label: None, size: bytes, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        g.encoder().copy_buffer_to_buffer(&src, 0, &dst, 0, bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        g.encoder().map_buffer_on_submit(&dst, MapMode::Read, .., move |result| { let _ = tx.send(result); });
        g.flush(); g.device.poll(PollType::Wait { submission_index: g.last_submit.clone(), timeout: Some(Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        let result: Vec<u32> = dst.slice(..).get_mapped_range().unwrap().chunks_exact(4).map(|v| u32::from_le_bytes(v.try_into().unwrap())).collect();
        dst.unmap();
        eprintln!("uniform-pack-readback {}", serde_json::json!({"bytes": bytes, "words": result,
            "metrics": g.uniform_pack.metrics(g.presents, g.draws, g.submits, g.errors.load(Ordering::Relaxed))}));
        result
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_same_submission_keeps_two_changed_shadow_snapshots_on_cache_hit() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        let (p, bgl, pipeline) = fixture(&mut g, 2);
        put(&mut g, 3, 48, 1, &[101, 102, 103, 104, 105, 106, 107, 108]);
        put(&mut g, 4, 48, 1, &[201, 202, 203, 204, 205, 206, 207, 208]);
        g.s.cb[2][0] = Cb { buf: 3, first: 1 }; g.s.cb[2][1] = Cb { buf: 4, first: 1 };
        control(&mut g, 0, 301, 302, 303);
        let a = dispatch(&mut g, &p, &bgl, &pipeline); let saved = g.ubo_slots[0][0].stage[..80].to_vec();
        overwrite(&mut g, 3, 1, &[401, 402, 403, 404, 405, 406, 407, 408]);
        put(&mut g, 5, 64, 2, &[501, 502, 503, 504, 505, 506, 507, 508]);
        g.s.cb[2][1] = Cb { buf: 5, first: 2 }; control(&mut g, 1, 601, 602, 603);
        let b = dispatch(&mut g, &p, &bgl, &pipeline);
        assert_eq!(g.submits, 0); assert_eq!(a.uid, b.uid); assert_eq!(a.dyn_off, Some(0)); assert_eq!(b.dyn_off, Some(256));
        assert_eq!(g.ubo_off, 512); assert_eq!(&g.ubo_slots[0][0].stage[..80], saved.as_slice());
        assert_eq!((g.ubo_slots[0][0].lo, g.ubo_slots[0][0].hi), (0, 336));
        assert_eq!(read_output(&mut g, 160), [101,102,103,104,105,106,107,108,201,202,203,204,205,206,207,208,0,301,302,303,
            401,402,403,404,405,406,407,408,501,502,503,504,505,506,507,508,1,601,602,603]);
        let metrics = g.uniform_pack.metrics(g.presents, g.draws, g.submits, 0);
        assert_eq!(metrics["calls"], 2); assert_eq!(metrics["pack_bytes"], 160);
        assert_eq!(metrics["direct_calls"].as_u64().unwrap() > 0, g.uniform_pack.enabled);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_completed_frame_slot_reuse_clears_old_members_and_truncated_tails() {
        let _present_isolation = HeadlessPresentIsolation::new();
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        let (p, bgl, pipeline) = fixture(&mut g, 4);
        put(&mut g, 3, 48, 1, &[101,102,103,104,105,106,107,108]);
        put(&mut g, 4, 48, 1, &[201,202,203,204,205,206,207,208]);
        g.s.cb[2][0] = Cb { buf: 3, first: 1 }; g.s.cb[2][1] = Cb { buf: 4, first: 1 }; control(&mut g, 0, 301,302,303);
        let a = dispatch(&mut g, &p, &bgl, &pipeline); let chunk_uid = g.ubo_slots[0][0].id;
        assert_eq!(a.dyn_off, Some(0)); g.present(0);
        assert!(g.frame_slot_done[0].is_some()); assert_eq!(g.ubo_slot, 1);
        overwrite(&mut g, 3, 1, &[401,402,403,404,405,406,407,408]);
        put(&mut g, 5, 64, 2, &[501,502,503,504,505,506,507,508]);
        g.s.cb[2][1] = Cb { buf: 5, first: 2 }; control(&mut g, 1, 601,602,603);
        let b = dispatch(&mut g, &p, &bgl, &pipeline); assert_eq!(b.dyn_off, Some(RING_CHUNK as u32)); g.present(0);
        assert!(g.frame_slot_done[0].is_some() && g.frame_slot_done[1].is_some()); assert_eq!(g.ubo_slot, 2);
        g.s.cb[2][0] = Cb::default(); put(&mut g, 6, 24, 1, &[701,702]);
        g.s.cb[2][1] = Cb { buf: 6, first: 1 }; control(&mut g, 2, 801,802,803);
        let c = dispatch(&mut g, &p, &bgl, &pipeline); assert_eq!(c.dyn_off, Some((2 * RING_CHUNK) as u32));
        // Third consumed slot has real work. Production present submits it,
        // records slot2, then takes/waits the recorded slot0 submission.
        assert!(g.frame_slot_done[0].is_some()); g.present(0);
        assert!(g.frame_slot_done[2].is_some()); assert!(g.frame_slot_done[0].is_none()); assert_eq!(g.ubo_slot, 0);
        assert_eq!(g.thread_wall[THREAD_UNIFORM_WAIT].calls, 1);
        g.s.cb[2][0] = Cb { buf: 3, first: 3 };
        g.buf_mut(6).unwrap().shadow = None; control(&mut g, 3, 901,902,903);
        assert!(g.ubo_slots[0][0].stage[..64].iter().any(|&v| v != 0));
        let d = dispatch(&mut g, &p, &bgl, &pipeline); assert_eq!(d.dyn_off, Some(0));
        assert_eq!([a.uid,b.uid,c.uid,d.uid], [a.uid; 4]); assert_eq!(g.ubo_slots[0][0].id, chunk_uid);
        assert_eq!(&g.ubo_slots[0][0].stage[..64], &[0;64]);
        assert_eq!(read_output(&mut g, 320), [101,102,103,104,105,106,107,108,201,202,203,204,205,206,207,208,0,301,302,303,
            401,402,403,404,405,406,407,408,501,502,503,504,505,506,507,508,1,601,602,603,
            0,0,0,0,0,0,0,0,701,702,0,0,0,0,0,0,2,801,802,803,
            0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,3,901,902,903]);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_chunk_headroom_boundary_preserves_snapshots_and_dirty_ranges() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        let (p, bgl, pipeline) = fixture(&mut g, 2);
        put(&mut g, 3, 32, 0, &[1,2,3,4,5,6,7,8]); put(&mut g, 4, 32, 0, &[9,10,11,12,13,14,15,16]);
        g.s.cb[2][0] = Cb { buf: 3, first: 0 }; g.s.cb[2][1] = Cb { buf: 4, first: 0 };
        // Create the original chunk before arranging its near-end cursor.
        let (ci, _) = g.uniform_alloc(0); assert_eq!(ci, 0);
        let last = RING_CHUNK - UBO_WINDOW - 256; g.ubo_off = last; control(&mut g, 0, 17,18,19);
        let a = dispatch(&mut g, &p, &bgl, &pipeline);
        assert_eq!(a.dyn_off, Some(last as u32)); assert_eq!(g.ubo_chunk, 0);
        assert_eq!((g.ubo_slots[0][0].lo, g.ubo_slots[0][0].hi), (last, last + 80));
        overwrite(&mut g, 3, 0, &[21,22,23,24,25,26,27,28]); control(&mut g, 1, 37,38,39);
        let b = dispatch(&mut g, &p, &bgl, &pipeline);
        assert_eq!(g.ubo_chunk, 1); assert_eq!(b.dyn_off, Some(0)); assert_ne!(a.uid, b.uid);
        assert_ne!(g.ubo_slots[0][0].id, g.ubo_slots[0][1].id);
        assert_eq!((g.ubo_slots[0][1].lo, g.ubo_slots[0][1].hi), (0,80));
        assert_eq!(read_output(&mut g, 160), [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,0,17,18,19,
            21,22,23,24,25,26,27,28,9,10,11,12,13,14,15,16,1,37,38,39]);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pack_limits_and_stage_selection_keep_rejection_and_allocator_effects() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        let (mut p, bgl, _) = fixture(&mut g, 1);
        for (stage, id, value) in [(0,3,11u8),(1,4,22),(2,5,33)] {
            g.create_buffer(id, 16, 4, 0, 0); g.buf_mut(id).unwrap().shadow.as_mut().unwrap().fill(value);
            g.s.cb[stage][0] = Cb { buf: id, first: 0 };
        }
        p.pack = Some(Pack { size: 16, members: vec![PackMember { binding: 0, size: 16, offset: 0 }] });
        for (stage, expected) in [(0,11u8),(1,22),(2,33)] {
            let (ci, off) = g.pack_copy(&p, stage).unwrap();
            assert_eq!(&g.ubo_slots[0][ci].stage[off as usize..off as usize+16], &[expected;16]);
        }
        let cursor = (g.ubo_chunk,g.ubo_off); let dirty = (g.ubo_slots[0][0].lo,g.ubo_slots[0][0].hi);
        let prior = g.ubo_slots[0][0].stage.clone(); p.pack = None; assert!(g.pack_copy(&p,2).is_none());
        p.pack = Some(Pack { size: UBO_WINDOW+1, members: vec![] }); assert!(g.pack_copy(&p,2).is_none());
        assert_eq!((g.ubo_chunk,g.ubo_off), cursor); assert_eq!((g.ubo_slots[0][0].lo,g.ubo_slots[0][0].hi), dirty);
        assert_eq!(g.ubo_slots[0][0].stage, prior);
        p.pack = Some(Pack { size: UBO_WINDOW, members: vec![PackMember { binding: 0, size: 16, offset: UBO_WINDOW-16 }] });
        let (ci,off) = g.pack_copy(&p,2).unwrap(); let block = &g.ubo_slots[0][ci].stage[off as usize..(off+UBO_WINDOW) as usize];
        assert!(block[..UBO_WINDOW as usize-16].iter().all(|&v| v==0)); assert_eq!(&block[UBO_WINDOW as usize-16..], &[33;16]);
        assert_eq!(g.ubo_off, cursor.1+UBO_WINDOW);
        // The original stage loop packs binding0 before failing binding15.
        // A missing constant table must not move packing behind the failure.
        p.pack = Some(Pack { size: 16, members: vec![PackMember { binding: 0, size: 16, offset: 0 }] });
        let mut constants = p.info.bindings.iter().find(|b| b.kind == Kind::Uniform).unwrap().clone();
        constants.binding = 15; p.group_bindings.get_mut(&0).unwrap().insert(1, constants);
        let before = g.ubo_off; assert!(g.stage_bind_group(2, &p, 0, &bgl).is_none());
        assert_eq!(g.ubo_off, before+256); assert_eq!(&g.ubo_slots[0][g.ubo_chunk].stage[before as usize..before as usize+16], &[33;16]);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }
}

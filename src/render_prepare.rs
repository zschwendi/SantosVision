//! Allocation-free alternatives for immutable render preparation metadata.
//! Counters describe executed source sites, not allocator samples or GPU time.
use super::{Binding, Ds, PassInfo, Rt};
use std::{borrow::Cow, time::Instant};

#[derive(PartialEq, Eq, Debug)]
pub(super) enum PassKey {
    Legacy(Vec<u32>),
    Fixed([u32; 37]),
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::*;

    #[test]
    fn fixed_pass_key_preserves_all_target_words_and_independent_depth_flags() {
        let rt: Vec<Rt> = (0..8).map(|i| Rt { tex: i * 7, mip: i % 3, slice: i + 9, fmt: i + 20 }).collect();
        let ds = Ds { tex: 91, mip: 2, slice: 3, fmt: 40, flags: 0 };
        let mut old = RenderPrepare::new(false); let mut new = RenderPrepare::new(true);
        let baseline = old.pass_key(&rt, &ds);
        assert_eq!(baseline.words(), new.pass_key(&rt, &ds).words());
        for slot in 0..8 {
            for field in 0..4 {
                let mut changed = rt.clone(); let r = &mut changed[slot];
                match field { 0 => r.tex ^= 0x1000, 1 => r.mip ^= 1, 2 => r.slice ^= 1, _ => r.fmt ^= 1 }
                let key = new.pass_key(&changed, &ds);
                assert_eq!(old.pass_key(&changed, &ds).words(), key.words());
                assert_ne!(baseline.words(), key.words());
            }
        }
        for field in 0..5 {
            let mut changed = ds.clone();
            match field { 0 => changed.tex += 1, 1 => changed.mip += 1, 2 => changed.slice += 1, 3 => changed.fmt += 1, _ => changed.flags = 1 }
            assert_eq!(old.pass_key(&rt, &changed).words(), new.pass_key(&rt, &changed).words());
            assert_ne!(baseline.words(), new.pass_key(&rt, &changed).words());
        }
        for flags in [1, 2, 3] {
            let mut changed = ds.clone(); changed.flags = flags;
            assert_eq!(new.pass_key(&rt, &changed).words()[36], flags);
        }
    }

    #[test]
    fn borrowed_descriptors_preserve_order_and_scalar_depth_metadata() {
        let info = parse_wgsl("@group(0) @binding(161) var a: texture_storage_2d<rgba8unorm, write>;\n@group(0) @binding(15) var<uniform> b: vec4u;");
        let mut old = RenderPrepare::new(false); let mut new = RenderPrepare::new(true);
        let cloned = old.bindings(&info.bindings); let borrowed = new.bindings(&info.bindings);
        assert!(matches!(&cloned, Cow::Owned(_))); assert!(matches!(&borrowed, Cow::Borrowed(_)));
        assert_eq!(borrowed.as_ptr(), info.bindings.as_ptr()); assert_ne!(cloned.as_ptr(), info.bindings.as_ptr());
        assert_eq!(borrowed[0].format_name.as_ptr(), info.bindings[0].format_name.as_ptr());
        assert_ne!(cloned[0].format_name.as_ptr(), info.bindings[0].format_name.as_ptr());
        for (a, b) in cloned.iter().zip(borrowed.iter()) { assert_eq!(format!("{a:?}"), format!("{b:?}")); }
        let pi = PassInfo { rt_formats: vec![None, Some(TextureFormat::Rgba8Unorm)], samples: 1,
            ds_format: Some(TextureFormat::Depth32Float), ro_depth: true, ro_stencil: false, width: 319, height: 177 };
        assert_eq!(old.dimensions(&pi), (319, 177, true)); assert_eq!(new.dimensions(&pi), (319, 177, true));
        let a = old.metrics(0, 0, 0, 0, 0); let b = new.metrics(0, 0, 0, 0, 0);
        assert_eq!(a["descriptor_string_clone_sites"], 1); assert_eq!(b["descriptor_string_clone_sites_avoided"], 1);
        assert_eq!(a["rt_format_vec_clone_sites"], 1); assert_eq!(b["rt_format_vec_clone_sites_avoided"], 1);
    }

    #[cfg(target_os = "macos")]
    fn gpu() -> Gpu {
        let mut config = wasmtime::Config::new(); config.wasm_threads(true); config.shared_memory(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let mem = SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).unwrap();
        Gpu::new(mem, 0, 65536, 0, None).unwrap()
    }

    #[cfg(target_os = "macos")]
    fn program(g: &mut Gpu, source: &str, constants: Option<[u32; 4]>) -> Program {
        let module = g.device.create_shader_module(ShaderModuleDescriptor { label: Some("same label"), source: ShaderSource::Wgsl(source.into()) });
        let info = parse_wgsl(source); let mut groups: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
        for b in &info.bindings { groups.entry(b.group).or_default().push(b.clone()); }
        let consts = constants.map(|words| {
            let buffer = g.device.create_buffer(&BufferDescriptor { label: None, size: 16, usage: BufferUsages::UNIFORM, mapped_at_creation: true });
            { let mut mapped = buffer.slice(..).get_mapped_range_mut().unwrap();
                let mut bytes = [0; 16];
                for (bytes, word) in bytes.chunks_exact_mut(4).zip(words) { bytes.copy_from_slice(&word.to_le_bytes()); }
                mapped.copy_from_slice(&bytes); }
            buffer.unmap(); buffer
        });
        Program { hud_kind:None, module, info, consts, pack: None, text: source.into(), flat: vec![], group_bindings: groups,
            variants: RefCell::new(HashMap::new()), label: "same label".into(), uid: g.uid(), vid: false, iid: false,
            out_locs: vec![], in_locs: vec![], clip: false }
    }

    #[cfg(target_os = "macos")]
    fn view(g: &mut Gpu, id: u32, resource: u32, is_buf: bool) {
        g.set_obj(id, Obj::Uav(ViewObj { res: resource, is_buf, fmt: 28, vdim: 4,
            a: 0, b: 4, c: 0, d: 0, views: HashMap::new() }));
    }

    #[cfg(target_os = "macos")]
    fn compute(g: &mut Gpu, p: &Program, bg: &BgResult, bgl: &BindGroupLayout) {
        let layout = g.device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
        let pipeline = g.device.create_compute_pipeline(&ComputePipelineDescriptor { label: None, layout: Some(&layout), module: &p.module,
            entry_point: Some("main"), compilation_options: Default::default(), cache: None });
        let mut pass = g.encoder().begin_compute_pass(&ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg.bg, bg.dyn_off.as_ref().map(std::slice::from_ref).unwrap_or(&[]));
        pass.dispatch_workgroups(1, 1, 1);
    }

    #[cfg(target_os = "macos")]
    fn mapped(g: &mut Gpu, buffer: &Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        g.encoder().map_buffer_on_submit(buffer, MapMode::Read, .., move |result| { let _ = tx.send(result); });
        g.flush();
        g.device.poll(PollType::Wait { submission_index: g.last_submit.clone(), timeout: Some(Duration::from_secs(10)) }).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        let bytes = buffer.slice(..).get_mapped_range().unwrap().to_vec(); buffer.unmap(); bytes
    }

    #[cfg(target_os = "macos")]
    fn buffer_bytes(g: &mut Gpu, source: &Buffer, offset: u64, size: u64) -> Vec<u8> {
        g.end_pass();
        let output = g.device.create_buffer(&BufferDescriptor { label: None, size, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        g.encoder().copy_buffer_to_buffer(source, offset, &output, 0, size); mapped(g, &output)
    }

    #[cfg(target_os = "macos")]
    fn texture_bytes(g: &mut Gpu, id: u32, mip: u32, layer: u32, aspect: TextureAspect, bpp: u32) -> Vec<u8> {
        g.end_pass(); let t = g.tex(id).unwrap(); let texture = t.tex.clone();
        let (width, height) = ((t.desc.w >> mip).max(1), (t.desc.h >> mip).max(1));
        let row = (width * bpp).div_ceil(256) * 256;
        let output = g.device.create_buffer(&BufferDescriptor { label: None, size: row as u64 * height as u64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        g.encoder().copy_texture_to_buffer(TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: Origin3d { x: 0, y: 0, z: layer }, aspect },
            TexelCopyBufferInfo { buffer: &output, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) } },
            Extent3d { width, height, depth_or_array_layers: 1 });
        mapped(g, &output).chunks_exact(row as usize).flat_map(|row| row[..(width * bpp) as usize].iter().copied()).collect()
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_same_label_distinct_uid_layouts_and_cached_binding_effects_read_back() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        g.create_buffer(1, 16, BIND_UAV, 0, 4); view(&mut g, 2, 1, true);
        g.create_texture(11, 2, 4, 4, 1, 1, 1, 28, BIND_RT | BIND_UAV, 1); view(&mut g, 12, 11, false);
        let a = program(&mut g, "@group(0) @binding(15) var<uniform> c: vec4u; @group(0) @binding(160) var<storage, read_write> out: array<u32>; @compute @workgroup_size(1) fn main() { out[0] = c.x + c.y; }", Some([11, 22, 33, 44]));
        let b = program(&mut g, "@group(0) @binding(15) var<uniform> c: vec4u; @group(0) @binding(160) var out: texture_storage_2d<rgba8unorm, write>; @compute @workgroup_size(1) fn main() { textureStore(out, vec2i(0), vec4f(c) / 255.0); }", Some([7, 77, 177, 255]));
        let la = g.bgl_for(&a, 2, 0); let lb = g.bgl_for(&b, 2, 0);
        assert_ne!(la.1, lb.1); assert_eq!(g.bgl_for(&a, 2, 0).1, la.1);
        // A uniform-only layout is legal for all shader stages without the
        // optional VERTEX_WRITABLE_STORAGE feature requested by neither path.
        let stages = program(&mut g, "@group(0) @binding(15) var<uniform> c: vec4u; @compute @workgroup_size(1) fn main() { }", Some([0; 4]));
        let lc = g.bgl_for(&stages, 2, 0);
        assert_ne!(g.bgl_for(&stages, 0, 0).1, lc.1); assert_ne!(g.bgl_for(&stages, 1, 0).1, lc.1);
        assert_ne!(g.bgl_for(&stages, 2, 1).1, lc.1);
        g.s.uav[2][0] = 2; let ba = g.stage_bind_group(2, &a, 0, &la).unwrap(); compute(&mut g, &a, &ba, &la.0);
        let output = g.buf(1).unwrap().gpu.clone().unwrap();
        assert_eq!(&buffer_bytes(&mut g, &output, 0, 16)[..4], &33u32.to_le_bytes());
        g.s.uav[2][0] = 12; let bb = g.stage_bind_group(2, &b, 0, &lb).unwrap(); compute(&mut g, &b, &bb, &lb.0);
        assert_eq!(&texture_bytes(&mut g, 11, 0, 0, TextureAspect::All, 4)[..4], &[7, 77, 177, 255]);
        // A cache hit must still materialize a pending clear and record use.
        g.pending_clears.insert(11, vec![(0, 0, PendingClear { color: Some(Color { r: 0.2, g: 0.4, b: 0.6, a: 1.0 }), ..Default::default() })]);
        let again = g.stage_bind_group(2, &b, 0, &lb).unwrap(); assert_eq!(again.uid, bb.uid);
        assert!(!g.pending_clears.contains_key(&11)); assert!(g.used(11)); assert!(g.tex(11).unwrap().in_bg);
        compute(&mut g, &b, &again, &lb.0);
        let pixels = texture_bytes(&mut g, 11, 0, 0, TextureAspect::All, 4);
        assert_eq!(&pixels[..4], &[7, 77, 177, 255]);
        assert!(pixels[4..].chunks_exact(4).all(|pixel| pixel == [51, 102, 153, 255]));
        let metrics = g.render_prepare.metrics(0, 0, g.passes, g.submits, 0);
        assert!(metrics["bgl_hits"].as_u64().unwrap() > 0); assert!(metrics["bind_group_hits"].as_u64().unwrap() > 0);
        assert_eq!(metrics["numeric_key_lookups"].as_u64().unwrap() > 0, g.render_prepare.enabled);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_pass_keys_separate_mrt_holes_mips_layers_formats_and_depth_flags() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        g.create_texture(1, 2, 4, 4, 1, 2, 2, 28, BIND_RT, 1);
        g.s.rt[2] = Rt { tex: 1, mip: 0, slice: 0, fmt: 28 };
        g.pending_clears.insert(1, vec![(0, 0, PendingClear { color: Some(Color { r: 1., g: 0., b: 0., a: 1. }), ..Default::default() })]);
        assert!(g.ensure_pass()); let passes = g.passes; assert!(g.ensure_pass()); assert_eq!(g.passes, passes);
        assert_eq!(g.render_prepare.dimensions(g.pass_info.as_ref().unwrap()), (4, 4, false));
        g.s.rt[2].mip = 1; g.s.rt[2].slice = 1;
        g.pending_clears.insert(1, vec![(1, 1, PendingClear { color: Some(Color { r: 0., g: 1., b: 0., a: 1. }), ..Default::default() })]);
        assert!(g.ensure_pass()); assert_eq!(g.passes, passes + 1);
        assert_eq!(g.render_prepare.dimensions(g.pass_info.as_ref().unwrap()), (2, 2, false));
        let color = g.s.rt[2].clone(); g.s.rt[2] = Rt::default(); g.s.rt[0] = color;
        assert!(g.ensure_pass()); assert_eq!(g.passes, passes + 2);
        g.s.rt[0].fmt = 29; assert!(g.ensure_pass()); assert_eq!(g.passes, passes + 3);
        assert_eq!(g.pass_info.as_ref().unwrap().rt_formats, [Some(TextureFormat::Rgba8UnormSrgb)]);
        assert!(texture_bytes(&mut g, 1, 0, 0, TextureAspect::All, 4).chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
        assert!(texture_bytes(&mut g, 1, 1, 1, TextureAspect::All, 4).chunks_exact(4).all(|pixel| pixel == [0, 255, 0, 255]));
        g.create_texture(3, 2, 4, 4, 1, 1, 1, 20, BIND_DS, 1); g.s.rt.fill(Rt::default());
        g.s.ds = Ds { tex: 3, mip: 0, slice: 0, fmt: 0, flags: 0 };
        g.pending_clears.insert(3, vec![(0, 0, PendingClear { depth: Some(0.25), stencil: Some(5), ..Default::default() })]);
        assert!(g.ensure_pass()); let depth_passes = g.passes;
        for flags in [1, 2, 3] { g.s.ds.flags = flags; assert!(g.ensure_pass());
            assert_eq!(g.pass_info.as_ref().unwrap().ro_depth, flags & 1 != 0);
            assert_eq!(g.pass_info.as_ref().unwrap().ro_stencil, flags & 2 != 0); }
        assert_eq!(g.passes, depth_passes + 3);
        assert!(texture_bytes(&mut g, 3, 0, 0, TextureAspect::DepthOnly, 4).chunks_exact(4).all(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()) == 0.25));
        assert!(texture_bytes(&mut g, 3, 0, 0, TextureAspect::StencilOnly, 1).iter().all(|&value| value == 5));
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn metal_borrowed_packed_binding_keeps_each_dynamic_snapshot_before_cache_hit() {
        let mut g = gpu(); let scope = g.device.push_error_scope(ErrorFilter::Validation);
        g.create_buffer(1, 16, BIND_UAV, 0, 4); view(&mut g, 2, 1, true); g.s.uav[2][0] = 2;
        g.create_buffer(3, 48, 4, 0, 0); g.s.cb[2][0] = Cb { buf: 3, first: 1 };
        let mut p = program(&mut g, "@group(0) @binding(0) var<uniform> c: vec4u; @group(0) @binding(160) var<storage, read_write> out: array<u32>; @compute @workgroup_size(1) fn main() { out[c.y] = c.x; out[2] = c.z; out[3] = c.w; }", None);
        p.pack = Some(Pack { members: vec![PackMember { binding: 0, size: 16, offset: 0 }], size: 16 });
        let layout = g.bgl_for(&p, 2, 0);
        g.buf_mut(3).unwrap().version += 1;
        { let shadow = g.buf_mut(3).unwrap().shadow.as_mut().unwrap(); shadow.fill(99); shadow[16..32].fill(0); shadow[16..20].copy_from_slice(&7u32.to_le_bytes()); }
        let a = g.stage_bind_group(2, &p, 0, &layout).unwrap(); compute(&mut g, &p, &a, &layout.0);
        g.buf_mut(3).unwrap().version += 1;
        { let shadow = g.buf_mut(3).unwrap().shadow.as_mut().unwrap(); shadow[16..20].copy_from_slice(&11u32.to_le_bytes()); shadow[20..24].copy_from_slice(&1u32.to_le_bytes()); }
        let b = g.stage_bind_group(2, &p, 0, &layout).unwrap();
        assert_eq!(a.uid, b.uid); assert_eq!(a.dyn_off, Some(0)); assert_eq!(b.dyn_off, Some(256));
        assert_eq!((g.ubo_slots[0][0].lo, g.ubo_slots[0][0].hi), (0, 272));
        let uniform = &g.ubo_slots[0][0].stage;
        assert_eq!(&uniform[..4], &7u32.to_le_bytes()); assert!(uniform[16..256].iter().all(|&v| v == 0));
        assert_eq!(&uniform[256..260], &11u32.to_le_bytes());
        compute(&mut g, &p, &b, &layout.0);
        let out = g.buf(1).unwrap().gpu.clone().unwrap(); let bytes = buffer_bytes(&mut g, &out, 0, 16);
        assert_eq!(&bytes[..4], &7u32.to_le_bytes()); assert_eq!(&bytes[4..8], &11u32.to_le_bytes()); assert_eq!(&bytes[8..], &[0; 8]);
        let error = pollster::block_on(scope.pop()); assert!(error.is_none(), "{error:?}"); assert_eq!(g.errors.load(Ordering::Relaxed), 0);
    }
}

impl PassKey {
    pub(super) fn words(&self) -> &[u32] {
        match self { Self::Legacy(words) => words, Self::Fixed(words) => words }
    }
}

#[derive(Default)]
struct Counts {
    bgl_lookups: u64, bgl_hits: u64, bgl_misses: u64,
    numeric_key_lookups: u64, legacy_string_key_builds: u64,
    descriptor_prepares: u64, nonempty_descriptor_prepares: u64,
    descriptors: u64, descriptor_vec_clone_sites: u64,
    descriptor_vec_clone_sites_avoided: u64, descriptor_string_clone_sites: u64,
    descriptor_string_clone_sites_avoided: u64,
    pass_key_builds: u64, heap_pass_keys: u64, stack_pass_keys: u64,
    pass_reuse_hits: u64, draw_dimension_reads: u64, pass_info_clones: u64,
    pass_info_clone_sites_avoided: u64, rt_format_vec_clone_sites: u64,
    rt_format_vec_clone_sites_avoided: u64,
    bind_group_lookups: u64, bind_group_hits: u64, bind_group_misses: u64,
}

pub(super) struct RenderPrepare {
    pub(super) enabled: bool,
    epoch: Instant,
    counts: Counts,
}

impl RenderPrepare {
    pub(super) fn from_environment() -> Self {
        Self::new(crate::draw_setup::render_prepare_enabled())
    }

    fn new(enabled: bool) -> Self {
        Self { enabled, epoch: Instant::now(), counts: Counts::default() }
    }

    pub(super) fn bgl_lookup(&mut self, hit: bool) {
        self.counts.bgl_lookups += 1;
        if hit { self.counts.bgl_hits += 1; } else { self.counts.bgl_misses += 1; }
        if self.enabled { self.counts.numeric_key_lookups += 1; }
        else { self.counts.legacy_string_key_builds += 1; }
    }

    pub(super) fn bindings<'a>(&mut self, bindings: &'a [Binding]) -> Cow<'a, [Binding]> {
        let c = &mut self.counts;
        c.descriptor_prepares += 1;
        c.descriptors += bindings.len() as u64;
        if !bindings.is_empty() {
            c.nonempty_descriptor_prepares += 1;
            if self.enabled { c.descriptor_vec_clone_sites_avoided += 1; }
            else { c.descriptor_vec_clone_sites += 1; }
        }
        // Vec cloning visits the entire slice before any binding-effect early
        // return. Count all nonempty String clones at that same source site.
        let strings = bindings.iter().filter(|binding| !binding.format_name.is_empty()).count() as u64;
        if self.enabled { c.descriptor_string_clone_sites_avoided += strings; }
        else { c.descriptor_string_clone_sites += strings; }
        if self.enabled { Cow::Borrowed(bindings) } else { Cow::Owned(bindings.to_vec()) }
    }

    pub(super) fn pass_key(&mut self, rt: &[Rt], ds: &Ds) -> PassKey {
        self.counts.pass_key_builds += 1;
        if self.enabled {
            // Closed renderer state always has eight RT slots. Do not silently
            // truncate malformed/future state to fit the fixed representation.
            assert_eq!(rt.len(), 8);
            let mut words = [0; 37];
            for (i, r) in rt.iter().enumerate() {
                words[i * 4..i * 4 + 4].copy_from_slice(&[r.tex, r.mip, r.slice, r.fmt]);
            }
            words[32..].copy_from_slice(&[ds.tex, ds.mip, ds.slice, ds.fmt, ds.flags]);
            self.counts.stack_pass_keys += 1;
            PassKey::Fixed(words)
        } else {
            let mut words = Vec::with_capacity(37);
            for r in rt { words.extend([r.tex, r.mip, r.slice, r.fmt]); }
            words.extend([ds.tex, ds.mip, ds.slice, ds.fmt, ds.flags]);
            self.counts.heap_pass_keys += 1;
            PassKey::Legacy(words)
        }
    }

    pub(super) fn pass_reused(&mut self) { self.counts.pass_reuse_hits += 1; }

    pub(super) fn dimensions(&mut self, info: &PassInfo) -> (u32, u32, bool) {
        self.counts.draw_dimension_reads += 1;
        if self.enabled {
            self.counts.pass_info_clone_sites_avoided += 1;
            if !info.rt_formats.is_empty() { self.counts.rt_format_vec_clone_sites_avoided += 1; }
            (info.width, info.height, info.ro_depth)
        } else {
            self.counts.pass_info_clones += 1;
            if !info.rt_formats.is_empty() { self.counts.rt_format_vec_clone_sites += 1; }
            let info = info.clone();
            (info.width, info.height, info.ro_depth)
        }
    }

    pub(super) fn bind_group_lookup(&mut self, hit: bool) {
        self.counts.bind_group_lookups += 1;
        if hit { self.counts.bind_group_hits += 1; } else { self.counts.bind_group_misses += 1; }
    }

    pub(super) fn metrics(&self, presents: u64, draws: u64, passes: u64, submits: u64, validation_errors: u64) -> serde_json::Value {
        let c = &self.counts;
        let mut result = serde_json::json!({"schema": 1, "enabled": self.enabled,
            "elapsed_s": self.epoch.elapsed().as_secs_f64(), "source_presents": presents,
            "draws": draws, "passes": passes, "submits": submits, "validation_errors": validation_errors,
            "semantics": "cumulative executed source-site counters and workload; not allocator samples or GPU time"});
        for (name, count) in [
            ("bgl_lookups", c.bgl_lookups), ("bgl_hits", c.bgl_hits), ("bgl_misses", c.bgl_misses),
            ("numeric_key_lookups", c.numeric_key_lookups), ("legacy_string_key_builds", c.legacy_string_key_builds),
            ("bgl_string_key_sites_avoided", c.numeric_key_lookups),
            ("descriptor_prepares", c.descriptor_prepares), ("nonempty_descriptor_prepares", c.nonempty_descriptor_prepares),
            ("descriptors", c.descriptors), ("descriptor_vec_clone_sites", c.descriptor_vec_clone_sites),
            ("descriptor_vec_clone_sites_avoided", c.descriptor_vec_clone_sites_avoided),
            ("descriptor_string_clone_sites", c.descriptor_string_clone_sites),
            ("descriptor_string_clone_sites_avoided", c.descriptor_string_clone_sites_avoided),
            ("pass_key_builds", c.pass_key_builds), ("heap_pass_keys", c.heap_pass_keys), ("stack_pass_keys", c.stack_pass_keys),
            ("pass_reuse_hits", c.pass_reuse_hits), ("draw_dimension_reads", c.draw_dimension_reads),
            ("pass_info_clones", c.pass_info_clones), ("pass_info_clone_sites_avoided", c.pass_info_clone_sites_avoided),
            ("rt_format_vec_clone_sites", c.rt_format_vec_clone_sites), ("rt_format_vec_clone_sites_avoided", c.rt_format_vec_clone_sites_avoided),
            ("bind_group_lookups", c.bind_group_lookups), ("bind_group_hits", c.bind_group_hits), ("bind_group_misses", c.bind_group_misses),
        ] { result[name] = count.into(); }
        result
    }
}

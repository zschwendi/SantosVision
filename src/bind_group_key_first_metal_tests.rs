// Opt-in-path differential qualification. Run serially with the host GPU slot.
use super::*;

fn uniform(gpu: &mut Gpu, value: u32, index: u32) {
    let buffer = gpu.buf_mut(1).unwrap();
    buffer.version += 1;
    buffer.shadow.as_mut().unwrap()[..8]
        .copy_from_slice(&[value.to_le_bytes(), index.to_le_bytes()].concat());
}
fn pipeline(gpu: &Gpu, program: &Program, layout: &(BindGroupLayout, u64)) -> ComputePipeline {
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout.0)],
            immediate_size: 0,
        });
    gpu.device
        .create_compute_pipeline(&ComputePipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            module: &program.module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        })
}
fn dispatch(gpu: &mut Gpu, pipeline: &ComputePipeline, bound: &BgResult) {
    let mut pass = gpu
        .encoder()
        .begin_compute_pass(&ComputePassDescriptor::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(
        0,
        &bound.bg,
        bound
            .dyn_off
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&[]),
    );
    pass.dispatch_workgroups(1, 1, 1);
}
fn read_buffers(gpu: &mut Gpu, sources: &[Buffer]) -> Vec<u32> {
    let output = gpu.device.create_buffer(&BufferDescriptor {
        label: None,
        size: 64 * sources.len() as u64,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for (index, source) in sources.iter().enumerate() {
        gpu.encoder()
            .copy_buffer_to_buffer(source, 0, &output, index as u64 * 64, 64);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    gpu.encoder()
        .map_buffer_on_submit(&output, MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
    gpu.flush();
    gpu.device
        .poll(PollType::Wait {
            submission_index: gpu.last_submit.clone(),
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let bytes = output.slice(..).get_mapped_range().unwrap();
    let words = bytes
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    drop(bytes);
    output.unmap();
    words
}
fn key(gpu: &Gpu, uid: u64) -> Vec<u64> {
    gpu.bind_groups
        .iter()
        .find(|(_, (_, id))| *id == uid)
        .unwrap()
        .0
        .clone()
}
fn seed_texture(gpu: &Gpu, color: [u8; 4]) {
    gpu.queue.write_texture(
        TexelCopyTextureInfo {
            texture: &gpu.tex(10).unwrap().tex,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        &color,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

fn ordinary_scenario(key_first: bool, textures: usize) -> serde_json::Value {
    let (mut gpu, program, layout) = fixture(textures);
    gpu.bind_group_prepare.key_first = key_first;
    gpu.upload_dedup =
        crate::upload_dedup::UploadDedup::new(true, crate::upload_dedup::Caps::DEFAULT);
    let scope = gpu.device.push_error_scope(ErrorFilter::Validation);
    let pipeline = pipeline(&gpu, &program, &layout);
    seed_texture(&gpu, [255, 0, 0, 255]);
    let mut records = Vec::new();
    let mut bind = |gpu: &mut Gpu, value, index| {
        uniform(gpu, value, index);
        let bound = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
        assert!(gpu.used(10) && gpu.used(2));
        assert!(gpu.tex(10).unwrap().in_bg && gpu.buf(2).unwrap().in_bg);
        records.push(serde_json::json!({"key":key(gpu,bound.uid),"dynamic_offset":bound.dyn_off}));
        dispatch(gpu, &pipeline, &bound);
        bound
    };
    let first = bind(&mut gpu, 7, 0);
    // A hit must still consume the pending clear and fresh uniform data.
    gpu.clear_rt(10, 0, 0, [0., 1., 0., 1.]);
    assert!(gpu.pending_clears.contains_key(&10));
    // The normal UAV is deliberately ineligible for upload dedup. Inject one
    // tracked lifetime only to prove this source-site hook still runs on hits.
    let track = gpu.upload_dedup.track_new(1, 0, true, false);
    gpu.buf_mut(2).unwrap().dedup = track;
    let hit = bind(&mut gpu, 11, 1);
    assert!(!gpu.pending_clears.contains_key(&10));
    assert_eq!(
        gpu.buf(2).unwrap().dedup,
        crate::upload_dedup::Track::Disqualified
    );
    assert_eq!(first.uid, hit.uid);
    assert_ne!(first.dyn_off, hit.dyn_off);
    // Parent allocation replacement keeps the guest SRV ID, discards the
    // stale native view, falls through once, then returns to the key-first hit.
    gpu.create_texture(10, 2, 1, 1, 1, 1, 1, 28, BIND_SRV | BIND_RT, 1);
    seed_texture(&gpu, [0, 0, 255, 255]);
    assert!(gpu.view_obj(11).unwrap().views.is_empty());
    let replaced = bind(&mut gpu, 13, 2);
    assert_ne!(replaced.uid, hit.uid);
    let second_hit = bind(&mut gpu, 17, 3);
    assert_eq!(replaced.uid, second_hit.uid);
    // Force an actual pack allocation to a new native ring chunk on a miss.
    gpu.ubo_off = RING_CHUNK - UBO_WINDOW;
    gpu.ubo_chunk = 0;
    let chunk = bind(&mut gpu, 19, 4);
    assert_eq!(gpu.ubo_chunk, 1);
    assert_ne!(chunk.uid, second_hit.uid);
    assert_eq!(chunk.dyn_off, Some(0));
    let old_buffer = gpu.buf(2).unwrap().gpu.clone().unwrap();
    gpu.create_buffer(2, 64, BIND_UAV, 0, 4);
    let buffer_replaced = bind(&mut gpu, 23, 0);
    assert_ne!(buffer_replaced.uid, chunk.uid);
    let sampler = gpu.device.create_sampler(&SamplerDescriptor::default());
    gpu.set_obj(
        12,
        Obj::State(Rc::new(StateObj {
            words: vec![0],
            sampler: Some(sampler),
        })),
    );
    let sampler_replaced = bind(&mut gpu, 29, 1);
    assert_ne!(sampler_replaced.uid, buffer_replaced.uid);
    let new_buffer = gpu.buf(2).unwrap().gpu.clone().unwrap();
    let words = read_buffers(&mut gpu, &[old_buffer, new_buffer]);
    assert_eq!(&words[..5], &[7 + 255 * textures as u32, 11, 13, 17, 19]);
    assert_eq!(&words[16..18], &[23, 29]);
    assert_eq!(gpu.errors.load(Ordering::Relaxed), 0);
    assert!(pollster::block_on(scope.pop()).is_none());
    let metrics = gpu.bind_group_prepare.metrics();
    assert_eq!(metrics["hits"], 2);
    assert_eq!(metrics["misses"], 5);
    if key_first {
        assert_eq!(metrics["key_first_hits"], 2);
        assert_eq!(metrics["key_first_fallbacks"], 2);
        assert_eq!(
            metrics["key_first_hit_binding_items"],
            serde_json::json!({"uniform":2,"sampler":2,"texture":2*textures,"storage_buffer":2})
        );
    }
    let mut snapshots = gpu.uniform_snapshot.metrics();
    // CPU-clock measurements are intentionally nondeterministic; compare all
    // semantic counters and cache state, never elapsed nanoseconds.
    snapshots
        .as_object_mut()
        .unwrap()
        .remove("key_check_cpu_ns");
    snapshots
        .as_object_mut()
        .unwrap()
        .remove("key_check_cpu_max_ns");
    assert_eq!(
        gpu.upload_dedup.metrics(0.)["stats_interval"]["disqualified"]["storage_binding"],
        1
    );
    serde_json::json!({"records":records,"words":words,"uniform_snapshots":snapshots,
        "write_guards":gpu.upload_dedup.metrics(0.),"hits":metrics["hits"],"misses":metrics["misses"]})
}

#[test]
fn metal_key_first_matches_native_keys_contents_clears_offsets_and_replacements() {
    let original = ordinary_scenario(false, 1);
    let optimized = ordinary_scenario(true, 1);
    assert_eq!(original,optimized,"production paths must match full keys, offsets, outputs, uniform snapshots and write-guard accounting");
}

#[test]
fn metal_key_first_eight_repeated_textures_match_actual_shader_output() {
    assert_eq!(ordinary_scenario(false, 8), ordinary_scenario(true, 8));
}

#[test]
fn metal_key_first_preflight_is_borrowed_and_rejects_proxy_dummy_and_legacy_offsets() {
    let (mut gpu, program, layout) = fixture(1);
    assert!(!gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
    assert!(gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    let before = key(
        &gpu,
        *gpu.bind_groups.values().next().map(|(_, uid)| uid).unwrap(),
    );
    let mut proxy = program.info.bindings.clone();
    proxy
        .iter_mut()
        .find(|b| b.kind == Kind::Texture)
        .unwrap()
        .sample = Sample::Depth;
    assert!(!gpu.key_first_bindings_eligible(2, &program, &proxy));
    gpu.s.smp[2][0] = 0;
    assert!(!gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    gpu.s.smp[2][0] = 12;
    gpu.view_obj_mut(3).unwrap().a = 1;
    assert!(!gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    gpu.view_obj_mut(3).unwrap().a = 0;
    let mut counter = program.info.bindings.clone();
    counter
        .iter_mut()
        .find(|b| b.kind == Kind::Storage)
        .unwrap()
        .binding = 176;
    assert!(!gpu.key_first_bindings_eligible(2, &program, &counter));
    assert_eq!(gpu.ubo_off, 256);
    assert_eq!(gpu.submits, 0);
    assert!(gpu.enc.is_none());
    assert_eq!(
        before,
        key(
            &gpu,
            *gpu.bind_groups.values().next().map(|(_, uid)| uid).unwrap()
        )
    );
}

fn depth_scenario(key_first: bool, force_miss: bool) -> serde_json::Value {
    let (mut gpu, mut program, _) = fixture(0);
    gpu.bind_group_prepare.key_first = key_first;
    gpu.create_texture(20, 2, 1, 1, 1, 1, 1, 40, BIND_SRV | BIND_DS, 1);
    gpu.set_obj(
        21,
        Obj::Srv(ViewObj {
            res: 20,
            is_buf: false,
            fmt: 40,
            vdim: 4,
            a: 0,
            b: 1,
            c: 0,
            d: 1,
            views: HashMap::new(),
        }),
    );
    gpu.s.srv[2][0] = 21;
    let text="@group(0) @binding(0) var<uniform> c: vec4u; @group(0) @binding(32) var image: texture_depth_2d; @group(0) @binding(160) var<storage,read_write> out: array<u32>; @compute @workgroup_size(1) fn main() {out[c.y]=u32(textureLoad(image,vec2i(0),0)*1000.0)+c.x;}";
    program.module = gpu.device.create_shader_module(ShaderModuleDescriptor {
        label: None,
        source: ShaderSource::Wgsl(text.into()),
    });
    program.info = parse_wgsl(text);
    program.group_bindings = BTreeMap::from([(0, program.info.bindings.clone())]);
    program.uid = gpu.uid();
    program.text = text.into();
    let layout = gpu.bgl_for(&program, 2, 0);
    let pipeline = pipeline(&gpu, &program, &layout);
    let scope = gpu.device.push_error_scope(ErrorFilter::Validation);
    gpu.clear_ds(20, 0, 0, 1, 0.25, 0);
    uniform(&mut gpu, 7, 0);
    let warm = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
    let warm_key = key(&gpu, warm.uid);
    dispatch(&mut gpu, &pipeline, &warm);
    assert!(gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    assert_eq!(
        gpu.fp_sessions
            .foreground_depth_draw(20, 1, fg::logic::Relation::FrozenSame, 1, 44395),
        fg::logic::Decision::OpenOverlay
    );
    assert!(gpu.foreground_snapshot(20, true));
    gpu.fp_sessions.opened(20);
    // Insert the clear directly so materialize_clears itself must close the
    // live foreground session through the complete nested snapshot/proxy path.
    gpu.pending_clears.insert(
        20,
        vec![(
            0,
            0,
            PendingClear {
                depth: Some(0.75),
                ..Default::default()
            },
        )],
    );
    if force_miss {
        gpu.ubo_off = RING_CHUNK - UBO_WINDOW;
        gpu.ubo_chunk = 0;
    }
    uniform(&mut gpu, 11, 1);
    let hit = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
    assert_eq!(warm.uid == hit.uid, !force_miss);
    let next_key = key(&gpu, hit.uid);
    assert!(gpu.fp_images.get(&20).unwrap().after.is_some());
    assert_eq!(
        gpu.fp_sessions.overlay(20).unwrap().phase,
        fg::logic::Phase::Closed
    );
    assert!(!gpu.pending_clears.contains_key(&20));
    dispatch(&mut gpu, &pipeline, &hit);
    let source = gpu.buf(2).unwrap().gpu.clone().unwrap();
    let words = read_buffers(&mut gpu, &[source]);
    assert_eq!(&words[..2], &[257, 761]);
    assert_eq!(gpu.errors.load(Ordering::Relaxed), 0);
    assert!(pollster::block_on(scope.pop()).is_none());
    let metrics = gpu.bind_group_prepare.metrics();
    if key_first {
        assert_eq!(metrics["key_first_eligible_lookups"], 1);
        assert_eq!(metrics["key_first_hits"], u64::from(!force_miss));
        assert_eq!(
            metrics["key_first_fallbacks"], 1,
            "only the initial uncached native SRV uses the reference path"
        );
    }
    let mut snapshots = gpu.uniform_snapshot.metrics();
    snapshots
        .as_object_mut()
        .unwrap()
        .remove("key_check_cpu_ns");
    snapshots
        .as_object_mut()
        .unwrap()
        .remove("key_check_cpu_max_ns");
    serde_json::json!({"words":words,"keys":[warm_key,next_key],"dynamic_offsets":[warm.dyn_off,hit.dyn_off],
        "uniform_snapshots":snapshots,"hits":metrics["hits"],"misses":metrics["misses"]})
}

#[test]
fn metal_key_first_pending_depth_clear_preserves_foreground_snapshot_on_hit() {
    assert_eq!(depth_scenario(false, false), depth_scenario(true, false));
}

#[test]
fn metal_key_first_pending_depth_clear_preserves_foreground_snapshot_on_miss() {
    assert_eq!(depth_scenario(false, true), depth_scenario(true, true));
}

fn aligned_scenario(key_first: bool) -> serde_json::Value {
    let (mut gpu, program, layout) = fixture(0);
    gpu.bind_group_prepare.key_first = key_first;
    gpu.create_buffer(2, 512, BIND_UAV, 0, 4);
    gpu.view_obj_mut(3).unwrap().a = 64;
    assert!(gpu.key_first_bindings_eligible(2, &program, &program.info.bindings));
    let scope = gpu.device.push_error_scope(ErrorFilter::Validation);
    let pipeline = pipeline(&gpu, &program, &layout);
    uniform(&mut gpu, 7, 0);
    let first = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
    let first_key = key(&gpu, first.uid);
    assert_eq!(first_key.last(), Some(&256));
    dispatch(&mut gpu, &pipeline, &first);
    uniform(&mut gpu, 11, 1);
    let hit = gpu.stage_bind_group(2, &program, 0, &layout).unwrap();
    assert_eq!(hit.uid, first.uid);
    assert_ne!(hit.dyn_off, first.dyn_off);
    dispatch(&mut gpu, &pipeline, &hit);
    let source = gpu.buf(2).unwrap().gpu.clone().unwrap();
    let range = gpu.device.create_buffer(&BufferDescriptor {
        label: None,
        size: 64,
        usage: BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    gpu.encoder()
        .copy_buffer_to_buffer(&source, 256, &range, 0, 64);
    let words = read_buffers(&mut gpu, &[range]);
    assert_eq!(&words[..2], &[7, 11]);
    assert_eq!(gpu.errors.load(Ordering::Relaxed), 0);
    assert!(pollster::block_on(scope.pop()).is_none());
    if key_first {
        let metrics = gpu.bind_group_prepare.metrics();
        assert_eq!(metrics["key_first_eligible_lookups"], 2);
        assert_eq!(metrics["key_first_hits"], 1);
        assert_eq!(metrics["key_first_fallbacks"], 0);
    }
    serde_json::json!({"words":words,"key":first_key,"dynamic_offsets":[first.dyn_off,hit.dyn_off]})
}

#[test]
fn metal_key_first_aligned_nonzero_storage_offset_and_explicit_size_match() {
    assert_eq!(aligned_scenario(false), aligned_scenario(true));
}

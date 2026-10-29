//! Authored offscreen native-Gpu regression. Real exec binding commands and
//! do_draw/stage_bind_group/dummy/pipeline paths, no game or device integration.
use super::*;
const COMMAND: u64 = 0x1_0000;
const SIZE: u32 = 4;
const MATERIAL: [u8; 4] = [51, 102, 153, 204];
const SENTINEL: [u8; 4] = [7, 9, 11, 13];

fn command(g: &mut Gpu, opcode: u32, words: &[u32]) {
    let bytes: Vec<_> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
    assert!(crate::write_bytes(&g.mem, COMMAND, &bytes));
    g.exec(opcode, COMMAND / 4, words.len() as u64);
}
fn input(g: &mut Gpu, stage: u32, slot: u32, view: u32) {
    command(g, 24, &[stage, slot, 1, view]);
}
fn outputs(g: &mut Gpu, target: u32, mip: u32, layer: u32) {
    let mut words = [0; 37];
    if target != 0 {
        words[..8].copy_from_slice(&[target, mip, layer, 28, 2, mip, 0, 28]);
    }
    command(g, 26, &words);
}
fn upload(g: &Gpu, id: u32, mip: u32, layer: u32, color: [u8; 4]) {
    let side = SIZE >> mip;
    g.queue.write_texture(
        TexelCopyTextureInfo {
            texture: &g.tex(id).unwrap().tex,
            mip_level: mip,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        &color.repeat((side * side) as usize),
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(side * 4),
            rows_per_image: Some(side),
        },
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
    );
}
fn read(g: &mut Gpu, id: u32, mip: u32, layer: u32) -> Vec<u8> {
    g.flush();
    let side = SIZE >> mip;
    let texture = g.tex(id).unwrap().tex.clone();
    let buffer = g.device.create_buffer(&BufferDescriptor {
        label: Some("native overlap readback"),
        size: 256 * side as u64,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = g.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture: &texture,
            mip_level: mip,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(side),
            },
        },
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
    );
    let (tx, rx) = std::sync::mpsc::channel();
    e.map_buffer_on_submit(&buffer, MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    let submission = g.queue.submit([e.finish()]);
    g.device
        .poll(PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let bytes = (0..side)
        .flat_map(|y| mapped[(y * 256) as usize..(y * 256 + side * 4) as usize].to_vec())
        .collect();
    drop(mapped);
    buffer.unmap();
    bytes
}
fn program(g: &mut Gpu, text: &str) -> Rc<Program> {
    let module = g.device.create_shader_module(ShaderModuleDescriptor {
        label: Some("native color binding regression"),
        source: ShaderSource::Wgsl(text.into()),
    });
    let info = parse_wgsl(text);
    let mut groups: BTreeMap<u32, Vec<Binding>> = BTreeMap::new();
    for b in &info.bindings {
        groups.entry(b.group).or_default().push(b.clone());
    }
    Rc::new(Program {
        hud_kind: None,
        module,
        info,
        consts: None,
        pack: None,
        text: text.into(),
        flat: vec![],
        group_bindings: groups,
        variants: RefCell::new(HashMap::new()),
        label: "native color binding regression".into(),
        uid: g.uid(),
        vid: false,
        iid: false,
        out_locs: vec![],
        in_locs: vec![],
        clip: false,
    })
}
fn gpu(fast: bool, layer: u32) -> Gpu {
    let mut config = wasmtime::Config::new();
    config.wasm_threads(true);
    config.shared_memory(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, wasmtime::MemoryType::shared(4, 4)).unwrap();
    let mut g = Gpu::new(memory, 0, 65536, 0, None).unwrap();
    g.pipe_jobs = None;
    g.bind_group_prepare = BindGroupPrepare::new(fast);
    for id in 1..=4 {
        g.create_texture(id, 2, SIZE, SIZE, 1, 2, 2, 28, BIND_RT | BIND_SRV, 1);
    }
    for (id, resource, slice) in [(11, 1, layer), (13, 3, 0)] {
        command(&mut g, 17, &[id, 2, resource, 28, 4, 0, 1, slice, 1]);
    }
    let vs=program(&mut g,"@vertex fn main(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);}");
    let ps=program(&mut g,"@group(1) @binding(32) var source:texture_2d<f32>;struct O{@location(0)a:vec4f,@location(1)b:vec4f};@fragment fn main()->O{return O(textureLoad(source,vec2i(0),0),vec4f(0,1,0,1));}");
    for (id, stage, p) in [(21, 0, vs), (22, 1, ps)] {
        g.set_obj(
            id,
            Obj::Shader(ShaderObj {
                stage,
                hash: "authored native overlap".into(),
                sig: vec![],
                program: Some(Some(p)),
            }),
        );
    }
    g.set_obj(
        23,
        Obj::Layout(Rc::new(LayoutObj {
            els: vec![],
            slots: vec![],
        })),
    );
    g.set_obj(
        24,
        Obj::State(Rc::new(StateObj {
            words: vec![3, 1, 0, 0, 0, 0, 1, 1],
            sampler: None,
        })),
    );
    command(&mut g, 18, &[0, 21]);
    command(&mut g, 18, &[1, 22]);
    command(&mut g, 19, &[23]);
    command(&mut g, 20, &[4]);
    command(&mut g, 29, &[24]);
    g
}
fn scenario(fast: bool, key_first: bool, name: &str) {
    let disjoint = name.starts_with("disjoint");
    let layer = if name == "disjoint_layer" { 1 } else { 0 };
    let mip = if name == "disjoint_mip" { 1 } else { 0 };
    let side = SIZE >> mip;
    let mut g = gpu(fast, layer);
    g.bind_group_prepare.key_first = key_first;
    upload(&g, 1, 0, layer, MATERIAL);
    upload(&g, 1, mip, 0, SENTINEL);
    if disjoint {
        upload(&g, 1, 0, layer, MATERIAL);
    }
    upload(&g, 2, mip, 0, SENTINEL);
    upload(&g, 3, 0, 0, MATERIAL);
    upload(&g, 4, 0, 0, SENTINEL);
    // Populate a real cached native view/BG before the binding transition.
    input(&mut g, 1, 0, 11);
    let ps = g.shader_program(22).unwrap();
    let bgl = g.bgl_for(&ps, 1, 1);
    let cached = g.stage_bind_group(1, &ps, 1, &bgl).unwrap().uid;
    for stage in [0, 2] {
        input(&mut g, stage, 0, 11);
    }
    input(&mut g, 1, 1, 13); // Unrelated slot must survive output binding.
    if name == "output_then_input" {
        outputs(&mut g, 1, mip, 0);
        for stage in 0..3 {
            input(&mut g, stage, 0, 11);
        }
    } else {
        outputs(&mut g, 1, mip, 0);
    }
    for stage in 0..3 {
        assert_eq!(
            g.s.srv[stage][0],
            11,
            "{name} stage{stage}"
        );
    }
    assert_eq!(g.s.srv[1][1], 13);
    if name == "temporary_alias" {
        outputs(&mut g, 0, 0, 0);
        assert_eq!(g.s.srv[1][0], 11);
        let restored = g.stage_bind_group(1, &ps, 1, &bgl).unwrap().uid;
        assert_eq!(cached, restored, "unalias restores the original cached source binding");
        outputs(&mut g, 1, mip, 0);
    }
    let rebound = g.stage_bind_group(1, &ps, 1, &bgl).unwrap().uid;
    if !disjoint {
        assert_ne!(
            cached, rebound,
            "cached source must not bypass actual draw feedback protection"
        );
    }
    command(&mut g, 31, &[1, 0, 0, side, side]);
    command(&mut g, 32, &[0, 3, 1, 0, 0, 0]);
    // A second real draw changes the right half; first draw's left pixels and MRT1 survive.
    input(&mut g, 1, 0, 13);
    command(&mut g, 31, &[1, side / 2, 0, side, side]);
    command(&mut g, 32, &[0, 3, 1, 0, 0, 0]);
    let actual = read(&mut g, 1, mip, 0);
    let expected: Vec<u8> = (0..side)
        .flat_map(|_| {
            (0..side).flat_map(|x| {
                if disjoint || x >= side / 2 {
                    MATERIAL
                } else {
                    [0; 4]
                }
            })
        })
        .collect();
    assert_eq!(actual, expected, "{name} fast={fast}");
    assert_eq!(
        read(&mut g, 2, mip, 0),
        [0, 255, 0, 255].repeat((side * side) as usize)
    );
    assert_eq!(
        read(&mut g, 4, 0, 0),
        SENTINEL.repeat((SIZE * SIZE) as usize)
    );
    assert_eq!(g.draws, 2);
    assert_eq!(g.skipped, 0);
    assert_eq!(g.errors.load(Ordering::SeqCst), 0);
    // Graphics feedback protection never changes the guest-selected resources.
    outputs(&mut g, 0, 0, 0);
    assert_eq!(g.s.srv[1][0], 13);
    eprintln!("native color binding PASS {name} fast={fast}: 2 draws, exact two-MRT pixels, unchanged unrelated texture, zero errors");
}
#[test]
fn metal_native_color_binding_orders_feedback_and_disjoint_subresources() {
    for (fast, key_first) in [(false, false), (true, false), (true, true)] {
        for name in [
            "input_then_output",
            "output_then_input",
            "temporary_alias",
            "disjoint_mip",
            "disjoint_layer",
        ] {
            scenario(fast, key_first, name);
        }
    }
}

#[test]
fn metal_native_depth_targets_do_not_null_color_binding_state() {
    let mut g = gpu(true, 0);
    g.create_texture(90, 2, SIZE, SIZE, 1, 1, 1, 20, BIND_DS | BIND_SRV, 1);
    command(&mut g, 17, &[91, 2, 90, 41, 4, 0, 1, 0, 1]);
    for flags in [0, 1, 2, 3] {
        input(&mut g, 1, 5, 91);
        let mut words = [0; 37];
        words[..4].copy_from_slice(&[2, 0, 0, 28]);
        words[32..].copy_from_slice(&[90, 0, 0, 20, flags]);
        command(&mut g, 26, &words);
        assert_eq!(g.s.srv[1][5], 91, "depth flags {flags}");
    }
    assert_eq!(g.errors.load(Ordering::SeqCst), 0);
}


/// An input that temporarily aliases an output remains selected for a later
/// legal draw. Read the prior output without rebinding its SRV.
#[test]
fn metal_native_color_state_batches_restore_source_pixels_without_rebind() {
    for (fast, key_first) in [(false, false), (true, false), (true, true)] {
        for input_before_output in [false, true] {
            let mut g = gpu(fast, 0);
            g.bind_group_prepare.key_first = key_first;
            upload(&g, 1, 0, 0, MATERIAL);
            upload(&g, 2, 0, 0, SENTINEL);
            upload(&g, 4, 0, 0, SENTINEL);
            if input_before_output { input(&mut g, 1, 0, 11); }
            outputs(&mut g, 1, 0, 0);
            if !input_before_output { input(&mut g, 1, 0, 11); }
            // Force actual feedback binding resolution/cache creation before
            // changing output. A dummy here must not mutate retained state.
            let ps = g.shader_program(22).unwrap();
            let bgl = g.bgl_for(&ps, 1, 1);
            let feedback = g.stage_bind_group(1, &ps, 1, &bgl).unwrap().uid;
            outputs(&mut g, 4, 0, 0);
            let legal = g.stage_bind_group(1, &ps, 1, &bgl).unwrap().uid;
            command(&mut g, 31, &[1, 0, 0, SIZE, SIZE]);
            command(&mut g, 32, &[0, 3, 1, 0, 0, 0]);
            // Pixel assertion deliberately precedes state assertions so the
            // previous implementation demonstrates its visibly black input.
            assert_eq!(read(&mut g, 4, 0, 0), MATERIAL.repeat((SIZE * SIZE) as usize),
                "source lost between state packets: fast={fast} key_first={key_first} input_before_output={input_before_output}");
            assert_eq!(read(&mut g, 1, 0, 0), MATERIAL.repeat((SIZE * SIZE) as usize));
            assert_eq!(read(&mut g, 2, 0, 0), [0, 255, 0, 255].repeat((SIZE * SIZE) as usize));
            assert_eq!(g.s.srv[1][0], 11);
            assert_ne!(feedback, legal);
            assert_eq!(g.draws, 1);
            assert_eq!(g.skipped, 0);
            assert_eq!(g.errors.load(Ordering::SeqCst), 0);
        }
    }
}

/// A graphics target is not an attachment of a compute dispatch. Preserve its
/// selected compute input regardless of graphics binding packet order.
#[test]
fn metal_native_compute_reads_retained_graphics_target_pixels() {
    for (fast, key_first) in [(false, false), (true, false), (true, true)] {
        for input_before_output in [false, true] {
            let mut g = gpu(fast, 0);
            g.bind_group_prepare.key_first = key_first;
            upload(&g, 1, 0, 0, SENTINEL);
            upload(&g, 2, 0, 0, SENTINEL);
            upload(&g, 3, 0, 0, MATERIAL);
            g.create_texture(5, 2, SIZE, SIZE, 1, 1, 1, 28, BIND_UAV | BIND_SRV, 1);
            upload(&g, 5, 0, 0, SENTINEL);
            command(&mut g, 37, &[15, 2, 5, 28, 4, 0, 0, 0, 1]);
            let cs = program(&mut g, "@group(0) @binding(32) var source:texture_2d<f32>; @group(0) @binding(160) var destination:texture_storage_2d<rgba8unorm,write>; @compute @workgroup_size(1,1,1) fn main(@builtin(global_invocation_id) id:vec3u){textureStore(destination,vec2i(id.xy),textureLoad(source,vec2i(id.xy),0));}");
            g.set_obj(25, Obj::Shader(ShaderObj {
                stage: 2, hash: "authored compute graphics overlap".into(), sig: vec![], program: Some(Some(cs)),
            }));
            command(&mut g, 18, &[2, 25]);
            command(&mut g, 38, &[2, 0, 1, 15, u32::MAX]);
            if input_before_output { input(&mut g, 2, 0, 11); }
            outputs(&mut g, 1, 0, 0);
            input(&mut g, 1, 0, 13);
            command(&mut g, 31, &[1, 0, 0, SIZE, SIZE]);
            command(&mut g, 32, &[0, 3, 1, 0, 0, 0]);
            if !input_before_output { input(&mut g, 2, 0, 11); }
            command(&mut g, 39, &[SIZE, SIZE, 1]);
            assert_eq!(read(&mut g, 5, 0, 0), MATERIAL.repeat((SIZE * SIZE) as usize),
                "compute input lost to graphics target: fast={fast} key_first={key_first} input_before_output={input_before_output}");
            assert_eq!(read(&mut g, 1, 0, 0), MATERIAL.repeat((SIZE * SIZE) as usize));
            assert_eq!(read(&mut g, 2, 0, 0), [0, 255, 0, 255].repeat((SIZE * SIZE) as usize));
            assert_eq!(g.s.srv[2][0], 11);
            assert_eq!(g.draws, 1);
            assert_eq!(g.skipped, 0);
            assert_eq!(g.errors.load(Ordering::SeqCst), 0);
        }
    }
}

/// A sampled depth proxy has a distinct native allocation from its color
/// source. It must retain the pre-draw source even when that color is an RT.
#[test]
fn metal_native_color_depth_proxy_keeps_source_pixels() {
    for (fast, key_first) in [(false, false), (true, false), (true, true)] {
        let mut g = gpu(fast, 0);
        g.bind_group_prepare.key_first = key_first;
        upload(&g, 1, 0, 0, MATERIAL);
        upload(&g, 2, 0, 0, SENTINEL);
        upload(&g, 3, 0, 0, SENTINEL);
        let ps = program(&mut g, "@group(1) @binding(32) var source:texture_depth_2d; struct O{@location(0)a:vec4f,@location(1)b:vec4f}; @fragment fn main()->O{let d=textureLoad(source,vec2i(0),0);return O(vec4f(d,d,d,1),vec4f(0,1,0,1));}");
        g.set_obj(22, Obj::Shader(ShaderObj {
            stage: 1, hash: "authored distinct depth proxy".into(), sig: vec![], program: Some(Some(ps)),
        }));
        outputs(&mut g, 1, 0, 0);
        input(&mut g, 1, 0, 11);
        command(&mut g, 31, &[1, 0, 0, SIZE, SIZE]);
        command(&mut g, 32, &[0, 3, 1, 0, 0, 0]);
        assert_eq!(read(&mut g, 1, 0, 0), [MATERIAL[0], MATERIAL[0], MATERIAL[0], 255].repeat((SIZE * SIZE) as usize));
        assert_eq!(read(&mut g, 2, 0, 0), [0, 255, 0, 255].repeat((SIZE * SIZE) as usize));
        assert_eq!(read(&mut g, 3, 0, 0), SENTINEL.repeat((SIZE * SIZE) as usize));
        assert_eq!(g.s.srv[1][0], 11);
        assert_eq!(g.draws, 1);
        assert_eq!(g.skipped, 0);
        assert_eq!(g.errors.load(Ordering::SeqCst), 0);
    }
}

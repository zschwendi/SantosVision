//! Offscreen production-Gpu regressions for optional HUD extraction.
//! Synthetic programs deliberately bypass source-pin acquisition in this white-box
//! fixture. They exercise resource invalidation and fallback, never qualify game UI.
use super::*;

static SERIAL: Mutex<()> = Mutex::new(());
const WORDS: u64 = 0x1_0000;

fn gpu() -> Gpu {
    let mut config = wasmtime::Config::new();
    config.wasm_threads(true); config.shared_memory(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, wasmtime::MemoryType::shared(4, 4)).unwrap();
    let mut g = Gpu::new(memory, 0, 65536, 0, None).unwrap();
    g.pipe_jobs = None; // Exercise deterministic production synchronous compilation.
    for id in [1, 2, 3] { g.create_texture(id, 2, 1, 1, 1, 1, 1, 28, BIND_RT | BIND_SRV | BIND_UAV, 1); }
    g
}

fn view() -> crate::xr_view::WorldView {
    let identity = [1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.];
    crate::xr_view::WorldView { provenance_epoch: crate::xr_view::view_epoch(),
        camera_sequence: 1, request_frame: 1, head_pose_frame: 1,
        presentation_time_seconds: 1., eye_index: 0, view_count: 2,
        game_from_eye: identity, game_from_stage: identity,
        projection_from_eye: identity, depth_reverse_z: true, headset_applied: true, optics:crate::xr_view::ScopeOptics::default() }
}

fn arm(g: &mut Gpu) {
    let texture = g.tex(1).unwrap().tex.clone();
    let images = g.reserve_hud_images(&texture).unwrap();
    g.hud_capture = Some(HudCapture { final_id: 1, final_texture: texture,
        view: view(), images: Some(images), draws: 0,
        scratch: HashMap::from([(2, g.tex(2).unwrap().tex.clone())]), rejected: None });
}

fn command(g: &mut Gpu, opcode: u32, words: &[u32]) {
    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    assert!(crate::write_bytes(&g.mem, WORDS, &bytes));
    g.hud_non_draw_command(opcode, WORDS / 4, words.len() as u64);
}

fn resource_view(g: &mut Gpu, id: u32, resource: u32, uav: bool) {
    let value = ViewObj { res: resource, is_buf: false, fmt: 28, vdim: 4,
        a: 0, b: 1, c: 0, d: 1, views: HashMap::new() };
    g.set_obj(id, if uav { Obj::Uav(value) } else { Obj::Srv(value) });
}

#[test]
fn hud_final_resource_reads_and_writes_reject_decomposition() {
    let _serial = SERIAL.lock().unwrap();
    let mut g = gpu();
    for (opcode, words) in [(34, vec![2,0,0,0,0,1]), (35, vec![2,1]),
        (36, vec![2,0,1]), (42, vec![1]), (5, vec![1]), (13, vec![1]),
        (4, vec![1]), (3, vec![1])] {
        arm(&mut g); command(&mut g, opcode, &words);
        assert!(g.hud_capture.as_ref().unwrap().rejected.is_some(), "opcode {opcode}");
    }
    resource_view(&mut g, 20, 1, true);
    for opcode in [40, 41] {
        arm(&mut g); command(&mut g, opcode, &[20]);
        assert!(g.hud_capture.as_ref().unwrap().rejected.is_some(), "view opcode {opcode}");
    }
    arm(&mut g); g.s.uav[2][0] = 20; command(&mut g, 39, &[]);
    assert!(g.hud_capture.as_ref().unwrap().rejected.is_some());
    g.s.uav[2][0] = 0; resource_view(&mut g, 21, 1, false);
    arm(&mut g); g.s.srv[2][0] = 21; command(&mut g, 39, &[]);
    assert!(g.hud_capture.as_ref().unwrap().rejected.is_some());
}

#[test]
fn hud_scratch_mutation_revokes_only_its_role_and_unrelated_operations_survive() {
    let _serial = SERIAL.lock().unwrap();
    let mut g = gpu();
    arm(&mut g); command(&mut g, 35, &[2,3]);
    let capture = g.hud_capture.as_ref().unwrap();
    assert!(!capture.scratch.contains_key(&2)); assert!(capture.rejected.is_none());
    arm(&mut g); command(&mut g, 35, &[3,3]);
    let capture = g.hud_capture.as_ref().unwrap();
    assert!(capture.scratch.contains_key(&2)); assert!(capture.rejected.is_none());
    command(&mut g, 999, &[]);
    assert!(g.hud_capture.as_ref().unwrap().rejected.is_some());
}

fn program(g: &mut Gpu, text: &str, kind: crate::xr_hud::ShaderKind) -> Rc<Program> {
    let module = g.device.create_shader_module(ShaderModuleDescriptor {
        label: Some("HUD fallback synthetic fixture"), source: ShaderSource::Wgsl(text.into()) });
    Rc::new(Program { hud_kind: Some(kind), module, info: parse_wgsl(text), consts: None,
        pack: None, text: text.to_owned(), flat: vec![], group_bindings: BTreeMap::new(),
        variants: RefCell::new(HashMap::new()), label: "HUD fallback synthetic fixture".into(),
        uid: g.uid(), vid: false, iid: false, out_locs: vec![], in_locs: vec![], clip: false })
}

struct TrackingGuard;
impl TrackingGuard {
    fn new() -> Self { crate::xr::set_active(true);
        assert_eq!(crate::xr_view::sv_xr_world_camera_set_enabled(1), 0);
        assert_eq!(crate::xr_hud::sv_xr_set_hud_ready(1), 0); Self }
}
impl Drop for TrackingGuard {
    fn drop(&mut self) { crate::xr_hud::sv_xr_set_hud_ready(0);
        crate::xr_view::sv_xr_world_camera_set_enabled(0); crate::xr::set_active(false); }
}

fn shaders(g: &mut Gpu) -> (Rc<Program>, Rc<Program>) {
    use crate::xr_hud::ShaderKind as K;
    let vertex = program(g, "@vertex fn main(@builtin(vertex_index) id:u32)->@builtin(position) vec4<f32> { var points=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));return vec4<f32>(points[id],0.,1.); }", K::ScaleformVertex);
    let pixel = program(g, "var<private> o0:vec4<f32>; @fragment fn main()-> @location(0) vec4<f32> {o0=vec4<f32>(0.,1.,0.,1.); return o0; }", K::ScaleformPixel);
    (vertex, pixel)
}

#[test]
fn hud_nonzero_final_attachment_and_graphics_uav_reject_before_draw() {
    let _serial = SERIAL.lock().unwrap(); let _tracking = TrackingGuard::new();
    let mut g = gpu(); let (vs, ps) = shaders(&mut g);
    arm(&mut g); g.s.rt[0] = Rt { tex: 2, mip: 0, slice: 0, fmt: 28 };
    g.s.rt[1] = Rt { tex: 1, mip: 0, slice: 0, fmt: 28 };
    assert!(g.prepare_hud_draw(&vs, Some(&ps)).is_none());
    assert!(g.hud_capture.as_ref().unwrap().rejected.is_some());
    g.s.rt[1] = Rt::default(); resource_view(&mut g, 20, 1, true);
    for stage in [0,1] { arm(&mut g); g.s.uav[stage][0] = 20;
        assert!(g.prepare_hud_draw(&vs, Some(&ps)).is_none());
        assert!(g.hud_capture.as_ref().unwrap().rejected.is_some()); g.s.uav[stage][0] = 0; }
}

#[test]
fn hud_scratch_secondary_attachment_cannot_keep_its_ui_certificate() {
    let _serial = SERIAL.lock().unwrap(); let _tracking = TrackingGuard::new();
    let mut g = gpu(); let (vs, ps) = shaders(&mut g); arm(&mut g);
    g.s.rt[0] = Rt { tex: 3, mip: 0, slice: 0, fmt: 28 };
    g.s.rt[1] = Rt { tex: 2, mip: 0, slice: 0, fmt: 28 };
    assert!(g.prepare_hud_draw(&vs, Some(&ps)).is_none());
    assert!(!g.hud_capture.as_ref().unwrap().scratch.contains_key(&2),
        "any MRT mutation must revoke a secondary scratch target's UI role");
}

fn pixel(g: &mut Gpu, id: u32) -> [u8;4] {
    g.flush(); let texture = g.tex(id).unwrap().tex.clone();
    let output = g.device.create_buffer(&BufferDescriptor { label: None, size: 256,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut encoder = g.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture: &texture, mip_level: 0,
        origin: Origin3d::ZERO, aspect: TextureAspect::All }, TexelCopyBufferInfo { buffer: &output,
        layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(1) } },
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
    let (tx,rx) = std::sync::mpsc::channel();
    encoder.map_buffer_on_submit(&output, MapMode::Read, .., move |result| { let _ = tx.send(result); });
    let submission = g.queue.submit([encoder.finish()]);
    g.device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(10)) }).unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    let rgba = output.slice(..).get_mapped_range().unwrap()[..4].try_into().unwrap(); output.unmap(); rgba
}

#[test]
fn hud_failed_optional_pipeline_still_executes_original_flat_draw() {
    let _serial = SERIAL.lock().unwrap(); let _tracking = TrackingGuard::new();
    let mut g = gpu(); let (vs, ps) = shaders(&mut g);
    // A failed optional shader specialization is a real production cache result.
    ps.variants.borrow_mut().insert("|hudSome(Opaque)".into(), None);
    for (id,stage,p) in [(10,0,vs),(11,1,ps)] { g.set_obj(id, Obj::Shader(ShaderObj {
        stage, hash: "synthetic fixture".into(), sig: vec![], program: Some(Some(p)) })); }
    g.set_obj(12, Obj::Layout(Rc::new(LayoutObj { els: vec![], slots: vec![] })));
    g.set_obj(13, Obj::State(Rc::new(StateObj { words: vec![3,1,0,0,0,0,1,0], sampler: None })));
    g.s.shader = [10,11,0]; g.s.layout = 12; g.s.rs = 13; g.s.topo = 4;
    g.s.rt[0] = Rt { tex: 1, mip: 0, slice: 0, fmt: 28 };
    g.clear_rt(1,0,0,[0.,0.,1.,1.]); arm(&mut g);
    let arguments: Vec<_> = [0u32,3,1,0,0,0].iter().flat_map(|n| n.to_le_bytes()).collect();
    assert!(crate::write_bytes(&g.mem, WORDS, &arguments));
    g.do_draw(WORDS / 4);
    assert!(g.hud_capture.as_ref().unwrap().rejected.as_deref().unwrap().contains("pipeline unavailable"));
    assert!(g.hud_draw.is_none()); assert_eq!(g.draws,1); assert_eq!(g.skipped,0);
    assert_eq!(pixel(&mut g,1),[0,255,0,255],"the original draw must survive a failed optional HUD pipeline");
    assert_eq!(g.errors.load(Ordering::Relaxed),0);
}

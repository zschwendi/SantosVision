//! Isolated same-eye raster density proof. No game runtime, publication or app integration.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::BTreeMap, error::Error, fs, path::Path, sync::mpsc};
use wgpu::util::DeviceExt;
use wgpu::*;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Mat = [f64; 16];
const CAPTURE_HASH: &str = "0f7d0b2a6e0ee251e3456cf8c91e9e7fdf9b051a62e8414a6cfbbe320d2289fc";
const VS_HASH: &str = "fc914d159a51b8bd8e6d35633dd385a6cef50fb1ed740d5ad3d86893a03acb61";
const PS_HASH: &str = "1e3d02aa02c1219e721c351c2fbcebc45154b2f9b1a8f2bdc8f1f6a96ff0daed";
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SCALE: f64 = 3.;

fn sha(b: &[u8]) -> String { format!("{:x}", Sha256::digest(b)) }
fn require(ok: bool, text: &str) -> Result<()> { if ok { Ok(()) } else { Err(text.into()) } }
fn hex(s: &str) -> Result<Vec<u8>> {
    require(s.len() % 2 == 0 && s.len() <= 1024 * 1024, "invalid bounded hex")?;
    (0..s.len()).step_by(2).map(|i| Ok(u8::from_str_radix(&s[i..i + 2], 16)?)).collect()
}
fn matrix_value(v: &Value) -> Result<Mat> {
    let a = v.as_array().ok_or("matrix array absent")?;
    require(a.len() == 16, "matrix length")?;
    let mut out = [0.; 16];
    for (o, v) in out.iter_mut().zip(a) { *o = v.as_f64().ok_or("matrix number absent")?; }
    require(out.iter().all(|v| v.is_finite()), "nonfinite matrix")?;
    Ok(out)
}
fn matrix_bytes(b: &[u8]) -> Mat {
    std::array::from_fn(|i| f32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap()) as f64)
}
fn mul(a: Mat, b: Mat) -> Mat {
    std::array::from_fn(|i| (0..4).map(|k| a[k * 4 + i % 4] * b[i / 4 * 4 + k]).sum())
}
fn inverse_rigid(a: Mat) -> Result<Mat> {
    let mut out = [0.; 16];
    for c in 0..3 { for r in 0..3 { out[c * 4 + r] = a[r * 4 + c]; } }
    for r in 0..3 { out[12 + r] = -(0..3).map(|c| out[c * 4 + r] * a[12 + c]).sum::<f64>(); }
    out[15] = 1.;
    let identity = mul(out, a);
    require(identity.iter().enumerate().all(|(i, x)| (*x - if i % 5 == 0 { 1. } else { 0. }).abs() < 0.00002), "camera is not rigid")?;
    Ok(out)
}
fn vector(a: Mat, p: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|r| (0..4).map(|c| a[c * 4 + r] * p[c]).sum())
}
fn patch_cb(original: &[u8], matrix: Mat) -> Vec<u8> {
    let mut result = original.to_vec();
    for (i, v) in matrix.iter().enumerate() { result[128 + i * 4..132 + i * 4].copy_from_slice(&(*v as f32).to_le_bytes()); }
    result
}
fn clip_crop(source: Mat, center: [f64; 2]) -> Mat {
    let mut out = source;
    for c in 0..4 {
        out[c * 4] = SCALE * (source[c * 4] - center[0] * source[c * 4 + 3]);
        out[c * 4 + 1] = SCALE * (source[c * 4 + 1] - center[1] * source[c * 4 + 3]);
    }
    out
}
fn crop_matrix(center: [f64; 2]) -> Mat {
    let mut a = [0.; 16];
    for i in 0..4 { a[i * 5] = 1.; }
    a[0] = SCALE; a[5] = SCALE; a[12] = -SCALE * center[0]; a[13] = -SCALE * center[1]; a
}
fn write_ppm(path: &Path, rgba: &[u8]) -> Result<()> {
    let mut bytes = format!("P6\n{} {}\n255\n", WIDTH, HEIGHT).into_bytes();
    // The four game G-buffer outputs are diagnostic values, not display-ready colors.
    for bgra in rgba.chunks_exact(4) { bytes.extend_from_slice(&[bgra[2], bgra[1], bgra[0]]); }
    fs::write(path, bytes)?; Ok(())
}

struct Input {
    value: Value, capture_hash: String, vs: String, ps: String,
    uniforms: BTreeMap<(u32, u32), Vec<u8>>, vertices: Vec<u8>, indices: Vec<u8>,
    source_clip: Mat, object: Mat, view: Mat, projection: Mat, center: [f64; 2], crop_origin: [u32; 2],
}
impl Input {
    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        require(bytes.len() < 2 * 1024 * 1024 && sha(&bytes) == CAPTURE_HASH, "capture hash admission failed")?;
        let x: Value = serde_json::from_slice(&bytes)?;
        require(x["shader"]["vertex_hash"] == "6a6a0898a51cf403" && x["shader"]["pixel_hash"] == "75cf2d53bf9497c9", "shader family admission")?;
        let vs = x["shader"]["vertex_source_wgsl"].as_str().ok_or("VS absent")?.to_owned();
        let ps = x["shader"]["pixel_source_wgsl"].as_str().ok_or("PS absent")?.to_owned();
        require(sha(vs.as_bytes()) == VS_HASH && sha(ps.as_bytes()) == PS_HASH, "shader source hash admission")?;
        require(x["world_view"]["depth_reverse_z"] == true && x["world_view"]["eye_index"] == 0, "captured eye/depth changed")?;
        require(x["draw"]["kind"] == 1 && x["draw"]["instances"] == 1 && x["draw"]["count"] == 1506, "captured draw changed")?;
        let mut uniforms = BTreeMap::new();
        for cb in x["constant_buffers"].as_array().ok_or("CBs absent")? {
            let stage = cb["stage"].as_u64().ok_or("CB stage")? as u32;
            let binding = cb["binding"].as_u64().ok_or("CB binding")? as u32;
            require(uniforms.insert((stage, binding), hex(cb["bytes_hex"].as_str().ok_or("CB data")?)?).is_none(), "duplicate CB")?;
        }
        let cb = uniforms.get(&(0, 1)).ok_or("camera CB absent")?;
        require(cb.len() == 256, "camera CB size changed")?;
        let object = matrix_bytes(&cb[..64]);
        let source_clip = matrix_bytes(&cb[128..192]);
        let camera = matrix_value(&x["world_view"]["game_from_eye"])?;
        let view = inverse_rigid(camera)?;
        let projection = matrix_value(&x["world_view"]["projection_from_eye"])?;
        let expected = mul(mul(projection, view), object);
        require(expected.iter().zip(source_clip).all(|(a, b)| (a - b).abs() < 0.00002), "pinned clip matrix contract failed")?;
        let row = &cb[240..256];
        require((0..3).all(|i| (f32::from_le_bytes(row[4 * i..4 * i + 4].try_into().unwrap()) as f64 - camera[12 + i]).abs() < 0.0002), "captured camera CB mismatch")?;
        let vertices = hex(x["vertex_buffers"][0]["bytes_hex"].as_str().ok_or("VB absent")?)?;
        let indices = hex(x["index_buffer"]["bytes_hex"].as_str().ok_or("IB absent")?)?;
        require(vertices.len() == 16056 && vertices.len() % 36 == 0 && x["index_buffer"]["format"] == 57, "captured buffer format changed")?;
        let mut low = [f64::INFINITY; 2]; let mut high = [f64::NEG_INFINITY; 2];
        for v in vertices.chunks_exact(36) {
            let p = [f32::from_le_bytes(v[0..4].try_into().unwrap()) as f64, f32::from_le_bytes(v[4..8].try_into().unwrap()) as f64, f32::from_le_bytes(v[8..12].try_into().unwrap()) as f64, 1.];
            let q = vector(source_clip, p);
            require(q.iter().all(|x| x.is_finite()) && q[3] > 0. && q[2] >= 0. && q[2] <= q[3], "draw crosses camera/depth plane")?;
            for i in 0..2 { low[i] = low[i].min(q[i] / q[3]); high[i] = high[i].max(q[i] / q[3]); }
        }
        let geometric_center = [(low[0] + high[0]) * 0.5, (low[1] + high[1]) * 0.5];
        let crop_origin = [((((geometric_center[0]+1.)*0.5*WIDTH as f64*SCALE)-WIDTH as f64*0.5)/2.).round() as u32 * 2,
                           ((((1.-geometric_center[1])*0.5*HEIGHT as f64*SCALE)-HEIGHT as f64*0.5)/2.).round() as u32 * 2];
        let center = [2.*(crop_origin[0] as f64+WIDTH as f64*0.5)/(WIDTH as f64*SCALE)-1.,
                      1.-2.*(crop_origin[1] as f64+HEIGHT as f64*0.5)/(HEIGHT as f64*SCALE)];
        require(center.iter().all(|x| x.abs() + 1. / SCALE <= 1.), "ROI not nested within source")?;
        Ok(Self { value: x, capture_hash: sha(&bytes), vs, ps, uniforms, vertices, indices, source_clip, object, view, projection, center, crop_origin })
    }
    fn reference(&self, center: [f64; 2]) -> (Mat, Mat) {
        // Independently construct narrow projection from original physical matrix.
        let crop = crop_matrix(center);
        let direct = mul(mul(mul(crop, self.projection), self.view), self.object);
        // Captured CB records original producer Float32 rounding. Preserve that
        // measured residual rather than silently changing source Z/W or camera.
        let expected_source = mul(mul(self.projection, self.view), self.object);
        let residual = std::array::from_fn(|i| self.source_clip[i] - expected_source[i]);
        let transformed_residual = mul(crop, residual);
        let reference = std::array::from_fn(|i| direct[i] + transformed_residual[i]);
        (reference, direct)
    }
}

struct GPU { device: Device, queue: Queue, pipeline: RenderPipeline, pixel_group: BindGroup,
    vertex_layout: BindGroupLayout, vertex: Buffer, index: Buffer, adapter: String }
impl GPU {
    fn new(input: &Input) -> Result<Self> {
        let mut descriptor = InstanceDescriptor::new_without_display_handle(); descriptor.backends = Backends::METAL;
        let instance = Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions { power_preference: PowerPreference::HighPerformance, compatible_surface: None, force_fallback_adapter: false, apply_limit_buckets: false }))?;
        require(adapter.features().contains(Features::DEPTH32FLOAT_STENCIL8), "Metal depth/stencil format unavailable")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor { label: Some("private same-eye density fixture"), required_features: Features::DEPTH32FLOAT_STENCIL8, required_limits: adapter.limits(), ..Default::default() }))?;
        let uniform_entries = |stage: u32| input.uniforms.iter().filter(|((s, _), _)| *s == stage).map(|((_, binding), bytes)| BindGroupLayoutEntry { binding: *binding, visibility: if stage == 0 { ShaderStages::VERTEX } else { ShaderStages::FRAGMENT }, ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: std::num::NonZeroU64::new(bytes.len() as u64) }, count: None }).collect::<Vec<_>>();
        let vertex_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("captured VS CB bindings"), entries: &uniform_entries(0) });
        let mut pixel_entries = uniform_entries(1);
        pixel_entries.push(BindGroupLayoutEntry { binding: 16, visibility: ShaderStages::FRAGMENT, ty: BindingType::Sampler(SamplerBindingType::Filtering), count: None });
        pixel_entries.push(BindGroupLayoutEntry { binding: 32, visibility: ShaderStages::FRAGMENT, ty: BindingType::Texture { sample_type: TextureSampleType::Float { filterable: true }, view_dimension: TextureViewDimension::D2, multisampled: false }, count: None });
        let pixel_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("captured PS bindings with synthetic material"), entries: &pixel_entries });
        let uniform_buffers = input.uniforms.iter().filter(|((s, _), _)| *s == 1).map(|((_, b), v)| (*b, device.create_buffer_init(&util::BufferInitDescriptor { label: Some("captured PS CB"), contents: v, usage: BufferUsages::UNIFORM }))).collect::<Vec<_>>();
        let texture = device.create_texture(&TextureDescriptor { label: Some("SYNTHETIC deterministic material"), size: Extent3d { width: 256, height: 256, depth_or_array_layers: 1 }, mip_level_count: 9, sample_count: 1, dimension: TextureDimension::D2, format: TextureFormat::Rgba8Unorm, usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST, view_formats: &[] });
        for level in 0..9 {
            let size = 256 >> level; let mut pixels = Vec::new();
            for y in 0..size { for x in 0..size { pixels.extend_from_slice(&[(x * 255 / size.max(1)) as u8, (y * 255 / size.max(1)) as u8, if (x + y) % 2 == 0 { 240 } else { 16 }, 255]); } }
            queue.write_texture(TexelCopyTextureInfo { texture: &texture, mip_level: level, origin: Origin3d::ZERO, aspect: TextureAspect::All }, &pixels, TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size * 4), rows_per_image: Some(size) }, Extent3d { width: size, height: size, depth_or_array_layers: 1 });
        }
        let view = texture.create_view(&TextureViewDescriptor::default());
        let sampler = device.create_sampler(&SamplerDescriptor { label: Some("explicit synthetic material sampler"), address_mode_u: AddressMode::Repeat, address_mode_v: AddressMode::Repeat, mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, mipmap_filter: MipmapFilterMode::Linear, ..Default::default() });
        let mut entries = uniform_buffers.iter().map(|(b, v)| BindGroupEntry { binding: *b, resource: v.as_entire_binding() }).collect::<Vec<_>>();
        entries.push(BindGroupEntry { binding: 16, resource: BindingResource::Sampler(&sampler) });
        entries.push(BindGroupEntry { binding: 32, resource: BindingResource::TextureView(&view) });
        let pixel_group = device.create_bind_group(&BindGroupDescriptor { label: Some("synthetic material with original constants"), layout: &pixel_layout, entries: &entries });
        let vs = device.create_shader_module(ShaderModuleDescriptor { label: Some("pinned real VS_TransformD"), source: ShaderSource::Wgsl(Cow::Borrowed(&input.vs)) });
        let ps = device.create_shader_module(ShaderModuleDescriptor { label: Some("pinned real PS_DeferredTextured"), source: ShaderSource::Wgsl(Cow::Borrowed(&input.ps)) });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&vertex_layout), Some(&pixel_layout)], immediate_size: 0 });
        let attributes = [VertexAttribute { format: VertexFormat::Float32x3, offset: 0, shader_location: 0 }, VertexAttribute { format: VertexFormat::Unorm8x4, offset: 24, shader_location: 1 }, VertexAttribute { format: VertexFormat::Float32x2, offset: 28, shader_location: 2 }, VertexAttribute { format: VertexFormat::Float32x3, offset: 12, shader_location: 3 }];
        let targets = (0..4).map(|i| Some(ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: if i == 0 { ColorWrites::RED | ColorWrites::GREEN | ColorWrites::BLUE } else { ColorWrites::ALL } })).collect::<Vec<_>>();
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor { label: Some("same-eye pinned shader scratch"), layout: Some(&layout), vertex: VertexState { module: &vs, entry_point: Some("main"), compilation_options: PipelineCompilationOptions::default(), buffers: &[Some(VertexBufferLayout { array_stride: 36, step_mode: VertexStepMode::Vertex, attributes: &attributes })] }, fragment: Some(FragmentState { module: &ps, entry_point: Some("main"), compilation_options: PipelineCompilationOptions::default(), targets: &targets }), primitive: PrimitiveState { topology: PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() }, depth_stencil: Some(DepthStencilState { format: TextureFormat::Depth32FloatStencil8, depth_write_enabled: Some(true), depth_compare: Some(CompareFunction::GreaterEqual), stencil: StencilState::default(), bias: DepthBiasState::default() }), multisample: MultisampleState::default(), multiview_mask: None, cache: None });
        let vertex = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("captured complete interleaved VB"), contents: &input.vertices, usage: BufferUsages::VERTEX });
        let index = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("captured IB"), contents: &input.indices, usage: BufferUsages::INDEX });
        Ok(Self { device, queue, pipeline, pixel_group, vertex_layout, vertex, index, adapter: format!("{:?}", adapter.get_info()) })
    }
    fn render(&self, input: &Input, label: &str, clip: Mat, width: u32, height: u32) -> Result<Vec<Vec<u8>>> {
        let uniforms = input.uniforms.iter().filter(|((s, _), _)| *s == 0).map(|((_, b), original)| {
            let bytes = if *b == 1 { patch_cb(original, clip) } else { original.clone() };
            (*b, self.device.create_buffer_init(&util::BufferInitDescriptor { label: Some(label), contents: &bytes, usage: BufferUsages::UNIFORM }))
        }).collect::<Vec<_>>();
        let entries = uniforms.iter().map(|(b, buffer)| BindGroupEntry { binding: *b, resource: buffer.as_entire_binding() }).collect::<Vec<_>>();
        let group = self.device.create_bind_group(&BindGroupDescriptor { label: Some(label), layout: &self.vertex_layout, entries: &entries });
        let textures = (0..5).map(|i| self.device.create_texture(&TextureDescriptor { label: Some(label), size: Extent3d { width, height, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format: if i == 4 { TextureFormat::Depth32FloatStencil8 } else { TextureFormat::Bgra8Unorm }, usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC, view_formats: &[] })).collect::<Vec<_>>();
        let views = textures.iter().map(|t| t.create_view(&TextureViewDescriptor::default())).collect::<Vec<_>>();
        let mut encoder = self.device.create_command_encoder(&CommandEncoderDescriptor { label: Some(label) });
        {
            let colors = views[..4].iter().map(|view| Some(RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Color::TRANSPARENT), store: StoreOp::Store } })).collect::<Vec<_>>();
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor { label: Some(label), color_attachments: &colors, depth_stencil_attachment: Some(RenderPassDepthStencilAttachment { view: &views[4], depth_ops: Some(Operations { load: LoadOp::Clear(0.), store: StoreOp::Store }), stencil_ops: Some(Operations { load: LoadOp::Clear(0x17), store: StoreOp::Store }) }), timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            pass.set_pipeline(&self.pipeline); pass.set_bind_group(0, &group, &[]); pass.set_bind_group(1, &self.pixel_group, &[]);
            pass.set_vertex_buffer(0, self.vertex.slice(..)); pass.set_index_buffer(self.index.slice(..), IndexFormat::Uint16);
            pass.set_viewport(0., 0., width as f32, height as f32, 0., 1.); pass.set_scissor_rect(0, 0, width, height);
            pass.draw_indexed(0..1506, 0, 0..1);
        }
        let mut buffers = Vec::new();
        for i in 0..6 {
            let bpp = if i == 5 { 1 } else { 4 }; let row = width * bpp; let pitch = row.div_ceil(256) * 256;
            let buffer = self.device.create_buffer(&BufferDescriptor { label: Some(label), size: pitch as u64 * height as u64, usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
            encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture: &textures[i.min(4)], mip_level: 0, origin: Origin3d::ZERO, aspect: if i == 4 { TextureAspect::DepthOnly } else if i == 5 { TextureAspect::StencilOnly } else { TextureAspect::All } }, TexelCopyBufferInfo { buffer: &buffer, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(height) } }, Extent3d { width, height, depth_or_array_layers: 1 });
            buffers.push((buffer, row as usize, pitch as usize));
        }
        let submission = self.queue.submit([encoder.finish()]);
        self.device.poll(PollType::Wait { submission_index: Some(submission), timeout: Some(std::time::Duration::from_secs(60)) })?;
        let mut result = Vec::new();
        for (buffer, row, pitch) in buffers {
            let (send, receive) = mpsc::channel(); buffer.slice(..).map_async(MapMode::Read, move |r| { let _ = send.send(r); });
            self.device.poll(PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(60)) })?; receive.recv()??;
            let mapped = buffer.slice(..).get_mapped_range()?; let mut tight = Vec::new();
            for y in 0..height as usize { tight.extend_from_slice(&mapped[y * pitch..y * pitch + row]); }
            drop(mapped); buffer.unmap(); result.push(tight);
        }
        Ok(result)
    }
}

fn covered(data: &[Vec<u8>]) -> usize { data[4].chunks_exact(4).filter(|v| f32::from_le_bytes((*v).try_into().unwrap()) > 0.).count() }
fn comparison(a: &[Vec<u8>], b: &[Vec<u8>]) -> Value {
    let diffs = a.iter().zip(b).map(|(a, b)| a.iter().zip(b).filter(|(a, b)| a != b).count()).collect::<Vec<_>>();
    let max_byte_difference = a.iter().zip(b).map(|(a,b)|a.iter().zip(b).map(|(a,b)|a.abs_diff(*b)).max().unwrap_or(0)).collect::<Vec<_>>();
    let depth_ulp = a[4].chunks_exact(4).zip(b[4].chunks_exact(4)).map(|(a,b)|u32::from_le_bytes(a.try_into().unwrap()).abs_diff(u32::from_le_bytes(b.try_into().unwrap()))).max().unwrap_or(0);
    let coverage_differences = a[4].chunks_exact(4).zip(b[4].chunks_exact(4)).filter(|(a,b)|(f32::from_le_bytes((*a).try_into().unwrap())>0.)!=(f32::from_le_bytes((*b).try_into().unwrap())>0.)).count();
    let depth_error = a[4].chunks_exact(4).zip(b[4].chunks_exact(4)).map(|(a,b)| (f32::from_le_bytes(a.try_into().unwrap()) - f32::from_le_bytes(b.try_into().unwrap())).abs()).fold(0f32, f32::max);
    json!({"differing_bytes_by_attachment": diffs,"max_byte_difference_by_attachment":max_byte_difference,"max_depth_ULPs":depth_ulp,"coverage_different_pixels":coverage_differences, "all_attachment_bytes_equal": a == b, "max_absolute_depth_error":depth_error})
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    require(args.len() == 3 || (args.len() == 4 && args[3] == "--cpu-only"), "usage: fixture CAPTURE_JSON OUTPUT_DIRECTORY [--cpu-only]")?;
    let input = Input::load(Path::new(&args[1]))?; let out = Path::new(&args[2]); fs::create_dir_all(out)?;
    let center_clip = clip_crop(input.source_clip, input.center);
    let (reference, direct) = input.reference(input.center);
    let cb = &input.uniforms[&(0,1)];
    let center_bytes = patch_cb(cb, center_clip); let reference_bytes = patch_cb(cb, reference);
    require(center_bytes == reference_bytes, "independent residual-preserving center reference differs at Float32 CB precision")?;
    let original_z_w = (0..4).all(|c| center_clip[c * 4 + 2] == input.source_clip[c * 4 + 2] && center_clip[c * 4 + 3] == input.source_clip[c * 4 + 3]);
    require(original_z_w && center_bytes[240..256] == cb[240..256], "center changed captured Z/W or camera row")?;
    let mut report = json!({"schema":1,"scope":"isolated same-eye pinned native shader geometry experiment; no game runtime or finished-image publication", "capture_sha256":input.capture_hash,"vertex_shader_sha256":VS_HASH,"pixel_shader_sha256":PS_HASH,"adapter":null,"gpu_executed":false,"source_eye":input.value["world_view"]["eye_index"],"original_game_from_eye":input.value["world_view"]["game_from_eye"],"source_projection":input.projection,"source_clip_cb":input.source_clip,"center_clip_cb":center_clip,"source_request_frame":input.value["world_view"]["request_frame"],"region":{"scale":SCALE,"ndc_center":input.center,"kind":"declared nested ROI around captured draw; NOT physical-frustum center","nested_half_extent":1. / SCALE},"material":"synthetic deterministic gradient/checker mip chain; original sampled game texture/sampler absent from capture", "fixture_state":"declared four BGRA8 MRTs (MRT0 RGB-only), D32FloatS8 reverse-Z GreaterEqual, no culling/blending, stencil retained clear0x17; not asserted original full draw state", "wide_baseline_interpretation":"duplicate standalone rendering only; not independent original-game target/material parity","reference":"independent narrow P*inverse(gameFromEye)*object plus transformed measured original Float32 clip-CB residual; raw direct reference also reported", "captured_z_w_and_camera_preserved":true,"center_reference_cb_exact":true,"original_vs_direct_matrix_max_error":input.source_clip.iter().zip(mul(mul(input.projection,input.view),input.object)).map(|(a,b)|(a-b).abs()).fold(0f64,f64::max),"limits":["one draw and synthetic material only","no completed lighting/transparency/sky/post chain","no LOD or culling coverage certification","no app/source publication or hardware acceptance","raw G-buffer previews are not display-ready game imagery"]});
    if args.len() == 4 { fs::write(out.join("cpu-admission.json"), serde_json::to_vec_pretty(&report)?)?; println!("CPU admission passed; GPU not initialized."); return Ok(()); }
    let gpu = GPU::new(&input)?; let baseline = gpu.render(&input,"wide-baseline",input.source_clip,WIDTH,HEIGHT)?;
    let replay = gpu.render(&input,"wide-replay",input.source_clip,WIDTH,HEIGHT)?;
    let physical = gpu.render(&input,"physical-center-empty-control",clip_crop(input.source_clip,[0.,0.]),WIDTH,HEIGHT)?;
    let center = gpu.render(&input,"nested-center",center_clip,WIDTH,HEIGHT)?;
    let expected = gpu.render(&input,"independent-center-reference",reference,WIDTH,HEIGHT)?;
    let uncorrected = gpu.render(&input,"direct-reference-without-captured-rounding",direct,WIDTH,HEIGHT)?;
    let high_resolution = gpu.render(&input,"original-CB-high-density-reference",input.source_clip,WIDTH*3,HEIGHT*3)?;
    let raster_reference = high_resolution.iter().enumerate().map(|(i,data)| {
        let bpp=if i==5 {1} else {4}; let mut crop=Vec::new();
        for y in input.crop_origin[1]..input.crop_origin[1]+HEIGHT {
            let first=((y*(WIDTH*3)+input.crop_origin[0]) as usize)*bpp;
            crop.extend_from_slice(&data[first..first+WIDTH as usize*bpp]);
        }
        crop
    }).collect::<Vec<_>>();
    report["high_resolution_reference"] = json!({"size":[WIDTH*3,HEIGHT*3],"crop_origin":input.crop_origin,"original_captured_CB_verbatim":true,"comparison":comparison(&center,&raster_reference),"covered_samples":covered(&raster_reference)});
    report["gpu_executed"] = json!(true); report["adapter"] = json!(gpu.adapter);
    report["wide_duplicate_determinism"] = comparison(&baseline,&replay); report["center_parity"] = comparison(&center,&expected);
    report["direct_unadjusted_reference"] = comparison(&center,&uncorrected);
    report["covered_samples"] = json!({"wide":covered(&baseline),"physical_center_control":covered(&physical),"nested_center":covered(&center),"independent_center_reference":covered(&expected)});
    report["renders"] = json!(7);
    for (name, data) in [("wide",&baseline),("center",&center),("reference",&expected),("physical-center",&physical),("raster-reference",&raster_reference)] {
        report["attachment_hashes"][name] = json!(data.iter().map(|bytes|sha(bytes)).collect::<Vec<_>>());
        write_ppm(&out.join(format!("{name}-gbuffer0.ppm")),&data[0])?;
    }
    let raster_comparison = comparison(&center,&raster_reference);
    let exact_color_stencil = (0..4).all(|i|center[i]==raster_reference[i]) && center[5]==raster_reference[5];
    let raster_accepted = exact_color_stencil && raster_comparison["coverage_different_pixels"]==0
        && raster_comparison["max_depth_ULPs"].as_u64().is_some_and(|v|v<=2);
    report["high_resolution_reference"]["acceptance"] = json!({"all_four_MRT_and_stencil_bytes_exact":exact_color_stencil,"coverage_must_be_exact":true,"depth_tolerance_ULPs":2,"reason":"original Z/W and camera preserved; equivalent raster positions can round interpolation differently after clip XY transform","passed":raster_accepted});
    let passed = baseline == replay && center == expected && raster_accepted && covered(&baseline) > 0 && covered(&center) >= 32 && covered(&center) > covered(&baseline) * 4 && physical[4].iter().all(|x| *x == 0);
    report["passed"] = json!(passed);
    fs::write(out.join("result.json"),serde_json::to_vec_pretty(&report)?)?;
    require(passed,"bounded GPU proof failed; inspect result.json")?;
    println!("PASS: 7 Metal renders; duplicate wide and secondary center exact; unchanged-shader raster oracle exact four MRTs/stencil/coverage, depth within 2 ULPs; {}→{} covered samples in declared 3× nested ROI. No game/app/device actions.",covered(&baseline),covered(&center));
    Ok(())
}

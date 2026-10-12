//! Actual-GPU fixtures for the opt-in Metal skip of empty per-pass "Pre Pass"
//! command buffers in the vendored wgpu-core (`MUGUET_METAL_SKIP_EMPTY_PREPASS`).
//!
//! The switch is read when a device is created, so run this file in two
//! processes, switch off and switch on. Either way every fixture requires the
//! same stated bytes: pass output, the order of passes and copies between
//! them, discard clears, query results and lazy initialization. The counters
//! are then checked against what the process's switch implies, read through
//! `Instance::generate_report`. They count command buffers, not GPU time.
//! Each fixture owns its instance, so its counters start at zero.
use std::sync::mpsc;
use std::time::Duration;
use wgpu::*;

struct Fixture { instance: Instance, device: Device, queue: Queue }

/// A real Metal device. The default instance flags validate indirect draws in
/// every build profile; `indirect_validation: false` removes only that flag.
fn fixture(indirect_validation: bool) -> Fixture {
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = Backends::METAL;
    if !indirect_validation { desc.flags = desc.flags.difference(InstanceFlags::VALIDATION_INDIRECT_CALL); }
    let instance = Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).expect("real Metal test adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).expect("real Metal test device");
    Fixture { instance, device, queue }
}

/// [render skipped, compute skipped, retained: switch off, discard fixups,
/// query resets, indirect validation; then how many of the render skips were
/// on a device that validates indirect draws].
type Counts = [u64; 7];

/// Checks the instance's cumulative pre-pass decisions: `on` when this
/// process has the switch on, otherwise every one of `passes` kept as before.
#[track_caller]
fn assert_counts(f: &Fixture, on: Counts, passes: u64) {
    let report = f.instance.generate_report().expect("wgpu-core backend").metal_prepass;
    let requested = std::env::var("MUGUET_METAL_SKIP_EMPTY_PREPASS").ok().as_deref() == Some("1");
    assert_eq!(report.enabled, requested, "the real Metal device must exercise the requested pre-pass mode");
    let counts: Counts = [report.render_skipped, report.compute_skipped, report.retained_disabled,
        report.retained_discard_fixups, report.retained_query_resets,
        report.retained_indirect_validation, report.render_skipped_empty_indirect_batch];
    let wanted = if report.enabled { on } else { [0, 0, passes, 0, 0, 0, 0] };
    assert_eq!(counts, wanted, "pre-pass decisions with the switch {}", if report.enabled { "on" } else { "off" });
}

fn solid_pipeline(device: &Device, rgba: [f32; 4]) -> RenderPipeline {
    // One triangle that covers the whole target, in one flat colour.
    let source = format!(r#"
        @vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {{
            let positions = array<vec2f, 3>(vec2f(-1., -1.), vec2f(3., -1.), vec2f(-1., 3.));
            return vec4f(positions[index], 0., 1.);
        }}
        @fragment fn fs() -> @location(0) vec4f {{ return vec4f({:?}, {:?}, {:?}, {:?}); }}
    "#, rgba[0], rgba[1], rgba[2], rgba[3]);
    let shader = device.create_shader_module(ShaderModuleDescriptor { label: Some("pre-pass fixture triangle"), source: ShaderSource::Wgsl(source.into()) });
    device.create_render_pipeline(&RenderPipelineDescriptor { label: Some("pre-pass fixture pipeline"), layout: None,
        vertex: VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: PrimitiveState::default(), depth_stencil: None, multisample: MultisampleState::default(),
        fragment: Some(FragmentState { module: &shader, entry_point: Some("fs"), compilation_options: Default::default(),
            targets: &[Some(ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: ColorWrites::ALL })] }),
        multiview_mask: None, cache: None })
}

fn compute_pipeline(device: &Device, source: &str) -> ComputePipeline {
    let module = device.create_shader_module(ShaderModuleDescriptor { label: Some("pre-pass fixture compute"), source: ShaderSource::Wgsl(source.into()) });
    device.create_compute_pipeline(&ComputePipelineDescriptor { label: None, layout: None, module: &module,
        entry_point: Some("main"), compilation_options: Default::default(), cache: None })
}

/// Writes word i = 3 i + 1 into sixteen words.
const FILL: &str = "@group(0) @binding(0) var<storage, read_write> out: array<u32>; @compute @workgroup_size(1) fn main() { for (var i = 0u; i < 16u; i++) { out[i] = i * 3u + 1u; } }";
/// Writes word i of the second buffer = word i of the first + 5.
const ADD: &str = "@group(0) @binding(0) var<storage, read> src: array<u32>; @group(0) @binding(1) var<storage, read_write> dst: array<u32>; @compute @workgroup_size(1) fn main() { for (var i = 0u; i < 16u; i++) { dst[i] = src[i] + 5u; } }";

fn words(word: impl Fn(u32) -> u32) -> Vec<u8> { (0..16u32).flat_map(|i| word(i).to_le_bytes()).collect() }

/// Sixteen words of storage that nothing has written yet.
fn storage(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor { label: Some("pre-pass fixture storage"), size: 64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn bind(device: &Device, pipeline: &ComputePipeline, buffers: &[&Buffer]) -> BindGroup {
    let entries: Vec<BindGroupEntry> = buffers.iter().enumerate()
        .map(|(binding, buffer)| BindGroupEntry { binding: binding as u32, resource: buffer.as_entire_binding() }).collect();
    device.create_bind_group(&BindGroupDescriptor { label: None, layout: &pipeline.get_bind_group_layout(0), entries: &entries })
}

fn compute(encoder: &mut CommandEncoder, pipeline: &ComputePipeline, group: &BindGroup) {
    let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.dispatch_workgroups(1, 1, 1);
}

/// A 4x4 target that nothing has written yet.
fn target(device: &Device) -> Texture {
    device.create_texture(&TextureDescriptor { label: Some("pre-pass fixture target"),
        size: Extent3d { width: 4, height: 4, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
        dimension: TextureDimension::D2, format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT, view_formats: &[] })
}

fn begin<'a>(encoder: &'a mut CommandEncoder, view: &TextureView, load: LoadOp<Color>, store: StoreOp, queries: Option<&QuerySet>) -> RenderPass<'a> {
    encoder.begin_render_pass(&RenderPassDescriptor { label: Some("pre-pass fixture pass"),
        color_attachments: &[Some(RenderPassColorAttachment { view, depth_slice: None, resolve_target: None, ops: Operations { load, store } })],
        depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: queries, multiview_mask: None })
}

/// Loads the target and paints its left half in the pipeline's colour.
fn paint_left_half(encoder: &mut CommandEncoder, view: &TextureView, pipeline: &RenderPipeline) {
    let mut pass = begin(encoder, view, LoadOp::Load, StoreOp::Store, None);
    pass.set_pipeline(pipeline);
    pass.set_scissor_rect(0, 0, 2, 4);
    pass.draw(0..3, 0..1);
}

fn output(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor { label: Some("pre-pass fixture readback"), size: 1024,
        usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false })
}

/// Records a copy of the target, at this point of the encoder, into `output`.
fn copy_target(encoder: &mut CommandEncoder, texture: &Texture, output: &Buffer) {
    encoder.copy_texture_to_buffer(TexelCopyTextureInfo { texture, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
        TexelCopyBufferInfo { buffer: output, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(4) } },
        Extent3d { width: 4, height: 4, depth_or_array_layers: 1 });
}

/// The sixteen RGBA texels of a `copy_target` readback, without row padding.
fn texels(padded: &[u8]) -> Vec<u8> { (0..4).flat_map(|row| padded[row * 256..row * 256 + 16].to_vec()).collect() }

/// Sixteen texels: `left` in columns 0 and 1, `right` in columns 2 and 3.
fn halves(left: [u8; 4], right: [u8; 4]) -> Vec<u8> { (0..16).flat_map(|texel| if texel % 4 < 2 { left } else { right }).collect() }

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const ZERO: [u8; 4] = [0, 0, 0, 0];

/// Submits the encoder and returns what each readback buffer then holds.
fn submit_and_read(f: &Fixture, mut encoder: CommandEncoder, outputs: &[&Buffer]) -> Vec<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    for output in outputs {
        let tx = tx.clone();
        encoder.map_buffer_on_submit(output, MapMode::Read, .., move |result| { let _ = tx.send(result.is_ok()); });
    }
    let submitted = f.queue.submit([encoder.finish()]);
    f.device.poll(PollType::Wait { submission_index: Some(submitted), timeout: Some(Duration::from_secs(10)) }).unwrap();
    for _ in outputs { assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "a readback buffer failed to map"); }
    outputs.iter().map(|output| {
        let bytes = output.slice(..).get_mapped_range().unwrap().to_vec();
        output.unmap();
        bytes
    }).collect()
}

#[test]
fn metal_prepass_render_and_compute_bytes_keep_pass_and_copy_order() {
    let f = fixture(false);
    let scope = f.device.push_error_scope(ErrorFilter::Validation);
    let (red, green) = (solid_pipeline(&f.device, [1., 0., 0., 1.]), solid_pipeline(&f.device, [0., 1., 0., 1.]));
    let (fill, add) = (compute_pipeline(&f.device, FILL), compute_pipeline(&f.device, ADD));
    let texture = target(&f.device);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let (first, second) = (storage(&f.device), storage(&f.device));
    let (fill_group, add_group) = (bind(&f.device, &fill, &[&first]), bind(&f.device, &add, &[&first, &second]));
    let outputs: Vec<Buffer> = (0..4).map(|_| output(&f.device)).collect();

    // One encoder: every pass and every copy between passes is its own HAL
    // command buffer, so their order is what a missing or misplaced pre-pass
    // buffer would disturb.
    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    {
        let mut pass = begin(&mut encoder, &view, LoadOp::Clear(Color::BLACK), StoreOp::Store, None);
        pass.set_pipeline(&red);
        pass.draw(0..3, 0..1);
    }
    copy_target(&mut encoder, &texture, &outputs[0]); // after the first pass, before the second
    paint_left_half(&mut encoder, &view, &green);
    copy_target(&mut encoder, &texture, &outputs[1]);
    compute(&mut encoder, &fill, &fill_group);
    encoder.copy_buffer_to_buffer(&first, 0, &outputs[2], 0, 64);
    compute(&mut encoder, &add, &add_group); // reads what the first compute pass wrote
    encoder.copy_buffer_to_buffer(&second, 0, &outputs[3], 0, 64);
    let bytes = submit_and_read(&f, encoder, &outputs.iter().collect::<Vec<_>>());

    assert_eq!(texels(&bytes[0]), halves(RED, RED), "the first pass alone");
    assert_eq!(texels(&bytes[1]), halves(GREEN, RED), "the second pass loaded the first one's output");
    assert_eq!(&bytes[2][..64], &words(|i| i * 3 + 1)[..], "the first compute pass");
    assert_eq!(&bytes[3][..64], &words(|i| i * 3 + 6)[..], "the second compute pass read the first one's output");
    assert!(pollster::block_on(scope.pop()).is_none());
    // No discard, no query, and a device that does not validate indirect
    // draws at all: all four pre-pass buffers are empty.
    assert_counts(&f, [2, 2, 0, 0, 0, 0, 0], 4);
}

#[test]
fn metal_prepass_discarded_surface_is_cleared_before_the_pass_that_loads_it() {
    let f = fixture(false);
    let scope = f.device.push_error_scope(ErrorFilter::Validation);
    let green = solid_pipeline(&f.device, [0., 1., 0., 1.]);
    let texture = target(&f.device);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let outputs = [output(&f.device), output(&f.device)];

    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    // Red is written and thrown away.
    drop(begin(&mut encoder, &view, LoadOp::Clear(Color { r: 1., g: 0., b: 0., a: 1. }), StoreOp::Discard, None));
    // Loading a surface discarded earlier in the same encoder: it has to be
    // cleared first, and that clear lives in this pass's pre-pass buffer.
    drop(begin(&mut encoder, &view, LoadOp::Load, StoreOp::Store, None));
    copy_target(&mut encoder, &texture, &outputs[0]);
    // Nothing is discarded any more.
    paint_left_half(&mut encoder, &view, &green);
    copy_target(&mut encoder, &texture, &outputs[1]);
    let bytes = submit_and_read(&f, encoder, &[&outputs[0], &outputs[1]]);

    assert_eq!(texels(&bytes[0]), halves(ZERO, ZERO), "a discarded surface must read as zero, not as what was discarded");
    assert_eq!(texels(&bytes[1]), halves(GREEN, ZERO));
    assert!(pollster::block_on(scope.pop()).is_none());
    // The first and third pass have nothing to clear; the second keeps its pre-pass.
    assert_counts(&f, [2, 0, 0, 1, 0, 0, 0], 3);
}

#[test]
fn metal_prepass_query_pass_keeps_its_prepass_and_a_deferred_resolve_stays_in_place() {
    let f = fixture(false);
    let scope = f.device.push_error_scope(ErrorFilter::Validation);
    let (red, green) = (solid_pipeline(&f.device, [1., 0., 0., 1.]), solid_pipeline(&f.device, [0., 1., 0., 1.]));
    let texture = target(&f.device);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let queries = f.device.create_query_set(&QuerySetDescriptor { label: Some("pre-pass fixture queries"), ty: QueryType::Occlusion, count: 2 });
    let results = f.device.create_buffer(&BufferDescriptor { label: Some("pre-pass fixture query results"), size: 16,
        usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false });
    let filler = f.device.create_buffer(&BufferDescriptor { label: Some("pre-pass fixture filler"), size: 16,
        usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST, mapped_at_creation: false });
    f.queue.write_buffer(&filler, 0, &[0xee; 16]);
    let outputs = [output(&f.device), output(&f.device)];

    // A pass that begins a query has that query reset in its pre-pass.
    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    {
        let mut pass = begin(&mut encoder, &view, LoadOp::Clear(Color::BLACK), StoreOp::Store, Some(&queries));
        pass.set_pipeline(&red);
        pass.begin_occlusion_query(0);
        pass.draw(0..3, 0..1);
        pass.end_occlusion_query();
    }
    let _ = f.queue.submit([encoder.finish()]);
    assert_counts(&f, [0, 0, 0, 0, 1, 0, 0], 1);

    // Query 1 was never written, so this resolve is deferred to submit and
    // inserted at the list index recorded here: after the filler copy, before
    // the pass and the copy that follow. The pass after it has an empty
    // pre-pass, which must not move that index.
    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    encoder.copy_buffer_to_buffer(&filler, 0, &results, 0, 16);
    encoder.resolve_query_set(&queries, 0..2, &results, 0);
    paint_left_half(&mut encoder, &view, &green);
    encoder.copy_buffer_to_buffer(&results, 0, &outputs[0], 0, 16);
    copy_target(&mut encoder, &texture, &outputs[1]);
    let bytes = submit_and_read(&f, encoder, &[&outputs[0], &outputs[1]]);

    assert_ne!(&bytes[0][..8], &[0xee; 8], "the resolve ran after the filler copy and before the readback copy");
    assert!(u64::from_le_bytes(bytes[0][..8].try_into().unwrap()) > 0, "the drawn query keeps its result: {:02x?}", &bytes[0][..16]);
    assert_eq!(&bytes[0][8..16], &[0; 8], "the unused query is cleared by the resolve, after the filler copy");
    assert_eq!(texels(&bytes[1]), halves(GREEN, RED));
    assert!(pollster::block_on(scope.pop()).is_none());
    assert_counts(&f, [1, 0, 0, 0, 1, 0, 0], 2);
}

#[test]
fn metal_prepass_with_indirect_validation_is_kept_only_for_a_real_indirect_draw() {
    // Default flags, as the game's device has them: indirect draws are validated.
    let f = fixture(true);
    let scope = f.device.push_error_scope(ErrorFilter::Validation);
    let (red, green) = (solid_pipeline(&f.device, [1., 0., 0., 1.]), solid_pipeline(&f.device, [0., 1., 0., 1.]));
    let fill = compute_pipeline(&f.device, FILL);
    let texture = target(&f.device);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let first = storage(&f.device);
    let fill_group = bind(&f.device, &fill, &[&first]);
    // Three vertices, one instance.
    let arguments = f.device.create_buffer(&BufferDescriptor { label: Some("pre-pass fixture indirect arguments"), size: 16,
        usage: BufferUsages::INDIRECT | BufferUsages::COPY_DST, mapped_at_creation: false });
    f.queue.write_buffer(&arguments, 0, &[3u32, 1, 0, 0].iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<u8>>());
    let outputs: Vec<Buffer> = (0..3).map(|_| output(&f.device)).collect();

    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    // An ordinary draw: the device validates indirect draws, but this pass
    // has none, so its pre-pass would be empty.
    {
        let mut pass = begin(&mut encoder, &view, LoadOp::Clear(Color::BLACK), StoreOp::Store, None);
        pass.set_pipeline(&red);
        pass.draw(0..3, 0..1);
    }
    copy_target(&mut encoder, &texture, &outputs[0]);
    // A real indirect draw, validated by a compute pass in its pre-pass.
    {
        let mut pass = begin(&mut encoder, &view, LoadOp::Load, StoreOp::Store, None);
        pass.set_pipeline(&green);
        pass.set_scissor_rect(0, 0, 2, 4);
        pass.draw_indirect(&arguments, 0);
    }
    copy_target(&mut encoder, &texture, &outputs[1]);
    // A compute pre-pass never holds indirect validation.
    compute(&mut encoder, &fill, &fill_group);
    encoder.copy_buffer_to_buffer(&first, 0, &outputs[2], 0, 64);
    let bytes = submit_and_read(&f, encoder, &outputs.iter().collect::<Vec<_>>());

    assert_eq!(texels(&bytes[0]), halves(RED, RED));
    assert_eq!(texels(&bytes[1]), halves(GREEN, RED), "the validated indirect draw ran");
    assert_eq!(&bytes[2][..64], &words(|i| i * 3 + 1)[..]);
    assert!(pollster::block_on(scope.pop()).is_none());
    // The direct pass goes without a pre-pass; the indirect one keeps it.
    assert_counts(&f, [1, 1, 0, 0, 0, 1, 1], 3);
}

#[test]
fn metal_prepass_skip_leaves_lazy_memory_initialization_to_submit() {
    let f = fixture(false);
    let scope = f.device.push_error_scope(ErrorFilter::Validation);
    let green = solid_pipeline(&f.device, [0., 1., 0., 1.]);
    let add = compute_pipeline(&f.device, ADD);
    let texture = target(&f.device);
    let view = texture.create_view(&TextureViewDescriptor::default());
    let (never_written, second) = (storage(&f.device), storage(&f.device));
    let add_group = bind(&f.device, &add, &[&never_written, &second]);
    let outputs = [output(&f.device), output(&f.device)];

    // Neither the texture nor the first buffer has ever been written. Their
    // zero-initialization is recorded on the command buffer and done at
    // submit, not in a pre-pass, so both passes still go without one.
    let mut encoder = f.device.create_command_encoder(&CommandEncoderDescriptor::default());
    paint_left_half(&mut encoder, &view, &green);
    copy_target(&mut encoder, &texture, &outputs[0]);
    compute(&mut encoder, &add, &add_group);
    encoder.copy_buffer_to_buffer(&second, 0, &outputs[1], 0, 64);
    let bytes = submit_and_read(&f, encoder, &[&outputs[0], &outputs[1]]);

    assert_eq!(texels(&bytes[0]), halves(GREEN, ZERO), "the half the pass did not paint was zero-initialized");
    assert_eq!(&bytes[1][..64], &words(|_| 5)[..], "the compute pass read zero-initialized words");
    assert!(pollster::block_on(scope.pop()).is_none());
    assert_counts(&f, [1, 1, 0, 0, 0, 0, 0], 2);
}

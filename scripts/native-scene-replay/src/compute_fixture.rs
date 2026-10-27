//! Independent authored compute-to-raster oracle; no game or eye attribution.
use crate::{model::*, sha, validate::Input, Result};
use std::{collections::BTreeMap, fs, num::NonZeroU64, path::Path};
use wgpu_types::*;

const WIDTH: u32 = 13;
const HEIGHT: u32 = 7;
const COMPUTE: &str = r#"
struct Params { multiply: f32, add: f32, width: u32, height: u32, };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var output: texture_storage_2d<rgba32float, write>;
@compute @workgroup_size(4, 3, 1)
fn main(@builtin(global_invocation_id) p: vec3u) {
    if (p.x >= params.width || p.y >= params.height) { return; }
    // Prove preservation of preexisting bytes in a partially written output.
    if (p.x == 0u || p.y == 0u) { return; }
    let v = textureLoad(source, vec2i(p.xy), 0);
    textureStore(output, vec2i(p.xy), vec4f(
        v.r * params.multiply + params.add, v.g + f32(p.x),
        v.b + f32(p.y), 255.0));
}"#;
const VERTEX: &str = r#"
@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    return vec4f(p[i], 0.5, 1);
}"#;
const FRAGMENT: &str = r#"
@group(0) @binding(0) var computed: texture_2d<f32>;
@fragment fn main(@builtin(position) p: vec4f) -> @location(0) vec4f {
    return textureLoad(computed, vec2i(p.xy), 0) / 255.0;
}"#;

fn shader(text: &str) -> Shader {
    Shader { wgsl: text.into(), sha256: sha(text.as_bytes()), entry: "main".into(), exact_final_variant: true }
}
fn slice(id: &str, mip: u32) -> Slice {
    Slice { resource: id.into(), mip, layer: if id == "compute-storage" { 1 } else { 0 }, aspect: Aspect::Color }
}
fn version(id: &str, mip: u32, content_version: u64) -> Version {
    Version { slice: slice(id, mip), content_version }
}
fn transition(id: &str, mip: u32, before: u64, after: u64) -> Transition {
    Transition { slice: slice(id, mip), before, after }
}
fn blob(scene: &mut Scene, files: &mut BTreeMap<String, Vec<u8>>, name: &str, bytes: Vec<u8>) -> String {
    scene.blobs.push(Blob { path: name.into(), bytes: bytes.len() as u64, sha256: sha(&bytes) });
    files.insert(name.into(), bytes);
    name.into()
}
fn float_image(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u32; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            for c in pixel(x, y) { bytes.extend((c as f32).to_le_bytes()); }
        }
    }
    bytes
}
fn source_pixel(x: u32, y: u32) -> [u32; 4] { [x + 2 * y, 10 + 3 * x + y, 20 + x + 4 * y, 1] }
fn initial_storage(x: u32, y: u32) -> [u32; 4] { [17 + x, 29 + y, 43 + x + y, 211] }
fn untouched_mip(x: u32, y: u32) -> [u32; 4] { [121 + x % 7, 131 + y % 5, 151, 227] }
fn expected_pixel(x: u32, y: u32) -> [u32; 4] {
    if x == 0 || y == 0 { initial_storage(x, y) }
    else {
        // Closed-form integer oracle, independent of WGSL evaluation/readback.
        [2 * x + 4 * y + 3, 10 + 4 * x + y, 20 + x + 5 * y, 255]
    }
}
fn expected_color() -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH { bytes.extend(expected_pixel(x, y).map(|c| u8::try_from(c).unwrap())); }
    }
    bytes
}
fn texture_view(mip: u32) -> TextureViewDescriptor<Option<String>> {
    TextureViewDescriptor { dimension: Some(TextureViewDimension::D2), format: Some(TextureFormat::Rgba32Float),
        base_mip_level: mip, mip_level_count: Some(1), base_array_layer: 0, array_layer_count: Some(1),
        ..Default::default() }
}
fn texture_binding(binding: u32, id: &str, mip: u32, generation: u64) -> Binding {
    Binding { group: 0, binding, value: BindingValue::Texture { texture: id.into(), view: TextureViewDescriptor { base_array_layer: if id == "compute-storage" { 1 } else { 0 }, ..texture_view(mip) },
        versions: vec![version(id, mip, generation)] } }
}
fn sampled_layout(binding: u32, visibility: ShaderStages) -> BindGroupLayoutEntry {
    BindGroupLayoutEntry { binding, visibility, ty: BindingType::Texture {
        sample_type: TextureSampleType::Float { filterable: false },
        view_dimension: TextureViewDimension::D2, multisampled: false }, count: None }
}

pub fn make(out: &Path) -> Result<()> {
    let mut scene = Scene { schema: 1, provenance: Provenance::Authored { description:
        "13x7 independent CPU compute oracle: sampled RGBA32F plus offset uniform writes selected mip1; partial border, mip0 and separate array layer retained; following raster consumes the same output allocation. No native game/eye/density proof.".into() },
        eye: None, blobs: vec![], textures: vec![], buffers: vec![], pipelines: vec![], compute_pipelines: vec![],
        commands: vec![], expected: vec![], original_submission_completed: true, original_gpu_errors: Some(0), omitted_commands: 0 };
    let mut files = BTreeMap::new();
    let source = blob(&mut scene, &mut files, "compute-source.bin", float_image(WIDTH, HEIGHT, source_pixel));
    let base = blob(&mut scene, &mut files, "storage-mip0-before.bin", float_image(2 * WIDTH, 2 * HEIGHT, untouched_mip));
    let selected = blob(&mut scene, &mut files, "storage-mip1-before.bin", float_image(WIDTH, HEIGHT, initial_storage));
    let color = blob(&mut scene, &mut files, "compute-color-before.bin", [7, 11, 13, 17].repeat((WIDTH * HEIGHT) as usize));
    for (id, width, height, mips, format, mutable_target, initial_blobs) in [
        ("compute-source", WIDTH, HEIGHT, 1, TextureFormat::Rgba32Float, false, vec![source]),
        ("compute-storage", 2 * WIDTH, 2 * HEIGHT, 2, TextureFormat::Rgba32Float, true, vec![base, selected]),
        ("compute-color", WIDTH, HEIGHT, 1, TextureFormat::Rgba8Unorm, true, vec![color]),
    ] {
        scene.textures.push(Texture { id: id.into(), native_allocation_id: None,
            descriptor: TextureDesc { dimension: TextureDimension::D2, width, height, layers: if id == "compute-storage" { 2 } else { 1 }, mips, format, view_formats: vec![] },
            mutable_target, initial: (0..if id == "compute-storage" { 2 } else { 1 }).flat_map(|layer| {
                initial_blobs.iter().enumerate().map(move |(mip, blob)| SliceInitial {
                    mip: mip as u32, layer, aspect: Aspect::Color, content_version: 1, origin: Initial::Captured { blob: blob.clone() } })
            }).collect() });
    }
    // A real nonzero aligned binding offset proves the selected immutable range.
    let mut uniforms = vec![0xff; 256];
    uniforms.extend(2.0f32.to_le_bytes()); uniforms.extend(3.0f32.to_le_bytes());
    uniforms.extend(WIDTH.to_le_bytes()); uniforms.extend(HEIGHT.to_le_bytes());
    let uniform_blob = blob(&mut scene, &mut files, "compute-uniform.bin", uniforms);
    scene.buffers.push(Buffer { id: "compute-uniform".into(), native_allocation_id: None, content_version: 1,
        source_offset: 0, blob: uniform_blob });
    scene.compute_pipelines.push(ComputePipeline { id: "compute-oracle".into(), shader: shader(COMPUTE), layouts: vec![vec![
        sampled_layout(0, ShaderStages::COMPUTE),
        BindGroupLayoutEntry { binding: 1, visibility: ShaderStages::COMPUTE, ty: BindingType::Buffer {
            ty: BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: NonZeroU64::new(16) }, count: None },
        BindGroupLayoutEntry { binding: 2, visibility: ShaderStages::COMPUTE, ty: BindingType::StorageTexture {
            access: StorageTextureAccess::WriteOnly, format: TextureFormat::Rgba32Float, view_dimension: TextureViewDimension::D2 }, count: None },
    ]] });
    scene.commands.push(Command::Dispatch { dispatch: Dispatch { eye: None, pipeline: "compute-oracle".into(),
        bindings: vec![texture_binding(0, "compute-source", 0, 1),
            Binding { group: 0, binding: 1, value: BindingValue::Buffer { buffer: "compute-uniform".into(), offset: 256, size: 16 } },
            texture_binding(2, "compute-storage", 1, 1)],
        workgroups: [WIDTH.div_ceil(4), HEIGHT.div_ceil(3), 1], writes: vec![transition("compute-storage", 1, 1, 2)] } });
    scene.pipelines.push(Pipeline { id: "compute-copy-raster".into(), vertex: shader(VERTEX), fragment: Some(shader(FRAGMENT)),
        layouts: vec![vec![sampled_layout(0, ShaderStages::FRAGMENT)]], vertex_layouts: vec![], primitive: PrimitiveState::default(),
        depth_stencil: None, multisample: Default::default(), targets: vec![Some(ColorTargetState {
            format: TextureFormat::Rgba8Unorm, blend: None, write_mask: ColorWrites::ALL })] });
    scene.commands.push(Command::Draw { draw: Draw { eye: None, pipeline: "compute-copy-raster".into(),
        bindings: vec![texture_binding(0, "compute-storage", 1, 2)], vertex_buffers: vec![], index_buffer: None,
        args: DrawArgs::Direct { first_vertex: 0, vertices: 3, first_instance: 0, instances: 1 },
        colors: vec![Some(Attachment { resource: "compute-color".into(), mip: 0, layer: 0, view_format: TextureFormat::Rgba8Unorm })],
        depth: None, viewport: [0., 0., WIDTH as f32, HEIGHT as f32, 0., 1.], scissor: [0, 0, WIDTH, HEIGHT], blend: [0.; 4], stencil: 0,
        writes: vec![transition("compute-color", 0, 1, 3)] } });
    for (id, mip, generation, name, expected) in [
        ("compute-storage", 0, 1, "storage-mip0-expected.bin", float_image(2 * WIDTH, 2 * HEIGHT, untouched_mip)),
        ("compute-storage", 1, 2, "storage-mip1-expected.bin", float_image(WIDTH, HEIGHT, expected_pixel)),
        ("compute-color", 0, 3, "compute-color-expected.bin", expected_color()),
    ] {
        let name = blob(&mut scene, &mut files, name, expected);
        scene.expected.push(Expected { version: version(id, mip, generation), blob: name });
    }
    // The unselected layer must remain byte-identical across dispatch and raster.
    for (mip, blob) in [(0, "storage-mip0-before.bin"), (1, "storage-mip1-before.bin")] {
        scene.expected.push(Expected { version: Version { slice: Slice { resource: "compute-storage".into(),
            mip, layer: 0, aspect: Aspect::Color }, content_version: 1 }, blob: blob.into() });
    }
    fs::create_dir(out)?;
    for (name, bytes) in files { fs::write(out.join(name), bytes)?; }
    fs::write(out.join("scene.json"), serde_json::to_vec_pretty(&scene)?)?;
    Input::load(&out.join("scene.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compute_oracle_exercises_edges_mip_identity_and_exact_float_arithmetic() {
        assert_eq!([WIDTH.div_ceil(4), HEIGHT.div_ceil(3), 1], [4, 3, 1]);
        assert_eq!(expected_pixel(1, 1), [9, 15, 26, 255]);
        assert_eq!(expected_pixel(12, 5), [47, 63, 57, 255]);
        assert_eq!(expected_pixel(0, 3), [17, 32, 46, 211]);
        assert_eq!(expected_pixel(12, 0), [29, 29, 55, 211]);
        assert_eq!(expected_pixel(12, 6), [51, 64, 62, 255]);
        let before = float_image(WIDTH, HEIGHT, initial_storage);
        let expected = float_image(WIDTH, HEIGHT, expected_pixel);
        assert_eq!(expected.len(), 13 * 7 * 16);
        assert_eq!(before.chunks_exact(16).zip(expected.chunks_exact(16)).filter(|(a, b)| a != b).count(), 72);
        assert_eq!(expected_color().len(), 13 * 7 * 4);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                for c in expected_pixel(x, y) {
                    assert!(c <= 255);
                    assert_eq!((c as f32) as u32, c);
                }
            }
        }
        assert_ne!(float_image(2 * WIDTH, 2 * HEIGHT, untouched_mip), expected);
    }
}

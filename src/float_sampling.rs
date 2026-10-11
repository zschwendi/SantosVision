//! Filtering for 32-bit float 2D/3D textures without FLOAT32_FILTERABLE.
//! Texture storage remains unchanged. Only selected texture/sampler pairs become
//! textureLoad operations; all other shader operations retain their original code.
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

#[derive(Clone, Debug)]
pub struct SamplerState {
    pub group: u32,
    pub binding: u32,
    pub words: Vec<u32>,
}

pub struct Rewrite {
    pub text: String,
    pub removed_samplers: Vec<(u32, u32)>,
    pub rewritten_calls: usize,
}

static DECL: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"@group\((\d+)u?\)\s*@binding\((\d+)u?\)\s*var\s+(\w+)\s*:\s*(texture_[23]d<f32>|sampler)\s*;"
).unwrap());
static CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"\b(textureSampleLevel|textureSampleBias|textureSampleGrad|textureSampleBaseClampToEdge|textureSample|textureGather)\s*\("
).unwrap());

fn arguments(text: &str, open: usize) -> Result<(usize, Vec<&str>)> {
    let mut depth = 0i32;
    let mut start = open + 1;
    let mut args = Vec::new();
    for (index, byte) in text.bytes().enumerate().skip(start) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' if depth == 0 => {
                args.push(text[start..index].trim());
                return Ok((index + 1, args));
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => { args.push(text[start..index].trim()); start = index + 1; }
            _ => {},
        }
        anyhow::ensure!(depth >= 0, "unbalanced texture call");
    }
    bail!("unterminated texture call")
}

/// State words follow the existing GPU sampler decoder, including its address
/// mapping (D3D border/mirror-once already map to clamp/mirror in that decoder).
fn helpers(prefix: &str, texture: &str, words: &[u32], dimensions: u32) -> String {
    let word = |i: usize| words.get(i).copied().unwrap_or(0);
    let filter = word(0);
    let anisotropic = filter & 0x7f == 0x55;
    let mag = anisotropic || filter & 4 != 0;
    let min = anisotropic || filter & 16 != 0;
    let mip = anisotropic || filter & 1 != 0;
    let lod_min = f32::from_bits(word(11)).max(0.0);
    let lod_max = f32::from_bits(word(12)).clamp(0.0, 32.0).max(lod_min);
    let address = |name: &str, mode: u32| -> String {
        let body = match mode {
            2 | 5 => "let period = 2 * n; let j = ((i % period) + period) % period; return select(j, period - 1 - j, j >= n);",
            3 | 4 => "return clamp(i, 0, n - 1);",
            _ => "return ((i % n) + n) % n;",
        };
        format!("fn {prefix}_{name}(i: i32, n: i32) -> i32 {{ {body} }}\n")
    };
    let int_vector = format!("vec{dimensions}<i32>");
    let float_vector = format!("vec{dimensions}<f32>");
    let mut out = address("u", word(1)) + &address("v", word(2));
    let address_coordinate = if dimensions == 3 {
        out += &address("w", word(3));
        format!("{int_vector}({prefix}_u(p.x, size.x), {prefix}_v(p.y, size.y), {prefix}_w(p.z, size.z))")
    } else { format!("{int_vector}({prefix}_u(p.x, size.x), {prefix}_v(p.y, size.y))") };
    let interpolation = if dimensions == 3 {
        format!(r#"
    let near_top = mix({prefix}_load(base, level), {prefix}_load(base + vec3<i32>(1, 0, 0), level), weight.x);
    let near_bottom = mix({prefix}_load(base + vec3<i32>(0, 1, 0), level), {prefix}_load(base + vec3<i32>(1, 1, 0), level), weight.x);
    let far_top = mix({prefix}_load(base + vec3<i32>(0, 0, 1), level), {prefix}_load(base + vec3<i32>(1, 0, 1), level), weight.x);
    let far_bottom = mix({prefix}_load(base + vec3<i32>(0, 1, 1), level), {prefix}_load(base + vec3<i32>(1, 1, 1), level), weight.x);
    return mix(mix(near_top, near_bottom, weight.y), mix(far_top, far_bottom, weight.y), weight.z);
"#)
    } else {
        format!(r#"
    let top = mix({prefix}_load(base, level), {prefix}_load(base + vec2<i32>(1, 0), level), weight.x);
    let bottom = mix({prefix}_load(base + vec2<i32>(0, 1), level), {prefix}_load(base + vec2<i32>(1, 1), level), weight.x);
    return mix(top, bottom, weight.y);
"#)
    };
    out += &format!(r#"
fn {prefix}_load(p: {int_vector}, level: i32) -> vec4<f32> {{
    let size = {int_vector}(textureDimensions({texture}, level));
    return textureLoad({texture}, {address_coordinate}, level);
}}
fn {prefix}_level(uv: {float_vector}, level: i32, linear: bool, offset: {int_vector}) -> vec4<f32> {{
    let size = {float_vector}(textureDimensions({texture}, level));
    if (!linear) {{ return {prefix}_load({int_vector}(floor(uv * size)) + offset, level); }}
    let location = uv * size - {float_vector}(0.5);
    let base = {int_vector}(floor(location)) + offset;
    let weight = fract(location);
    {interpolation}
}}
fn {prefix}_sample(uv: {float_vector}, requested: f32, offset: {int_vector}) -> vec4<f32> {{
    let last = f32(textureNumLevels({texture}) - 1u);
    let lod = clamp(clamp(requested, {lod_min:.9}, {lod_max:.9}), 0.0, last);
    let linear = select({mag}, {min}, lod > 0.0);
    if ({mip}) {{
        let low = i32(floor(lod));
        return mix({prefix}_level(uv, low, linear, offset), {prefix}_level(uv, min(low + 1, i32(last)), linear, offset), fract(lod));
    }}
    return {prefix}_level(uv, i32(floor(lod + 0.5)), linear, offset);
}}
fn {prefix}_lod(dx: {float_vector}, dy: {float_vector}) -> f32 {{
    let size = {float_vector}(textureDimensions({texture}, 0));
    let x = dx * size;
    let y = dy * size;
    return 0.5 * log2(max(max(dot(x, x), dot(y, y)), 1.0e-20));
}}
"#);
    if dimensions == 2 { out += &format!(r#"
fn {prefix}_gather(uv: vec2<f32>, component: u32, offset: vec2<i32>) -> vec4<f32> {{
    let size = vec2<f32>(textureDimensions({texture}, 0));
    let base = vec2<i32>(floor(uv * size - vec2<f32>(0.5))) + offset;
    return vec4<f32>({prefix}_load(base + vec2<i32>(0, 1), 0)[component],
        {prefix}_load(base + vec2<i32>(1, 1), 0)[component],
        {prefix}_load(base + vec2<i32>(1, 0), 0)[component], {prefix}_load(base, 0)[component]);
}}
"#); }
    out
}

pub fn rewrite(text: &str, selected: &[(u32, u32)], states: &[SamplerState]) -> Result<Rewrite> {
    let mut textures = HashMap::new();
    let mut samplers = HashMap::new();
    for declaration in DECL.captures_iter(text) {
        let pair = (declaration[1].parse::<u32>()?, declaration[2].parse::<u32>()?);
        if &declaration[4] == "sampler" {
            samplers.insert(declaration[3].to_string(), pair);
        } else if selected.contains(&pair) {
            let dimensions = if &declaration[4] == "texture_3d<f32>" { 3 } else { 2 };
            textures.insert(declaration[3].to_string(), (pair, dimensions));
        }
    }
    anyhow::ensure!(textures.len() == selected.len(), "selected float32 binding is not texture_2d<f32> or texture_3d<f32>");
    let mut edits = Vec::new();
    let mut pairs = BTreeMap::new();
    for call in CALL.captures_iter(text) {
        let whole = call.get(0).unwrap();
        let name = &call[1];
        let (end, args) = arguments(text, whole.end() - 1)?;
        let tex_arg = if name == "textureGather" { 1 } else { 0 };
        let Some(&((group, binding), dimensions)) = args.get(tex_arg).and_then(|name| textures.get(*name)) else { continue };
        anyhow::ensure!(dimensions == 2 || !matches!(name, "textureGather" | "textureSampleBaseClampToEdge"), "{name} requires a 2D texture");
        let sampler_name = *args.get(tex_arg + 1).context("missing float32 sampler")?;
        let sampler_pair = *samplers.get(sampler_name).context("float32 sampler must be a declared global")?;
        let state = states.iter().find(|state| (state.group, state.binding) == sampler_pair)
            .context("missing float32 sampler state")?;
        anyhow::ensure!(state.words.first().copied().unwrap_or(0) & 0x80 == 0, "comparison sampler bound to ordinary float32 sample");
        let prefix = format!("sv_f32_g{group}_t{binding}_g{}_s{}", sampler_pair.0, sampler_pair.1);
        pairs.entry(prefix.clone()).or_insert_with(|| helpers(&prefix, args[tex_arg], &state.words, dimensions));
        let uv = *args.get(tex_arg + 2).context("missing float32 sample coordinate")?;
        let (expected, lod) = match name {
            "textureSample" => (3, format!("{prefix}_lod(dpdx({uv}), dpdy({uv}))")),
            "textureSampleBias" => (4, format!("{prefix}_lod(dpdx({uv}), dpdy({uv})) + ({})", args.get(3).context("missing sample bias")?)),
            "textureSampleLevel" => (4, format!("f32({})", args.get(3).context("missing sample LOD")?)),
            "textureSampleGrad" => (5, format!("{prefix}_lod({}, {})", args.get(3).context("missing dx")?, args.get(4).context("missing dy")?)),
            "textureSampleBaseClampToEdge" => (3, "0.0".into()),
            "textureGather" => (4, String::new()),
            _ => unreachable!(),
        };
        anyhow::ensure!(args.len() == expected || args.len() == expected + 1, "unsupported {name} overload for selected float32 texture");
        let zero_offset = format!("vec{dimensions}<i32>(0)");
        let offset = args.get(expected).copied().unwrap_or(&zero_offset);
        let replacement = if name == "textureGather" {
            format!("{prefix}_gather({uv}, u32({}), {offset})", args[0])
        } else {
            let uv = if name == "textureSampleBaseClampToEdge" {
                format!("clamp({uv}, vec2<f32>(0.5) / vec2<f32>(textureDimensions({}, 0)), vec2<f32>(1.0) - vec2<f32>(0.5) / vec2<f32>(textureDimensions({}, 0)))", args[0], args[0])
            } else { uv.to_string() };
            format!("{prefix}_sample({uv}, {lod}, {offset})")
        };
        edits.push((whole.start(), end, replacement));
    }
    for pair in edits.windows(2) { anyhow::ensure!(pair[0].1 <= pair[1].0, "nested float32 sampling expression is unsupported"); }
    let count = edits.len();
    let mut result = text.to_string();
    for (start, end, replacement) in edits.into_iter().rev() { result.replace_range(start..end, &replacement); }
    for helper in pairs.values() { result.push_str(helper); }
    let mut removed = Vec::new();
    let mut declarations = Vec::new();
    for declaration in DECL.captures_iter(&result) {
        if &declaration[4] != "sampler" { continue; }
        let name = &declaration[3];
        let token = Regex::new(&format!(r"\b{}\b", regex::escape(name)))?;
        if token.find_iter(&result).count() == 1 {
            declarations.push(declaration.get(0).unwrap().range());
            removed.push((declaration[1].parse()?, declaration[2].parse()?));
        }
    }
    for range in declarations.into_iter().rev() { result.replace_range(range, ""); }
    // Validate semantic correctness and ensure selected textures have no remaining
    // ImageSample operations, including any syntax we intentionally did not rewrite.
    let module = wgpu::naga::front::wgsl::parse_str(&result).map_err(|error| anyhow::anyhow!("{}", error.emit_to_string(&result)))?;
    let info = wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::all())
        .validate(&module).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let targets: HashSet<_> = module.global_variables.iter().filter_map(|(handle, global)| {
        global.binding.as_ref().filter(|binding| selected.contains(&(binding.group, binding.binding))).map(|_| handle)
    }).collect();
    // Entry-point sampling sets include texture arguments forwarded through
    // helper functions; a direct-expression scan alone would miss that case.
    for entry in 0..module.entry_points.len() {
        anyhow::ensure!(!info.get_entry_point(entry).sampling_set.iter().any(|sample| targets.contains(&sample.image)),
            "selected float32 texture still participates in hardware sampling");
    }
    let functions = module.functions.iter().map(|(_, function)| function).chain(module.entry_points.iter().map(|entry| &entry.function));
    for function in functions {
        for (_, expression) in function.expressions.iter() {
            if let wgpu::naga::Expression::ImageSample { image, .. } = expression {
                if let wgpu::naga::Expression::GlobalVariable(global) = function.expressions[*image] {
                    anyhow::ensure!(!targets.contains(&global), "float32 texture still has a hardware sample after rewrite");
                }
            }
        }
    }
    Ok(Rewrite { text: result, removed_samplers: removed, rewritten_calls: count })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_sampling_overloads_validate_and_leave_other_textures_intact() {
        let source = r#"diagnostic(off, derivative_uniformity);
@group(1) @binding(16) var s: sampler;
@group(1) @binding(32) var t: texture_2d<f32>;
@group(1) @binding(33) var other: texture_2d<f32>;
@fragment fn main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
return textureSample(t,s,uv) + textureSampleLevel(t,s,uv,1.0,vec2<i32>(1,0)) +
textureSampleBias(t,s,uv,0.5) + textureSampleGrad(t,s,uv,dpdx(uv),dpdy(uv)) +
textureGather(0,t,s,uv) + textureSampleBaseClampToEdge(t,s,uv) + textureSample(other,s,uv);
}"#;
        let state = SamplerState { group: 1, binding: 16, words: vec![0x15, 1, 2, 3, 0, 1, 0, 0, 0, 0, 0, 0, 32.0f32.to_bits()] };
        let result = rewrite(source, &[(1,32)], &[state]).unwrap();
        assert_eq!(result.rewritten_calls, 6);
        assert!(result.removed_samplers.is_empty());
        assert!(result.text.contains("textureSample(other,s,uv)"));
        assert!(result.text.contains("textureLoad(t,"));
    }

    #[test]
    fn vertex_explicit_level_removes_unused_sampler_and_keeps_depth_precision() {
        let source = "@group(0) @binding(16) var s: sampler; @group(0) @binding(32) var t: texture_2d<f32>; @vertex fn main() -> @builtin(position) vec4<f32> { return textureSampleLevel(t,s,vec2<f32>(0.5),0.0); }";
        let state = SamplerState { group: 0, binding: 16, words: vec![0,3,3,3] };
        let result = rewrite(source, &[(0,32)], &[state]).unwrap();
        assert_eq!(result.removed_samplers, [(0,16)]);
        assert!(result.text.contains("texture_2d<f32>"));
        assert!(!result.text.contains("dpdx"));
    }

    #[test]
    fn volume_sampling_uses_eight_texels_and_all_three_address_modes() {
        let source = r#"diagnostic(off, derivative_uniformity);
@group(0) @binding(18) var s: sampler;
@group(0) @binding(34) var volume: texture_3d<f32>;
@fragment fn main(@location(0) uv: vec3<f32>) -> @location(0) vec4<f32> {
return textureSample(volume,s,uv) + textureSampleLevel(volume,s,uv,0.5,vec3<i32>(1,0,-1)) +
textureSampleBias(volume,s,uv,0.5) + textureSampleGrad(volume,s,uv,dpdx(uv),dpdy(uv));
}"#;
        let state = SamplerState { group: 0, binding: 18, words: vec![0x15,1,2,3,0,1,0,0,0,0,0,0,32.0f32.to_bits()] };
        let result = rewrite(source, &[(0,34)], &[state]).unwrap();
        assert_eq!(result.rewritten_calls, 4);
        assert_eq!(result.removed_samplers, [(0,18)]);
        assert!(result.text.contains("texture_3d<f32>"));
        assert!(result.text.contains("_w(p.z, size.z)"));
        assert!(result.text.contains("vec3<i32>(1, 1, 1)"));
        assert!(result.text.contains("weight.z"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_volume_filtering_preserves_float32_and_numeric_addressing() {
        use wgpu::*;
        let mut descriptor = InstanceDescriptor::new_without_display_handle();
        descriptor.backends = Backends::METAL;
        let instance = Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default())).unwrap();
        assert!(!device.features().contains(Features::FLOAT32_FILTERABLE));
        let source = r#"@group(0) @binding(18) var s: sampler;
@group(0) @binding(34) var volume: texture_3d<f32>;
@group(0) @binding(160) var<storage,read_write> values: array<vec4<f32>>;
@compute @workgroup_size(1) fn main() {
values[0] = textureSampleLevel(volume,s,vec3<f32>(0.5),0.0);
values[1] = textureSampleLevel(volume,s,vec3<f32>(0.25,0.25,0.75),0.0);
values[2] = textureSampleLevel(volume,s,vec3<f32>(1.25,-0.25,2.0),0.0);
values[3] = textureSampleLevel(volume,s,vec3<f32>(0.625),0.0);
}"#;
        let state = SamplerState { group: 0, binding: 18, words: vec![0x14,1,2,3,0,1,0,0,0,0,0,0,32.0f32.to_bits()] };
        let rewritten = rewrite(source, &[(0,34)], &[state]).unwrap();
        let shader = device.create_shader_module(ShaderModuleDescriptor { label: Some("Numeric 3D float32 filtering"),
            source: ShaderSource::Wgsl(rewritten.text.into()) });
        let entries = [BindGroupLayoutEntry { binding: 34, visibility: ShaderStages::COMPUTE,
            ty: BindingType::Texture { sample_type: TextureSampleType::Float { filterable: false },
                view_dimension: TextureViewDimension::D3, multisampled: false }, count: None },
            BindGroupLayoutEntry { binding: 160, visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer { ty: BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false, min_binding_size: None }, count: None }];
        let group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: None, entries: &entries });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor { label: None,
            bind_group_layouts: &[Some(&group_layout)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&ComputePipelineDescriptor { label: None,
            layout: Some(&layout), module: &shader, entry_point: Some("main"),
            compilation_options: Default::default(), cache: None });
        let size = Extent3d { width: 2, height: 2, depth_or_array_layers: 2 };
        let texture = device.create_texture(&TextureDescriptor { label: None, size, mip_level_count: 1,
            sample_count: 1, dimension: TextureDimension::D3, format: TextureFormat::Rgba32Float,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let texels: Vec<u8> = (0..8).flat_map(|corner| (corner as f32).to_le_bytes().repeat(4)).collect();
        queue.write_texture(TexelCopyTextureInfo { texture: &texture, mip_level: 0,
            origin: Origin3d::ZERO, aspect: TextureAspect::All }, &texels,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(32), rows_per_image: Some(2) }, size);
        let output = device.create_buffer(&BufferDescriptor { label: None, size: 64,
            usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC, mapped_at_creation: false });
        let readback = device.create_buffer(&BufferDescriptor { label: None, size: 64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ, mapped_at_creation: false });
        let view = texture.create_view(&TextureViewDescriptor::default());
        let group = device.create_bind_group(&BindGroupDescriptor { label: None, layout: &group_layout,
            entries: &[BindGroupEntry { binding: 34, resource: BindingResource::TextureView(&view) },
                BindGroupEntry { binding: 160, resource: output.as_entire_binding() }] });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        { let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline); pass.set_bind_group(0,&group,&[]); pass.dispatch_workgroups(1,1,1); }
        encoder.copy_buffer_to_buffer(&output,0,&readback,0,64);
        let submission = queue.submit([encoder.finish()]);
        let (send, receive) = std::sync::mpsc::channel();
        readback.slice(..).map_async(MapMode::Read, move |result| { let _ = send.send(result); });
        device.poll(PollType::Wait { submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        receive.recv_timeout(std::time::Duration::from_secs(1)).unwrap().unwrap();
        let bytes = readback.slice(..).get_mapped_range().unwrap();
        for (sample, expected) in [3.5f32,4.0,4.0,5.25].into_iter().enumerate() {
            for channel in 0..4 {
                let offset = sample * 16 + channel * 4;
                let actual = f32::from_le_bytes(bytes[offset..offset+4].try_into().unwrap());
                assert!((actual-expected).abs() < 1e-5, "sample{sample} channel{channel}: {actual} != {expected}");
            }
        }
        drop(bytes); readback.unmap();
    }
}

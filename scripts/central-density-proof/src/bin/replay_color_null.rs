//! Exact color-null replay. Captures remain private; no full-frame quality claim.
//! No game runtime, camera patch, publication, save or device integration.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    num::NonZeroU64,
    path::{Component, Path, PathBuf},
    sync::mpsc,
};
use wgpu::{util::DeviceExt, *};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const NATIVE_VS: &str = "6153fb6404c740ac049805cd05964a8e1ae36860f3f3f0c3e344e51335b8e538";
const NATIVE_PS: &str = "21bd6330cd32748471b4f917ff7e3a3ea99838978391a2eafd096eb58f093e2d";
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn difference_counts(a: &[u8], b: &[u8], pixel_bytes: usize) -> (usize, usize) {
    assert_eq!(a.len(), b.len());
    assert_eq!(a.len() % pixel_bytes, 0);
    (
        a.iter().zip(b).filter(|(a, b)| a != b).count(),
        a.chunks_exact(pixel_bytes)
            .zip(b.chunks_exact(pixel_bytes))
            .filter(|(a, b)| a != b)
            .count(),
    )
}
fn number(v: &Value, key: &str) -> Result<u64> {
    v[key]
        .as_u64()
        .ok_or_else(|| format!("missing integer {key}").into())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("missing string {key}").into())
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .ok_or_else(|| format!("missing array {key}").into())
}
fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}

struct Input {
    manifest: Value,
    manifest_sha: String,
    blobs: BTreeMap<String, Vec<u8>>,
    width: u32,
    height: u32,
}
impl Input {
    fn load(path: &Path) -> Result<Self> {
        let raw = fs::read(path)?;
        require(raw.len() < 8 * 1024 * 1024, "manifest size")?;
        let m: Value = serde_json::from_slice(&raw)?;
        require(
            m["schema"] == 3 && m["kind"] == "native_color_null_draw",
            "requires separate native_color_null_draw schema3",
        )?;
        require(
            m["status"] == "captured"
                && m["source"]["method"] == "Gpu::do_draw resolved bindings"
                && m["source"]["backend"] == "Metal",
            "actual original Metal capture required",
        )?;
        let d = &m["draw"];
        require(
            d["vertex_hash"] == "4e1d5d361e0ed0bc"
                && d["pixel_hash"] == "7f531ca66548aefe"
                && sha(text(d, "vertex_wgsl")?.as_bytes()) == NATIVE_VS
                && sha(text(d, "pixel_wgsl")?.as_bytes()) == NATIVE_PS,
            "exact shader source pins",
        )?;
        let root = path.parent().ok_or("capture parent")?.canonicalize()?;
        let mut blobs = BTreeMap::new();
        let mut total = 0u64;
        for b in array(&m, "blobs")? {
            let name = text(b, "path")?;
            let rel = Path::new(name);
            require(
                !rel.is_absolute() && rel.components().all(|c| matches!(c, Component::Normal(_))),
                "local blob path",
            )?;
            let full = root.join(rel).canonicalize()?;
            require(full.starts_with(&root), "blob escape")?;
            let length = number(b, "byte_length")?;
            total = total.checked_add(length).ok_or("byte overflow")?;
            require(
                total <= MAX_BYTES && fs::metadata(&full)?.len() == length,
                "blob size/capture limit",
            )?;
            let data = fs::read(full)?;
            require(sha(&data) == text(b, "sha256")?, "blob hash")?;
            require(blobs.insert(name.into(), data).is_none(), "duplicate blob")?;
        }
        let at = array(&m, "attachments")?;
        require(
            at.len() == 1 || at.len() == 3,
            "color0 and optional depth/stencil only",
        )?;
        let width = number(&at[0], "width")? as u32;
        let height = number(&at[0], "height")? as u32;
        require(
            width > 0 && height > 0 && width <= 4096 && height <= 4096,
            "target size",
        )?;
        for (i, a) in at.iter().enumerate() {
            require(
                a["role"] == ["color0", "depth", "stencil"][i]
                    && number(a, "width")? == u64::from(width)
                    && number(a, "height")? == u64::from(height)
                    && a["mip"] == 0
                    && a["slice"] == 0,
                "target subresource",
            )?;
            let f: TextureFormat = decode(&a["format"])?;
            require(
                f == if i == 0 {
                    TextureFormat::Bgra8Unorm
                } else {
                    TextureFormat::Depth32FloatStencil8
                },
                "target format",
            )?;
            for key in ["initial_blob", "final_blob"] {
                let b = blobs.get(text(a, key)?).ok_or("target bytes missing")?;
                require(
                    b.len() == width as usize * height as usize * if i == 2 { 1 } else { 4 },
                    "tight target bytes",
                )?;
                if i == 1 {
                    require(
                        b.chunks_exact(4).all(|x| {
                            let z = f32::from_le_bytes(x.try_into().unwrap());
                            z.is_finite() && (0.0..=1.0).contains(&z)
                        }),
                        "depth range",
                    )?;
                }
            }
        }
        require(
            d["draw"]["kind"] == 0
                && d["draw"]["count"] == 6
                && d["draw"]["instances"] == 1
                && d["draw"]["start_instance"] == 0,
            "pinned nonindexed draw",
        )?;
        require(
            d.get("index_buffer").is_none_or(Value::is_null),
            "unexpected index buffer",
        )?;
        require(
            d["world_view"] == m["world_view"]
                && d["target_domain"] == m["target_domain"]
                && m["target_domain"]["retained_native_parents_compared"] == true
                && number(&m["target_domain"], "depth_flags")? <= 3,
            "original source identity",
        )?;
        let primitive: PrimitiveState = decode(&d["pipeline"]["primitive"])?;
        require(
            primitive.topology == PrimitiveTopology::TriangleList
                && primitive.polygon_mode == PolygonMode::Fill
                && !primitive.conservative,
            "primitive contract",
        )?;
        let multi: MultisampleState = decode(&d["pipeline"]["multisample"])?;
        require(multi.count == 1, "single sample")?;
        let targets: Vec<Option<ColorTargetState>> = decode(&d["pipeline"]["targets"])?;
        require(
            targets.len() == 1
                && targets[0]
                    .as_ref()
                    .is_some_and(|t| t.format == TextureFormat::Bgra8Unorm),
            "one exact color target",
        )?;
        let depth: Option<DepthStencilState> = decode(&d["pipeline"]["depth_stencil"])?;
        require(
            (at.len() == 3) == depth.is_some()
                && depth
                    .as_ref()
                    .is_none_or(|d| d.format == TextureFormat::Depth32FloatStencil8),
            "depth target state",
        )?;
        let mut vertex_slots = std::collections::BTreeSet::new();
        for v in array(d, "vertex_buffers")? {
            let slot = number(v, "slot")?;
            let bytes = blobs.get(text(v, "blob")?).ok_or("vertex bytes")?;
            require(
                slot < 16 && vertex_slots.insert(slot) && number(v, "offset")? < bytes.len() as u64,
                "vertex range",
            )?;
        }
        for (slot, v) in array(&d["pipeline"], "vertex_layouts")?.iter().enumerate() {
            if v.is_null() {
                continue;
            }
            require(vertex_slots.contains(&(slot as u64)), "missing vertex slot")?;
            let vb = array(d, "vertex_buffers")?
                .iter()
                .find(|v| v["slot"] == slot)
                .unwrap();
            let bytes = &blobs[text(vb, "blob")?];
            let stride = number(v, "array_stride")?;
            let attrs: Vec<VertexAttribute> = decode(&v["attributes"])?;
            let last = attrs
                .iter()
                .map(|a| a.offset + a.format.size())
                .max()
                .unwrap_or(0);
            let count = if v["step_mode"] == "instance" {
                1
            } else {
                number(&d["draw"], "start_vertex")?
                    .checked_add(6)
                    .ok_or("vertex overflow")?
            };
            let end = count
                .saturating_sub(1)
                .checked_mul(stride)
                .and_then(|n| n.checked_add(last))
                .and_then(|n| n.checked_add(number(vb, "offset").ok()?))
                .ok_or("vertex range overflow")?;
            require(end <= bytes.len() as u64, "vertex buffer incomplete")?;
        }
        let vp: [f32; 6] = decode(&d["viewport"])?;
        require(
            vp.iter().all(|x| x.is_finite())
                && vp[0] >= 0.
                && vp[1] >= 0.
                && vp[2] > 0.
                && vp[3] > 0.
                && vp[0] + vp[2] <= width as f32
                && vp[1] + vp[3] <= height as f32
                && vp[4] >= 0.
                && vp[5] >= vp[4]
                && vp[5] <= 1.,
            "viewport",
        )?;
        let sc: [u32; 4] = decode(&d["scissor"])?;
        require(
            sc[0].checked_add(sc[2]).is_some_and(|n| n <= width)
                && sc[1].checked_add(sc[3]).is_some_and(|n| n <= height),
            "scissor",
        )?;
        let mut slots = std::collections::BTreeSet::new();
        for b in array(d, "bindings")? {
            let group = number(b, "group")?;
            let n = number(b, "binding")?;
            require(
                group == 1 && [0, 18, 21, 34, 37].contains(&n) && slots.insert(n),
                "pinned binding contract",
            )?;
            let layout: BindGroupLayoutEntry = decode(&b["layout"])?;
            require(
                layout.binding as u64 == n && layout.count.is_none(),
                "layout binding",
            )?;
            match text(b, "kind")? {
                "uniform" => {
                    require(
                        n == 0
                            && matches!(
                                layout.ty,
                                BindingType::Buffer {
                                    ty: BufferBindingType::Uniform,
                                    has_dynamic_offset: false,
                                    ..
                                }
                            ),
                        "static actual uniform",
                    )?;
                    let bytes = blobs.get(text(b, "blob")?).ok_or("uniform blob")?;
                    require(
                        number(b, "size")? == 64
                            && number(b, "offset")?
                                .checked_add(64)
                                .is_some_and(|n| n <= bytes.len() as u64),
                        "packed actual PS constants",
                    )?;
                }
                "sampler" => {
                    require(
                        [18, 21].contains(&n) && matches!(layout.ty, BindingType::Sampler(_)),
                        "sampler",
                    )?;
                    let _: wgpu_types::SamplerDescriptor<Option<String>> =
                        decode(&b["descriptor"])?;
                }
                "texture" => {
                    require(
                        [34, 37].contains(&n)
                            && matches!(
                                layout.ty,
                                BindingType::Texture {
                                    sample_type: TextureSampleType::Float { .. },
                                    view_dimension: TextureViewDimension::D2,
                                    multisampled: false
                                }
                            ),
                        "texture binding",
                    )?;
                    let t = &b["descriptor"];
                    let tw = number(t, "width")?;
                    let th = number(t, "height")?;
                    let levels = number(t, "mip_level_count")?;
                    require(
                        tw > 0
                            && th > 0
                            && tw <= 16384
                            && th <= 16384
                            && levels > 0
                            && levels <= 15
                            && t["depth_or_array_layers"] == 1
                            && t["sample_count"] == 1,
                        "material extent",
                    )?;
                    let f: TextureFormat = decode(&t["format"])?;
                    let (bw, bh) = f.block_dimensions();
                    let bpp = f.block_copy_size(None).ok_or("material format")?;
                    let mut seen = std::collections::BTreeSet::new();
                    for sub in array(b, "subresources")? {
                        let mip = number(sub, "mip_level")?;
                        require(
                            mip < levels && sub["layer"] == 0 && seen.insert(mip),
                            "material mip",
                        )?;
                        let sw = (tw >> mip).max(1);
                        let sh = (th >> mip).max(1);
                        let row = sw.div_ceil(bw as u64) * bpp as u64;
                        require(
                            number(sub, "width")? == sw
                                && number(sub, "height")? == sh
                                && number(sub, "packed_row_bytes")? == row,
                            "material shape",
                        )?;
                        require(
                            blobs
                                .get(text(sub, "blob")?)
                                .is_some_and(|b| b.len() as u64 == row * sh.div_ceil(bh as u64)),
                            "material bytes",
                        )?;
                    }
                    let view: wgpu_types::TextureViewDescriptor<Option<String>> =
                        decode(&b["view"])?;
                    require(
                        (view.base_mip_level as u64) < levels
                            && view.base_array_layer == 0
                            && view.array_layer_count.is_none_or(|n| n == 1),
                        "material view",
                    )?;
                    let count = view
                        .mip_level_count
                        .unwrap_or(levels as u32 - view.base_mip_level);
                    require(
                        count > 0
                            && view
                                .base_mip_level
                                .checked_add(count)
                                .is_some_and(|e| e as u64 <= levels)
                            && (view.base_mip_level..view.base_mip_level + count)
                                .all(|m| seen.contains(&(m as u64))),
                        "visible mips missing",
                    )?;
                    if n == 37 {
                        require(
                            b["native_dummy"] == true,
                            "actual resolved dummy capture required",
                        )?;
                        require(
                            tw == 1
                                && th == 1
                                && levels == 1
                                && f == TextureFormat::Rgba8Unorm
                                && array(b, "subresources")?.len() == 1
                                && blobs[text(&b["subresources"][0], "blob")?] == [0, 0, 0, 0],
                            "actual captured native dummy must implement zero NULL sampling",
                        )?;
                    }
                }
                _ => return Err("unsupported binding".into()),
            }
        }
        require(slots.len() == 5, "complete pixel bindings")?;
        let alias = &m["alias"];
        require(
            alias["group"] == 1
                && alias["binding"] == 37
                && alias["target_role"] == "color0"
                && alias["mip"] == 0
                && alias["layer"] == 0,
            "actual original alias",
        )?;
        require(
            alias["native_parent_matches_retained_null_cause"] == true
                && alias["dummy_parent_differs_from_output"] == true,
            "native parent proof",
        )?;
        let alias_view: wgpu_types::TextureViewDescriptor<Option<String>> = decode(&alias["view"])?;
        require(
            alias_view.format == Some(TextureFormat::Bgra8Unorm)
                && alias_view.dimension == Some(TextureViewDimension::D2)
                && alias_view.base_mip_level == 0
                && alias_view.mip_level_count == Some(1)
                && alias_view.base_array_layer == 0
                && alias_view.array_layer_count == Some(1)
                && alias["retained_cause"]["raw_view"] == json!([87, 4, 0, 1, 0, 0]),
            "retained original alias view",
        )?;
        let key = array(alias, "resolved_group_key")?;
        require(
            key.len() == 9
                && key[2] == 1
                && key[6].as_u64().is_some_and(|n| n > 0)
                && key[7] == 0
                && key[8] == 0,
            "resolved dummy binding key",
        )?;
        for key in [
            "single_original_draw",
            "initial_contents_captured",
            "original_final_captured",
            "natural_submission_completed",
            "queries_inactive",
        ] {
            require(m["closure"][key] == true, "original closure proof")?;
        }
        require(
            m["closure"]["unmodelled_commands"] == 0 && m["closure"]["gpu_errors"] == 0,
            "unqualified submission",
        )?;
        Ok(Self {
            manifest: m,
            manifest_sha: sha(&raw),
            blobs,
            width,
            height,
        })
    }
    fn blob(&self, name: &str) -> Result<&[u8]> {
        self.blobs
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("missing blob {name}").into())
    }
    fn attachments(&self, which: &str) -> Result<Vec<Vec<u8>>> {
        array(&self.manifest, "attachments")?
            .iter()
            .map(|a| Ok(self.blob(text(a, which)?)?.to_vec()))
            .collect()
    }
}
struct Draw {
    pipeline: RenderPipeline,
    groups: Vec<BindGroup>,
    vertices: Vec<(u32, Buffer, u64)>,
    args: Value,
    viewport: [f32; 6],
    scissor: [u32; 4],
    blend: Color,
    stencil: u32,
}
fn upload(
    queue: &Queue,
    texture: &Texture,
    mip: u32,
    layer: u32,
    aspect: TextureAspect,
    width: u32,
    height: u32,
    bytes: &[u8],
    row: u32,
) {
    let (block_width, block_height) = texture.format().block_dimensions();
    queue.write_texture(
        TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect,
        },
        bytes,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(height),
        },
        Extent3d {
            width: width.div_ceil(block_width) * block_width,
            height: height.div_ceil(block_height) * block_height,
            depth_or_array_layers: 1,
        },
    );
}
enum Bound {
    Buffer(Buffer, u64, Option<NonZeroU64>),
    Texture(TextureView),
    Sampler(Sampler),
}
fn draw(
    device: &Device,
    queue: &Queue,
    input: &Input,
    d: &Value,
    alias: Option<&Texture>,
) -> Result<Draw> {
    let mut layouts = Vec::new();
    let mut groups = Vec::new();
    for group in 0..2 {
        let bindings = array(d, "bindings")?
            .iter()
            .filter(|b| b["group"] == group)
            .collect::<Vec<_>>();
        let entries = bindings
            .iter()
            .map(|b| decode::<BindGroupLayoutEntry>(&b["layout"]))
            .collect::<Result<Vec<_>>>()?;
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("captured binding layout"),
            entries: &entries,
        });
        let mut resources = Vec::new();
        for b in &bindings {
            let resource = match text(b, "kind")? {
                "uniform" | "buffer" => {
                    let bytes = input.blob(text(b, "blob")?)?;
                    let offset = number(b, "offset")?;
                    let size = number(b, "size")?;
                    require(
                        offset
                            .checked_add(size)
                            .is_some_and(|n| n <= bytes.len() as u64),
                        "buffer binding range",
                    )?;
                    let ty = if b["kind"] == "uniform" {
                        BufferUsages::UNIFORM
                    } else {
                        BufferUsages::STORAGE
                    };
                    Bound::Buffer(
                        device.create_buffer_init(&util::BufferInitDescriptor {
                            label: Some("captured immutable buffer"),
                            contents: bytes,
                            usage: ty,
                        }),
                        offset,
                        NonZeroU64::new(size),
                    )
                }
                "sampler" => {
                    let desc: wgpu_types::SamplerDescriptor<Option<String>> =
                        decode(&b["descriptor"])?;
                    Bound::Sampler(device.create_sampler(&desc.map_label(|s| s.as_deref())))
                }
                "texture" => {
                    if b["group"] == 1 && b["binding"] == 37 && alias.is_some() {
                        let view: wgpu_types::TextureViewDescriptor<Option<String>> =
                            decode(&input.manifest["alias"]["view"])?;
                        resources.push(Bound::Texture(
                            alias
                                .unwrap()
                                .create_view(&view.map_label(|s| s.as_deref())),
                        ));
                        continue;
                    }
                    let t = &b["descriptor"];
                    let format: TextureFormat = decode(&t["format"])?;
                    let dimension: TextureDimension = decode(&t["dimension"])?;
                    let width = number(t, "width")? as u32;
                    let height = number(t, "height")? as u32;
                    let levels = number(t, "mip_level_count")? as u32;
                    let layers = number(t, "depth_or_array_layers")? as u32;
                    require(
                        dimension == TextureDimension::D2
                            && layers == 1
                            && number(t, "sample_count")? == 1,
                        "first replay accepts single-layer2D material",
                    )?;
                    let view: wgpu_types::TextureViewDescriptor<Option<String>> =
                        decode(&b["view"])?;
                    let view_formats = view
                        .format
                        .filter(|f| *f != format)
                        .into_iter()
                        .collect::<Vec<_>>();
                    let texture = device.create_texture(&TextureDescriptor {
                        label: Some("captured actual material"),
                        size: Extent3d {
                            width,
                            height,
                            depth_or_array_layers: layers,
                        },
                        mip_level_count: levels,
                        sample_count: 1,
                        dimension,
                        format,
                        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                        view_formats: &view_formats,
                    });
                    let mut seen = std::collections::BTreeSet::new();
                    for sub in array(b, "subresources")? {
                        let mip = number(sub, "mip_level")? as u32;
                        let layer = number(sub, "layer")? as u32;
                        require(
                            mip < levels && layer < layers && seen.insert((mip, layer)),
                            "bad/duplicate texture subresource",
                        )?;
                        let sw = (width >> mip).max(1);
                        let sh = (height >> mip).max(1);
                        require(
                            number(sub, "width")? == sw as u64
                                && number(sub, "height")? == sh as u64,
                            "texture mip dimensions",
                        )?;
                        let (bw, bh) = format.block_dimensions();
                        let row = sw.div_ceil(bw)
                            * format
                                .block_copy_size(None)
                                .ok_or("uncopyable material format")?;
                        let data = input.blob(text(sub, "blob")?)?;
                        require(
                            number(sub, "packed_row_bytes")? == row as u64
                                && data.len() == (row * sh.div_ceil(bh)) as usize,
                            "material mip bytes",
                        )?;
                        upload(
                            queue,
                            &texture,
                            mip,
                            layer,
                            TextureAspect::All,
                            sw,
                            sh,
                            data,
                            row,
                        );
                    }
                    let count = view.mip_level_count.unwrap_or(levels - view.base_mip_level);
                    require(
                        (view.base_mip_level..view.base_mip_level + count)
                            .all(|m| seen.contains(&(m, 0))),
                        "visible material mip absent",
                    )?;
                    Bound::Texture(texture.create_view(&view.map_label(|s| s.as_deref())))
                }
                _ => unreachable!(),
            };
            resources.push(resource);
        }
        let entries = bindings
            .iter()
            .zip(&resources)
            .map(|(b, r)| BindGroupEntry {
                binding: b["binding"].as_u64().unwrap() as u32,
                resource: match r {
                    Bound::Buffer(buffer, offset, size) => BindingResource::Buffer(BufferBinding {
                        buffer,
                        offset: *offset,
                        size: *size,
                    }),
                    Bound::Texture(t) => BindingResource::TextureView(t),
                    Bound::Sampler(s) => BindingResource::Sampler(s),
                },
            })
            .collect::<Vec<_>>();
        groups.push(device.create_bind_group(&BindGroupDescriptor {
            label: Some("captured bindings"),
            layout: &layout,
            entries: &entries,
        }));
        layouts.push(layout);
    }
    let vs = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("captured resolved VS"),
        source: ShaderSource::Wgsl(text(d, "vertex_wgsl")?.into()),
    });
    let ps = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("captured resolved PS"),
        source: ShaderSource::Wgsl(text(d, "pixel_wgsl")?.into()),
    });
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &layouts.iter().map(Some).collect::<Vec<_>>(),
        immediate_size: 0,
    });
    let vbs = array(&d["pipeline"], "vertex_layouts")?
        .iter()
        .map(|v| {
            if v.is_null() {
                Ok(None)
            } else {
                Ok(Some((
                    number(v, "array_stride")?,
                    decode::<VertexStepMode>(&v["step_mode"])?,
                    decode::<Vec<VertexAttribute>>(&v["attributes"])?,
                )))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let buffers = vbs
        .iter()
        .map(|v| {
            v.as_ref().map(|(s, step, attrs)| VertexBufferLayout {
                array_stride: *s,
                step_mode: *step,
                attributes: attrs,
            })
        })
        .collect::<Vec<_>>();
    let targets: Vec<Option<ColorTargetState>> = decode(&d["pipeline"]["targets"])?;
    let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("captured exact pipeline"),
        layout: Some(&layout),
        vertex: VertexState {
            module: &vs,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        fragment: Some(FragmentState {
            module: &ps,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        primitive: decode(&d["pipeline"]["primitive"])?,
        depth_stencil: decode(&d["pipeline"]["depth_stencil"])?,
        multisample: decode(&d["pipeline"]["multisample"])?,
        multiview_mask: None,
        cache: None,
    });
    let vertices = array(d, "vertex_buffers")?
        .iter()
        .map(|v| {
            Ok((
                number(v, "slot")? as u32,
                device.create_buffer_init(&util::BufferInitDescriptor {
                    label: Some("captured VB"),
                    contents: input.blob(text(v, "blob")?)?,
                    usage: BufferUsages::VERTEX,
                }),
                number(v, "offset")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let blend: [f64; 4] = decode(&d["blend_constant"])?;
    Ok(Draw {
        pipeline,
        groups,
        vertices,
        args: d["draw"].clone(),
        viewport: decode(&d["viewport"])?,
        scissor: decode(&d["scissor"])?,
        blend: Color {
            r: blend[0],
            g: blend[1],
            b: blend[2],
            a: blend[3],
        },
        stencil: number(d, "stencil_reference")? as u32,
    })
}

fn readback(
    device: &Device,
    queue: &Queue,
    textures: &[Texture],
    width: u32,
    height: u32,
) -> Result<Vec<Vec<u8>>> {
    let mut e = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("exact span aspect readback"),
    });
    let mut buffers = Vec::new();
    for i in 0..if textures.len() == 2 { 3 } else { 1 } {
        let bpp = if i == 2 { 1 } else { 4 };
        let row = width * bpp;
        let pitch = row.div_ceil(256) * 256;
        let b = device.create_buffer(&BufferDescriptor {
            label: None,
            size: pitch as u64 * height as u64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        e.copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: &textures[i.min(1)],
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: if i == 1 {
                    TextureAspect::DepthOnly
                } else if i == 2 {
                    TextureAspect::StencilOnly
                } else {
                    TextureAspect::All
                },
            },
            TexelCopyBufferInfo {
                buffer: &b,
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(height),
                },
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        buffers.push((b, row as usize, pitch as usize));
    }
    let sub = queue.submit([e.finish()]);
    device.poll(PollType::Wait {
        submission_index: Some(sub),
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    let mut result = Vec::new();
    for (b, row, pitch) in buffers {
        let (tx, rx) = mpsc::channel();
        b.slice(..).map_async(MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })?;
        rx.recv()??;
        let m = b.slice(..).get_mapped_range()?;
        let mut tight = Vec::new();
        for y in 0..height as usize {
            tight.extend_from_slice(&m[y * pitch..y * pitch + row]);
        }
        drop(m);
        b.unmap();
        result.push(tight);
    }
    Ok(result)
}

fn restore_depth(
    device: &Device,
    queue: &Queue,
    target: &Texture,
    input: &Input,
    bytes: &[u8],
) -> Result<()> {
    // Buffer-to-depth copies are forbidden by wgpu. This identity fragment path
    // writes each captured Float32 value, then readback must prove exact import.
    let source = device.create_texture(&TextureDescriptor {
        label: Some("captured initial depth values"),
        size: Extent3d {
            width: input.width,
            height: input.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::R32Float,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    upload(
        queue,
        &source,
        0,
        0,
        TextureAspect::All,
        input.width,
        input.height,
        bytes,
        input.width * 4,
    );
    let shader=device.create_shader_module(ShaderModuleDescriptor{label:Some("exact initial depth restore"),source:ShaderSource::Wgsl("@group(0) @binding(0) var src:texture_2d<f32>; @vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);} @fragment fn fs(@builtin(position)p:vec4f)->@builtin(frag_depth) f32{return textureLoad(src,vec2i(p.xy),0).x;}".into())});
    let bgl = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: None,
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Texture {
                sample_type: TextureSampleType::Float { filterable: false },
                view_dimension: TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let p = device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("identity depth import"),
        layout: Some(&layout),
        vertex: VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: Some(DepthStencilState {
            format: TextureFormat::Depth32FloatStencil8,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::Always),
            stencil: StencilState::default(),
            bias: DepthBiasState::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let v = source.create_view(&TextureViewDescriptor::default());
    let bg = device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: BindingResource::TextureView(&v),
        }],
    });
    let target_view = target.create_view(&TextureViewDescriptor::default());
    let mut e = device.create_command_encoder(&CommandEncoderDescriptor { label: None });
    {
        let mut pass = e.begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &target_view,
                depth_ops: Some(Operations {
                    load: LoadOp::Clear(0.),
                    store: StoreOp::Store,
                }),
                stencil_ops: Some(Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&p);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit([e.finish()]);
    Ok(())
}

fn targets(
    device: &Device,
    queue: &Queue,
    input: &Input,
    initial: &[Vec<u8>],
) -> Result<Vec<Texture>> {
    let textures = (0..if initial.len() == 3 { 2 } else { 1 })
        .map(|i| {
            device.create_texture(&TextureDescriptor {
                label: Some("actual initialized color-null target"),
                size: Extent3d {
                    width: input.width,
                    height: input.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: if i == 0 {
                    TextureFormat::Bgra8Unorm
                } else {
                    TextureFormat::Depth32FloatStencil8
                },
                usage: TextureUsages::RENDER_ATTACHMENT
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST
                    | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        })
        .collect::<Vec<_>>();
    upload(
        queue,
        &textures[0],
        0,
        0,
        TextureAspect::All,
        input.width,
        input.height,
        &initial[0],
        input.width * 4,
    );
    if initial.len() == 3 {
        upload(
            queue,
            &textures[1],
            0,
            0,
            TextureAspect::StencilOnly,
            input.width,
            input.height,
            &initial[2],
            input.width,
        );
        restore_depth(device, queue, &textures[1], input, &initial[1])?;
    }
    require(
        readback(device, queue, &textures, input.width, input.height)? == initial,
        "exact initial target import",
    )?;
    Ok(textures)
}
fn encode(
    device: &Device,
    queue: &Queue,
    textures: &[Texture],
    draws: &[&Draw],
    input: &Input,
) -> Result<()> {
    let views = textures
        .iter()
        .map(|t| t.create_view(&TextureViewDescriptor::default()))
        .collect::<Vec<_>>();
    let mut e = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("captured color-null draw"),
    });
    {
        let colors = [Some(RenderPassColorAttachment {
            view: &views[0],
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Load,
                store: StoreOp::Store,
            },
        })];
        // Copy the production attachment read-only contract, not just pipeline state.
        let flags = number(&input.manifest["target_domain"], "depth_flags")?;
        let ds = views.get(1).map(|view| RenderPassDepthStencilAttachment {
            view,
            depth_ops: if flags & 1 == 0 {
                Some(Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                })
            } else {
                None
            },
            stencil_ops: if flags & 2 == 0 {
                Some(Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                })
            } else {
                None
            },
        });
        let mut p = e.begin_render_pass(&RenderPassDescriptor {
            label: Some("original initialized load/store"),
            color_attachments: &colors,
            depth_stencil_attachment: ds,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for d in draws {
            p.set_pipeline(&d.pipeline);
            for (i, g) in d.groups.iter().enumerate() {
                p.set_bind_group(i as u32, g, &[]);
            }
            for (slot, b, offset) in &d.vertices {
                p.set_vertex_buffer(*slot, b.slice(*offset..));
            }
            let v = d.viewport;
            p.set_viewport(v[0], v[1], v[2], v[3], v[4], v[5]);
            p.set_scissor_rect(d.scissor[0], d.scissor[1], d.scissor[2], d.scissor[3]);
            p.set_blend_constant(d.blend);
            p.set_stencil_reference(d.stencil);
            let start = number(&d.args, "start_vertex")? as u32;
            let count = number(&d.args, "count")? as u32;
            p.draw(
                start..start.checked_add(count).ok_or("vertex overflow")?,
                0..1,
            );
        }
    }
    queue.submit([e.finish()]);
    device.poll(PollType::Wait {
        submission_index: None,
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    Ok(())
}
fn channel_changes(a: &[u8], b: &[u8]) -> Value {
    let mut c = [0usize; 4];
    let mut rgb = 0;
    let mut rgba = 0;
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for i in 0..4 {
            c[i] += usize::from(x[i] != y[i]);
        }
        rgb += usize::from(x[..3] != y[..3]);
        rgba += usize::from(x != y);
    }
    json!({"blue":c[0],"green":c[1],"red":c[2],"alpha":c[3],"rgb_changed_pixels":rgb,"rgba_changed_pixels":rgba})
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    require(
        args.len() == 3 || (args.len() == 4 && args[3] == "--cpu-only"),
        "usage: replay_color_null MANIFEST NEW_OUTPUT_DIRECTORY [--cpu-only]",
    )?;
    let input = Input::load(Path::new(&args[1]))?;
    let out = PathBuf::from(&args[2]);
    fs::create_dir(&out)?;
    let initial = input.attachments("initial_blob")?;
    let expected = input.attachments("final_blob")?;
    let mut report = json!({"schema":1,"kind":"actual_color_null_replay","manifest_sha256":input.manifest_sha,
        "scope":"One original actual PS_BlitDepthToAlpha draw and explicit historical alias replay. No finished-world, headset, frame-rate or bad-scene acceptance.",
        "actual_game_capture":true,"gpu_executed":false,"captured_native_dummy_zero_verified":true,"input_provenance":input.manifest["source"],"world_view":input.manifest["world_view"],
        "original_draw_count":1,"width":input.width,"height":input.height,"effective_pipeline":input.manifest["draw"]["pipeline"],
        "original_color_channel_changes":channel_changes(&initial[0],&expected[0]),"original_draw_changed_any_attachment":initial!=expected});
    if args.len() == 4 {
        fs::write(
            out.join("admission.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("CPU admission passed; GPU not initialized");
        return Ok(());
    }
    let mut id = InstanceDescriptor::new_without_display_handle();
    id.backends = Backends::METAL;
    let instance = Instance::new(id);
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        power_preference: PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    let features = Features::DEPTH32FLOAT_STENCIL8
        | Features::TEXTURE_COMPRESSION_BC
        | Features::DEPTH_CLIP_CONTROL;
    require(
        adapter.features().contains(features),
        "captured Metal features",
    )?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        label: Some("private exact color-null replay"),
        required_features: features,
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let fixed_targets = targets(&device, &queue, &input, &initial)?;
    let fixed = draw(&device, &queue, &input, &input.manifest["draw"], None)?;
    encode(&device, &queue, &fixed_targets, &[&fixed], &input)?;
    let actual = readback(&device, &queue, &fixed_targets, input.width, input.height)?;
    let fixed_error = pollster::block_on(scope.pop());
    require(
        fixed_error.is_none(),
        &format!("corrected validation error: {fixed_error:?}"),
    )?;
    report["gpu_executed"] = true.into();
    report["adapter"] = json!(format!("{:?}", adapter.get_info()));
    report["initial_attachment_import_exact"] = true.into();
    report["corrected_final_bytes_equal_original"] = json!(actual == expected);
    report["attachments"]=json!(actual.iter().zip(&expected).enumerate().map(|(i,(a,b))|{
        let stride=if i==2{1}else{4};let delta=difference_counts(&initial[i],b,stride);let diff=difference_counts(a,b,stride);
        json!({"role":input.manifest["attachments"][i]["role"],"initial_sha256":sha(&initial[i]),"original_final_sha256":sha(b),"replayed_sha256":sha(a),
            "original_changed_bytes":delta.0,"original_changed_pixels":delta.1,"differing_bytes":diff.0,"differing_pixels":diff.1})
    }).collect::<Vec<_>>());
    // Each historical branch imports the same exact original targets afresh.
    // The optional doubled draw is explicitly a pass-discard control, not an
    // assertion that the game executed this two-draw sequence.
    let mut historical = Vec::new();
    for valid_first in [false, true] {
        let setup = device.push_error_scope(ErrorFilter::Validation);
        let old_targets = targets(&device, &queue, &input, &initial)?;
        let old = draw(
            &device,
            &queue,
            &input,
            &input.manifest["draw"],
            Some(&old_targets[0]),
        )?;
        device.poll(PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })?;
        require(
            pollster::block_on(setup.pop()).is_none(),
            "historical setup validation",
        )?;
        let failure = device.push_error_scope(ErrorFilter::Validation);
        if valid_first {
            encode(&device, &queue, &old_targets, &[&fixed, &old], &input)?;
        } else {
            encode(&device, &queue, &old_targets, &[&old], &input)?;
        }
        let error =
            pollster::block_on(failure.pop()).ok_or("historical alias unexpectedly accepted")?;
        let message = error.to_string();
        require(
            message.contains("conflict")
                && (message.contains("RESOURCE") || message.contains("Resource"))
                && (message.contains("COLOR_TARGET")
                    || message.contains("ColorTarget")
                    || message.contains("COLOR_ATTACHMENT")),
            "wrong historical validation failure",
        )?;
        let reads = device.push_error_scope(ErrorFilter::Validation);
        let old_output = readback(&device, &queue, &old_targets, input.width, input.height)?;
        require(
            pollster::block_on(reads.pop()).is_none(),
            "post-error independent readback validation",
        )?;
        require(
            old_output == initial,
            "historical rejected pass modified original target",
        )?;
        for (i, data) in old_output.iter().enumerate() {
            fs::write(
                out.join(format!(
                    "historical-{}-aspect{i}.bin",
                    if valid_first {
                        "valid-first-control"
                    } else {
                        "single"
                    }
                )),
                data,
            )?;
        }
        historical.push(json!({"original_draw_replay_count":if valid_first{2}else{1},"valid_first_same_draw_control":valid_first,
            "draw_order_matches_single_captured_draw":!valid_first,"bindings_match_original_capture":false,"historical_binding_reconstruction":true,"expected_alias_validation_error":message,"all_target_bytes_unchanged":true,
            "aspect_sha256":old_output.iter().map(|b|sha(b)).collect::<Vec<_>>() }));
    }
    report["historical_branches"] = json!(historical);
    report["corrected_color_channel_changes"] = channel_changes(&initial[0], &actual[0]);
    for (i, data) in actual.iter().enumerate() {
        fs::write(out.join(format!("corrected-aspect{i}.bin")), data)?;
    }
    fs::write(out.join("result.json"), serde_json::to_vec_pretty(&report)?)?;
    require(
        actual == expected,
        "corrected replay differs from original draw",
    )?;
    require(
        initial != expected,
        "original draw makes no change; meaningful image proof absent",
    )?;
    println!("PASS: original corrected draw byte-exact; historical alias rejected; all imported targets unchanged");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rgba_metrics_preserve_alpha_distinction() {
        assert_eq!(
            channel_changes(&[1, 2, 3, 4, 5, 6, 7, 8], &[1, 2, 3, 0, 0, 6, 7, 8]),
            json!({"blue":1,"green":0,"red":0,"alpha":1,"rgb_changed_pixels":1,"rgba_changed_pixels":2})
        );
    }
    #[test]
    fn compare_bytes_and_pixels() {
        assert_eq!(
            difference_counts(&[1, 2, 3, 4, 5, 6, 7, 8], &[0, 0, 3, 4, 5, 6, 7, 9], 4),
            (3, 2)
        );
    }
}

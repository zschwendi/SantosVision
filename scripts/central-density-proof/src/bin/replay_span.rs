//! Private two-draw replay. Imports original attachment contents and real bindings.
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
const NATIVE_VS: &str = "04f353256b2a27c7141076f68c572d451f551a0b8370bafd6f521fc8dc90cb94";
const NATIVE_PS: &str = "1e31e158d1f81369d39401dabee56c4b988eec1110f81b21f5b99ad3c8e6c5af";
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
        require(raw.len() < 8 * 1024 * 1024, "manifest exceeds limit")?;
        let m: Value = serde_json::from_slice(&raw)?;
        require(
            m["schema"] == 2 && m["kind"] == "native_draw_span",
            "requires native_draw_span schema2",
        )?;
        require(
            array(&m, "draws")?.len() == 2,
            "exactly two original draws required",
        )?;
        let native = m["source"]["method"] == "Gpu::do_draw resolved bindings";
        let authored = m["source"]["method"] == "authored_api_validation_fixture";
        require(
            (native && m["status"] == "captured")
                || (authored && m["status"] == "authored_fixture"),
            "unknown input provenance",
        )?;
        if native {
            require(
                m["source"]["backend"] == "Metal",
                "native capture backend mismatch",
            )?;
            for d in array(&m, "draws")? {
                require(
                    d["vertex_hash"] == "6a6a0898a51cf403"
                        && d["pixel_hash"] == "75cf2d53bf9497c9"
                        && sha(text(d, "vertex_wgsl")?.as_bytes()) == NATIVE_VS
                        && sha(text(d, "pixel_wgsl")?.as_bytes()) == NATIVE_PS,
                    "native resolved shader source pin mismatch",
                )?;
            }
        }
        require(
            m["world_view"].is_object()
                && m["target_domain"]
                    .as_object()
                    .is_some_and(|d| !d.is_empty()),
            "exact view and native target domain absent",
        )?;
        for key in [
            "provenance_epoch",
            "camera_sequence",
            "request_frame",
            "head_pose_frame",
        ] {
            require(number(&m["world_view"], key)? > 0, "invalid world identity")?;
        }
        require(
            number(&m["world_view"], "eye_index")? < 2
                && m["world_view"]["headset_applied"] == true
                && m["world_view"]["depth_reverse_z"] == true,
            "unqualified original source eye",
        )?;
        for key in ["game_from_eye", "projection_from_eye"] {
            let a = array(&m["world_view"], key)?;
            require(
                a.len() == 16 && a.iter().all(|n| n.as_f64().is_some_and(f64::is_finite)),
                "nonfinite/missing source matrix",
            )?;
        }
        require(
            m["draws"][0]["vertex_wgsl"] == m["draws"][1]["vertex_wgsl"]
                && m["draws"][0]["pixel_wgsl"] == m["draws"][1]["pixel_wgsl"],
            "first span requires one exact resolved shader family",
        )?;
        let root = path
            .parent()
            .ok_or("manifest directory absent")?
            .canonicalize()?;
        let mut blobs = BTreeMap::new();
        let mut total = 0u64;
        for b in array(&m, "blobs")? {
            let name = text(b, "path")?;
            let relative = Path::new(name);
            require(
                !relative.is_absolute()
                    && relative
                        .components()
                        .all(|p| matches!(p, Component::Normal(_))),
                "blob path must remain within capture",
            )?;
            let full = root.join(relative).canonicalize()?;
            require(full.starts_with(&root), "blob symlink escapes capture")?;
            let length = number(b, "byte_length")?;
            total = total.checked_add(length).ok_or("capture size overflow")?;
            require(
                total <= MAX_BYTES && fs::metadata(&full)?.len() == length,
                "blob size/total limit",
            )?;
            let bytes = fs::read(&full)?;
            require(sha(&bytes) == text(b, "sha256")?, "blob hash mismatch")?;
            require(
                blobs.insert(name.to_owned(), bytes).is_none(),
                "duplicate blob path",
            )?;
        }
        let at = array(&m, "attachments")?;
        require(
            at.len() == 6,
            "four color and separate depth/stencil aspects required",
        )?;
        let width = number(&at[0], "width")? as u32;
        let height = number(&at[0], "height")? as u32;
        require(
            width > 0 && height > 0 && width <= 4096 && height <= 4096,
            "attachment dimensions out of bounds",
        )?;
        for (i, a) in at.iter().enumerate() {
            let role = if i < 4 {
                format!("color{i}")
            } else {
                ["depth", "stencil"][i - 4].into()
            };
            require(
                a["role"] == role
                    && number(a, "width")? == width as u64
                    && number(a, "height")? == height as u64,
                "attachment role/dimension mismatch",
            )?;
            let format: TextureFormat = decode(&a["format"])?;
            require(
                format
                    == if i < 4 {
                        TextureFormat::Bgra8Unorm
                    } else {
                        TextureFormat::Depth32FloatStencil8
                    },
                "unsupported exact attachment format",
            )?;
            let bytes = width as usize * height as usize * if i == 5 { 1 } else { 4 };
            for key in ["initial_blob", "final_blob"] {
                require(
                    blobs.get(text(a, key)?).is_some_and(|b| b.len() == bytes),
                    "attachment blob is not tight/full-sized",
                )?;
            }
            if i == 4 {
                for key in ["initial_blob", "final_blob"] {
                    require(
                        blobs[text(a, key)?].chunks_exact(4).all(|x| {
                            let z = f32::from_le_bytes(x.try_into().unwrap());
                            z.is_finite() && (0.0..=1.0).contains(&z)
                        }),
                        "unsupported nonfinite/out-of-range original depth",
                    )?;
                }
            }
        }
        for d in array(&m, "draws")? {
            require(
                number(&d["draw"], "kind")? == 1 && number(&d["draw"], "instances")? > 0,
                "first replay accepts direct indexed draws only",
            )?;
            require(
                d["world_view"] == m["world_view"],
                "draw is not the same individual eye/pose/epoch",
            )?;
            require(
                d["target_domain"] == m["target_domain"],
                "draw target generation/subresource changed",
            )?;
            require(
                !text(d, "vertex_wgsl")?.is_empty() && !text(d, "pixel_wgsl")?.is_empty(),
                "resolved shaders absent",
            )?;
            let primitive: PrimitiveState = decode(&d["pipeline"]["primitive"])?;
            require(
                primitive.topology == PrimitiveTopology::TriangleList
                    && !primitive.conservative
                    && primitive.polygon_mode == PolygonMode::Fill,
                "unsupported primitive state",
            )?;
            let multi: MultisampleState = decode(&d["pipeline"]["multisample"])?;
            require(multi.count == 1, "multisample replay unsupported")?;
            let targets: Vec<Option<ColorTargetState>> = decode(&d["pipeline"]["targets"])?;
            require(
                targets.len() == 4
                    && targets.iter().all(|x| {
                        x.as_ref()
                            .is_some_and(|t| t.format == TextureFormat::Bgra8Unorm)
                    }),
                "four exact color targets required",
            )?;
            let depth: DepthStencilState = decode(&d["pipeline"]["depth_stencil"])?;
            require(
                depth.format == TextureFormat::Depth32FloatStencil8,
                "depth pipeline format mismatch",
            )?;
            let _ = array(&d["pipeline"], "vertex_layouts")?;
            let ib = &d["index_buffer"];
            let index_format: IndexFormat = decode(&ib["format"])?;
            let index_size = if index_format == IndexFormat::Uint16 {
                2u64
            } else {
                4
            };
            let first = number(&d["draw"], "start_index")?;
            let count = number(&d["draw"], "count")?;
            let end = first
                .checked_add(count)
                .and_then(|n| n.checked_mul(index_size))
                .and_then(|n| n.checked_add(number(ib, "offset").ok()?))
                .ok_or("index range overflow")?;
            require(
                count > 0
                    && blobs
                        .get(text(ib, "blob")?)
                        .is_some_and(|b| end <= b.len() as u64),
                "captured index range incomplete",
            )?;
            let mut vertex_slots = std::collections::BTreeSet::new();
            for vb in array(d, "vertex_buffers")? {
                let slot = number(vb, "slot")?;
                require(
                    slot < 16
                        && vertex_slots.insert(slot)
                        && blobs.get(text(vb, "blob")?).is_some_and(|b| {
                            number(vb, "offset").is_ok_and(|o| o < b.len() as u64)
                        }),
                    "invalid captured vertex range",
                )?;
            }
            for (slot, layout) in array(&d["pipeline"], "vertex_layouts")?.iter().enumerate() {
                require(
                    layout.is_null() || vertex_slots.contains(&(slot as u64)),
                    "pipeline vertex slot not captured",
                )?;
            }
            let viewport: [f32; 6] = decode(&d["viewport"])?;
            require(
                viewport.iter().all(|x| x.is_finite())
                    && viewport[0] >= 0.
                    && viewport[1] >= 0.
                    && viewport[2] > 0.
                    && viewport[3] > 0.
                    && viewport[0] + viewport[2] <= width as f32
                    && viewport[1] + viewport[3] <= height as f32
                    && viewport[4] >= 0.
                    && viewport[5] >= viewport[4]
                    && viewport[5] <= 1.,
                "viewport outside retained target",
            )?;
            let scissor: [u32; 4] = decode(&d["scissor"])?;
            require(
                scissor[0]
                    .checked_add(scissor[2])
                    .is_some_and(|x| x <= width)
                    && scissor[1]
                        .checked_add(scissor[3])
                        .is_some_and(|x| x <= height),
                "scissor outside retained target",
            )?;
            let mut slots = std::collections::BTreeSet::new();
            for b in array(d, "bindings")? {
                let layout: BindGroupLayoutEntry = decode(&b["layout"])?;
                require(layout.count.is_none(), "binding arrays unsupported")?;
                require(
                    number(b, "group")? <= 1 && number(b, "binding")? == layout.binding as u64,
                    "binding index mismatch",
                )?;
                require(
                    slots.insert((number(b, "group")?, layout.binding)),
                    "duplicate binding",
                )?;
                match text(b, "kind")? {
                    "uniform" | "buffer" => {
                        require(
                            matches!(
                                layout.ty,
                                BindingType::Buffer {
                                    ty: BufferBindingType::Uniform
                                        | BufferBindingType::Storage { read_only: true },
                                    has_dynamic_offset: false,
                                    ..
                                }
                            ),
                            "mutable/dynamic buffer unsupported",
                        )?;
                        require(blobs.contains_key(text(b, "blob")?), "buffer blob missing")?;
                    }
                    "texture" => {
                        require(
                            matches!(
                                layout.ty,
                                BindingType::Texture {
                                    sample_type: TextureSampleType::Float { .. },
                                    view_dimension: TextureViewDimension::D2,
                                    multisampled: false
                                }
                            ),
                            "first replay accepts float2D sampled material only",
                        )?;
                        let t = &b["descriptor"];
                        let width = number(t, "width")?;
                        let height = number(t, "height")?;
                        let levels = number(t, "mip_level_count")?;
                        require(
                            width > 0
                                && height > 0
                                && width <= 16384
                                && height <= 16384
                                && levels > 0
                                && levels <= 15
                                && number(t, "depth_or_array_layers")? == 1
                                && number(t, "sample_count")? == 1,
                            "material dimensions/sample count unsupported",
                        )?;
                        let format: TextureFormat = decode(&t["format"])?;
                        let (bw, bh) = format.block_dimensions();
                        let bpp = format
                            .block_copy_size(None)
                            .ok_or("uncopyable material format")?;
                        let mut seen = std::collections::BTreeSet::new();
                        for sub in array(b, "subresources")? {
                            let mip = number(sub, "mip_level")?;
                            require(
                                mip < levels && number(sub, "layer")? == 0 && seen.insert(mip),
                                "bad/duplicate material mip",
                            )?;
                            let sw = (width >> mip).max(1);
                            let sh = (height >> mip).max(1);
                            let row = sw.div_ceil(bw as u64) * bpp as u64;
                            require(
                                number(sub, "width")? == sw
                                    && number(sub, "height")? == sh
                                    && number(sub, "packed_row_bytes")? == row,
                                "material subresource dimensions",
                            )?;
                            require(
                                blobs.get(text(sub, "blob")?).is_some_and(|b| {
                                    b.len() as u64 == row * sh.div_ceil(bh as u64)
                                }),
                                "material mip blob incomplete",
                            )?;
                        }
                        let view: wgpu_types::TextureViewDescriptor<Option<String>> =
                            decode(&b["view"])?;
                        require(
                            (view.base_mip_level as u64) < levels
                                && view.base_array_layer == 0
                                && view.array_layer_count.is_none_or(|n| n == 1),
                            "material view range unsupported",
                        )?;
                        let count = view
                            .mip_level_count
                            .unwrap_or(levels as u32 - view.base_mip_level);
                        require(
                            count > 0
                                && view
                                    .base_mip_level
                                    .checked_add(count)
                                    .is_some_and(|end| end as u64 <= levels)
                                && (view.base_mip_level..view.base_mip_level + count)
                                    .all(|m| seen.contains(&(m as u64))),
                            "visible material mip missing",
                        )?;
                    }
                    "sampler" => {
                        require(
                            matches!(layout.ty, BindingType::Sampler(_)),
                            "sampler layout mismatch",
                        )?;
                        let _: wgpu_types::SamplerDescriptor<Option<String>> =
                            decode(&b["descriptor"])?;
                    }
                    _ => return Err("unsupported binding kind".into()),
                }
            }
        }
        require(
            m["closure"]["ordered_two_draws"] == true
                && m["closure"]["unmodelled_commands"] == 0
                && m["closure"]["resource_mutation_rejections"] == 0,
            "span not closed",
        )?;
        require(
            m["closure"]["initial_contents_captured"] == true
                && m["closure"]["original_final_captured"] == true
                && (m["closure"]["natural_submission_completed"] == true
                    || (m["source"]["method"] == "authored_api_validation_fixture"
                        && m["closure"]["reference_submission_completed"] == true)),
            "original attachment/submission proof absent",
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
    index: Buffer,
    index_format: IndexFormat,
    index_offset: u64,
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
fn draw(device: &Device, queue: &Queue, input: &Input, d: &Value) -> Result<Draw> {
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
        depth_stencil: Some(decode(&d["pipeline"]["depth_stencil"])?),
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
    let ib = &d["index_buffer"];
    let index = device.create_buffer_init(&util::BufferInitDescriptor {
        label: Some("captured IB"),
        contents: input.blob(text(ib, "blob")?)?,
        usage: BufferUsages::INDEX,
    });
    let blend: [f64; 4] = decode(&d["blend_constant"])?;
    Ok(Draw {
        pipeline,
        groups,
        vertices,
        index,
        index_format: decode(&ib["format"])?,
        index_offset: number(ib, "offset")?,
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
    for i in 0..6 {
        let bpp = if i == 5 { 1 } else { 4 };
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
                texture: &textures[i.min(4)],
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: if i == 4 {
                    TextureAspect::DepthOnly
                } else if i == 5 {
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

fn main() -> Result<()> {
    let a = std::env::args().collect::<Vec<_>>();
    require(
        a.len() == 3 || (a.len() == 4 && a[3] == "--cpu-only"),
        "usage: replay_span MANIFEST NEW_OUTPUT_DIR [--cpu-only]",
    )?;
    let input = Input::load(Path::new(&a[1]))?;
    let out = PathBuf::from(&a[2]);
    fs::create_dir(&out)?;
    let mut report = json!({"schema":1,"scope":"two original draws with actual immutable material/state and inherited attachments; source-list only; no finished image or runtime publication","manifest_sha256":input.manifest_sha,"gpu_executed":false,"same_eye":input.manifest["world_view"],"draw_count":2});
    report["input_provenance"] = input.manifest["source"].clone();
    report["actual_game_capture"] =
        json!(input.manifest["source"]["method"] == "Gpu::do_draw resolved bindings");
    if input.manifest["source"]["method"] == "authored_api_validation_fixture" {
        report["scope"] = json!("Authored synthetic API fixture only; two literal direct reference draws; not actual game imagery or natural game submission");
    }
    if a.len() == 4 {
        fs::write(
            out.join("admission.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("CPU admission passed; GPU not initialized");
        return Ok(());
    }
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = Backends::METAL;
    let instance = Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        power_preference: PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    let required = Features::DEPTH32FLOAT_STENCIL8
        | Features::TEXTURE_COMPRESSION_BC
        | Features::DEPTH_CLIP_CONTROL;
    require(
        adapter.features().contains(required),
        "required captured features unavailable",
    )?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        label: Some("private actual two-draw replay"),
        required_features: required,
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let textures = (0..5)
        .map(|i| {
            device.create_texture(&TextureDescriptor {
                label: Some("persistent inherited span target"),
                size: Extent3d {
                    width: input.width,
                    height: input.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: if i < 4 {
                    TextureFormat::Bgra8Unorm
                } else {
                    TextureFormat::Depth32FloatStencil8
                },
                usage: TextureUsages::RENDER_ATTACHMENT
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST,
                view_formats: &[],
            })
        })
        .collect::<Vec<_>>();
    let initial = input.attachments("initial_blob")?;
    let expected = input.attachments("final_blob")?;
    for i in 0..4 {
        upload(
            &queue,
            &textures[i],
            0,
            0,
            TextureAspect::All,
            input.width,
            input.height,
            &initial[i],
            input.width * 4,
        );
    }
    upload(
        &queue,
        &textures[4],
        0,
        0,
        TextureAspect::StencilOnly,
        input.width,
        input.height,
        &initial[5],
        input.width,
    );
    restore_depth(&device, &queue, &textures[4], &input, &initial[4])?;
    let imported = readback(&device, &queue, &textures, input.width, input.height)?;
    require(
        imported == initial,
        "initial attachment import is not byte-exact",
    )?;
    let draws = array(&input.manifest, "draws")?
        .iter()
        .map(|d| draw(&device, &queue, &input, d))
        .collect::<Result<Vec<_>>>()?;
    let views = textures
        .iter()
        .map(|t| t.create_view(&TextureViewDescriptor::default()))
        .collect::<Vec<_>>();
    let mut e = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("exact ordered two-draw span"),
    });
    {
        let colors = views[..4]
            .iter()
            .map(|v| {
                Some(RenderPassColorAttachment {
                    view: v,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })
            })
            .collect::<Vec<_>>();
        let mut pass = e.begin_render_pass(&RenderPassDescriptor {
            label: Some("persistent original target domain"),
            color_attachments: &colors,
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &views[4],
                depth_ops: Some(Operations {
                    load: LoadOp::Load,
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
        for d in &draws {
            pass.set_pipeline(&d.pipeline);
            for (i, g) in d.groups.iter().enumerate() {
                pass.set_bind_group(i as u32, g, &[]);
            }
            for (slot, b, offset) in &d.vertices {
                pass.set_vertex_buffer(*slot, b.slice(*offset..));
            }
            pass.set_index_buffer(d.index.slice(d.index_offset..), d.index_format);
            let v = d.viewport;
            pass.set_viewport(v[0], v[1], v[2], v[3], v[4], v[5]);
            pass.set_scissor_rect(d.scissor[0], d.scissor[1], d.scissor[2], d.scissor[3]);
            pass.set_blend_constant(d.blend);
            pass.set_stencil_reference(d.stencil);
            let start = number(&d.args, "start_index")? as u32;
            let count = number(&d.args, "count")? as u32;
            let first = number(&d.args, "start_instance")? as u32;
            let instances = number(&d.args, "instances")? as u32;
            let base = d.args["base_vertex"].as_i64().ok_or("base vertex")? as i32;
            pass.draw_indexed(
                start..start.checked_add(count).ok_or("draw index overflow")?,
                base,
                first
                    ..first
                        .checked_add(instances)
                        .ok_or("draw instance overflow")?,
            );
        }
    }
    queue.submit([e.finish()]);
    let actual = readback(&device, &queue, &textures, input.width, input.height)?;
    let error = pollster::block_on(scope.pop());
    require(
        error.is_none(),
        &format!("Metal validation error: {error:?}"),
    )?;
    report["gpu_executed"] = json!(true);
    report["adapter"] = json!(format!("{:?}", adapter.get_info()));
    report["initial_attachment_import_exact"] = json!(true);
    report["all_original_final_attachment_bytes_equal"] = json!(actual == expected);
    report["attachments"] = json!(actual
        .iter()
        .zip(&expected)
        .enumerate()
        .map(|(i, (a, b))| {
            let pixel_bytes = if i == 5 { 1 } else { 4 };
            let (original_changed_bytes, original_changed_pixels) =
                difference_counts(&initial[i], b, pixel_bytes);
            let (replay_changed_bytes, replay_changed_pixels) =
                difference_counts(&initial[i], a, pixel_bytes);
            let (differing_bytes, differing_pixels) = difference_counts(a, b, pixel_bytes);
            json!({"aspect":i,"role":input.manifest["attachments"][i]["role"],
            "actual_sha256":sha(a),"original_sha256":sha(b),"initial_sha256":sha(&initial[i]),
            "differing_bytes":differing_bytes,"differing_pixels":differing_pixels,
            "original_initial_to_final_changed_bytes":original_changed_bytes,
            "original_initial_to_final_changed_pixels":original_changed_pixels,
            "replay_initial_to_final_changed_bytes":replay_changed_bytes,
            "replay_initial_to_final_changed_pixels":replay_changed_pixels})
        })
        .collect::<Vec<_>>());
    report["original_draws_changed_attachment_contents"] = json!(initial != expected);
    for (i, data) in actual.iter().enumerate() {
        fs::write(out.join(format!("replayed-aspect{i}.bin")), data)?;
    }
    fs::write(out.join("result.json"), serde_json::to_vec_pretty(&report)?)?;
    require(
        actual == expected,
        "replay differs from actual original final span",
    )?;
    require(
        initial != expected,
        "original span leaves every attachment unchanged; no meaningful draw proof",
    )?;
    println!("PASS: two original draws; six initial and six final attachment aspects byte-exact");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_and_pixel_change_counts_are_distinct() {
        assert_eq!(
            difference_counts(&[0; 8], &[1, 2, 3, 4, 0, 0, 0, 5], 4),
            (5, 2)
        );
        assert_eq!(
            difference_counts(&[0; 8], &[1, 2, 3, 4, 0, 0, 0, 5], 1),
            (5, 5)
        );
        assert_eq!(difference_counts(&[0; 8], &[0; 8], 4), (0, 0));
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        value: Value,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "v-native-span-admission-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let mut blobs = Vec::new();
            for (name, data) in [
                ("color.bin", vec![1, 2, 3, 4]),
                ("depth.bin", vec![0, 0, 0, 0]),
                ("stencil.bin", vec![7]),
                ("index.bin", vec![0; 6]),
            ] {
                fs::write(root.join(name), &data).unwrap();
                blobs.push(json!({"path":name,"byte_length":data.len(),"sha256":sha(&data)}));
            }
            let identity = (0..16)
                .map(|i| if i % 5 == 0 { 1.0 } else { 0.0 })
                .collect::<Vec<_>>();
            let world = json!({"provenance_epoch":2,"camera_sequence":3,"request_frame":4,"head_pose_frame":5,"eye_index":0,"headset_applied":true,"depth_reverse_z":true,"game_from_eye":identity,"projection_from_eye":identity});
            let domain = json!({"allocation_generation":1});
            let attachments=(0..6).map(|i|{let (role,format,blob)=if i<4{(format!("color{i}"),TextureFormat::Bgra8Unorm,"color.bin")}else if i==4{("depth".into(),TextureFormat::Depth32FloatStencil8,"depth.bin")}else{("stencil".into(),TextureFormat::Depth32FloatStencil8,"stencil.bin")};json!({"role":role,"format":format,"width":1,"height":1,"initial_blob":blob,"final_blob":blob})}).collect::<Vec<_>>();
            let draw = json!({"draw":{"kind":1,"instances":1,"count":3,"start_index":0,"base_vertex":0,"start_instance":0},"world_view":world,"target_domain":domain,
                "vertex_wgsl":"CPU admission test only; never submitted", "pixel_wgsl":"CPU admission test only; never submitted",
                "pipeline":{"primitive":PrimitiveState::default(),"multisample":MultisampleState::default(),"targets":(0..4).map(|_|Some(ColorTargetState{format:TextureFormat::Bgra8Unorm,blend:None,write_mask:ColorWrites::ALL})).collect::<Vec<_>>(),
                    "depth_stencil":DepthStencilState{format:TextureFormat::Depth32FloatStencil8,depth_write_enabled:Some(true),depth_compare:Some(CompareFunction::GreaterEqual),stencil:StencilState::default(),bias:DepthBiasState::default()},"vertex_layouts":[]},
                "bindings":[],"vertex_buffers":[],"index_buffer":{"format":IndexFormat::Uint16,"offset":0,"blob":"index.bin"},"viewport":[0,0,1,1,0,1],"scissor":[0,0,1,1],"blend_constant":[0,0,0,0],"stencil_reference":7});
            let value = json!({"schema":2,"kind":"native_draw_span","status":"authored_fixture","source":{"method":"authored_api_validation_fixture"},"world_view":world,"target_domain":domain,"blobs":blobs,"attachments":attachments,"draws":[draw.clone(),draw],
                "closure":{"ordered_two_draws":true,"unmodelled_commands":0,"resource_mutation_rejections":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":false,"reference_submission_completed":true}});
            Self { root, value }
        }
        fn load(&self, value: &Value) -> Result<Input> {
            let path = self.root.join("manifest.json");
            fs::write(&path, serde_json::to_vec(value)?)?;
            Input::load(&path)
        }
        fn rejects(&self, value: Value, reason: &str) {
            let error = match self.load(&value) {
                Ok(_) => panic!("unexpected CPU admission"),
                Err(e) => e.to_string(),
            };
            assert!(error.contains(reason), "{error} did not contain {reason}");
        }
    }
    #[test]
    fn cpu_admission_requires_original_same_eye_closed_span() {
        let f = Fixture::new();
        assert!(f.load(&f.value).is_ok());
        for (field, value, reason) in [
            ("world_view", json!(null), "same individual eye"),
            (
                "target_domain",
                json!({"allocation_generation":2}),
                "generation/subresource",
            ),
            ("vertex_wgsl", json!("different shader"), "shader family"),
        ] {
            let mut j = f.value.clone();
            j["draws"][1][field] = value;
            f.rejects(j, reason);
        }
        for field in [
            "initial_contents_captured",
            "original_final_captured",
            "reference_submission_completed",
        ] {
            let mut j = f.value.clone();
            j["closure"][field] = json!(false);
            f.rejects(j, "proof absent");
        }
        for field in ["unmodelled_commands", "resource_mutation_rejections"] {
            let mut j = f.value.clone();
            j["closure"][field] = json!(1);
            f.rejects(j, "span not closed");
        }
        let mut j = f.value.clone();
        j["attachments"][2]["initial_blob"] = json!("absent.bin");
        f.rejects(j, "tight/full-sized");
        let mut j = f.value.clone();
        j["draws"][1]["draw"]["count"] = json!(4);
        f.rejects(j, "index range incomplete");
        let mut j = f.value.clone();
        j["draws"][0]["scissor"] = json!([1, 0, 1, 1]);
        f.rejects(j, "scissor outside");
    }
    #[test]
    fn cpu_admission_rejects_path_escape_hash_and_nonfinite_depth() {
        let f = Fixture::new();
        let mut j = f.value.clone();
        j["blobs"][0]["path"] = json!("../color.bin");
        f.rejects(j, "path must remain");
        let mut j = f.value.clone();
        j["blobs"][0]["sha256"] = json!("0".repeat(64));
        f.rejects(j, "hash mismatch");
        let bytes = f32::NAN.to_le_bytes();
        fs::write(f.root.join("depth.bin"), bytes).unwrap();
        let mut j = f.value.clone();
        j["blobs"][1]["sha256"] = json!(sha(&bytes));
        f.rejects(j, "nonfinite/out-of-range");
    }
    #[test]
    fn cpu_admission_requires_all_visible_material_mips() {
        let f = Fixture::new();
        let mut j = f.value.clone();
        let view: wgpu_types::TextureViewDescriptor<Option<String>> = Default::default();
        let texture = json!({"group":1,"binding":32,"kind":"texture","layout":BindGroupLayoutEntry{binding:32,visibility:ShaderStages::FRAGMENT,ty:BindingType::Texture{sample_type:TextureSampleType::Float{filterable:true},view_dimension:TextureViewDimension::D2,multisampled:false},count:None},
            "descriptor":{"width":1,"height":1,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":TextureDimension::D2,"format":TextureFormat::Rgba8Unorm},"view":view,"subresources":[{"width":1,"height":1,"packed_row_bytes":4,"mip_level":0,"layer":0,"blob":"color.bin"}]});
        j["draws"][0]["bindings"] = json!([texture]);
        assert!(f.load(&j).is_ok());
        let mut missing = j.clone();
        missing["draws"][0]["bindings"][0]["subresources"] = json!([]);
        f.rejects(missing, "visible material mip missing");
        let mut short = j.clone();
        short["draws"][0]["bindings"][0]["subresources"][0]["blob"] = json!("stencil.bin");
        f.rejects(short, "mip blob incomplete");
        let mut duplicate = j.clone();
        let mip = duplicate["draws"][0]["bindings"][0]["subresources"][0].clone();
        duplicate["draws"][0]["bindings"][0]["subresources"]
            .as_array_mut()
            .unwrap()
            .push(mip);
        f.rejects(duplicate, "duplicate material mip");
    }
}

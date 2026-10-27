//! Strict schema-5 importer: one original dispatch, with no inferred eye ownership.
use crate::{
    model::*,
    require, sha,
    validate::{aspects, Input, MAX_BYTES},
    Result,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};
use wgpu_types::*;
fn get<'a>(v: &'a Value, k: &str) -> Result<&'a Value> {
    v.get(k)
        .ok_or_else(|| format!("native capture missing {k}").into())
}
fn num(v: &Value, k: &str) -> Result<u64> {
    get(v, k)?
        .as_u64()
        .ok_or_else(|| format!("native capture invalid {k}").into())
}
fn u32n(v: &Value, k: &str) -> Result<u32> {
    Ok(num(v, k)?.try_into()?)
}
fn txt<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    get(v, k)?
        .as_str()
        .ok_or_else(|| format!("native capture invalid string {k}").into())
}
fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    get(v, k)?
        .as_array()
        .ok_or_else(|| format!("native capture invalid array {k}").into())
}
fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}
fn texture_id(n: u64) -> Result<String> {
    require(n > 0, "native allocation ID zero")?;
    Ok(format!("native-texture:{n}"))
}
fn version(v: &Value) -> Result<Version> {
    let sub = get(v, "subresource")?;
    let n = num(v, "content_version")?;
    require(n > 0, "native content version zero")?;
    Ok(Version {
        slice: Slice {
            resource: texture_id(num(v, "allocation_id")?)?,
            mip: u32n(sub, "mip")?,
            layer: u32n(sub, "layer")?,
            aspect: decode(get(sub, "aspect")?)?,
        },
        content_version: n,
    })
}
fn descriptor(v: &Value) -> Result<TextureDesc> {
    require(v["sample_count"] == 1, "native texture shape unsupported")?;
    let d = TextureDesc {
        dimension: decode(get(v, "dimension")?)?,
        width: u32n(v, "width")?,
        height: u32n(v, "height")?,
        layers: u32n(v, "depth_or_array_layers")?,
        mips: u32n(v, "mip_level_count")?,
        format: decode(get(v, "format")?)?,
        view_formats: vec![],
    };
    require(
        d.width > 0
            && d.height > 0
            && d.width <= 16384
            && d.height <= 16384
            && d.layers > 0
            && d.layers
                <= if d.dimension == TextureDimension::D3 {
                    2048
                } else {
                    256
                }
            && d.mips > 0
            && d.mips <= 15,
        "native descriptor bound",
    )?;
    Ok(d)
}
fn same_desc(a: &TextureDesc, b: &TextureDesc) -> bool {
    a.dimension == b.dimension
        && a.width == b.width
        && a.height == b.height
        && a.layers == b.layers
        && a.mips == b.mips
        && a.format == b.format
}
fn add_texture(s: &mut Scene, id: u64, d: TextureDesc, target: bool) -> Result<usize> {
    let name = texture_id(id)?;
    if let Some(i) = s.textures.iter().position(|t| t.id == name) {
        require(
            same_desc(&s.textures[i].descriptor, &d),
            "native allocation descriptor changed",
        )?;
        s.textures[i].mutable_target |= target;
        return Ok(i);
    }
    let count = d.mips as usize * d.subresource_layers() as usize * aspects(d.format).len();
    require(
        count <= 8192 && s.textures.iter().map(|t| t.initial.len()).sum::<usize>() + count <= 16384,
        "compute subresource metadata bound",
    )?;
    let mut initial = vec![];
    for mip in 0..d.mips {
        for layer in 0..d.subresource_layers() {
            for aspect in aspects(d.format) {
                initial.push(SliceInitial {
                    mip,
                    layer,
                    aspect,
                    content_version: 0,
                    origin: Initial::Unknown {
                        reason:
                            "Unselected subresource; native version and bytes were not captured"
                                .into(),
                    },
                })
            }
        }
    }
    s.textures.push(Texture {
        id: name,
        native_allocation_id: Some(id),
        descriptor: d,
        mutable_target: target,
        initial,
    });
    Ok(s.textures.len() - 1)
}
fn initial(s: &mut Scene, index: usize, v: &Version, blob: &str) -> Result<()> {
    let t = &mut s.textures[index];
    require(
        t.id == v.slice.resource,
        "native subresource parent mismatch",
    )?;
    let dst = t
        .initial
        .iter_mut()
        .find(|i| i.mip == v.slice.mip && i.layer == v.slice.layer && i.aspect == v.slice.aspect)
        .ok_or("native subresource outside descriptor")?;
    if dst.content_version != 0 {
        require(
            dst.content_version == v.content_version,
            "sampled native content changed within interval",
        )?;
        let old = match &dst.origin {
            Initial::Captured { blob } => blob,
            _ => return Err("native initial origin conflict".into()),
        };
        let a = s
            .blobs
            .iter()
            .find(|b| b.path == *old)
            .ok_or("initial blob missing")?;
        let b = s
            .blobs
            .iter()
            .find(|b| b.path == blob)
            .ok_or("initial blob missing")?;
        require(
            a.sha256 == b.sha256 && a.bytes == b.bytes,
            "same native version has different bytes",
        )?;
    } else {
        dst.content_version = v.content_version;
        dst.origin = Initial::Captured { blob: blob.into() };
    }
    Ok(())
}
fn buffer(s: &mut Scene, v: &Value) -> Result<String> {
    let allocation = num(v, "native_allocation_id")?;
    let snapshot = num(v, "content_version")?;
    let offset = num(v, "source_offset")?;
    let blob = txt(v, "blob")?;
    require(
        allocation > 0 && snapshot > 0,
        "native immutable buffer identity absent",
    )?;
    let bytes = s
        .blobs
        .iter()
        .find(|b| b.path == blob)
        .ok_or("native buffer blob missing")?
        .bytes;
    let id = format!("native-buffer:{allocation}:{snapshot}:{offset}:{bytes}");
    if let Some(b) = s.buffers.iter().find(|b| b.id == id) {
        let old = s
            .blobs
            .iter()
            .find(|x| x.path == b.blob)
            .ok_or("buffer blob absent")?;
        let new = s
            .blobs
            .iter()
            .find(|x| x.path == blob)
            .ok_or("buffer blob absent")?;
        require(
            old.sha256 == new.sha256 && old.bytes == new.bytes,
            "same native buffer snapshot has different bytes",
        )?;
    } else {
        s.buffers.push(Buffer {
            id: id.clone(),
            native_allocation_id: Some(allocation),
            content_version: snapshot,
            source_offset: offset,
            blob: blob.into(),
        });
    }
    Ok(id)
}
fn shader(registry: &Value, reference: &Value) -> Result<Shader> {
    require(
        reference["status"] == "exact" && reference["compilation_options"] == "wgpu default",
        "actual final shader unavailable or nondefault specialization",
    )?;
    let hash = txt(reference, "sha256")?;
    let text = txt(&registry["sources"][hash], "wgsl")?;
    require(sha(text.as_bytes()) == hash, "actual final shader hash")?;
    Ok(Shader {
        wgsl: text.into(),
        sha256: hash.into(),
        entry: txt(reference, "entry_point")?.into(),
        exact_final_variant: true,
    })
}

fn shape(v: &Value, allowed: &[&str]) -> Result<()> {
    let o = v.as_object().ok_or("compute metadata must be object")?;
    require(
        o.keys().all(|k| allowed.contains(&k.as_str())),
        "unmodelled compute metadata",
    )
}
fn blob<'a>(s: &'a Scene, name: &str) -> Result<&'a Blob> {
    s.blobs
        .iter()
        .find(|b| b.path == name)
        .ok_or_else(|| "compute blob absent".into())
}
fn view_slices(
    d: &TextureDesc,
    v: &TextureViewDescriptor<Option<String>>,
) -> Result<BTreeSet<(u32, u32, Aspect)>> {
    let mip_count = v.mip_level_count.unwrap_or(
        d.mips
            .checked_sub(v.base_mip_level)
            .ok_or("compute view mip base")?,
    );
    let remaining_layers = d
        .subresource_layers()
        .checked_sub(v.base_array_layer)
        .ok_or("compute view layer base")?;
    let layer_count = v.array_layer_count.unwrap_or(match v.dimension {
        Some(TextureViewDimension::D1 | TextureViewDimension::D2 | TextureViewDimension::D3) => 1,
        Some(TextureViewDimension::Cube) => 6,
        _ => remaining_layers,
    });
    let mip_end = v
        .base_mip_level
        .checked_add(mip_count)
        .ok_or("compute view mip overflow")?;
    let layer_end = v
        .base_array_layer
        .checked_add(layer_count)
        .ok_or("compute view layer overflow")?;
    require(
        mip_count > 0
            && layer_count > 0
            && mip_end <= d.mips
            && layer_end <= d.subresource_layers(),
        "compute view range",
    )?;
    let aspect = match v.aspect {
        TextureAspect::All if !d.format.is_depth_stencil_format() => Aspect::Color,
        TextureAspect::All | TextureAspect::DepthOnly
            if aspects(d.format).contains(&Aspect::Depth) =>
        {
            Aspect::Depth
        }
        TextureAspect::StencilOnly if aspects(d.format).contains(&Aspect::Stencil) => {
            Aspect::Stencil
        }
        _ => return Err("compute sampled aspect unsupported".into()),
    };
    Ok((v.base_mip_level..mip_end)
        .flat_map(|m| {
            (v.base_array_layer..layer_end).map({
                let aspect = aspect.clone();
                move |l| (m, l, aspect.clone())
            })
        })
        .collect())
}
fn version_identity(seen: &mut BTreeMap<u64, Slice>, v: &Version) -> Result<()> {
    if let Some(prior) = seen.insert(v.content_version, v.slice.clone()) {
        require(
            prior == v.slice,
            "compute content version names different subresources",
        )?;
    }
    Ok(())
}
pub(crate) fn convert(m: &Value, source_hash: String) -> Result<Scene> {convert_inner(m,source_hash,false)}
/// Only schema7 retains the original query events around this captured dispatch.
pub(crate) fn convert_graph_qualified(m:&Value,source_hash:String)->Result<Scene>{convert_inner(m,source_hash,true)}
fn convert_inner(m: &Value, source_hash: String, graph_queries:bool) -> Result<Scene> {
    shape(
        m,
        &[
            "schema",
            "kind",
            "status",
            "source",
            "context",
            "dispatch",
            "attachments",
            "blobs",
            "allocated_readback_bytes",
            "prior_rejected_candidates",
            "closure",
            "normal_frame_publication_changed",
            "finished_frame_or_density_accepted",
            "coherent_stereo_accepted",
            "uniform_binding_normalization",
            "initialization_ledger",
            "effective_shader_sources",
        ],
    )?;
    require(
        m["schema"] == 5 && m["kind"] == "native_compute_dispatch" && m["status"] == "captured",
        "requires schema5 native compute",
    )?;
    let closure = get(m, "closure")?;
    shape(
        closure,
        &[
            "single_original_dispatch",
            "unmodelled_commands",
            "initial_contents_captured",
            "original_final_captured",
            "natural_submission_completed",
            "queries_inactive",
            "queries_original_only",
            "gpu_errors",
        ],
    )?;
    for k in [
        "single_original_dispatch",
        "initial_contents_captured",
        "original_final_captured",
        "natural_submission_completed",
    ] {
        require(closure[k] == true, "compute original closure incomplete")?;
    }
    require(closure["queries_inactive"]==true || (graph_queries&&closure["queries_inactive"]==false&&closure["queries_original_only"]==true), "compute original query closure incomplete")?;
    require(graph_queries||closure.get("queries_original_only").is_none(),"original-only query scope requires schema7")?;
    require(
        num(closure, "unmodelled_commands")? == 0 && num(closure, "gpu_errors")? == 0,
        "compute omissions or GPU errors",
    )?;
    let source = get(m, "source")?;
    shape(source, &["method", "backend", "encoder_epoch"])?;
    require(
        source["backend"] == "Metal"
            && source["method"] == "Gpu::do_dispatch actual final pipeline and resolved bindings"
            && num(source, "encoder_epoch")? > 0
            && m["normal_frame_publication_changed"] == false
            && m["finished_frame_or_density_accepted"] == false
            && m["coherent_stereo_accepted"] == false,
        "compute source scope",
    )?;
    let context = get(m, "context")?;
    shape(
        context,
        &["native_present", "preceding_main_world", "qualification"],
    )?;
    require(num(context,"native_present")?>0&&get(context,"preceding_main_world")?.is_object()
        &&context["qualification"]=="preceding actual main-target draw in this present; not a full dependency or stereo certificate",
        "compute contextual observation absent")?;
    let dispatch = get(m, "dispatch")?;
    shape(
        dispatch,
        &[
            "workgroups",
            "command_sequence",
            "bindings",
            "pipeline",
            "writes",
        ],
    )?;
    require(
        num(dispatch, "command_sequence")? > 0,
        "compute command sequence absent",
    )?;
    let groups: [u32; 3] = decode(get(dispatch, "workgroups")?)?;
    require(
        groups.iter().all(|&n| n > 0 && n <= 65535)
            && groups
                .iter()
                .try_fold(1u64, |a, &n| a.checked_mul(n as u64))
                .is_some_and(|n| n <= 1 << 20),
        "compute workgroup bounds",
    )?;
    let p = get(dispatch, "pipeline")?;
    shape(p, &["effective_shader", "compilation_options"])?;
    require(
        p["compilation_options"] == "wgpu default",
        "compute specialization unsupported",
    )?;
    let registry = get(m, "effective_shader_sources")?;
    require(
        registry["schema"] == 1
            && registry["complete"] == true
            && registry["failure"].is_null()
            && registry["method"] == "successful actual ShaderModule creation",
        "compute exact shader registry missing",
    )?;
    let shader = shader(registry, get(p, "effective_shader")?)?;
    require(
        shader.entry == "main"
            && registry["sources"]
                .as_object()
                .is_some_and(|s| s.len() == 1),
        "compute final module selection",
    )?;
    let mut s = Scene {
        schema: 1,
        provenance: Provenance::Native {
            source_sha256: source_hash,
        },
        eye: None,
        blobs: vec![],
        textures: vec![],
        buffers: vec![],
        pipelines: vec![],
        compute_pipelines: vec![],
        commands: vec![],
        expected: vec![],
        original_submission_completed: true,
        original_gpu_errors: Some(0),
        omitted_commands: 0,
    };
    let mut names = BTreeSet::new();
    let mut allocated = 0u64;
    let mut packed = 0u64;
    let blobs = array(m, "blobs")?;
    require(
        !blobs.is_empty() && blobs.len() <= 16384,
        "compute blob count",
    )?;
    for b in blobs {
        shape(
            b,
            &[
                "path",
                "byte_length",
                "sha256",
                "packed_row_bytes",
                "row_count",
                "readback_pitch",
            ],
        )?;
        let name = txt(b, "path")?;
        let rel = Path::new(name);
        let bytes = num(b, "byte_length")?;
        require(
            !rel.as_os_str().is_empty()
                && !rel.is_absolute()
                && rel.components().all(|c| matches!(c, Component::Normal(_)))
                && names.insert(name.to_owned())
                && !matches!(
                    name,
                    "scene.json" | "native-capture.json" | "native-import.json"
                ),
            "compute unique local blob path",
        )?;
        let hash = txt(b, "sha256")?;
        require(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "compute blob hash syntax",
        )?;
        let row = num(b, "packed_row_bytes")?;
        let rows = num(b, "row_count")?;
        let pitch = num(b, "readback_pitch")?;
        require(
            row > 0 && rows > 0 && pitch >= row && row.checked_mul(rows) == Some(bytes),
            "compute blob row extent",
        )?;
        allocated = allocated
            .checked_add(
                pitch
                    .checked_mul(rows)
                    .ok_or("compute blob pitch overflow")?,
            )
            .ok_or("compute staging overflow")?;
        packed = packed
            .checked_add(bytes)
            .ok_or("compute payload overflow")?;
        require(
            allocated <= MAX_BYTES && packed <= MAX_BYTES,
            "compute aggregate allocation bound",
        )?;
        s.blobs.push(Blob {
            path: name.into(),
            bytes,
            sha256: hash.into(),
        });
    }
    require(
        allocated == num(m, "allocated_readback_bytes")?,
        "compute readback accounting mismatch",
    )?;
    let entries = array(dispatch, "bindings")?;
    require(
        !entries.is_empty() && entries.len() <= 256,
        "compute binding count",
    )?;
    let mut layouts = vec![];
    let mut bindings = vec![];
    let mut slots = BTreeSet::new();
    let mut versions = BTreeMap::new();
    let mut output_indices = BTreeMap::<u32, usize>::new();
    let mut output_parents = BTreeSet::new();
    let mut input_parents = BTreeSet::new();
    for (index, b) in entries.iter().enumerate() {
        let group = u32n(b, "group")?;
        let slot = u32n(b, "binding")?;
        require(
            group == 0 && slot < 256 && slots.insert(slot),
            "compute duplicate or unsupported binding",
        )?;
        let layout: BindGroupLayoutEntry = decode(get(b, "layout")?)?;
        require(
            serde_json::to_value(layout)? == *get(b, "layout")?,
            "unmodelled compute layout fields",
        )?;
        require(
            layout.binding == slot
                && layout.visibility == ShaderStages::COMPUTE
                && layout.count.is_none(),
            "compute exact layout visibility/count",
        )?;
        let value = match txt(b, "kind")? {
            kind @ ("uniform" | "readonly_storage") => {
                shape(
                    b,
                    &[
                        "group",
                        "binding",
                        "kind",
                        "layout",
                        "blob",
                        "size",
                        "source_offset",
                        "native_allocation_id",
                        "content_version",
                        "identity_basis",
                        "version_namespace",
                        "offset",
                        "original_dynamic_offset",
                    ],
                )?;
                let actual = match layout.ty {
                    BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        ..
                    } => "uniform",
                    BindingType::Buffer {
                        ty: BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        ..
                    } => "readonly_storage",
                    _ => return Err("compute writable or dynamic buffer layout unsupported".into()),
                };
                require(b["identity_basis"]=="retained actual wgpu Buffer handle equality"
                    &&b["version_namespace"]=="capture buffer upload revision or immutable selected uniform snapshot",
                    "compute native buffer identity basis")?;
                require(
                    kind == actual
                        && num(b, "offset")? == 0
                        && num(b, "size")? == blob(&s, txt(b, "blob")?)?.bytes
                        && num(b, "size")? > 0
                        && num(b, "size")? % 4 == 0
                        && num(b, "source_offset")? % 4 == 0
                        && num(b, "source_offset")?
                            .checked_add(num(b, "size")?)
                            .is_some(),
                    "compute selected buffer range",
                )?;
                let dynamic = get(b, "original_dynamic_offset")?;
                require(
                    dynamic.is_null()
                        || (kind == "uniform"
                            && dynamic
                                .as_u64()
                                .is_some_and(|n| n <= u32::MAX as u64 && n % 256 == 0)),
                    "compute original uniform offset",
                )?;
                BindingValue::Buffer {
                    buffer: buffer(&mut s, b)?,
                    offset: 0,
                    size: num(b, "size")?,
                }
            }
            "sampler" => {
                shape(b, &["group", "binding", "kind", "layout", "descriptor"])?;
                require(
                    matches!(layout.ty, BindingType::Sampler(_)),
                    "compute sampler layout",
                )?;
                let descriptor: SamplerDescriptor<Option<String>> = decode(get(b, "descriptor")?)?;
                require(
                    serde_json::to_value(&descriptor)? == *get(b, "descriptor")?,
                    "unmodelled sampler fields",
                )?;
                BindingValue::Sampler { descriptor }
            }
            kind @ ("texture" | "storage_texture") => {
                shape(
                    b,
                    &[
                        "group",
                        "binding",
                        "kind",
                        "layout",
                        "native_allocation_id",
                        "view",
                        "descriptor",
                        "subresources",
                        "immutable_native_dummy",
                    ],
                )?;
                let storage = kind == "storage_texture";
                let id = num(b, "native_allocation_id")?;
                let d = descriptor(get(b, "descriptor")?)?;
                shape(
                    get(b, "descriptor")?,
                    &[
                        "width",
                        "height",
                        "depth_or_array_layers",
                        "mip_level_count",
                        "sample_count",
                        "dimension",
                        "format",
                    ],
                )?;
                let view: TextureViewDescriptor<Option<String>> = decode(get(b, "view")?)?;
                require(
                    serde_json::to_value(&view)? == *get(b, "view")?,
                    "unmodelled texture view fields",
                )?;
                require(
                    get(b, "immutable_native_dummy")?.is_boolean(),
                    "compute dummy identity missing",
                )?;
                if storage {
                    require(
                        matches!(layout.ty,BindingType::StorageTexture{access:StorageTextureAccess::WriteOnly,view_dimension:TextureViewDimension::D2,format} if format==view.format.unwrap_or(d.format))
                            && d.dimension == TextureDimension::D2
                            && view.dimension == Some(TextureViewDimension::D2)
                            && view.mip_level_count == Some(1)
                            && view.array_layer_count == Some(1)
                            && !d.format.is_depth_stencil_format()
                            && d.format.block_dimensions() == (1, 1)
                            && b["immutable_native_dummy"] == false
                            && output_parents.insert(id),
                        "compute output scope/parent alias",
                    )?;
                    output_indices.insert(slot, index);
                } else {
                    require(
                        matches!(layout.ty,BindingType::Texture{multisampled:false,view_dimension,..} if Some(view_dimension)==view.dimension),
                        "compute sampled texture layout",
                    )?;
                    input_parents.insert(id);
                }
                let i = add_texture(&mut s, id, d, storage)?;
                if let Some(f) = view.format {
                    if f != s.textures[i].descriptor.format
                        && !s.textures[i].descriptor.view_formats.contains(&f)
                    {
                        s.textures[i].descriptor.view_formats.push(f);
                    }
                }
                let expected = view_slices(&s.textures[i].descriptor, &view)?;
                let mut got = BTreeSet::new();
                let mut selected = vec![];
                for sub in array(b, "subresources")? {
                    shape(
                        sub,
                        &[
                            "mip_level",
                            "layer",
                            "aspect",
                            "blob",
                            "packed_row_bytes",
                            "width",
                            "height",
                            "depth",
                            "version",
                        ],
                    )?;
                    let v = version(get(sub, "version")?)?;
                    let aspect: Aspect = decode(get(sub, "aspect")?)?;
                    require(
                        v.slice.resource == texture_id(id)?
                            && v.slice.mip == u32n(sub, "mip_level")?
                            && v.slice.layer == u32n(sub, "layer")?
                            && v.slice.aspect == aspect
                            && got.insert((v.slice.mip, v.slice.layer, aspect)),
                        "compute subresource identity/duplicate",
                    )?;
                    version_identity(&mut versions, &v)?;
                    let (w, h, row, rows) = crate::validate::slice_bytes(
                        &s.textures[i].descriptor,
                        v.slice.mip,
                        &v.slice.aspect,
                    )?;
                    require(
                        u32n(sub, "width")? == w
                            && u32n(sub, "height")? == h
                            && u32n(sub, "packed_row_bytes")? == row
                            && u32n(sub, "depth")?
                                == s.textures[i].descriptor.mip_depth(v.slice.mip)
                            && blob(&s, txt(sub, "blob")?)?.bytes == row as u64 * rows as u64,
                        "compute selected texture byte extent",
                    )?;
                    initial(&mut s, i, &v, txt(sub, "blob")?)?;
                    selected.push(v);
                }
                require(
                    got == expected,
                    "compute selected view has omitted/excess versions",
                )?;
                BindingValue::Texture {
                    texture: texture_id(id)?,
                    view,
                    versions: selected,
                }
            }
            _ => return Err("unsupported compute binding kind".into()),
        };
        layouts.push(layout);
        bindings.push(Binding {
            group,
            binding: slot,
            value,
        });
    }
    require(
        !output_indices.is_empty()
            && output_indices.len() <= 8
            && input_parents.is_disjoint(&output_parents),
        "compute output count/sampled alias",
    )?;
    let attachments = array(m, "attachments")?;
    let changes = array(dispatch, "writes")?;
    require(
        attachments.len() == output_indices.len() && changes.len() == attachments.len(),
        "compute omitted output/transition",
    )?;
    let mut attachment_slots = BTreeSet::new();
    let mut writes = vec![];
    for a in attachments {
        shape(
            a,
            &[
                "role",
                "binding",
                "native_allocation_id",
                "format",
                "descriptor",
                "view",
                "width",
                "height",
                "mip",
                "slice",
                "initial_blob",
                "initial_version",
                "final_blob",
                "final_version",
            ],
        )?;
        let slot = u32n(a, "binding")?;
        require(attachment_slots.insert(slot), "compute duplicate output")?;
        let b = &entries[*output_indices
            .get(&slot)
            .ok_or("compute output not bound")?];
        let subs = array(b, "subresources")?;
        require(subs.len() == 1, "compute output requires one subresource")?;
        let sub = &subs[0];
        let before = version(get(a, "initial_version")?)?;
        let after = version(get(a, "final_version")?)?;
        require(
            a["role"] == format!("storage-b{slot}")
                && a["native_allocation_id"] == b["native_allocation_id"]
                && a["descriptor"] == b["descriptor"]
                && a["view"] == b["view"]
                && a["format"] == b["descriptor"]["format"]
                && a["width"] == sub["width"]
                && a["height"] == sub["height"]
                && a["mip"] == sub["mip_level"]
                && a["slice"] == sub["layer"]
                && a["initial_blob"] == sub["blob"]
                && a["initial_version"] == sub["version"]
                && before.slice == after.slice
                && before.slice.aspect == Aspect::Color
                && after.content_version > before.content_version,
            "compute attachment differs from actual storage resource",
        )?;
        require(
            !versions.contains_key(&after.content_version),
            "compute final version reuses existing version",
        )?;
        version_identity(&mut versions, &after)?;
        let matches = changes
            .iter()
            .filter(|c| c["before"] == a["initial_version"] && c["after"] == a["final_version"])
            .count();
        require(matches == 1, "compute final transition mismatch/duplicate")?;
        let final_blob = txt(a, "final_blob")?;
        require(
            blob(&s, final_blob)?.bytes == blob(&s, txt(a, "initial_blob")?)?.bytes,
            "compute final output byte extent",
        )?;
        writes.push(Transition {
            slice: before.slice,
            before: before.content_version,
            after: after.content_version,
        });
        s.expected.push(Expected {
            version: after,
            blob: final_blob.into(),
        });
    }
    for c in changes {
        shape(c, &["before", "after"])?;
    }
    let mut used = BTreeSet::new();
    for t in &s.textures {
        for init in &t.initial {
            if let Initial::Captured { blob } = &init.origin {
                used.insert(blob.clone());
            }
        }
    }
    for b in &s.buffers {
        used.insert(b.blob.clone());
    }
    for e in &s.expected {
        used.insert(e.blob.clone());
    }
    require(
        used == names,
        "compute payload includes unmodelled or unused data",
    )?;
    let id = "native-compute:0".to_owned();
    s.compute_pipelines.push(ComputePipeline {
        id: id.clone(),
        shader,
        layouts: vec![layouts],
    });
    s.commands.push(Command::Dispatch {
        dispatch: Dispatch {
            eye: None,
            pipeline: id,
            bindings,
            workgroups: groups,
            writes,
        },
    });
    Ok(s)
}

// Duplicate JSON keys otherwise silently overwrite earlier admission evidence.
struct UniqueValue(Value);
impl<'de> serde::Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<UniqueValue, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<UniqueValue, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<UniqueValue, E> {
                self.visit_unit()
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<UniqueValue, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element::<UniqueValue>()? {
                    out.push(v.0);
                }
                Ok(UniqueValue(Value::Array(out)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<UniqueValue, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = a.next_entry::<String, UniqueValue>()? {
                    if out.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(UniqueValue(Value::Object(out)))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn native(path: &Path, out: &Path) -> Result<()> {
    require(
        fs::metadata(path)?.len() <= 16 << 20,
        "compute manifest bound",
    )?;
    let raw = fs::read(path)?;
    require(raw.len() <= 16 << 20, "compute manifest bound")?;
    let manifest: Value = serde_json::from_slice::<UniqueValue>(&raw)?.0;
    let scene = convert(&manifest, sha(&raw))?;
    let root = path
        .parent()
        .ok_or("compute manifest parent")?
        .canonicalize()?;
    let mut payload = vec![];
    for b in &scene.blobs {
        let p = root.join(&b.path).canonicalize()?;
        require(p.starts_with(&root), "compute blob escaped capture")?;
        require(
            fs::metadata(&p)?.len() == b.bytes,
            "compute blob pre-read length",
        )?;
        let bytes = fs::read(&p)?;
        require(
            bytes.len() as u64 == b.bytes && sha(&bytes) == b.sha256,
            "compute blob length/hash mismatch",
        )?;
        payload.push((b.path.clone(), bytes));
    }
    fs::create_dir(out)?;
    for (name, bytes) in payload {
        let p = out.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(p, bytes)?;
    }
    let scene_bytes = serde_json::to_vec_pretty(&scene)?;
    fs::write(out.join("native-capture.json"), &raw)?;
    fs::write(out.join("scene.json"), &scene_bytes)?;
    Input::load(&out.join("scene.json"))?;
    fs::write(
        out.join("native-import.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
        "schema":1,"adapter":"schema5 single native compute dispatch v1","capture_sha256":sha(&raw),"scene_sha256":sha(&scene_bytes),
        "native_identifiers_preserved":true,"preceding_world_is_context_only":true,"eye_ownership_assigned":false,
        "finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false}))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn reference(id: u64, n: u64, mip: u32) -> Value {
        json!({"allocation_id":id,"content_version":n,"subresource":{"mip":mip,"layer":0,"aspect":"color"}})
    }
    fn fixture() -> (Value, BTreeMap<String, Vec<u8>>) {
        let text="@group(0) @binding(0) var s:texture_2d<f32>; @group(0) @binding(1) var o:texture_storage_2d<rgba32float,write>; @compute @workgroup_size(1) fn main(@builtin(global_invocation_id)p:vec3u){if(p.x<2u&&p.y<2u){textureStore(o,vec2i(p.xy),textureLoad(s,vec2i(p.xy),0));}}";
        let hash = sha(text.as_bytes());
        let d = json!({"width":4,"height":4,"depth_or_array_layers":1,"mip_level_count":2,"sample_count":1,"dimension":"2d","format":"rgba32float"});
        let view = TextureViewDescriptor::<Option<String>> {
            dimension: Some(TextureViewDimension::D2),
            base_mip_level: 1,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(1),
            ..Default::default()
        };
        let payload = BTreeMap::from([
            ("input.bin".into(), vec![0u8; 64]),
            ("initial.bin".into(), vec![1u8; 64]),
            ("final.bin".into(), vec![0u8; 64]),
        ]);
        let bindings=[(0,100,10,"texture","input.bin",BindingType::Texture{sample_type:TextureSampleType::Float{filterable:false},view_dimension:TextureViewDimension::D2,multisampled:false}),
            (1,200,20,"storage_texture","initial.bin",BindingType::StorageTexture{access:StorageTextureAccess::WriteOnly,format:TextureFormat::Rgba32Float,view_dimension:TextureViewDimension::D2})]
            .into_iter().map(|(binding,id,n,kind,name,ty)|json!({"group":0,"binding":binding,"kind":kind,"layout":BindGroupLayoutEntry{binding,visibility:ShaderStages::COMPUTE,ty,count:None},"native_allocation_id":id,"descriptor":d,"view":view,"immutable_native_dummy":false,
                "subresources":[{"mip_level":1,"layer":0,"aspect":"color","blob":name,"packed_row_bytes":32,"width":2,"height":2,"depth":1,"version":reference(id,n,1)}]})).collect::<Vec<_>>();
        let blobs=payload.iter().map(|(path,bytes)|json!({"path":path,"byte_length":bytes.len(),"sha256":sha(bytes),"packed_row_bytes":32,"row_count":2,"readback_pitch":256})).collect::<Vec<_>>();
        (
            json!({"schema":5,"kind":"native_compute_dispatch","status":"captured","source":{"method":"Gpu::do_dispatch actual final pipeline and resolved bindings","backend":"Metal","encoder_epoch":1},
            "context":{"native_present":1,"preceding_main_world":{},"qualification":"preceding actual main-target draw in this present; not a full dependency or stereo certificate"},
            "dispatch":{"workgroups":[2,2,1],"command_sequence":100,"bindings":bindings,"pipeline":{"effective_shader":{"status":"exact","sha256":hash,"entry_point":"main","compilation_options":"wgpu default"},"compilation_options":"wgpu default"},"writes":[{"before":reference(200,20,1),"after":reference(200,30,1)}]},
            "attachments":[{"role":"storage-b1","binding":1,"native_allocation_id":200,"format":"rgba32float","descriptor":d,"view":view,"width":2,"height":2,"mip":1,"slice":0,"initial_blob":"initial.bin","final_blob":"final.bin","initial_version":reference(200,20,1),"final_version":reference(200,30,1)}],
            "blobs":blobs,"allocated_readback_bytes":1536,"closure":{"single_original_dispatch":true,"unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0},
            "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false,
            "effective_shader_sources":{"schema":1,"method":"successful actual ShaderModule creation","complete":true,"failure":null,"sources":{hash:{"wgsl":text}}}}),
            payload,
        )
    }
    #[test]
    fn original_query_events_require_finished_graph_conversion() -> Result<()> {
        let(mut m,_)=fixture();m["closure"]["queries_inactive"]=false.into();m["closure"]["queries_original_only"]=true.into();
        assert!(convert(&m,"a".repeat(64)).is_err());
        assert!(convert_graph_qualified(&m,"a".repeat(64))?.eye.is_none());
        m["closure"]["queries_original_only"]=false.into();assert!(convert_graph_qualified(&m,"a".repeat(64)).is_err());
        Ok(())
    }
    #[test]
    fn compute_import_keeps_actual_mip_versions_and_never_assigns_context_eye() -> Result<()> {
        let (m, _) = fixture();
        let s = convert(&m, "raw-source-hash".into())?;
        assert!(s.eye.is_none());
        assert_eq!(s.compute_pipelines.len(), 1);
        assert_eq!(s.commands.len(), 1);
        let Command::Dispatch { dispatch } = &s.commands[0] else {
            panic!("not dispatch")
        };
        assert!(dispatch.eye.is_none());
        assert_eq!(dispatch.workgroups, [2, 2, 1]);
        assert_eq!(dispatch.writes[0].slice.mip, 1);
        assert_eq!(dispatch.writes[0].before, 20);
        assert_eq!(dispatch.writes[0].after, 30);
        assert!(matches!(
            s.textures[1].initial[0].origin,
            Initial::Unknown { .. }
        ));
        assert!(matches!(
            s.textures[1].initial[1].origin,
            Initial::Captured { .. }
        ));
        assert_eq!(s.textures[1].native_allocation_id, Some(200));
        assert_eq!(s.expected.len(), 1);
        Ok(())
    }
    #[test]
    fn compute_import_rejects_incomplete_identity_alias_and_unsupported_state() {
        let (base, _) = fixture();
        let mutations: Vec<Box<dyn Fn(&mut Value)>> = vec![
            Box::new(|m| m["closure"]["natural_submission_completed"] = json!(false)),
            Box::new(|m| m["closure"]["unmodelled_commands"] = json!(1)),
            Box::new(|m| m["closure"]["gpu_errors"] = json!(true)),
            Box::new(|m| m["dispatch"]["workgroups"] = json!([65535, 65535, 65535])),
            Box::new(|m| m["dispatch"]["workgroups"] = json!([0, 1, 1])),
            Box::new(|m| m["dispatch"]["writes"] = json!([])),
            Box::new(|m| m["dispatch"]["indirect_buffer"] = json!("unmodelled")),
            Box::new(|m| m["attachments"][0]["final_version"] = reference(200, 20, 1)),
            Box::new(|m| m["attachments"][0]["view"]["base_mip_level"] = json!(0)),
            Box::new(|m| m["attachments"][0]["native_allocation_id"] = json!(100)),
            Box::new(|m| {
                m["dispatch"]["bindings"][0]["subresources"][0]["packed_row_bytes"] = json!(16)
            }),
            Box::new(|m| m["dispatch"]["bindings"][0]["subresources"][0]["depth"] = json!(2)),
            Box::new(|m| {
                m["dispatch"]["bindings"][0]["subresources"][0]["version"] = reference(100, 20, 1)
            }),
            Box::new(|m| m["dispatch"]["bindings"][0]["subresources"] = json!([])),
            Box::new(|m| m["dispatch"]["bindings"][1]["immutable_native_dummy"] = json!(true)),
            Box::new(|m| m["dispatch"]["bindings"][0]["binding"] = json!(1)),
            Box::new(|m| m["dispatch"]["bindings"][0]["group"] = json!(1)),
            Box::new(|m| {
                m["dispatch"]["pipeline"]["effective_shader"]["status"] = json!("unresolved")
            }),
            Box::new(|m| {
                m["dispatch"]["pipeline"]["effective_shader"]["entry_point"] = json!("other")
            }),
            Box::new(|m| m["effective_shader_sources"]["complete"] = json!(false)),
            Box::new(|m| m["blobs"][0]["path"] = json!("../input.bin")),
            Box::new(|m| m["allocated_readback_bytes"] = json!(1535)),
            Box::new(|m| m["coherent_stereo_accepted"] = json!(true)),
        ];
        for (n, mutate) in mutations.into_iter().enumerate() {
            let mut m = base.clone();
            mutate(&mut m);
            assert!(convert(&m, "hash".into()).is_err(), "mutation {n} admitted");
        }
        let mut alias = base.clone();
        alias["dispatch"]["bindings"][0]["native_allocation_id"] = json!(200);
        alias["dispatch"]["bindings"][0]["subresources"][0]["version"] = reference(200, 20, 1);
        alias["dispatch"]["bindings"][0]["subresources"][0]["blob"] = json!("initial.bin");
        assert!(convert(&alias, "hash".into()).is_err());
    }
    #[test]
    fn compute_readonly_buffer_keeps_selected_range_and_rejects_writes() -> Result<()> {
        let (mut m, _) = fixture();
        let bytes = vec![7u8; 32];
        let layout = BindGroupLayoutEntry {
            binding: 2,
            visibility: ShaderStages::COMPUTE,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let entry = json!({"group":0,"binding":2,"kind":"readonly_storage","layout":layout,"blob":"readonly.bin",
            "native_allocation_id":7,"content_version":1,"source_offset":256,"offset":0,"size":32,"original_dynamic_offset":null,
            "identity_basis":"retained actual wgpu Buffer handle equality","version_namespace":"capture buffer upload revision or immutable selected uniform snapshot"});
        m["dispatch"]["bindings"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        m["blobs"].as_array_mut().unwrap().push(json!({"path":"readonly.bin","byte_length":32,"sha256":sha(&bytes),"packed_row_bytes":32,"row_count":1,"readback_pitch":32}));
        m["allocated_readback_bytes"] = json!(1568);
        let s = convert(&m, "hash".into())?;
        assert_eq!(s.buffers[0].native_allocation_id, Some(7));
        assert_eq!(s.buffers[0].source_offset, 256);
        let Command::Dispatch { dispatch } = &s.commands[0] else {
            panic!("dispatch")
        };
        assert!(matches!(
            &dispatch.bindings[2].value,
            BindingValue::Buffer {
                offset: 0,
                size: 32,
                ..
            }
        ));
        for (key, value) in [
            ("source_offset", json!(2)),
            ("source_offset", json!(u64::MAX - 3)),
            ("offset", json!(256)),
            ("size", json!(16)),
            ("original_dynamic_offset", json!(256)),
            ("kind", json!("uniform")),
            ("identity_basis", json!("guest ID")),
        ] {
            let mut bad = m.clone();
            bad["dispatch"]["bindings"][2][key] = value;
            assert!(convert(&bad, "hash".into()).is_err(), "{key}");
        }
        let mut bad = m.clone();
        let mut writable = layout;
        writable.ty = BindingType::Buffer {
            ty: BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        bad["dispatch"]["bindings"][2]["layout"] = json!(writable);
        assert!(convert(&bad, "hash".into()).is_err());
        let mut bad = m.clone();
        bad["dispatch"]["bindings"][2]["layout"]["ignored_binding_flags"] = json!(1);
        assert!(convert(&bad, "hash".into()).is_err());
        Ok(())
    }
    #[test]
    fn duplicate_json_keys_are_rejected_at_every_depth() {
        for raw in [r#"{"schema":5,"schema":4}"#, r#"{"a":[{"x":1,"x":2}]}"#] {
            assert!(serde_json::from_str::<UniqueValue>(raw).is_err());
        }
        assert!(serde_json::from_str::<UniqueValue>(r#"{"a":[1,1.5,null,true,"text"]}"#).is_ok());
    }
    #[test]
    fn native_compute_disk_admission_checks_raw_hash_and_payload() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "compute-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&root)?;
        let (m, payload) = fixture();
        let raw = serde_json::to_vec(&m)?;
        fs::write(root.join("capture.json"), &raw)?;
        for (p, b) in payload {
            fs::write(root.join(p), b)?;
        }
        let output = root.join("accepted");
        let result = native(&root.join("capture.json"), &output);
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        let i = Input::load(&output.join("scene.json"))?;
        assert!(
            matches!(i.scene.provenance,Provenance::Native{ref source_sha256} if *source_sha256==sha(&raw))
        );
        assert!(!i.targets_have_resolution_independent_initialization);
        assert_eq!(fs::read(output.join("native-capture.json"))?, raw);
        // The guarded native runner admits this shared receipt contract before
        // GPU execution; a compute-specific filename cannot satisfy it.
        let receipt: Value = serde_json::from_slice(&fs::read(output.join("native-import.json"))?)?;
        assert_eq!(receipt["capture_sha256"], sha(&raw));
        assert_eq!(
            receipt["scene_sha256"],
            sha(&fs::read(output.join("scene.json"))?)
        );
        assert_eq!(receipt["native_identifiers_preserved"], true);
        assert_eq!(receipt["eye_ownership_assigned"], false);
        assert!(!output.join("native-compute-import.json").exists());
        fs::write(root.join("input.bin"), [5u8; 64])?;
        assert!(native(&root.join("capture.json"), &root.join("corrupt")).is_err());
        assert!(!root.join("corrupt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}

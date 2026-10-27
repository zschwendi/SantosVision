//! Strict schema-4 adapter. Native allocation/version evidence is never synthesized.
use crate::{
    model::*,
    require, sha,
    validate::{aspects, Input, MAX_BYTES},
    Result,
};
use serde_json::Value;
use std::{
    borrow::Cow,
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
fn world(v: &Value) -> Result<EyeIdentity> {
    for key in ["game_from_eye", "game_from_stage", "projection_from_eye"] {
        let a = array(v, key)?;
        require(
            a.len() == 16
                && a.iter().all(|x| {
                    x.as_f64()
                        .is_some_and(|n| n.is_finite() && (n as f32).is_finite())
                }),
            "native world matrix",
        )?;
    }
    require(
        v["headset_applied"] == true
            && v["view_count"] == 2
            && get(v, "depth_reverse_z")?.is_boolean()
            && get(v, "optics")?.is_object(),
        "native full world attribution",
    )?;
    require(
        get(v, "presentation_time_seconds")?
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.),
        "native presentation time",
    )?;
    let optics = get(v, "optics")?;
    let _scope = num(optics, "scope_sequence")?;
    for key in [
        "magnification",
        "reference_fov_degrees",
        "actual_fov_degrees",
    ] {
        require(
            get(optics, key)?
                .as_f64()
                .is_some_and(|n| n.is_finite() && (n as f32).is_finite()),
            "native optical scalar missing",
        )?;
    }
    let e = EyeIdentity {
        epoch: num(v, "provenance_epoch")?,
        camera: num(v, "camera_sequence")?,
        request: num(v, "request_frame")?,
        head: num(v, "head_pose_frame")?,
        eye: u32n(v, "eye_index")?,
    };
    require(
        e.epoch > 0 && e.camera > 0 && e.request > 0 && e.head > 0 && e.eye < 2,
        "native eye identity",
    )?;
    Ok(e)
}

// References only compact the manifest; every semantic record remains exact.
fn expand_metadata(m: &Value) -> Result<Cow<'_, Value>> {
    let draws = array(m, "draws")?;
    require(draws.len() <= 4096, "shared draw count bound")?;
    let Some(encoding) = m.get("draw_metadata_encoding") else {
        require(
            m.get("shared_draw_metadata").is_none(),
            "shared metadata has no encoding",
        )?;
        for d in draws {
            require(
                [
                    "pipeline_ref",
                    "binding_refs",
                    "world_view_ref",
                    "target_domain_ref",
                ]
                .iter()
                .all(|k| d.get(*k).is_none()),
                "references have no encoding",
            )?;
        }
        return Ok(Cow::Borrowed(m));
    };
    require(
        encoding.as_str() == Some("shared_v1"),
        "unknown draw metadata encoding",
    )?;
    let table = get(m, "shared_draw_metadata")?
        .as_object()
        .ok_or("shared metadata object required")?;
    require(
        table.len() == 4
            && ["pipelines", "bindings", "world_views", "target_domains"]
                .iter()
                .all(|k| table.contains_key(*k)),
        "shared metadata table shape",
    )?;
    let pipelines = table["pipelines"]
        .as_array()
        .ok_or("shared pipeline table array required")?;
    let bindings = table["bindings"]
        .as_array()
        .ok_or("shared binding table array required")?;
    let worlds = table["world_views"]
        .as_array()
        .ok_or("shared world table array required")?;
    let domains = table["target_domains"]
        .as_array()
        .ok_or("shared target domain table array required")?;
    require(
        pipelines.len() <= 4096
            && bindings.len() <= 65536
            && worlds.len() <= 4096
            && domains.len() <= 4096,
        "shared metadata table count bound",
    )?;
    let sizes = |values: &[Value]| -> Result<Vec<usize>> {
        values
            .iter()
            .map(|v| {
                require(v.is_object(), "shared metadata record object required")?;
                Ok(serde_json::to_vec(v)?.len())
            })
            .collect()
    };
    let pipeline_bytes = sizes(pipelines)?;
    let binding_bytes = sizes(bindings)?;
    let world_bytes = sizes(worlds)?;
    let domain_bytes = sizes(domains)?;
    let mut expanded_bytes = serde_json::to_vec(m)?.len();
    require(expanded_bytes <= 32 << 20, "shared raw metadata bound")?;
    let mut references = 0usize;
    let mut selected = Vec::with_capacity(draws.len());
    for d in draws {
        let object = d.as_object().ok_or("shared draw object required")?;
        require(
            ["pipeline", "bindings", "world_view", "target_domain"]
                .iter()
                .all(|k| !object.contains_key(*k)),
            "inline and shared draw metadata coexist",
        )?;
        let p: usize = num(d, "pipeline_ref")?.try_into()?;
        let w: usize = num(d, "world_view_ref")?.try_into()?;
        let t: usize = num(d, "target_domain_ref")?.try_into()?;
        let refs = array(d, "binding_refs")?;
        require(
            refs.len() <= 256 && p < pipelines.len() && w < worlds.len() && t < domains.len(),
            "shared draw reference bound",
        )?;
        references = references
            .checked_add(refs.len())
            .ok_or("shared reference overflow")?;
        require(references <= 262144, "expanded binding count bound")?;
        expanded_bytes = expanded_bytes
            .checked_add(pipeline_bytes[p])
            .and_then(|n| n.checked_add(world_bytes[w]))
            .and_then(|n| n.checked_add(domain_bytes[t]))
            .ok_or("expanded metadata overflow")?;
        let mut indices = Vec::with_capacity(refs.len());
        for r in refs {
            let b: usize = r
                .as_u64()
                .ok_or("binding reference must be nonnegative integer")?
                .try_into()?;
            require(b < bindings.len(), "binding reference outside table")?;
            expanded_bytes = expanded_bytes
                .checked_add(binding_bytes[b])
                .ok_or("expanded metadata overflow")?;
            indices.push(b);
        }
        require(expanded_bytes <= 128 << 20, "expanded metadata byte bound")?;
        selected.push((p, w, t, indices));
    }
    // All sizes/indices pass before any repeated records are expanded.
    let mut expanded = m.clone();
    let top = expanded
        .as_object_mut()
        .ok_or("native manifest object required")?;
    top.remove("draw_metadata_encoding");
    top.remove("shared_draw_metadata");
    let output_draws = top.get_mut("draws").unwrap().as_array_mut().unwrap();
    for (draw, (p, w, t, refs)) in output_draws.iter_mut().zip(selected) {
        let object = draw.as_object_mut().unwrap();
        object.remove("pipeline_ref");
        object.remove("binding_refs");
        object.remove("world_view_ref");
        object.remove("target_domain_ref");
        object.insert("world_view".into(), worlds[w].clone());
        object.insert("target_domain".into(), domains[t].clone());
        object.insert("pipeline".into(), pipelines[p].clone());
        object.insert(
            "bindings".into(),
            Value::Array(refs.into_iter().map(|b| bindings[b].clone()).collect()),
        );
    }
    Ok(Cow::Owned(expanded))
}

fn convert(m: &Value, source_hash: String) -> Result<Scene> {
    let normalized = expand_metadata(m)?;
    let m = normalized.as_ref();
    require(
        m["schema"] == 4 && m["kind"] == "native_target_span" && m["status"] == "captured",
        "requires schema4 native target interval",
    )?;
    let c = get(m, "closure")?;
    for key in [
        "complete_target_interval",
        "first_executed_draw_captured",
        "last_executed_draw_captured",
        "initial_contents_captured",
        "original_final_captured",
        "natural_submission_completed",
        "queries_inactive",
    ] {
        require(c[key] == true, &format!("native closure missing {key}"))?;
    }
    require(
        c["unmodelled_commands"] == 0 && c["gpu_errors"] == 0,
        "native interval omissions/errors",
    )?;
    let start = num(c, "interval_start_command")?;
    let end = num(c, "interval_end_command")?;
    require(start > 0 && end > start, "native interval command order")?;
    require(
        m["source"]["backend"] == "Metal"
            && m["source"]["method"] == "Gpu::do_draw resolved bindings"
            && num(get(m, "source")?, "encoder_epoch")? > 0
            && m["normal_frame_publication_changed"] == false,
        "native source contract",
    )?;
    let registry = get(m, "effective_shader_sources")?;
    require(
        registry["complete"] == true,
        "final shader registry incomplete",
    )?;
    let w = get(m, "world_view")?;
    let eye = world(w)?;
    let domain = get(m, "target_domain")?;
    let domain_attachments = array(domain, "attachments")?;
    let mut s = Scene {
        schema: 1,
        provenance: Provenance::Native {
            source_sha256: source_hash,
        },
        eye: Some(eye),
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
    for b in array(m, "blobs")? {
        s.blobs.push(Blob {
            path: txt(b, "path")?.into(),
            bytes: num(b, "byte_length")?,
            sha256: txt(b, "sha256")?.into(),
        });
    }
    let mut attachments = BTreeMap::new();
    let mut finals = vec![];
    for a in array(m, "attachments")? {
        let role = txt(a, "role")?;
        require(
            matches!(
                role,
                "color0"
                    | "color1"
                    | "color2"
                    | "color3"
                    | "color4"
                    | "color5"
                    | "color6"
                    | "color7"
                    | "depth"
                    | "stencil"
            ),
            "native attachment role",
        )?;
        let allocation = num(a, "native_allocation_id")?;
        let format = decode(get(a, "format")?)?;
        let mip = u32n(a, "mip")?;
        let layer = u32n(a, "slice")?;
        // The selected target view does not imply a one-mip/one-layer allocation.
        let d = descriptor(get(a, "descriptor")?)?;
        require(
            d.dimension == TextureDimension::D2
                && mip < d.mips
                && layer < d.layers
                && u32n(a, "width")? == (d.width >> mip).max(1)
                && u32n(a, "height")? == (d.height >> mip).max(1),
            "native target extent differs",
        )?;
        let i = add_texture(&mut s, allocation, d, true)?;
        if format != s.textures[i].descriptor.format
            && !s.textures[i].descriptor.view_formats.contains(&format)
        {
            s.textures[i].descriptor.view_formats.push(format);
        }
        let before = version(get(a, "initial_version")?)?;
        let after = version(get(a, "final_version")?)?;
        let aspect = match role {
            "depth" => Aspect::Depth,
            "stencil" => Aspect::Stencil,
            _ => Aspect::Color,
        };
        require(
            before.slice == after.slice
                && before.slice.resource == texture_id(allocation)?
                && before.slice.mip == mip
                && before.slice.layer == layer
                && before.slice.aspect == aspect,
            "native target version identity",
        )?;
        let domain_role = if matches!(role, "depth" | "stencil") {
            "depth_stencil"
        } else {
            role
        };
        let original = domain_attachments
            .iter()
            .find(|a| a["role"] == domain_role)
            .ok_or("native target missing domain")?;
        require(
            num(original, "native_allocation_id")? == allocation
                && u32n(original, "mip")? == mip
                && u32n(original, "slice")? == layer
                && get(original, "format")? == get(a, "format")?,
            "native target domain disagrees",
        )?;
        initial(&mut s, i, &before, txt(a, "initial_blob")?)?;
        require(
            attachments
                .insert(
                    role.to_string(),
                    Attachment {
                        resource: texture_id(allocation)?,
                        mip,
                        layer,
                        view_format: format,
                    },
                )
                .is_none(),
            "native duplicate target aspect",
        )?;
        finals.push(Expected {
            version: after,
            blob: txt(a, "final_blob")?.into(),
        });
    }
    require(!attachments.is_empty(), "empty native target interval")?;
    let mut expected_roles = BTreeSet::new();
    let mut domain_roles = BTreeSet::new();
    for target in domain_attachments {
        require(
            domain_roles.insert(txt(target, "role")?),
            "duplicate native domain role",
        )?;
        match txt(target, "role")? {
            "depth_stencil" => {
                let f: TextureFormat = decode(get(target, "format")?)?;
                require(
                    matches!(
                        f,
                        TextureFormat::Depth32Float | TextureFormat::Depth32FloatStencil8
                    ),
                    "native domain depth format",
                )?;
                expected_roles.insert("depth".to_string());
                if f == TextureFormat::Depth32FloatStencil8 {
                    expected_roles.insert("stencil".to_string());
                }
            }
            role if matches!(
                role,
                "color0"
                    | "color1"
                    | "color2"
                    | "color3"
                    | "color4"
                    | "color5"
                    | "color6"
                    | "color7"
            ) =>
            {
                require(
                    expected_roles.insert(role.to_string()),
                    "duplicate native domain role",
                )?;
            }
            _ => return Err("native domain role unsupported".into()),
        }
    }
    require(
        expected_roles == attachments.keys().cloned().collect(),
        "native final/initial aspects omit bound target",
    )?;

    let draws = array(m, "draws")?;
    require(
        !draws.is_empty() && draws.len() <= 4096,
        "native draw bound",
    )?;
    let mut sequence = start;
    let mut pipeline_cache = BTreeMap::<String, usize>::new();
    for d in draws {
        let seq = num(d, "command_sequence")?;
        require(
            seq > sequence && seq < end,
            "native draw sequence outside interval",
        )?;
        sequence = seq;
        require(
            get(d, "world_view")? == w && get(d, "target_domain")? == domain,
            "native target/eye/ray identity changed",
        )?;
        let p = get(d, "pipeline")?;
        let refs = get(p, "effective_shaders")?;
        let vs = shader(registry, get(refs, "vertex")?)?;
        let ps = match refs.get("fragment") {
            Some(v) if !v.is_null() => Some(shader(registry, v)?),
            _ => None,
        };
        let mut layouts: Vec<Vec<BindGroupLayoutEntry>> = vec![];
        let mut bindings = vec![];
        for b in array(d, "bindings")? {
            let group = u32n(b, "group")?;
            let slot = u32n(b, "binding")?;
            require(group < 4, "native bind group bound")?;
            while layouts.len() <= group as usize {
                layouts.push(vec![])
            }
            let layout: BindGroupLayoutEntry = decode(get(b, "layout")?)?;
            require(layout.binding == slot, "native layout binding differs")?;
            layouts[group as usize].push(layout);
            let value = match txt(b, "kind")? {
                "uniform" | "buffer" | "readonly_storage" => BindingValue::Buffer {
                    buffer: buffer(&mut s, b)?,
                    offset: num(b, "offset")?,
                    size: num(b, "size")?,
                },
                "sampler" => BindingValue::Sampler {
                    descriptor: decode(get(b, "descriptor")?)?,
                },
                "texture" => {
                    let allocation = num(b, "native_allocation_id")?;
                    let i = add_texture(
                        &mut s,
                        allocation,
                        descriptor(get(b, "descriptor")?)?,
                        false,
                    )?;
                    let view: TextureViewDescriptor<Option<String>> = decode(get(b, "view")?)?;
                    if let Some(f) = view.format {
                        if f != s.textures[i].descriptor.format
                            && !s.textures[i].descriptor.view_formats.contains(&f)
                        {
                            s.textures[i].descriptor.view_formats.push(f);
                        }
                    }
                    let mut versions = vec![];
                    for sub in array(b, "subresources")? {
                        let v = version(get(sub, "version")?)?;
                        let aspect: Aspect = decode(get(sub, "aspect")?)?;
                        require(
                            v.slice.resource == texture_id(allocation)?
                                && v.slice.mip == u32n(sub, "mip_level")?
                                && v.slice.layer == u32n(sub, "layer")?
                                && v.slice.aspect == aspect,
                            "native sampled version/subresource mismatch",
                        )?;
                        let (_, _, row, _) = crate::validate::slice_bytes(
                            &s.textures[i].descriptor,
                            v.slice.mip,
                            &v.slice.aspect,
                        )?;
                        require(
                            u32n(sub, "packed_row_bytes")? == row,
                            "native sampled row extent",
                        )?;
                        let depth = sub.get("depth").and_then(Value::as_u64);
                        require(
                            depth == Some(s.textures[i].descriptor.mip_depth(v.slice.mip) as u64)
                                || (depth.is_none()
                                    && s.textures[i].descriptor.dimension != TextureDimension::D3),
                            "native sampled volume extent",
                        )?;
                        initial(&mut s, i, &v, txt(sub, "blob")?)?;
                        versions.push(v);
                    }
                    BindingValue::Texture {
                        texture: texture_id(allocation)?,
                        view,
                        versions,
                    }
                }
                _ => return Err("native binding kind unsupported".into()),
            };
            bindings.push(Binding {
                group,
                binding: slot,
                value,
            });
        }
        let mut pipeline = Pipeline {
            id: String::new(),
            vertex: vs,
            fragment: ps,
            layouts,
            vertex_layouts: decode(get(p, "vertex_layouts")?)?,
            primitive: decode(get(p, "primitive")?)?,
            depth_stencil: decode(get(p, "depth_stencil")?)?,
            multisample: decode(get(p, "multisample")?)?,
            targets: decode(get(p, "targets")?)?,
        };
        let pipeline_key = sha(&serde_json::to_vec(&pipeline)?);
        let existing = pipeline_cache.get(&pipeline_key).copied();
        let id = existing
            .map(|n| s.pipelines[n].id.clone())
            .unwrap_or_else(|| format!("native-pipeline:{}", s.pipelines.len()));
        pipeline.id = id.clone();
        let mut vbs = vec![];
        for v in array(d, "vertex_buffers")? {
            vbs.push(VertexBuffer {
                slot: u32n(v, "slot")?,
                buffer: buffer(&mut s, v)?,
                offset: num(v, "offset")?,
            });
        }
        let ib = match d.get("index_buffer") {
            Some(v) if !v.is_null() => Some(IndexBuffer {
                buffer: buffer(&mut s, v)?,
                offset: num(v, "offset")?,
                format: decode(get(v, "format")?)?,
            }),
            _ => None,
        };
        let a = get(d, "draw")?;
        let count = u32n(a, "count")?;
        let instances = u32n(a, "instances")?;
        let fi = u32n(a, "start_instance")?;
        let first = u32n(a, "first_vertex")?;
        let index = u32n(a, "start_index")?;
        let base: i32 = get(a, "base_vertex")?
            .as_i64()
            .ok_or("native base vertex")?
            .try_into()?;
        let args = match u32n(a, "kind")? {
            0 => {
                require(
                    ib.is_none() && index == 0 && base == 0,
                    "native direct/index contradiction",
                )?;
                DrawArgs::Direct {
                    vertices: count,
                    instances,
                    first_vertex: first,
                    first_instance: fi,
                }
            }
            1 => {
                require(
                    ib.is_some() && first == 0,
                    "native indexed vertex start contradiction",
                )?;
                DrawArgs::Indexed {
                    indices: count,
                    instances,
                    first_index: index,
                    base_vertex: base,
                    first_instance: fi,
                }
            }
            _ => return Err("native draw kind unsupported".into()),
        };
        // Preserve the actual bound target domain even for depth-only/no-PS draws.
        let color_count = attachments
            .keys()
            .filter_map(|k| {
                k.strip_prefix("color")
                    .and_then(|n| n.parse::<usize>().ok())
            })
            .max()
            .map_or(0, |n| n + 1);
        let colors = (0..color_count)
            .map(|i| attachments.get(&format!("color{i}")).cloned())
            .collect();
        let depth = if pipeline.depth_stencil.is_some() {
            Some(DepthAttachment {
                attachment: attachments
                    .get("depth")
                    .ok_or("native depth attachment absent")?
                    .clone(),
                read_only_depth: num(domain, "depth_flags")? & 1 != 0,
                read_only_stencil: num(domain, "depth_flags")? & 2 != 0,
            })
        } else {
            None
        };
        let mut writes = vec![];
        for change in array(d, "writes")? {
            let before = version(get(change, "before")?)?;
            let after = version(get(change, "after")?)?;
            require(
                before.slice == after.slice,
                "native write changed allocation/subresource",
            )?;
            writes.push(Transition {
                slice: before.slice,
                before: before.content_version,
                after: after.content_version,
            });
        }
        if existing.is_none() {
            pipeline_cache.insert(pipeline_key, s.pipelines.len());
            s.pipelines.push(pipeline);
        }
        s.commands.push(Command::Draw {
            draw: Draw {
                eye: s.eye.clone(),
                pipeline: id,
                bindings,
                vertex_buffers: vbs,
                index_buffer: ib,
                args,
                colors,
                depth,
                viewport: decode(get(d, "viewport")?)?,
                scissor: decode(get(d, "scissor")?)?,
                blend: decode(get(d, "blend_constant")?)?,
                stencil: u32n(d, "stencil_reference")?,
                writes,
            },
        });
    }
    s.expected = finals;
    Ok(s)
}

pub fn native(path: &Path, out: &Path) -> Result<()> {
    require(
        fs::metadata(path)?.len() <= 32 << 20,
        "native manifest bound",
    )?;
    let raw = fs::read(path)?;
    require(raw.len() <= 32 << 20, "native manifest bound")?;
    let manifest: Value = serde_json::from_slice(&raw)?;
    let mut scene = convert(&manifest, sha(&raw))?;
    let root = path
        .parent()
        .ok_or("native manifest parent")?
        .canonicalize()?;
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    let mut payload = vec![];
    for b in &scene.blobs {
        let rel = Path::new(&b.path);
        require(
            !rel.is_absolute()
                && rel.components().all(|c| matches!(c, Component::Normal(_)))
                && names.insert(b.path.clone())
                && b.path != "scene.json"
                && b.path != "native-capture.json"
                && b.path != "native-import.json",
            "native local unique blob path",
        )?;
        let p = root.join(rel).canonicalize()?;
        require(p.starts_with(&root), "native blob escapes root")?;
        total = total
            .checked_add(b.bytes)
            .ok_or("native payload overflow")?;
        require(
            total <= MAX_BYTES && fs::metadata(&p)?.len() == b.bytes,
            "native payload pre-read bound",
        )?;
        let bytes = fs::read(p)?;
        require(
            bytes.len() as u64 == b.bytes && sha(&bytes) == b.sha256,
            "native blob identity changed",
        )?;
        payload.push((rel.to_owned(), bytes));
    }
    let proofs = crate::native_clear::promote(&manifest, &mut scene, &payload)?;
    fs::create_dir(out)?;
    for (rel, bytes) in payload {
        let dest = out.join(rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(dest, bytes)?;
    }
    let scene_bytes = serde_json::to_vec_pretty(&scene)?;
    fs::write(out.join("native-capture.json"), &raw)?;
    fs::write(out.join("scene.json"), &scene_bytes)?;
    Input::load(&out.join("scene.json"))?;
    fs::write(
        out.join("native-import.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema":1,"adapter":"schema4 native complete target interval v1",
            "capture_sha256":sha(&raw),"scene_sha256":sha(&scene_bytes),
            "native_identifiers_preserved":true,
            "uniform_clear_initializers":proofs,
            "inherited_target_initializers_remain_captured":true,
            "finished_frame_or_density_accepted":false
        }))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn v(allocation: u64, version: u64, mip: u32, layer: u32) -> Value {
        json!({"allocation_id":allocation,"content_version":version,"subresource":{"mip":mip,"layer":layer,"aspect":"color"}})
    }
    fn fixture() -> (Value, BTreeMap<String, Vec<u8>>) {
        let vs="@vertex fn main(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);}";
        let ps="@group(0) @binding(0) var t:texture_2d_array<f32>; @fragment fn main()->@location(0)vec4f{return textureLoad(t,vec2i(0),0,0);}";
        let vh = sha(vs.as_bytes());
        let ph = sha(ps.as_bytes());
        let mut sources = serde_json::Map::new();
        sources.insert(vh.clone(), json!({"wgsl":vs}));
        sources.insert(ph.clone(), json!({"wgsl":ps}));
        let reference = |h: &str| json!({"status":"exact","sha256":h,"entry_point":"main","compilation_options":"wgpu default"});
        let mut matrix = [0f32; 16];
        for i in [0, 5, 10, 15] {
            matrix[i] = 1.;
        }
        let world = json!({"provenance_epoch":1,"camera_sequence":10,"request_frame":20,"head_pose_frame":20,"eye_index":0,"view_count":2,"headset_applied":true,"depth_reverse_z":false,"presentation_time_seconds":1.,"game_from_eye":matrix,"game_from_stage":matrix,"projection_from_eye":matrix,"optics":{"scope_sequence":0,"magnification":1.,"reference_fov_degrees":0.,"actual_fov_degrees":0.}});
        let domain = json!({"width":2,"height":2,"depth_flags":0,"attachments":[{"role":"color0","guest_id":1,"native_allocation_id":100,"format":TextureFormat::Rgba8Unorm,"mip":0,"slice":0}]});
        let descriptor = json!({"width":4,"height":4,"depth_or_array_layers":2,"mip_level_count":2,"sample_count":1,"dimension":"2d","format":TextureFormat::Rgba8Unorm});
        let view = TextureViewDescriptor::<Option<String>> {
            dimension: Some(TextureViewDimension::D2Array),
            format: Some(TextureFormat::Rgba8UnormSrgb),
            base_mip_level: 1,
            mip_level_count: Some(1),
            base_array_layer: 1,
            array_layer_count: Some(1),
            ..Default::default()
        };
        let layout = BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Texture {
                sample_type: TextureSampleType::Float { filterable: true },
                view_dimension: TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let binding = json!({"group":0,"binding":0,"layout":layout,"kind":"texture","native_allocation_id":200,"descriptor":descriptor,"view":view,"subresources":[{"mip_level":1,"layer":1,"aspect":"color","blob":"material.bin","packed_row_bytes":8,"version":v(200,2,1,1)}]});
        let pipeline = json!({"effective_shaders":{"vertex":reference(&vh),"fragment":reference(&ph)},"vertex_layouts":[],"primitive":PrimitiveState::default(),"depth_stencil":null,"multisample":MultisampleState::default(),"targets":[ColorTargetState{format:TextureFormat::Rgba8Unorm,blend:None,write_mask:ColorWrites::ALL}]});
        let draw = json!({"command_sequence":101,"world_view":world,"target_domain":domain,"pipeline":pipeline,"bindings":[binding],"vertex_buffers":[],"index_buffer":null,"draw":{"kind":0,"count":3,"instances":1,"start_index":0,"first_vertex":0,"base_vertex":0,"start_instance":0},"viewport":[0.,0.,2.,2.,0.,1.],"scissor":[0,0,2,2],"blend_constant":[0.,0.,0.,0.],"stencil_reference":0,"writes":[{"before":v(100,1,0,0),"after":v(100,3,0,0)}]});
        let bytes = BTreeMap::from([
            ("initial.bin".into(), [0, 0, 0, 255].repeat(4)),
            ("final.bin".into(), [255, 0, 0, 255].repeat(4)),
            ("material.bin".into(), [255, 0, 0, 255].repeat(4)),
        ]);
        let blobs = bytes
            .iter()
            .map(|(path, b)| json!({"path":path,"byte_length":b.len(),"sha256":sha(b)}))
            .collect::<Vec<_>>();
        (
            json!({"schema":4,"kind":"native_target_span","status":"captured","source":{"backend":"Metal","method":"Gpu::do_draw resolved bindings","encoder_epoch":1},"normal_frame_publication_changed":false,"world_view":world,"target_domain":domain,"draws":[draw],"attachments":[{"role":"color0","native_allocation_id":100,"width":2,"height":2,"descriptor":{"width":2,"height":2,"depth_or_array_layers":1,"mip_level_count":1,"sample_count":1,"dimension":"2d","format":TextureFormat::Rgba8Unorm},"format":TextureFormat::Rgba8Unorm,"mip":0,"slice":0,"initial_blob":"initial.bin","final_blob":"final.bin","initial_version":v(100,1,0,0),"final_version":v(100,3,0,0)}],"blobs":blobs,"effective_shader_sources":{"complete":true,"sources":sources},"closure":{"complete_target_interval":true,"interval_start_command":100,"interval_end_command":102,"first_executed_draw_captured":true,"last_executed_draw_captured":true,"unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,"queries_inactive":true,"gpu_errors":0}}),
            bytes,
        )
    }
    fn shared(m: &Value) -> Value {
        fn intern(table: &mut Vec<Value>, value: Value) -> usize {
            table.iter().position(|x| x == &value).unwrap_or_else(|| {
                table.push(value);
                table.len() - 1
            })
        }
        let mut encoded = m.clone();
        let mut pipelines = vec![];
        let mut bindings = vec![];
        let mut worlds = vec![];
        let mut domains = vec![];
        for d in encoded["draws"].as_array_mut().unwrap() {
            let o = d.as_object_mut().unwrap();
            let p = intern(&mut pipelines, o.remove("pipeline").unwrap());
            let w = intern(&mut worlds, o.remove("world_view").unwrap());
            let t = intern(&mut domains, o.remove("target_domain").unwrap());
            let refs = o
                .remove("bindings")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|b| intern(&mut bindings, b.clone()))
                .collect::<Vec<_>>();
            o.insert("pipeline_ref".into(), json!(p));
            o.insert("binding_refs".into(), json!(refs));
            o.insert("world_view_ref".into(), json!(w));
            o.insert("target_domain_ref".into(), json!(t));
        }
        encoded["draw_metadata_encoding"] = "shared_v1".into();
        encoded["shared_draw_metadata"] = json!({"pipelines":pipelines,"bindings":bindings,"world_views":worlds,"target_domains":domains});
        encoded
    }
    #[test]
    fn shared_metadata_roundtrip_and_adverse_references() -> Result<()> {
        let (inline, _) = fixture();
        let encoded = shared(&inline);
        assert_eq!(expand_metadata(&encoded)?.as_ref(), &inline);
        assert_eq!(
            serde_json::to_value(convert(&encoded, "0".repeat(64))?)?,
            serde_json::to_value(convert(&inline, "0".repeat(64))?)?
        );
        let reject = |change: &dyn Fn(&mut Value)| -> Result<()> {
            let mut bad = encoded.clone();
            change(&mut bad);
            require(
                expand_metadata(&bad).is_err(),
                "invalid shared metadata admitted",
            )
        };
        reject(&|m| m["draw_metadata_encoding"] = "shared_v2".into())?;
        reject(&|m| m["draw_metadata_encoding"] = Value::Null)?;
        reject(&|m| {
            m.as_object_mut().unwrap().remove("shared_draw_metadata");
        })?;
        reject(&|m| m["shared_draw_metadata"]["other"] = json!([]))?;
        reject(&|m| m["shared_draw_metadata"]["pipelines"] = json!({}))?;
        reject(&|m| m["shared_draw_metadata"]["bindings"][0] = json!(4))?;
        reject(&|m| m["draws"][0]["pipeline"] = inline["draws"][0]["pipeline"].clone())?;
        reject(&|m| m["draws"][0]["bindings"] = json!([]))?;
        reject(&|m| {
            m["draws"][0]
                .as_object_mut()
                .unwrap()
                .remove("pipeline_ref");
        })?;
        for v in [json!(-1), json!(0.5), json!("0"), json!(1)] {
            reject(&|m| m["draws"][0]["pipeline_ref"] = v.clone())?;
        }
        for v in [json!(-1), json!(0.5), json!("0"), json!(1)] {
            reject(&|m| m["draws"][0]["binding_refs"][0] = v.clone())?;
        }
        for (field, table, inline_key) in [
            ("world_view_ref", "world_views", "world_view"),
            ("target_domain_ref", "target_domains", "target_domain"),
        ] {
            for v in [json!(-1), json!(0.5), json!("0"), json!(1)] {
                reject(&|m| m["draws"][0][field] = v.clone())?;
            }
            reject(&|m| m["draws"][0][inline_key] = inline["draws"][0][inline_key].clone())?;
            reject(&|m| {
                m["draws"][0].as_object_mut().unwrap().remove(field);
            })?;
            reject(&|m| m["shared_draw_metadata"][table] = json!(vec![json!({}); 4097]))?;
            reject(&|m| m["shared_draw_metadata"][table][0] = json!(null))?;
        }
        let mut wrong_world = encoded.clone();
        wrong_world["shared_draw_metadata"]["world_views"][0]["eye_index"] = json!(1);
        assert!(convert(&wrong_world, "0".repeat(64)).is_err());
        let mut wrong_domain = encoded.clone();
        wrong_domain["shared_draw_metadata"]["target_domains"][0]["width"] = json!(4);
        assert!(convert(&wrong_domain, "0".repeat(64)).is_err());
        reject(&|m| m["draws"][0]["binding_refs"] = json!({}))?;
        reject(&|m| m["draws"][0]["binding_refs"] = json!(vec![0; 257]))?;
        reject(&|m| {
            let d = m["draws"][0].clone();
            m["draws"] = json!(vec![d; 4097]);
        })?;
        reject(&|m| m["shared_draw_metadata"]["pipelines"] = json!(vec![json!({}); 4097]))?;
        reject(&|m| m["shared_draw_metadata"]["bindings"] = json!(vec![json!({}); 65537]))?;
        reject(&|m| {
            let mut d = m["draws"][0].clone();
            d["binding_refs"] = json!(vec![0; 256]);
            m["draws"] = json!(vec![d; 1025]);
        })?;
        reject(&|m| {
            m["shared_draw_metadata"]["bindings"][0]["padding"] = json!("x".repeat(1 << 20));
            let d = m["draws"][0].clone();
            m["draws"] = json!(vec![d; 129]);
        })?;
        let mut unencoded = inline.clone();
        unencoded["draws"][0]["pipeline_ref"] = json!(0);
        assert!(expand_metadata(&unencoded).is_err());
        unencoded = inline.clone();
        unencoded["shared_draw_metadata"] = json!({});
        assert!(expand_metadata(&unencoded).is_err());
        Ok(())
    }
    #[test]
    fn native_schema4_identity_views_and_fail_closed_metadata() -> Result<()> {
        let (m, bytes) = fixture();
        let s = convert(&m, "0".repeat(64))?;
        assert_eq!(s.textures.len(), 2);
        assert_eq!(
            s.textures[1]
                .initial
                .iter()
                .filter(|i| i.content_version == 0)
                .count(),
            3
        );
        assert_eq!(s.commands.len(), 1);
        let dir = std::env::temp_dir().join(format!(
            "native-adapter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&dir)?;
        for (name, data) in &bytes {
            fs::write(dir.join(name), data)?;
        }
        fs::write(dir.join("capture.json"), serde_json::to_vec_pretty(&m)?)?;
        let mut high = m.clone();
        high["draws"][0]["bindings"][0]["subresources"][0]["version"]["content_version"] =
            ((1u64 << 63) | 1).into();
        let high_scene = convert(&high, "0".repeat(64))?;
        fs::write(
            dir.join("high-scene.json"),
            serde_json::to_vec_pretty(&high_scene)?,
        )?;
        Input::load(&dir.join("high-scene.json"))?;
        let mut ranges = s.clone();
        ranges.blobs.push(Blob {
            path: "range32.bin".into(),
            bytes: 32,
            sha256: sha(&[0; 32]),
        });
        let a = buffer(
            &mut ranges,
            &json!({"native_allocation_id":700,"content_version":1,"source_offset":0,"blob":"initial.bin"}),
        )?;
        let b = buffer(
            &mut ranges,
            &json!({"native_allocation_id":700,"content_version":1,"source_offset":0,"blob":"range32.bin"}),
        )?;
        assert_ne!(
            a, b,
            "same revision different captured ranges must remain distinct"
        );
        let out = dir.join("imported");
        native(&dir.join("capture.json"), &out)?;
        let shared_raw = serde_json::to_vec(&shared(&m))?;
        fs::write(dir.join("shared-capture.json"), &shared_raw)?;
        let shared_out = dir.join("shared-import");
        native(&dir.join("shared-capture.json"), &shared_out)?;
        assert_eq!(
            fs::read(shared_out.join("native-capture.json"))?,
            shared_raw
        );
        let shared_input = Input::load(&shared_out.join("scene.json"))?;
        assert!(
            matches!(shared_input.scene.provenance,Provenance::Native { source_sha256 } if source_sha256==sha(&shared_raw))
        );
        let input = Input::load(&out.join("scene.json"))?;
        assert!(!input.targets_have_resolution_independent_initialization);
        let reject = |change: &dyn Fn(&mut Value)| -> Result<()> {
            let mut bad = m.clone();
            change(&mut bad);
            require(
                convert(&bad, "0".repeat(64)).is_err(),
                "bad native metadata admitted",
            )
        };
        let mut repeated = m.clone();
        let mut second = repeated["draws"][0].clone();
        second["command_sequence"] = 102.into();
        second["writes"][0]["before"] = v(100, 3, 0, 0);
        second["writes"][0]["after"] = v(100, 4, 0, 0);
        repeated["draws"].as_array_mut().unwrap().push(second);
        repeated["attachments"][0]["final_version"] = v(100, 4, 0, 0);
        repeated["closure"]["interval_end_command"] = 103.into();
        let combined = convert(&repeated, "0".repeat(64))?;
        assert_eq!(
            combined.pipelines.len(),
            1,
            "same effective pipeline must be reused across original draws"
        );
        assert_eq!(combined.commands.len(), 2);
        reject(&|x| {
            let mut t = x["target_domain"]["attachments"][0].clone();
            t["role"] = "color1".into();
            x["target_domain"]["attachments"]
                .as_array_mut()
                .unwrap()
                .push(t);
        })?;
        reject(&|x| {
            let t = x["target_domain"]["attachments"][0].clone();
            x["target_domain"]["attachments"]
                .as_array_mut()
                .unwrap()
                .push(t);
        })?;
        reject(&|x| x["closure"]["complete_target_interval"] = false.into())?;
        reject(&|x| x["closure"]["first_executed_draw_captured"] = false.into())?;
        reject(&|x| x["closure"]["unmodelled_commands"] = 1.into())?;
        reject(&|x| x["closure"]["gpu_errors"] = 1.into())?;
        reject(&|x| x["draws"][0]["command_sequence"] = 102.into())?;
        reject(&|x| x["draws"][0]["world_view"]["eye_index"] = 1.into())?;
        reject(&|x| x["draws"][0]["world_view"]["game_from_stage"][12] = 0.01.into())?;
        reject(&|x| x["attachments"][0]["native_allocation_id"] = 0.into())?;
        reject(&|x| x["attachments"][0]["initial_version"]["content_version"] = 0.into())?;
        reject(&|x| {
            x["draws"][0]["pipeline"]["effective_shaders"]["vertex"]["sha256"] = "missing".into()
        })?;
        reject(&|x| {
            x["draws"][0]["draw"]
                .as_object_mut()
                .unwrap()
                .remove("first_vertex")
                .map(|_| ())
                .unwrap()
        })?;
        reject(&|x| {
            x["draws"][0]["bindings"][0]["subresources"][0]["version"]["subresource"]["layer"] =
                0.into()
        })?;
        reject(&|x| {
            x["draws"][0]["bindings"][0]["subresources"][0]["packed_row_bytes"] = 9.into()
        })?;
        // A D3 subresource is one whole mip volume, never a rebased 2D layer.
        let mut volume = m.clone();
        let b = &mut volume["draws"][0]["bindings"][0];
        b["descriptor"]["dimension"] = "3d".into();
        b["descriptor"]["depth_or_array_layers"] = 4.into();
        b["view"]["dimension"] = "3d".into();
        b["view"]["base_array_layer"] = 0.into();
        b["layout"]["ty"]["Texture"]["view_dimension"] = "3d".into();
        b["subresources"][0]["layer"] = 0.into();
        b["subresources"][0]["version"]["subresource"]["layer"] = 0.into();
        b["subresources"][0]["depth"] = 2.into();
        let volume_scene = convert(&volume, "0".repeat(64))?;
        let material = volume_scene
            .textures
            .iter()
            .find(|t| !t.mutable_target)
            .unwrap();
        assert_eq!(material.descriptor.dimension, TextureDimension::D3);
        assert_eq!(material.initial.len(), 2);
        assert_eq!(material.descriptor.mip_depth(1), 2);
        for depth in [Value::Null, json!(1), json!(4)] {
            let mut bad = volume.clone();
            bad["draws"][0]["bindings"][0]["subresources"][0]["depth"] = depth;
            require(
                convert(&bad, "0".repeat(64)).is_err(),
                "incorrect volume depth admitted",
            )?;
        }
        // Repeated material parent with a changed version must not become a second allocation.
        reject(&|x| {
            let mut d = x["draws"][0].clone();
            d["command_sequence"] = 102.into();
            d["bindings"][0]["subresources"][0]["version"]["content_version"] = 4.into();
            x["draws"].as_array_mut().unwrap().push(d);
            x["closure"]["interval_end_command"] = 103.into();
        })?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}

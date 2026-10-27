//! Explicit original-density adapters. Local replay IDs are NOT native identities.
use crate::{
    model::*,
    require, sha,
    validate::{written_slices, Input},
    Result,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
};
use wgpu_types::*;
fn n(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| format!("missing {k}").into())
}
fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}
fn arr<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().ok_or_else(|| format!("missing {k}").into())
}
fn buffer(s: &mut Scene, blob: &str) -> String {
    let id = format!("legacy-buffer:{blob}");
    if !s.buffers.iter().any(|b| b.id == id) {
        s.buffers.push(Buffer {
            id: id.clone(),
            native_allocation_id: None,
            content_version: 1,
            source_offset: 0,
            blob: blob.into(),
        });
    }
    id
}
pub fn legacy(path: &Path, out: &Path) -> Result<()> {
    require(
        fs::metadata(path)?.len() <= 32 << 20,
        "legacy manifest bound",
    )?;
    let raw = fs::read(path)?;
    require(raw.len() <= 32 << 20, "legacy manifest bound")?;
    let m: Value = serde_json::from_slice(&raw)?;
    let two = m["schema"] == 2 && m["kind"] == "native_draw_span";
    let one = m["schema"] == 3 && m["kind"] == "native_color_null_draw";
    require(
        (two || one)
            && m["status"] == "captured"
            && m["source"]["method"] == "Gpu::do_draw resolved bindings"
            && m["closure"]["natural_submission_completed"] == true
            && m["closure"]["initial_contents_captured"] == true
            && m["closure"]["original_final_captured"] == true
            && m["closure"]["unmodelled_commands"] == 0
            && if two {
                m["closure"]["ordered_two_draws"] == true
                    && m["closure"]["resource_mutation_rejections"] == 0
            } else {
                m["closure"]["single_original_draw"] == true
                    && m["closure"]["queries_inactive"] == true
                    && m["closure"]["gpu_errors"] == 0
            },
        "requires original native legacy capture",
    )?;
    let ds = if two {
        arr(&m, "draws")?.clone()
    } else {
        vec![m["draw"].clone()]
    };
    require(
        ds.len() == if two { 2 } else { 1 },
        "legacy draw cardinality",
    )?;
    for d in &ds {
        let (vs, ps) = if two {
            (
                "04f353256b2a27c7141076f68c572d451f551a0b8370bafd6f521fc8dc90cb94",
                "1e31e158d1f81369d39401dabee56c4b988eec1110f81b21f5b99ad3c8e6c5af",
            )
        } else {
            (
                "6153fb6404c740ac049805cd05964a8e1ae36860f3f3f0c3e344e51335b8e538",
                "21bd6330cd32748471b4f917ff7e3a3ea99838978391a2eafd096eb58f093e2d",
            )
        };
        require(
            sha(text(d, "vertex_wgsl")?.as_bytes()) == vs
                && sha(text(d, "pixel_wgsl")?.as_bytes()) == ps,
            "legacy final-variant source pin",
        )?;
        // New captures carry actual successful module sources; never ignore a
        // contradiction merely because their old guest source matches a pin.
        if let Some(registry) = m.get("effective_shader_sources") {
            require(
                registry["complete"] == true,
                "effective shader registry incomplete",
            )?;
            for (stage, expected, old) in [
                ("vertex", vs, "vertex_wgsl"),
                ("fragment", ps, "pixel_wgsl"),
            ] {
                let reference = &d["pipeline"]["effective_shaders"][stage];
                require(
                    reference["status"] == "exact"
                        && reference["sha256"] == expected
                        && reference["entry_point"] == "main"
                        && reference["compilation_options"] == "wgpu default",
                    "legacy effective shader variant differs",
                )?;
                let actual = text(&registry["sources"][expected], "wgsl")?;
                require(
                    sha(actual.as_bytes()) == expected && actual == text(d, old)?,
                    "effective shader source identity",
                )?;
            }
        }
    }
    let eye = if m["world_view"].is_null() {
        None
    } else {
        let v = &m["world_view"];
        Some(EyeIdentity {
            epoch: n(v, "provenance_epoch"),
            camera: n(v, "camera_sequence"),
            request: n(v, "request_frame"),
            head: n(v, "head_pose_frame"),
            eye: n(v, "eye_index") as u32,
        })
    };
    let mut s = Scene {
        schema: 1,
        provenance: Provenance::LegacyCaptured {
            manifest_sha256: sha(&raw),
        },
        eye,
        textures: vec![],
        buffers: vec![],
        blobs: vec![],
        pipelines: vec![],
        commands: vec![],
        expected: vec![],
        original_submission_completed: true,
        original_gpu_errors: if one { Some(0) } else { None },
        omitted_commands: 0,
    };
    let root = path.parent().ok_or("input parent")?.canonicalize()?;
    let mut payload = Vec::new();
    let mut total = 0u64;
    for b in arr(&m, "blobs")? {
        let name = text(b, "path")?;
        let rel = Path::new(name);
        require(
            !rel.is_absolute() && rel.components().all(|c| matches!(c, Component::Normal(_))),
            "blob path",
        )?;
        let f = root.join(rel).canonicalize()?;
        require(f.starts_with(&root), "blob escape")?;
        let expected = n(b, "byte_length");
        total = total.checked_add(expected).ok_or("legacy size overflow")?;
        require(
            total <= 256 << 20 && fs::metadata(&f)?.len() == expected,
            "legacy payload pre-read bound",
        )?;
        let data = fs::read(f)?;
        require(
            total <= 256 << 20
                && data.len() as u64 == n(b, "byte_length")
                && sha(&data) == text(b, "sha256")?,
            "legacy blob bound/hash",
        )?;
        s.blobs.push(Blob {
            path: name.into(),
            bytes: data.len() as u64,
            sha256: sha(&data),
        });
        payload.push((name.to_string(), data));
    }
    let mut current = BTreeMap::new();
    let mut refs = Vec::new();
    for a in arr(&m, "attachments")? {
        let role = text(a, "role")?;
        require(
            matches!(
                role,
                "color0" | "color1" | "color2" | "color3" | "depth" | "stencil"
            ) && a["mip"] == 0
                && a["slice"] == 0,
            "legacy pinned attachment domain",
        )?;
        let aspect = match role {
            "depth" => Aspect::Depth,
            "stencil" => Aspect::Stencil,
            _ => Aspect::Color,
        };
        let id = if aspect == Aspect::Color {
            format!("legacy-target:{role}")
        } else {
            "legacy-target:depth-stencil".into()
        };
        let format = decode(&a["format"])?;
        let init = SliceInitial {
            mip: 0,
            layer: 0,
            aspect: aspect.clone(),
            content_version: 1,
            origin: Initial::Captured {
                blob: text(a, "initial_blob")?.into(),
            },
        };
        if let Some(t) = s.textures.iter_mut().find(|t| t.id == id) {
            t.initial.push(init);
        } else {
            s.textures.push(Texture {
                id: id.clone(),
                native_allocation_id: None,
                descriptor: TextureDesc {
                    width: n(a, "width") as u32,
                    height: n(a, "height") as u32,
                    layers: 1,
                    mips: 1,
                    format,
                    view_formats: vec![],
                },
                mutable_target: true,
                initial: vec![init],
            });
        }
        let key = Slice {
            resource: id,
            mip: 0,
            layer: 0,
            aspect,
        };
        current.insert(key.clone(), 1);
        refs.push((key, text(a, "final_blob")?.to_string()));
    }
    let mut version = 100;
    for (ordinal, d) in ds.iter().enumerate() {
        let id = format!("legacy-pipeline:{ordinal}");
        let mut layouts = vec![vec![], vec![]];
        let mut bindings = Vec::new();
        for b in arr(d, "bindings")? {
            let group = n(b, "group") as u32;
            let binding = n(b, "binding") as u32;
            require(group < 2, "legacy graphics groups")?;
            layouts[group as usize].push(decode(&b["layout"])?);
            let value = match text(b, "kind")? {
                "uniform" | "buffer" => BindingValue::Buffer {
                    buffer: buffer(&mut s, text(b, "blob")?),
                    offset: n(b, "offset"),
                    size: n(b, "size"),
                },
                "sampler" => BindingValue::Sampler {
                    descriptor: decode(&b["descriptor"])?,
                },
                "texture" => {
                    let t = &b["descriptor"];
                    require(
                        t["dimension"] == "2d" && t["sample_count"] == 1,
                        "legacy texture shape",
                    )?;
                    let id = format!("legacy-texture:{ordinal}:{group}:{binding}");
                    let view: TextureViewDescriptor<Option<String>> = decode(&b["view"])?;
                    let format = decode(&t["format"])?;
                    let mut initial = Vec::new();
                    let mut versions = Vec::new();
                    let count = view.mip_level_count.unwrap_or(
                        (n(t, "mip_level_count") as u32).saturating_sub(view.base_mip_level),
                    );
                    let layers = view.array_layer_count.unwrap_or(
                        (n(t, "depth_or_array_layers") as u32)
                            .saturating_sub(view.base_array_layer),
                    );
                    let mip_end = view
                        .base_mip_level
                        .checked_add(count)
                        .ok_or("mip overflow")?;
                    let layer_end = view
                        .base_array_layer
                        .checked_add(layers)
                        .ok_or("layer overflow")?;
                    for sub in arr(b, "subresources")? {
                        let mip = n(sub, "mip_level") as u32;
                        let layer = n(sub, "layer") as u32;
                        initial.push(SliceInitial {
                            mip,
                            layer,
                            aspect: Aspect::Color,
                            content_version: 1,
                            origin: Initial::Captured {
                                blob: text(sub, "blob")?.into(),
                            },
                        });
                        if mip >= view.base_mip_level
                            && mip < mip_end
                            && layer >= view.base_array_layer
                            && layer < layer_end
                        {
                            versions.push(Version {
                                slice: Slice {
                                    resource: id.clone(),
                                    mip,
                                    layer,
                                    aspect: Aspect::Color,
                                },
                                content_version: 1,
                            });
                        }
                    }
                    s.textures.push(Texture {
                        id: id.clone(),
                        native_allocation_id: None,
                        descriptor: TextureDesc {
                            width: n(t, "width") as u32,
                            height: n(t, "height") as u32,
                            layers: n(t, "depth_or_array_layers") as u32,
                            mips: n(t, "mip_level_count") as u32,
                            format,
                            view_formats: view
                                .format
                                .filter(|f| *f != format)
                                .into_iter()
                                .collect(),
                        },
                        mutable_target: false,
                        initial,
                    });
                    BindingValue::Texture {
                        texture: id,
                        view,
                        versions,
                    }
                }
                _ => return Err("legacy binding unsupported".into()),
            };
            bindings.push(Binding {
                group,
                binding,
                value,
            });
        }
        let p = &d["pipeline"];
        s.pipelines.push(Pipeline {
            id: id.clone(),
            vertex: Shader {
                wgsl: text(d, "vertex_wgsl")?.into(),
                sha256: sha(text(d, "vertex_wgsl")?.as_bytes()),
                entry: "main".into(),
                exact_final_variant: true,
            },
            fragment: Some(Shader {
                wgsl: text(d, "pixel_wgsl")?.into(),
                sha256: sha(text(d, "pixel_wgsl")?.as_bytes()),
                entry: "main".into(),
                exact_final_variant: true,
            }),
            layouts,
            vertex_layouts: decode(&p["vertex_layouts"])?,
            primitive: decode(&p["primitive"])?,
            depth_stencil: decode(&p["depth_stencil"])?,
            multisample: decode(&p["multisample"])?,
            targets: decode(&p["targets"])?,
        });
        let mut vertex_buffers = Vec::new();
        for v in arr(d, "vertex_buffers")? {
            vertex_buffers.push(VertexBuffer {
                slot: n(v, "slot") as u32,
                buffer: buffer(&mut s, text(v, "blob")?),
                offset: n(v, "offset"),
            });
        }
        let a = &d["draw"];
        require(
            a["kind"] == 0 || a["kind"] == 1,
            "legacy draw kind unsupported",
        )?;
        let index_buffer = if n(a, "kind") == 1 {
            let i = &d["index_buffer"];
            Some(IndexBuffer {
                buffer: buffer(&mut s, text(i, "blob")?),
                offset: n(i, "offset"),
                format: decode(&i["format"])?,
            })
        } else {
            None
        };
        let args = if index_buffer.is_some() {
            DrawArgs::Indexed {
                first_index: n(a, "start_index") as u32,
                indices: n(a, "count") as u32,
                base_vertex: a["base_vertex"].as_i64().ok_or("base vertex")? as i32,
                first_instance: n(a, "start_instance") as u32,
                instances: n(a, "instances") as u32,
            }
        } else {
            DrawArgs::Direct {
                first_vertex: n(a, "start_vertex") as u32,
                vertices: n(a, "count") as u32,
                first_instance: n(a, "start_instance") as u32,
                instances: n(a, "instances") as u32,
            }
        };
        let colors = s
            .pipelines
            .last()
            .unwrap()
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                t.as_ref().map(|t| Attachment {
                    resource: format!("legacy-target:color{i}"),
                    mip: 0,
                    layer: 0,
                    view_format: t.format,
                })
            })
            .collect();
        let depth = s
            .pipelines
            .last()
            .unwrap()
            .depth_stencil
            .as_ref()
            .map(|v| DepthAttachment {
                attachment: Attachment {
                    resource: "legacy-target:depth-stencil".into(),
                    mip: 0,
                    layer: 0,
                    view_format: v.format,
                },
                read_only_depth: n(&m["target_domain"], "depth_flags") & 1 != 0,
                read_only_stencil: n(&m["target_domain"], "depth_flags") & 2 != 0,
            });
        let mut draw = Draw {
            eye: s.eye.clone(),
            pipeline: id,
            bindings,
            vertex_buffers,
            index_buffer,
            args,
            colors,
            depth,
            viewport: decode(&d["viewport"])?,
            scissor: decode(&d["scissor"])?,
            blend: decode(&d["blend_constant"])?,
            stencil: n(d, "stencil_reference") as u32,
            writes: vec![],
        };
        for key in written_slices(&draw, s.pipelines.last().unwrap()) {
            version += 1;
            draw.writes.push(Transition {
                slice: key.clone(),
                before: current[&key],
                after: version,
            });
            current.insert(key, version);
        }
        s.commands.push(Command::Draw { draw });
    }
    for (key, blob) in refs {
        s.expected.push(Expected {
            version: Version {
                content_version: current[&key],
                slice: key,
            },
            blob,
        });
    }
    fs::create_dir(out)?;
    for (name, data) in payload {
        let f = out.join(name);
        if let Some(p) = f.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(f, data)?;
    }
    fs::write(out.join("legacy-manifest.json"), raw)?;
    fs::write(out.join("scene.json"), serde_json::to_vec_pretty(&s)?)?;
    Input::load(&out.join("scene.json"))?;
    Ok(())
}

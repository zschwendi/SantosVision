//! Explicit authored API fixture; no game assets, camera or runtime provenance.
use crate::{
    model::*,
    require, sha,
    validate::{written_slices, Input},
    Result,
};
use std::{collections::BTreeMap, fs, path::Path};
use wgpu_types::*;
fn blob(s: &mut Scene, files: &mut BTreeMap<String, Vec<u8>>, name: &str, data: Vec<u8>) -> String {
    s.blobs.push(Blob {
        path: name.into(),
        bytes: data.len() as u64,
        sha256: sha(&data),
    });
    files.insert(name.into(), data);
    name.into()
}
fn shader(s: &str) -> Shader {
    Shader {
        wgsl: s.into(),
        sha256: sha(s.as_bytes()),
        entry: "main".into(),
        exact_final_variant: true,
    }
}
fn key(id: &str, aspect: Aspect) -> Slice {
    Slice {
        resource: id.into(),
        mip: 0,
        layer: 0,
        aspect,
    }
}
fn color(v: [f64; 4]) -> ClearValue {
    ClearValue::Color {
        rgba_bits: v.map(f64::to_bits),
    }
}
pub fn make(out: &Path) -> Result<()> {
    let mut s = Scene {
        schema: 1,
        provenance: Provenance::Authored {
            description:
                "16x16 independently calculated mixed-pass API oracle; no native game capture"
                    .into(),
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
    let mut files = BTreeMap::new();
    let mut states = BTreeMap::new();
    for (id, format) in [
        ("color", TextureFormat::Rgba8Unorm),
        ("depth", TextureFormat::Depth32FloatStencil8),
        ("copied", TextureFormat::Rgba8Unorm),
    ] {
        let initial = crate::validate::aspects(format)
            .into_iter()
            .map(|a| {
                states.insert(key(id, a.clone()), 0);
                SliceInitial {
                    mip: 0,
                    layer: 0,
                    aspect: a,
                    content_version: 0,
                    origin: Initial::Unknown {
                        reason: "authored target requires ordered original initialization".into(),
                    },
                }
            })
            .collect();
        s.textures.push(Texture {
            id: id.into(),
            native_allocation_id: None,
            descriptor: TextureDesc {
                dimension: TextureDimension::D2,
                width: 16,
                height: 16,
                layers: 1,
                mips: 1,
                format,
                view_formats: vec![],
            },
            mutable_target: true,
            initial,
        });
    }
    let mut initial = Vec::new();
    for mip in 0..2 {
        for layer in 0..2 {
            let side = 4 >> mip;
            let pixel = if mip == 1 && layer == 1 {
                [0, 255, 0, 255]
            } else if layer == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            let name = format!("material-{mip}-{layer}.bin");
            blob(&mut s, &mut files, &name, pixel.repeat(side * side));
            initial.push(SliceInitial {
                mip,
                layer,
                aspect: Aspect::Color,
                content_version: 1,
                origin: Initial::Captured { blob: name },
            });
        }
    }
    s.textures.push(Texture {
        id: "material".into(),
        native_allocation_id: None,
        descriptor: TextureDesc {
            dimension: TextureDimension::D2,
            width: 4,
            height: 4,
            layers: 2,
            mips: 2,
            format: TextureFormat::Rgba8Unorm,
            view_formats: vec![TextureFormat::Rgba8UnormSrgb],
        },
        mutable_target: false,
        initial,
    });
    let vb = blob(
        &mut s,
        &mut files,
        "vertex-texture.bin",
        0f32.to_le_bytes().repeat(4),
    );
    s.textures.push(Texture {
        id: "vertex-texture".into(),
        native_allocation_id: None,
        descriptor: TextureDesc {
            dimension: TextureDimension::D2,
            width: 2,
            height: 2,
            layers: 1,
            mips: 1,
            format: TextureFormat::R32Float,
            view_formats: vec![],
        },
        mutable_target: false,
        initial: vec![SliceInitial {
            mip: 0,
            layer: 0,
            aspect: Aspect::Color,
            content_version: 1,
            origin: Initial::Captured { blob: vb },
        }],
    });
    for (i, (id, tint)) in [
        ("red", [1f32, 0., 0., 1.]),
        ("blue", [0f32, 0., 1., 1.]),
        ("green", [0f32, 1., 0., 1.]),
    ]
    .into_iter()
    .enumerate()
    {
        let b = blob(
            &mut s,
            &mut files,
            &format!("{id}.bin"),
            tint.into_iter().flat_map(f32::to_le_bytes).collect(),
        );
        s.buffers.push(Buffer {
            id: id.into(),
            native_allocation_id: None,
            content_version: i as u64 + 1,
            source_offset: 32,
            blob: b,
        });
    }
    let coords = [-1f32, -1., 1., -1., 1., 1., -1., 1.];
    let b = blob(
        &mut s,
        &mut files,
        "vertices.bin",
        coords.into_iter().flat_map(f32::to_le_bytes).collect(),
    );
    s.buffers.push(Buffer {
        id: "vertices".into(),
        native_allocation_id: None,
        content_version: 1,
        source_offset: 0,
        blob: b,
    });
    let b = blob(
        &mut s,
        &mut files,
        "indices.bin",
        [0u16, 1, 2, 0, 2, 3]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect(),
    );
    s.buffers.push(Buffer {
        id: "indices".into(),
        native_allocation_id: None,
        content_version: 1,
        source_offset: 0,
        blob: b,
    });
    let tex = |binding, visibility, dimension, filterable| BindGroupLayoutEntry {
        binding,
        visibility,
        ty: BindingType::Texture {
            sample_type: TextureSampleType::Float { filterable },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    };
    let uniform = BindGroupLayoutEntry {
        binding: 0,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Buffer {
            ty: BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let vs_a="@group(0) @binding(0) var t:texture_2d<f32>; @vertex fn main(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i]+vec2f(textureLoad(t,vec2i(0,0),0).x),0.5,1);}";
    let fs_a="struct U{color:vec4f};@group(1) @binding(0)var<uniform>u:U;@group(1) @binding(1)var t:texture_2d_array<f32>;@group(1) @binding(2)var s:sampler;@fragment fn main()->@location(0)vec4f{return vec4f(min(u.color.rgb+textureSampleLevel(t,s,vec2f(0.5),0,0).rgb,vec3f(1)),1);}";
    let vs_b =
        "@vertex fn main(@location(0)p:vec2f)->@builtin(position)vec4f{return vec4f(p,0.5,1);}";
    let vs_c="@vertex fn main(@location(0)p:vec2f,@builtin(instance_index)i:u32)->@builtin(position)vec4f{return vec4f(p.x*0.5-0.5+f32(i),p.y,0.5,1);}";
    let fs_b="struct U{color:vec4f};@group(1) @binding(0)var<uniform>u:U;@fragment fn main()->@location(0)vec4f{return u.color;}";
    let depth = DepthStencilState {
        format: TextureFormat::Depth32FloatStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(CompareFunction::Always),
        stencil: Default::default(),
        bias: Default::default(),
    };
    let layout = Some(VertexLayout {
        array_stride: 8,
        step_mode: VertexStepMode::Vertex,
        attributes: vec![VertexAttribute {
            format: VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        }],
    });
    for (i, (id, vs, fs)) in [
        ("textured", vs_a, fs_a),
        ("indexed", vs_b, fs_b),
        ("instanced", vs_c, fs_b),
    ]
    .into_iter()
    .enumerate()
    {
        let mut ds = depth.clone();
        if i == 1 {
            ds.depth_write_enabled = Some(true);
            ds.depth_compare = Some(CompareFunction::GreaterEqual);
            ds.stencil = StencilState {
                front: StencilFaceState {
                    compare: CompareFunction::Always,
                    fail_op: StencilOperation::Keep,
                    depth_fail_op: StencilOperation::Keep,
                    pass_op: StencilOperation::Replace,
                },
                back: StencilFaceState {
                    compare: CompareFunction::Always,
                    fail_op: StencilOperation::Keep,
                    depth_fail_op: StencilOperation::Keep,
                    pass_op: StencilOperation::Replace,
                },
                read_mask: 255,
                write_mask: 255,
            };
        }
        s.pipelines.push(Pipeline {
            id: id.into(),
            vertex: shader(vs),
            fragment: Some(shader(fs)),
            layouts: if i == 0 {
                vec![
                    vec![tex(
                        0,
                        ShaderStages::VERTEX,
                        TextureViewDimension::D2,
                        false,
                    )],
                    vec![
                        uniform,
                        tex(
                            1,
                            ShaderStages::FRAGMENT,
                            TextureViewDimension::D2Array,
                            true,
                        ),
                        BindGroupLayoutEntry {
                            binding: 2,
                            visibility: ShaderStages::FRAGMENT,
                            ty: BindingType::Sampler(SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                ]
            } else {
                vec![vec![], vec![uniform]]
            },
            vertex_layouts: if i == 0 { vec![] } else { vec![layout.clone()] },
            primitive: PrimitiveState::default(),
            depth_stencil: Some(ds),
            multisample: Default::default(),
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        });
    }
    let mut ordinal = 0u64;
    let add_clear = |s: &mut Scene,
                     states: &mut BTreeMap<Slice, u64>,
                     ordinal: &mut u64,
                     k: Slice,
                     value: ClearValue,
                     format| {
        *ordinal += 1;
        let before = states[&k];
        states.insert(k.clone(), *ordinal);
        s.commands.push(Command::Clear {
            transition: Transition {
                slice: k,
                before,
                after: *ordinal,
            },
            value,
            view_format: format,
        });
    };
    add_clear(
        &mut s,
        &mut states,
        &mut ordinal,
        key("color", Aspect::Color),
        color([0., 0., 0., 1.]),
        TextureFormat::Rgba8Unorm,
    );
    add_clear(
        &mut s,
        &mut states,
        &mut ordinal,
        key("depth", Aspect::Depth),
        ClearValue::Depth {
            value_bits: 0.7f32.to_bits(),
        },
        TextureFormat::Depth32FloatStencil8,
    );
    add_clear(
        &mut s,
        &mut states,
        &mut ordinal,
        key("depth", Aspect::Stencil),
        ClearValue::Stencil { value: 3 },
        TextureFormat::Depth32FloatStencil8,
    );
    for i in 0..3 {
        if i == 1 {
            add_clear(
                &mut s,
                &mut states,
                &mut ordinal,
                key("depth", Aspect::Depth),
                ClearValue::Depth {
                    value_bits: 0.3f32.to_bits(),
                },
                TextureFormat::Depth32FloatStencil8,
            );
        }
        let mut bindings = vec![Binding {
            group: 1,
            binding: 0,
            value: BindingValue::Buffer {
                buffer: ["red", "blue", "green"][i].into(),
                offset: 0,
                size: 16,
            },
        }];
        if i == 0 {
            bindings.push(Binding {
                group: 0,
                binding: 0,
                value: BindingValue::Texture {
                    texture: "vertex-texture".into(),
                    view: TextureViewDescriptor {
                        dimension: Some(TextureViewDimension::D2),
                        ..Default::default()
                    },
                    versions: vec![Version {
                        slice: key("vertex-texture", Aspect::Color),
                        content_version: 1,
                    }],
                },
            });
            bindings.push(Binding {
                group: 1,
                binding: 1,
                value: BindingValue::Texture {
                    texture: "material".into(),
                    view: TextureViewDescriptor {
                        format: Some(TextureFormat::Rgba8UnormSrgb),
                        dimension: Some(TextureViewDimension::D2Array),
                        base_mip_level: 1,
                        mip_level_count: Some(1),
                        base_array_layer: 1,
                        array_layer_count: Some(1),
                        ..Default::default()
                    },
                    versions: vec![Version {
                        slice: Slice {
                            resource: "material".into(),
                            mip: 1,
                            layer: 1,
                            aspect: Aspect::Color,
                        },
                        content_version: 1,
                    }],
                },
            });
            bindings.push(Binding {
                group: 1,
                binding: 2,
                value: BindingValue::Sampler {
                    descriptor: SamplerDescriptor::default(),
                },
            });
        }
        let mut d = Draw {
            eye: None,
            pipeline: ["textured", "indexed", "instanced"][i].into(),
            bindings,
            vertex_buffers: if i == 0 {
                vec![]
            } else {
                vec![VertexBuffer {
                    slot: 0,
                    buffer: "vertices".into(),
                    offset: 0,
                }]
            },
            index_buffer: if i == 0 {
                None
            } else {
                Some(IndexBuffer {
                    buffer: "indices".into(),
                    offset: 0,
                    format: IndexFormat::Uint16,
                })
            },
            args: if i == 0 {
                DrawArgs::Direct {
                    first_vertex: 0,
                    vertices: 3,
                    first_instance: 0,
                    instances: 1,
                }
            } else {
                DrawArgs::Indexed {
                    first_index: 0,
                    indices: 6,
                    base_vertex: 0,
                    first_instance: 0,
                    instances: if i == 2 { 2 } else { 1 },
                }
            },
            colors: vec![Some(Attachment {
                resource: "color".into(),
                mip: 0,
                layer: 0,
                view_format: TextureFormat::Rgba8Unorm,
            })],
            depth: Some(DepthAttachment {
                attachment: Attachment {
                    resource: "depth".into(),
                    mip: 0,
                    layer: 0,
                    view_format: TextureFormat::Depth32FloatStencil8,
                },
                read_only_depth: false,
                read_only_stencil: false,
            }),
            viewport: [0., 0., 16., 16., 0., 1.],
            scissor: [[0, 0, 8, 16], [8, 0, 8, 16], [0, 8, 16, 8]][i],
            blend: [1.; 4],
            stencil: 7,
            writes: vec![],
        };
        for k in written_slices(&d, &s.pipelines[i]) {
            ordinal += 1;
            d.writes.push(Transition {
                slice: k.clone(),
                before: states[&k],
                after: ordinal,
            });
            states.insert(k, ordinal);
        }
        s.commands.push(Command::Draw { draw: d });
    }
    ordinal += 1;
    s.commands.push(Command::Copy {
        source: Version {
            slice: key("color", Aspect::Color),
            content_version: states[&key("color", Aspect::Color)],
        },
        destination: Transition {
            slice: key("copied", Aspect::Color),
            before: 0,
            after: ordinal,
        },
    });
    states.insert(key("copied", Aspect::Color), ordinal);
    let mut pixels = Vec::new();
    let mut depth_bytes = Vec::new();
    let mut stencil_bytes = Vec::new();
    for y in 0..16 {
        for x in 0..16 {
            pixels.extend_from_slice(if y >= 8 {
                &[0, 255, 0, 255]
            } else if x < 8 {
                &[255, 255, 0, 255]
            } else {
                &[0, 0, 255, 255]
            });
            depth_bytes.extend_from_slice(&(if x < 8 { 0.3f32 } else { 0.5f32 }).to_le_bytes());
            stencil_bytes.push(if x < 8 { 3 } else { 7 });
        }
    }
    for (id, aspect, data, name) in [
        ("color", Aspect::Color, pixels.clone(), "expected-color.bin"),
        ("copied", Aspect::Color, pixels, "expected-copy.bin"),
        ("depth", Aspect::Depth, depth_bytes, "expected-depth.bin"),
        (
            "depth",
            Aspect::Stencil,
            stencil_bytes,
            "expected-stencil.bin",
        ),
    ] {
        let k = key(id, aspect);
        let b = blob(&mut s, &mut files, name, data);
        s.expected.push(Expected {
            version: Version {
                content_version: states[&k],
                slice: k,
            },
            blob: b,
        });
    }
    require(!out.exists(), "fresh fixture output")?;
    fs::create_dir(out)?;
    for (name, data) in files {
        fs::write(out.join(name), data)?;
    }
    fs::write(out.join("scene.json"), serde_json::to_vec_pretty(&s)?)?;
    Input::load(&out.join("scene.json"))?;
    Ok(())
}

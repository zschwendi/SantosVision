//! Explicitly authored API validation, not captured game imagery. The original
//! reference uses literal state/bindings and never consumes the replay manifest.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
};
use wgpu::{util::DeviceExt, *};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const W: u32 = 32;
const H: u32 = 24;
const VS:&str="@group(0) @binding(0) var<uniform> z:vec4f; struct O{@builtin(position) p:vec4f,@location(0)uv:vec2f}; @vertex fn main(@location(0)p:vec2f,@location(1)uv:vec2f)->O{return O(vec4f(p,z.x,1),uv);}";
const PS:&str="@group(1) @binding(0) var<uniform> tint:vec4f; @group(1) @binding(16) var s:sampler; @group(1) @binding(32) var t:texture_2d<f32>; struct O{@location(0)a:vec4f,@location(1)b:vec4f,@location(2)c:vec4f,@location(3)d:vec4f}; @fragment fn main(@location(0)uv:vec2f)->O{let c=textureSample(t,s,uv)*tint;return O(c,c.yzwx,c.zwxy,c.wxyz);}";
fn sha(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
fn floats(x: &[f32]) -> Vec<u8> {
    x.iter().flat_map(|f| f.to_le_bytes()).collect()
}
fn blob(out: &Path, name: &str, bytes: &[u8], list: &mut Vec<Value>) -> Result<String> {
    fs::write(out.join(name), bytes)?;
    list.push(json!({"path":name,"byte_length":bytes.len(),"sha256":sha(bytes)}));
    Ok(name.into())
}
fn read(device: &Device, queue: &Queue, tex: &[Texture]) -> Result<Vec<Vec<u8>>> {
    let mut e = device.create_command_encoder(&Default::default());
    let mut list = Vec::new();
    for i in 0..6 {
        let row = W * if i == 5 { 1 } else { 4 };
        let pitch = row.div_ceil(256) * 256;
        let b = device.create_buffer(&BufferDescriptor {
            label: None,
            size: (pitch * H) as u64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        e.copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: &tex[i.min(4)],
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
                    rows_per_image: Some(H),
                },
            },
            Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
        list.push((b, row, pitch));
    }
    let sub = queue.submit([e.finish()]);
    device.poll(PollType::Wait {
        submission_index: Some(sub),
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    let mut result = Vec::new();
    for (b, row, pitch) in list {
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
        let mut bytes = Vec::new();
        for y in 0..H {
            bytes.extend_from_slice(&m[(y * pitch) as usize..(y * pitch + row) as usize]);
        }
        drop(m);
        b.unmap();
        result.push(bytes);
    }
    Ok(result)
}
fn main() -> Result<()> {
    let a = std::env::args().collect::<Vec<_>>();
    if a.len() != 2 && !(a.len() == 3 && a[2] == "--bc") {
        return Err("usage: span_fixture NEW_CAPTURE_DIR [--bc]".into());
    }
    let bc = a.len() == 3;
    let out = PathBuf::from(&a[1]);
    fs::create_dir(&out)?;
    let mut id = InstanceDescriptor::new_without_display_handle();
    id.backends = Backends::METAL;
    let instance = Instance::new(id);
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        power_preference: PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        required_features: Features::DEPTH32FLOAT_STENCIL8 | Features::TEXTURE_COMPRESSION_BC,
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let textures = (0..5)
        .map(|i| {
            device.create_texture(&TextureDescriptor {
                label: Some("authored original reference target"),
                size: Extent3d {
                    width: W,
                    height: H,
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
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        })
        .collect::<Vec<_>>();
    let views = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect::<Vec<_>>();
    let mut e = device.create_command_encoder(&Default::default());
    {
        let colors = views[..4]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                Some(RenderPassColorAttachment {
                    view: v,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color {
                            r: 0.125 + i as f64 / 16.,
                            g: 0.25,
                            b: 0.5,
                            a: 0.75,
                        }),
                        store: StoreOp::Store,
                    },
                })
            })
            .collect::<Vec<_>>();
        let _pass = e.begin_render_pass(&RenderPassDescriptor {
            label: Some("authored independent initial values"),
            color_attachments: &colors,
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &views[4],
                depth_ops: Some(Operations {
                    load: LoadOp::Clear(0.25),
                    store: StoreOp::Store,
                }),
                stencil_ops: Some(Operations {
                    load: LoadOp::Clear(63),
                    store: StoreOp::Store,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit([e.finish()]);
    let initial = read(&device, &queue, &textures)?;
    let uniform = |stage| BindGroupLayoutEntry {
        binding: 0,
        visibility: stage,
        ty: BindingType::Buffer {
            ty: BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: std::num::NonZeroU64::new(16),
        },
        count: None,
    };
    let sampler_entry = BindGroupLayoutEntry {
        binding: 16,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Sampler(SamplerBindingType::Filtering),
        count: None,
    };
    let tex_entry = BindGroupLayoutEntry {
        binding: 32,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Texture {
            sample_type: TextureSampleType::Float { filterable: true },
            view_dimension: TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let l0 = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: None,
        entries: &[uniform(ShaderStages::VERTEX)],
    });
    let l1 = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: None,
        entries: &[uniform(ShaderStages::FRAGMENT), sampler_entry, tex_entry],
    });
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&l0), Some(&l1)],
        immediate_size: 0,
    });
    let vs = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("authored fixture VS"),
        source: ShaderSource::Wgsl(VS.into()),
    });
    let ps = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("authored fixture PS"),
        source: ShaderSource::Wgsl(PS.into()),
    });
    let attrs = [
        VertexAttribute {
            format: VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        VertexAttribute {
            format: VertexFormat::Float32x2,
            offset: 8,
            shader_location: 1,
        },
    ];
    let format = if bc {
        TextureFormat::Bc1RgbaUnorm
    } else {
        TextureFormat::Rgba8Unorm
    };
    let size = if bc { 4 } else { 2 };
    let mips = if bc {
        vec![
            vec![0, 248, 31, 0, 228, 228, 228, 228],
            vec![224, 7, 31, 0, 0, 0, 0, 0],
            vec![255, 255, 0, 0, 85, 85, 85, 85],
        ]
    } else {
        vec![
            vec![
                240, 20, 80, 255, 40, 220, 60, 255, 30, 60, 210, 255, 200, 180, 100, 255,
            ],
            vec![100, 120, 140, 255],
        ]
    };
    let view_desc = TextureViewDescriptor {
        format: if bc {
            Some(TextureFormat::Bc1RgbaUnormSrgb)
        } else {
            None
        },
        ..Default::default()
    };
    let view_formats = view_desc.format.into_iter().collect::<Vec<_>>();
    let material = device.create_texture(&TextureDescriptor {
        label: Some("explicitly authored material"),
        size: Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.len() as u32,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
        view_formats: &view_formats,
    });
    let mut blobs = Vec::new();
    let mut subresources = Vec::new();
    for (mip, bytes) in mips.iter().enumerate() {
        let logical = (size >> mip).max(1);
        let physical = if bc { logical.div_ceil(4) * 4 } else { logical };
        let row = if bc { 8 } else { logical * 4 };
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &material,
                mip_level: mip as u32,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            bytes,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(if bc { physical / 4 } else { physical }),
            },
            Extent3d {
                width: physical,
                height: physical,
                depth_or_array_layers: 1,
            },
        );
        let name = blob(&out, &format!("material{mip}.bin"), bytes, &mut blobs)?;
        subresources.push(json!({"mip_level":mip,"layer":0,"width":logical,"height":logical,"packed_row_bytes":row,"blob":name}));
    }
    let view = material.create_view(&view_desc);
    let sd = SamplerDescriptor {
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..Default::default()
    };
    let sampler = device.create_sampler(&sd);
    let ibbytes = vec![0u8, 0, 1, 0, 2, 0, 0, 0];
    let ibpath = blob(&out, "index.bin", &ibbytes, &mut blobs)?;
    let ib = device.create_buffer_init(&util::BufferInitDescriptor {
        label: None,
        contents: &ibbytes,
        usage: BufferUsages::INDEX,
    });
    let identity = (0..16)
        .map(|i| if i % 5 == 0 { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();
    let world = json!({"provenance_epoch":1,"camera_sequence":1,"request_frame":1,"head_pose_frame":1,"eye_index":0,"headset_applied":true,"depth_reverse_z":true,"game_from_eye":identity,"projection_from_eye":identity,"note":"explicitly authored test identity; no game camera"});
    let domain = json!({"authored_fixture_target_generation":1,"width":W,"height":H});
    let mut records = Vec::new();
    for index in 0..2 {
        let vertices = if index == 0 {
            floats(&[-1., -1., 0., 1., 3., -1., 2., 1., -1., 3., 0., -1.])
        } else {
            floats(&[-1., -1., 0., 1., 1., -1., 1., 1., -1., 1., 0., 0.])
        };
        let vbpath = blob(&out, &format!("vb{index}.bin"), &vertices, &mut blobs)?;
        let vb = device.create_buffer_init(&util::BufferInitDescriptor {
            label: None,
            contents: &vertices,
            usage: BufferUsages::VERTEX,
        });
        let z = floats(&[if index == 0 { 0.6 } else { 0.8 }, 0., 0., 0.]);
        let tint = floats(&[
            if index == 0 { 0.7 } else { 0.9 },
            0.8,
            0.6,
            if index == 0 { 1.0 } else { 0.5 },
        ]);
        let zpath = blob(&out, &format!("z{index}.bin"), &z, &mut blobs)?;
        let tintpath = blob(&out, &format!("tint{index}.bin"), &tint, &mut blobs)?;
        let zb = device.create_buffer_init(&util::BufferInitDescriptor {
            label: None,
            contents: &z,
            usage: BufferUsages::UNIFORM,
        });
        let tb = device.create_buffer_init(&util::BufferInitDescriptor {
            label: None,
            contents: &tint,
            usage: BufferUsages::UNIFORM,
        });
        let g0 = device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout: &l0,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: zb.as_entire_binding(),
            }],
        });
        let g1 = device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout: &l1,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: tb.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 16,
                    resource: BindingResource::Sampler(&sampler),
                },
                BindGroupEntry {
                    binding: 32,
                    resource: BindingResource::TextureView(&view),
                },
            ],
        });
        let face = StencilFaceState {
            compare: if index == 0 {
                CompareFunction::Always
            } else {
                CompareFunction::Equal
            },
            fail_op: StencilOperation::Keep,
            depth_fail_op: StencilOperation::Keep,
            pass_op: if index == 0 {
                StencilOperation::Replace
            } else {
                StencilOperation::IncrementClamp
            },
        };
        let depth = DepthStencilState {
            format: TextureFormat::Depth32FloatStencil8,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::GreaterEqual),
            stencil: StencilState {
                front: face,
                back: face,
                read_mask: 255,
                write_mask: 255,
            },
            bias: Default::default(),
        };
        let targets = (0..4)
            .map(|i| {
                Some(ColorTargetState {
                    format: TextureFormat::Bgra8Unorm,
                    blend: if index == 0 {
                        None
                    } else {
                        Some(BlendState::ALPHA_BLENDING)
                    },
                    write_mask: if i == 0 {
                        ColorWrites::RED | ColorWrites::GREEN | ColorWrites::BLUE
                    } else {
                        ColorWrites::ALL
                    },
                })
            })
            .collect::<Vec<_>>();
        let primitive = PrimitiveState::default();
        let multi = MultisampleState::default();
        let p = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("literal authored direct reference"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &vs,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                buffers: &[Some(VertexBufferLayout {
                    array_stride: 16,
                    step_mode: VertexStepMode::Vertex,
                    attributes: &attrs,
                })],
            },
            fragment: Some(FragmentState {
                module: &ps,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            primitive,
            depth_stencil: Some(depth.clone()),
            multisample: multi,
            multiview_mask: None,
            cache: None,
        });
        let scissor = if index == 0 {
            [0, 0, W, H]
        } else {
            [3, 2, 20, 18]
        };
        let mut e = device.create_command_encoder(&Default::default());
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
                label: None,
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
            pass.set_pipeline(&p);
            pass.set_bind_group(0, &g0, &[]);
            pass.set_bind_group(1, &g1, &[]);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_index_buffer(ib.slice(..), IndexFormat::Uint16);
            pass.set_viewport(0., 0., W as f32, H as f32, 0., 1.);
            pass.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]);
            pass.set_blend_constant(Color::TRANSPARENT);
            pass.set_stencil_reference(42);
            pass.draw_indexed(0..3, 0, 0..1);
        }
        queue.submit([e.finish()]);
        records.push(json!({"world_view":world,"target_domain":domain,"draw":{"kind":1,"count":3,"instances":1,"start_index":0,"base_vertex":0,"start_instance":0},"vertex_wgsl":VS,"pixel_wgsl":PS,"pipeline":{"primitive":primitive,"depth_stencil":depth,"multisample":multi,"targets":targets,"vertex_layouts":[{"array_stride":16,"step_mode":VertexStepMode::Vertex,"attributes":attrs}]},"vertex_buffers":[{"slot":0,"offset":0,"blob":vbpath}],"index_buffer":{"format":IndexFormat::Uint16,"offset":0,"blob":ibpath},"viewport":[0,0,W,H,0,1],"scissor":scissor,"blend_constant":[0,0,0,0],"stencil_reference":42,
            "bindings":[{"group":0,"binding":0,"kind":"uniform","layout":uniform(ShaderStages::VERTEX),"blob":zpath,"offset":0,"size":16},{"group":1,"binding":0,"kind":"uniform","layout":uniform(ShaderStages::FRAGMENT),"blob":tintpath,"offset":0,"size":16},{"group":1,"binding":16,"kind":"sampler","layout":sampler_entry,"descriptor":sd},{"group":1,"binding":32,"kind":"texture","layout":tex_entry,"descriptor":{"width":size,"height":size,"depth_or_array_layers":1,"mip_level_count":mips.len(),"sample_count":1,"dimension":TextureDimension::D2,"format":format},"view":view_desc,"subresources":subresources}]}));
    }
    let final_bytes = read(&device, &queue, &textures)?;
    if let Some(error) = pollster::block_on(scope.pop()) {
        return Err(format!("reference validation: {error}").into());
    }
    let mut attachments = Vec::new();
    for i in 0..6 {
        let role = if i < 4 {
            format!("color{i}")
        } else {
            ["depth", "stencil"][i - 4].into()
        };
        let first = blob(
            &out,
            &format!("initial-{role}.bin"),
            &initial[i],
            &mut blobs,
        )?;
        let last = blob(
            &out,
            &format!("final-{role}.bin"),
            &final_bytes[i],
            &mut blobs,
        )?;
        attachments.push(json!({"role":role,"format":if i<4{TextureFormat::Bgra8Unorm}else{TextureFormat::Depth32FloatStencil8},"width":W,"height":H,"initial_blob":first,"final_blob":last}));
    }
    let m = json!({"schema":2,"kind":"native_draw_span","status":"authored_fixture","source":{"method":"authored_api_validation_fixture","backend":"Metal","adapter":format!("{:?}",adapter.get_info()),"note":"Explicitly synthetic inputs; literal reference renders independently of consumer JSON reconstruction"},"world_view":world,"target_domain":domain,"attachments":attachments,"draws":records,"blobs":blobs,"closure":{"ordered_two_draws":true,"unmodelled_commands":0,"resource_mutation_rejections":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":false,"reference_submission_completed":true}});
    fs::write(out.join("capture.json"), serde_json::to_vec_pretty(&m)?)?;
    println!(
        "Authored reference capture: {}",
        out.join("capture.json").display()
    );
    Ok(())
}

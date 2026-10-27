//! Generic resolved execution; each immutable input version has separate storage.
//! Pass splitting is explicit offline execution, not a production performance path.
use crate::{
    model::*,
    require, sha,
    validate::{slice_bytes, Input},
    Result,
};
use std::{collections::BTreeMap, num::NonZeroU64, sync::mpsc};
use w::util::DeviceExt;
use wgpu as w;

struct Pipeline {
    gpu: w::RenderPipeline,
    layouts: Vec<w::BindGroupLayout>,
}
struct Resources {
    textures: BTreeMap<String, w::Texture>,
    buffers: BTreeMap<String, w::Buffer>,
    pipelines: BTreeMap<String, Pipeline>,
}
pub struct Output {
    pub aspect_bytes: Vec<Vec<u8>>,
    pub report: serde_json::Value,
}
fn upload(
    q: &w::Queue,
    t: &w::Texture,
    mip: u32,
    layer: u32,
    a: w::TextureAspect,
    width: u32,
    height: u32,
    bytes: &[u8],
    row: u32,
) {
    let (bw, bh) = t.format().block_dimensions();
    q.write_texture(
        w::TexelCopyTextureInfo {
            texture: t,
            mip_level: mip,
            origin: w::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: a,
        },
        bytes,
        w::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(height.div_ceil(bh)),
        },
        w::Extent3d {
            width: width.div_ceil(bw) * bw,
            height: height.div_ceil(bh) * bh,
            depth_or_array_layers: 1,
        },
    );
}
fn view(t: &w::Texture, a: &Attachment) -> w::TextureView {
    t.create_view(&w::TextureViewDescriptor {
        format: Some(a.view_format),
        dimension: Some(w::TextureViewDimension::D2),
        base_mip_level: a.mip,
        mip_level_count: Some(1),
        base_array_layer: a.layer,
        array_layer_count: Some(1),
        ..Default::default()
    })
}
fn clear(
    device: &w::Device,
    q: &w::Queue,
    t: &w::Texture,
    s: &Slice,
    value: &ClearValue,
    format: w::TextureFormat,
) {
    let a = Attachment {
        resource: s.resource.clone(),
        mip: s.mip,
        layer: s.layer,
        view_format: format,
    };
    let v = view(t, &a);
    let mut e = device.create_command_encoder(&Default::default());
    let color = match value {
        ClearValue::Color { rgba_bits } => {
            let c = rgba_bits.map(f64::from_bits);
            Some(w::RenderPassColorAttachment {
                view: &v,
                depth_slice: None,
                resolve_target: None,
                ops: w::Operations {
                    load: w::LoadOp::Clear(w::Color {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    }),
                    store: w::StoreOp::Store,
                },
            })
        }
        _ => None,
    };
    let ds = match value {
        ClearValue::Depth { value_bits } => Some(w::RenderPassDepthStencilAttachment {
            view: &v,
            depth_ops: Some(w::Operations {
                load: w::LoadOp::Clear(f32::from_bits(*value_bits)),
                store: w::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ClearValue::Stencil { value } => Some(w::RenderPassDepthStencilAttachment {
            view: &v,
            depth_ops: None,
            stencil_ops: Some(w::Operations {
                load: w::LoadOp::Clear(*value),
                store: w::StoreOp::Store,
            }),
        }),
        _ => None,
    };
    let colors = if color.is_some() { vec![color] } else { vec![] };
    {
        let _p = e.begin_render_pass(&w::RenderPassDescriptor {
            label: Some("original ordered clear"),
            color_attachments: &colors,
            depth_stencil_attachment: ds,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    q.submit([e.finish()]);
}
fn restore_depth(
    device: &w::Device,
    q: &w::Queue,
    target: &w::Texture,
    s: &Slice,
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let src = device.create_texture(&w::TextureDescriptor {
        label: Some("original Float32 depth import"),
        size: w::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: w::TextureDimension::D2,
        format: w::TextureFormat::R32Float,
        usage: w::TextureUsages::COPY_DST | w::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    upload(
        q,
        &src,
        0,
        0,
        w::TextureAspect::All,
        width,
        height,
        data,
        width * 4,
    );
    let module=device.create_shader_module(w::ShaderModuleDescriptor{label:Some("exact depth identity"),source:w::ShaderSource::Wgsl("@group(0) @binding(0) var d:texture_2d<f32>; @vertex fn vs(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);} @fragment fn fs(@builtin(position)p:vec4f)->@builtin(frag_depth)f32{return textureLoad(d,vec2i(p.xy),0).x;}".into())});
    let layout = device.create_bind_group_layout(&w::BindGroupLayoutDescriptor {
        label: None,
        entries: &[w::BindGroupLayoutEntry {
            binding: 0,
            visibility: w::ShaderStages::FRAGMENT,
            ty: w::BindingType::Texture {
                sample_type: w::TextureSampleType::Float { filterable: false },
                view_dimension: w::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let pl = device.create_pipeline_layout(&w::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let format = target.format();
    let pipeline = device.create_render_pipeline(&w::RenderPipelineDescriptor {
        label: Some("original depth restoration"),
        layout: Some(&pl),
        vertex: w::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(w::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        primitive: Default::default(),
        depth_stencil: Some(w::DepthStencilState {
            format,
            depth_write_enabled: Some(true),
            depth_compare: Some(w::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let sv = src.create_view(&Default::default());
    let bg = device.create_bind_group(&w::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[w::BindGroupEntry {
            binding: 0,
            resource: w::BindingResource::TextureView(&sv),
        }],
    });
    let tv = view(
        target,
        &Attachment {
            resource: s.resource.clone(),
            mip: s.mip,
            layer: s.layer,
            view_format: format,
        },
    );
    let mut e = device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_render_pass(&w::RenderPassDescriptor {
            label: None,
            color_attachments: &[],
            depth_stencil_attachment: Some(w::RenderPassDepthStencilAttachment {
                view: &tv,
                depth_ops: Some(w::Operations {
                    load: w::LoadOp::Clear(0.),
                    store: w::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_pipeline(&pipeline);
        p.set_bind_group(0, &bg, &[]);
        p.draw(0..3, 0..1);
    }
    q.submit([e.finish()]);
    Ok(())
}
fn compile(device: &w::Device, p: &crate::model::Pipeline) -> Pipeline {
    let layouts = p
        .layouts
        .iter()
        .map(|l| {
            device.create_bind_group_layout(&w::BindGroupLayoutDescriptor {
                label: None,
                entries: l,
            })
        })
        .collect::<Vec<_>>();
    let pl = device.create_pipeline_layout(&w::PipelineLayoutDescriptor {
        label: Some(&p.id),
        bind_group_layouts: &layouts.iter().map(Some).collect::<Vec<_>>(),
        immediate_size: 0,
    });
    let vs = device.create_shader_module(w::ShaderModuleDescriptor {
        label: Some("exact final vertex variant"),
        source: w::ShaderSource::Wgsl(p.vertex.wgsl.clone().into()),
    });
    let fs = p.fragment.as_ref().map(|s| {
        device.create_shader_module(w::ShaderModuleDescriptor {
            label: Some("exact final fragment variant"),
            source: w::ShaderSource::Wgsl(s.wgsl.clone().into()),
        })
    });
    let buffers = p
        .vertex_layouts
        .iter()
        .map(|v| {
            v.as_ref().map(|v| w::VertexBufferLayout {
                array_stride: v.array_stride,
                step_mode: v.step_mode,
                attributes: &v.attributes,
            })
        })
        .collect::<Vec<_>>();
    let gpu = device.create_render_pipeline(&w::RenderPipelineDescriptor {
        label: Some(&p.id),
        layout: Some(&pl),
        vertex: w::VertexState {
            module: &vs,
            entry_point: Some(&p.vertex.entry),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        fragment: fs.as_ref().map(|s| w::FragmentState {
            module: s,
            entry_point: Some(&p.fragment.as_ref().unwrap().entry),
            compilation_options: Default::default(),
            targets: &p.targets,
        }),
        primitive: p.primitive,
        depth_stencil: p.depth_stencil.clone(),
        multisample: p.multisample,
        multiview_mask: None,
        cache: None,
    });
    Pipeline { gpu, layouts }
}
enum Bound {
    Buffer(w::Buffer, u64, NonZeroU64),
    Texture(w::TextureView),
    Sampler(w::Sampler),
}
fn draw(device: &w::Device, q: &w::Queue, r: &Resources, d: &Draw) -> Result<()> {
    let pipe = &r.pipelines[&d.pipeline];
    let mut groups = Vec::new();
    for (g, l) in pipe.layouts.iter().enumerate() {
        let items = d
            .bindings
            .iter()
            .filter(|b| b.group as usize == g)
            .collect::<Vec<_>>();
        let values = items
            .iter()
            .map(|b| match &b.value {
                BindingValue::Buffer {
                    buffer,
                    offset,
                    size,
                } => Bound::Buffer(
                    r.buffers[buffer].clone(),
                    *offset,
                    NonZeroU64::new(*size).unwrap(),
                ),
                BindingValue::Texture { texture, view, .. } => Bound::Texture(
                    r.textures[texture].create_view(&view.map_label(|s| s.as_deref())),
                ),
                BindingValue::Sampler { descriptor } => {
                    Bound::Sampler(device.create_sampler(&descriptor.map_label(|s| s.as_deref())))
                }
            })
            .collect::<Vec<_>>();
        let entries = items
            .iter()
            .zip(&values)
            .map(|(b, v)| w::BindGroupEntry {
                binding: b.binding,
                resource: match v {
                    Bound::Buffer(b, o, s) => w::BindingResource::Buffer(w::BufferBinding {
                        buffer: b,
                        offset: *o,
                        size: Some(*s),
                    }),
                    Bound::Texture(t) => w::BindingResource::TextureView(t),
                    Bound::Sampler(s) => w::BindingResource::Sampler(s),
                },
            })
            .collect::<Vec<_>>();
        groups.push(device.create_bind_group(&w::BindGroupDescriptor {
            label: Some("immutable versioned draw inputs"),
            layout: l,
            entries: &entries,
        }));
    }
    let cv = d
        .colors
        .iter()
        .map(|a| a.as_ref().map(|a| view(&r.textures[&a.resource], a)))
        .collect::<Vec<_>>();
    let colors = cv
        .iter()
        .map(|v| {
            v.as_ref().map(|v| w::RenderPassColorAttachment {
                view: v,
                depth_slice: None,
                resolve_target: None,
                ops: w::Operations {
                    load: w::LoadOp::Load,
                    store: w::StoreOp::Store,
                },
            })
        })
        .collect::<Vec<_>>();
    let dv = d
        .depth
        .as_ref()
        .map(|a| view(&r.textures[&a.attachment.resource], &a.attachment));
    let ds = d
        .depth
        .as_ref()
        .map(|a| w::RenderPassDepthStencilAttachment {
            view: dv.as_ref().unwrap(),
            depth_ops: if a.read_only_depth {
                None
            } else {
                Some(w::Operations {
                    load: w::LoadOp::Load,
                    store: w::StoreOp::Store,
                })
            },
            stencil_ops: if a.read_only_stencil
                || r.textures[&a.attachment.resource].format() == w::TextureFormat::Depth32Float
            {
                None
            } else {
                Some(w::Operations {
                    load: w::LoadOp::Load,
                    store: w::StoreOp::Store,
                })
            },
        });
    let mut e = device.create_command_encoder(&w::CommandEncoderDescriptor {
        label: Some("ordered heterogeneous original draw"),
    });
    {
        let mut p = e.begin_render_pass(&w::RenderPassDescriptor {
            label: Some("persistent original attachments"),
            color_attachments: &colors,
            depth_stencil_attachment: ds,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_pipeline(&pipe.gpu);
        for (i, g) in groups.iter().enumerate() {
            p.set_bind_group(i as u32, g, &[]);
        }
        for b in &d.vertex_buffers {
            p.set_vertex_buffer(b.slot, r.buffers[&b.buffer].slice(b.offset..));
        }
        let v = d.viewport;
        p.set_viewport(v[0], v[1], v[2], v[3], v[4], v[5]);
        p.set_scissor_rect(d.scissor[0], d.scissor[1], d.scissor[2], d.scissor[3]);
        p.set_blend_constant(w::Color {
            r: d.blend[0],
            g: d.blend[1],
            b: d.blend[2],
            a: d.blend[3],
        });
        p.set_stencil_reference(d.stencil);
        match d.args {
            DrawArgs::Direct {
                first_vertex,
                vertices,
                first_instance,
                instances,
            } => p.draw(
                first_vertex..first_vertex + vertices,
                first_instance..first_instance + instances,
            ),
            DrawArgs::Indexed {
                first_index,
                indices,
                base_vertex,
                first_instance,
                instances,
            } => {
                let i = d.index_buffer.as_ref().unwrap();
                p.set_index_buffer(r.buffers[&i.buffer].slice(i.offset..), i.format);
                p.draw_indexed(
                    first_index..first_index + indices,
                    base_vertex,
                    first_instance..first_instance + instances,
                );
            }
        }
    }
    q.submit([e.finish()]);
    Ok(())
}
fn read(
    device: &w::Device,
    q: &w::Queue,
    t: &w::Texture,
    s: &Slice,
    input: &Input,
) -> Result<Vec<u8>> {
    let (width, height, row, rows) =
        slice_bytes(&input.texture(&s.resource)?.descriptor, s.mip, &s.aspect)?;
    let pitch = row.div_ceil(256) * 256;
    let b = device.create_buffer(&w::BufferDescriptor {
        label: Some("original aspect verification"),
        size: pitch as u64 * rows as u64,
        usage: w::BufferUsages::COPY_DST | w::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = device.create_command_encoder(&Default::default());
    let (bw, bh) = t.format().block_dimensions();
    e.copy_texture_to_buffer(
        w::TexelCopyTextureInfo {
            texture: t,
            mip_level: s.mip,
            origin: w::Origin3d {
                x: 0,
                y: 0,
                z: s.layer,
            },
            aspect: s.aspect.gpu(),
        },
        w::TexelCopyBufferInfo {
            buffer: &b,
            layout: w::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(rows),
            },
        },
        w::Extent3d {
            width: width.div_ceil(bw) * bw,
            height: height.div_ceil(bh) * bh,
            depth_or_array_layers: 1,
        },
    );
    let sub = q.submit([e.finish()]);
    device.poll(w::PollType::Wait {
        submission_index: Some(sub),
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    let (tx, rx) = mpsc::channel();
    b.slice(..).map_async(w::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(w::PollType::Wait {
        submission_index: None,
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    rx.recv()??;
    let map = b.slice(..).get_mapped_range()?;
    let data = (0..rows as usize)
        .flat_map(|y| map[y * pitch as usize..y * pitch as usize + row as usize].to_vec())
        .collect();
    drop(map);
    b.unmap();
    Ok(data)
}
pub fn run(input: &Input) -> Result<Output> {
    let mut desc = w::InstanceDescriptor::new_without_display_handle();
    desc.backends = w::Backends::METAL;
    let instance = w::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&w::RequestAdapterOptions {
        power_preference: w::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    let wanted = w::Features::TEXTURE_COMPRESSION_BC
        | w::Features::DEPTH32FLOAT_STENCIL8
        | w::Features::DEPTH_CLIP_CONTROL
        | w::Features::FLOAT32_FILTERABLE
        | w::Features::DUAL_SOURCE_BLENDING
        | w::Features::CLIP_DISTANCES
        | w::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    let (device, q) = pollster::block_on(adapter.request_device(&w::DeviceDescriptor {
        label: Some("generic immutable native scene replay"),
        required_features: wanted & adapter.features(),
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;
    let scope = device.push_error_scope(w::ErrorFilter::Validation);
    let mut r = Resources {
        textures: BTreeMap::new(),
        buffers: BTreeMap::new(),
        pipelines: BTreeMap::new(),
    };
    for t in &input.scene.textures {
        let d = &t.descriptor;
        let mut usage = w::TextureUsages::COPY_DST
            | w::TextureUsages::COPY_SRC
            | w::TextureUsages::TEXTURE_BINDING;
        if t.mutable_target
            || matches!(
                d.format,
                w::TextureFormat::Depth32Float | w::TextureFormat::Depth32FloatStencil8
            )
        {
            usage |= w::TextureUsages::RENDER_ATTACHMENT;
        }
        let tex = device.create_texture(&w::TextureDescriptor {
            label: Some(&t.id),
            size: w::Extent3d {
                width: d.width,
                height: d.height,
                depth_or_array_layers: d.layers,
            },
            mip_level_count: d.mips,
            sample_count: 1,
            dimension: w::TextureDimension::D2,
            format: d.format,
            usage,
            view_formats: &d.view_formats,
        });
        for i in &t.initial {
            let key = Slice {
                resource: t.id.clone(),
                mip: i.mip,
                layer: i.layer,
                aspect: i.aspect.clone(),
            };
            match &i.origin {
                Initial::Unknown { .. } => {}
                Initial::UniformClear {
                    value, view_format, ..
                } => clear(&device, &q, &tex, &key, value, *view_format),
                Initial::Captured { blob } => {
                    let (w, h, row, _) = slice_bytes(d, i.mip, &i.aspect)?;
                    let bytes = input.data(blob)?;
                    if i.aspect == Aspect::Depth {
                        restore_depth(&device, &q, &tex, &key, bytes, w, h)?;
                    } else {
                        upload(&q, &tex, i.mip, i.layer, i.aspect.gpu(), w, h, bytes, row);
                    }
                }
            }
        }
        r.textures.insert(t.id.clone(), tex);
    }
    let mut initial_checks = 0;
    for t in &input.scene.textures {
        for i in &t.initial {
            if let Initial::Captured { blob } = &i.origin {
                let slice = Slice {
                    resource: t.id.clone(),
                    mip: i.mip,
                    layer: i.layer,
                    aspect: i.aspect.clone(),
                };
                require(
                    read(&device, &q, &r.textures[&t.id], &slice, input)? == input.data(blob)?,
                    "captured initial import differs",
                )?;
                initial_checks += 1;
            }
        }
    }
    for b in &input.scene.buffers {
        r.buffers.insert(
            b.id.clone(),
            device.create_buffer_init(&w::util::BufferInitDescriptor {
                label: Some(&b.id),
                contents: input.data(&b.blob)?,
                usage: w::BufferUsages::VERTEX
                    | w::BufferUsages::INDEX
                    | w::BufferUsages::UNIFORM
                    | w::BufferUsages::STORAGE,
            }),
        );
    }
    for p in &input.scene.pipelines {
        r.pipelines.insert(p.id.clone(), compile(&device, p));
    }
    for c in &input.scene.commands {
        match c {
            Command::Clear {
                transition,
                value,
                view_format,
            } => clear(
                &device,
                &q,
                &r.textures[&transition.slice.resource],
                &transition.slice,
                value,
                *view_format,
            ),
            Command::Draw { draw: d } => draw(&device, &q, &r, d)?,
            Command::Copy {
                source,
                destination,
            } => {
                let t = &r.textures[&source.slice.resource];
                let a = &source.slice;
                let b = &destination.slice;
                let (w, h, _, _) =
                    slice_bytes(&input.texture(&a.resource)?.descriptor, a.mip, &a.aspect)?;
                let mut e = device.create_command_encoder(&Default::default());
                e.copy_texture_to_texture(
                    w::TexelCopyTextureInfo {
                        texture: t,
                        mip_level: a.mip,
                        origin: w::Origin3d {
                            x: 0,
                            y: 0,
                            z: a.layer,
                        },
                        aspect: a.aspect.gpu(),
                    },
                    w::TexelCopyTextureInfo {
                        texture: &r.textures[&b.resource],
                        mip_level: b.mip,
                        origin: w::Origin3d {
                            x: 0,
                            y: 0,
                            z: b.layer,
                        },
                        aspect: b.aspect.gpu(),
                    },
                    w::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                q.submit([e.finish()]);
            }
        }
    }
    let mut output = Vec::new();
    let mut comparisons = Vec::new();
    for ex in &input.scene.expected {
        let bytes = read(
            &device,
            &q,
            &r.textures[&ex.version.slice.resource],
            &ex.version.slice,
            input,
        )?;
        let expected = input.data(&ex.blob)?;
        let count = bytes.iter().zip(expected).filter(|(a, b)| a != b).count();
        comparisons.push(serde_json::json!({"version":ex.version,"sha256":sha(&bytes),"expected_sha256":sha(expected),"different_bytes":count}));
        output.push(bytes);
    }
    let error = pollster::block_on(scope.pop());
    require(error.is_none(), &format!("replay validation: {error:?}"))?;
    let exact = comparisons.iter().all(|v| v["different_bytes"] == 0);
    Ok(Output {
        aspect_bytes: output,
        report: serde_json::json!({"schema":1,"gpu_executed":true,"manifest_sha256":input.manifest_sha256,"provenance":input.scene.provenance,"eye":input.scene.eye,"initial_subresources_byte_exact":initial_checks,"all_expected_bytes_equal":exact,"comparisons":comparisons,"heterogeneous_pipeline_count":r.pipelines.len(),"command_count":input.scene.commands.len(),"targets_have_resolution_independent_initialization":input.targets_have_resolution_independent_initialization,"finished_frame_or_density_accepted":false,"scope":"Generic original-density ordered replay. External dependency and complete finished-frame closure remain separate gates.","adapter":format!("{:?}",adapter.get_info())}),
    })
}

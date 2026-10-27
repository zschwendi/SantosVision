//! Authored API regression only. Models the documented D3D binding transition;
//! it neither invokes native guest commands nor proves the actual game's cause.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, ops::Range, path::PathBuf, sync::mpsc};
use wgpu::*;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SIDE: u32 = 16;
const MATERIAL: [u8; 4] = [51, 102, 153, 204];
const SENTINEL: [u8; 4] = [7, 9, 11, 13];
#[derive(Clone, Debug, PartialEq)]
struct Subresource {
    parent: u32,
    mips: Range<u32>,
    layers: Range<u32>,
}
fn overlaps(a: &Subresource, b: &Subresource) -> bool {
    a.parent == b.parent
        && a.mips.start < b.mips.end
        && b.mips.start < a.mips.end
        && a.layers.start < b.layers.end
        && b.layers.start < a.layers.end
}
#[derive(Default)]
struct BindingModel {
    input: Option<Subresource>,
    output: Option<Subresource>,
}
impl BindingModel {
    fn input(&mut self, value: Subresource) {
        self.input = (!self.output.as_ref().is_some_and(|o| overlaps(o, &value))).then_some(value);
    }
    fn output(&mut self, value: Option<Subresource>) {
        if value
            .as_ref()
            .is_some_and(|o| self.input.as_ref().is_some_and(|i| overlaps(i, o)))
        {
            self.input = None;
        }
        self.output = value;
    }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn texture(device: &Device, layers: u32) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("authored overlap fixture"),
        size: Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: layers,
        },
        mip_level_count: 2,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::TEXTURE_BINDING
            | TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
fn view(t: &Texture, mip: u32, count: u32, layer: u32) -> TextureView {
    t.create_view(&TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2),
        base_mip_level: mip,
        mip_level_count: Some(count),
        base_array_layer: layer,
        array_layer_count: Some(1),
        ..Default::default()
    })
}
fn upload(queue: &Queue, t: &Texture, mip: u32, layer: u32, value: [u8; 4]) {
    let side = SIDE >> mip;
    queue.write_texture(
        TexelCopyTextureInfo {
            texture: t,
            mip_level: mip,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        &value.repeat((side * side) as usize),
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(side * 4),
            rows_per_image: Some(side),
        },
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
    );
}
fn read(device: &Device, queue: &Queue, t: &Texture, mip: u32, layer: u32) -> Result<Vec<u8>> {
    let side = SIDE >> mip;
    let pitch = 256;
    let b = device.create_buffer(&BufferDescriptor {
        label: None,
        size: (pitch * side) as u64,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture: t,
            mip_level: mip,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &b,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(side),
            },
        },
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([e.finish()]);
    let (tx, rx) = mpsc::channel();
    b.slice(..).map_async(MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(PollType::Wait {
        submission_index: None,
        timeout: Some(std::time::Duration::from_secs(10)),
    })?;
    rx.recv()??;
    let m = b.slice(..).get_mapped_range()?;
    let result = (0..side)
        .flat_map(|y| m[(y * pitch) as usize..(y * pitch + side * 4) as usize].to_vec())
        .collect();
    drop(m);
    b.unmap();
    Ok(result)
}
fn pipeline(device: &Device, layout: &BindGroupLayout, unused: bool) -> RenderPipeline {
    let source = format!("@group(0) @binding(0) var t:texture_2d<f32>; @vertex fn vs(@builtin(vertex_index)i:u32)->@builtin(position)vec4f{{var p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3));return vec4f(p[i],0,1);}} struct O{{@location(0)a:vec4f,@location(1)b:vec4f}} @fragment fn fs()->O{{return O({},vec4f(0,1,0,1));}}",
        if unused { "vec4f(1,0,0,1)" } else { "textureLoad(t,vec2i(0,0),0)" });
    let module = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("authored overlap shader"),
        source: ShaderSource::Wgsl(source.into()),
    });
    let pl = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    let target = Some(ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: None,
        write_mask: ColorWrites::ALL,
    });
    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[target.clone(), target],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
fn group(device: &Device, layout: &BindGroupLayout, view: &TextureView) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: BindingResource::TextureView(view),
        }],
    })
}
fn draw(
    device: &Device,
    queue: &Queue,
    pipeline: &RenderPipeline,
    outputs: [&TextureView; 2],
    inputs: &[&BindGroup],
    second: Option<&BindGroup>,
    side: u32,
) -> Result<Option<String>> {
    let scope = device.push_error_scope(ErrorFilter::Validation);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let colors = outputs.map(|v| {
            Some(RenderPassColorAttachment {
                view: v,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                },
            })
        });
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            color_attachments: &colors,
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        for g in inputs {
            pass.set_bind_group(0, *g, &[]);
        }
        pass.draw(0..3, 0..1);
        if let Some(g) = second {
            pass.set_bind_group(0, g, &[]);
            pass.set_scissor_rect(side / 2, 0, side / 2, side);
            pass.draw(0..3, 0..1);
        }
    }
    queue.submit([encoder.finish()]);
    device.poll(PollType::Wait {
        submission_index: None,
        timeout: Some(std::time::Duration::from_secs(10)),
    })?;
    Ok(pollster::block_on(scope.pop()).map(|e| e.to_string()))
}
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: srv_overlap NEW_OUTPUT_DIRECTORY".into());
    }
    let out = PathBuf::from(&args[1]);
    fs::create_dir(&out)?;
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = Backends::METAL;
    let instance = Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
        power_preference: PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor {
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;
    let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: None,
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Texture {
                sample_type: TextureSampleType::Float { filterable: true },
                view_dimension: TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let used = pipeline(&device, &layout, false);
    let unused = pipeline(&device, &layout, true);
    let material = texture(&device, 1);
    upload(&queue, &material, 0, 0, MATERIAL);
    let dummy = texture(&device, 1); // Like native dummy_texture: WebGPU zero-initialization, no synthetic color substitution.
    let safe = group(&device, &layout, &view(&material, 0, 1, 0));
    let zero = group(&device, &layout, &view(&dummy, 0, 1, 0));
    let mut cases: Vec<Value> = Vec::new();
    for (name, unused_binding, replace_before_draw, broad_mips) in [
        ("overlap_consumed", false, false, false),
        ("overlap_declared_unused", true, false, false),
        ("overlap_replaced_before_draw", false, true, false),
        ("overlap_broad_mip_view", false, false, true),
    ] {
        let a = texture(&device, 1);
        let b = texture(&device, 1);
        let mip = if broad_mips { 1 } else { 0 };
        upload(&queue, &a, mip, 0, SENTINEL);
        upload(&queue, &b, mip, 0, SENTINEL);
        let overlap = group(
            &device,
            &layout,
            &view(&a, 0, if broad_mips { 2 } else { 1 }, 0),
        );
        let mut inputs = vec![&overlap];
        if replace_before_draw {
            inputs.push(&safe);
        }
        let error = draw(
            &device,
            &queue,
            if unused_binding { &unused } else { &used },
            [&view(&a, mip, 1, 0), &view(&b, mip, 1, 0)],
            &inputs,
            None,
            SIDE >> mip,
        )?;
        let error = error.ok_or("expected overlap validation failure missing")?;
        if !error.contains("conflicting usages") {
            return Err(error.into());
        }
        let original = read(&device, &queue, &a, mip, 0)?;
        let original_b = read(&device, &queue, &b, mip, 0)?;
        assert_eq!(original, SENTINEL.repeat(((SIDE >> mip).pow(2)) as usize));
        assert_eq!(original, original_b);
        cases.push(json!({"name":name,"expected_validation_error":error,"original_target_unchanged":true,"other_target_unchanged":true}));
    }
    for name in [
        "input_then_output",
        "output_then_input",
        "null_persists_after_output_unbind",
        "disjoint_mip",
        "disjoint_layer",
    ] {
        let a = texture(&device, 2);
        let b = texture(&device, 1);
        let untouched = texture(&device, 1);
        let mip = if name == "disjoint_mip" { 1 } else { 0 };
        let side = SIDE >> mip;
        upload(&queue, &a, mip, 0, SENTINEL);
        upload(&queue, &b, mip, 0, SENTINEL);
        upload(&queue, &untouched, 0, 0, SENTINEL);
        let output = Subresource {
            parent: 1,
            mips: mip..mip + 1,
            layers: 0..1,
        };
        let source_layer = if name == "disjoint_layer" { 1 } else { 0 };
        let input = Subresource {
            parent: 1,
            mips: 0..1,
            layers: source_layer..source_layer + 1,
        };
        let mut model = BindingModel::default();
        if name == "output_then_input" {
            model.output(Some(output.clone()));
            model.input(input.clone());
        } else {
            model.input(input.clone());
            model.output(Some(output.clone()));
        }
        if name == "null_persists_after_output_unbind" {
            model.output(None);
            assert!(model.input.is_none());
            model.output(Some(output.clone()));
        }
        let disjoint = name.starts_with("disjoint");
        assert_eq!(model.input.is_some(), disjoint);
        let source = if disjoint {
            upload(&queue, &a, 0, source_layer, MATERIAL);
            group(&device, &layout, &view(&a, 0, 1, source_layer))
        } else {
            zero.clone()
        };
        let error = draw(
            &device,
            &queue,
            &used,
            [&view(&a, mip, 1, 0), &view(&b, mip, 1, 0)],
            &[&source],
            Some(&safe),
            side,
        )?;
        if let Some(e) = error {
            return Err(e.into());
        }
        let actual = read(&device, &queue, &a, mip, 0)?;
        let other = read(&device, &queue, &b, mip, 0)?;
        let expected: Vec<u8> = (0..side)
            .flat_map(|_| {
                (0..side).flat_map(|x| {
                    if disjoint || x >= side / 2 {
                        MATERIAL
                    } else {
                        [0; 4]
                    }
                })
            })
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(other, [0, 255, 0, 255].repeat((side * side) as usize));
        assert_eq!(
            read(&device, &queue, &untouched, 0, 0)?,
            SENTINEL.repeat((SIDE * SIDE) as usize)
        );
        fs::write(out.join(format!("{name}.rgba")), &actual)?;
        cases.push(json!({"name":name,"null_binding":!disjoint,"two_draws_and_other_target_exact":true,"untouched_texture_exact":true,
            "changed_pixels":side*side,"sha256":sha(&actual),"validation_error":null}));
    }
    let source = include_bytes!("srv_overlap.rs");
    fs::write(out.join("source.rs"), source)?;
    fs::write(
        out.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"schema":1,"scope":"Authored API regression and documented binding-state model only; not actual game cause or native-command acceptance",
        "adapter":format!("{:?}",adapter.get_info()),"source_sha256":sha(source),"expected_failed_passes":4,"positive_two_draw_passes":5,"cases":cases,"passed":true}),
        )?,
    )?;
    println!("PASS: four expected overlap failures, five legal persistent two-draw passes; exact pixels and preservation");
    Ok(())
}

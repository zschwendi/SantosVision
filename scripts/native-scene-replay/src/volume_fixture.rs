//! Authored D1/D3 API oracle. No game bytes, camera or finished-image claim.
use crate::{model::*, sha, validate::Input, Result};
use std::{collections::BTreeMap, fs, path::Path};
use wgpu_types::*;

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const VERTEX: &str = r#"
@group(0) @binding(0) var line: texture_1d<f32>;
struct Out {
    @builtin(position) position: vec4f,
    @location(0) @interpolate(flat) vertex_line: f32,
};
@vertex fn main(@builtin(vertex_index) i: u32) -> Out {
    var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    var out: Out;
    out.position = vec4f(p[i], 0.5, 1);
    out.vertex_line = textureLoad(line, 3, 0).r;
    return out;
}"#;
const LOAD_FRAGMENT: &str = r#"
@group(0) @binding(0) var line: texture_1d<f32>;
@group(1) @binding(0) var volume: texture_3d<f32>;
@fragment fn main(@builtin(position) p: vec4f,
    @location(0) @interpolate(flat) vertex_line: f32) -> @location(0) vec4f {
    let x = u32(p.x); let y = u32(p.y);
    let cell = vec3i(i32(x % 2u), i32((x / 2u) % 2u), i32((x / 4u + y) % 2u));
    let v = textureLoad(volume, cell, 0);
    let l = textureLoad(line, i32(x), 0);
    return vec4f(v.r, v.g, l.b, vertex_line);
}"#;
const LINEAR_FRAGMENT: &str = r#"
@group(0) @binding(0) var line: texture_1d<f32>;
@group(1) @binding(0) var volume: texture_3d<f32>;
@group(1) @binding(1) var linear_sampler: sampler;
@fragment fn main(@builtin(position) p: vec4f,
    @location(0) @interpolate(flat) vertex_line: f32) -> @location(0) vec4f {
    let v = textureSampleLevel(volume, linear_sampler, vec3f(0.5), 0.0);
    let l = textureLoad(line, i32(p.x), 0);
    return vec4f(v.r, v.g, l.b, vertex_line);
}"#;
const DEPTH_VERTEX: &str = r#"
@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    var p = array<vec2f, 3>(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    return vec4f(p[i], 0.25, 1);
}"#;

fn shader(text: &str) -> Shader {
    Shader {
        wgsl: text.into(),
        sha256: sha(text.as_bytes()),
        entry: "main".into(),
        exact_final_variant: true,
    }
}
fn slice(id: &str, mip: u32, aspect: Aspect) -> Slice {
    Slice {
        resource: id.into(),
        mip,
        layer: 0,
        aspect,
    }
}
fn blob(
    scene: &mut Scene,
    files: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    bytes: Vec<u8>,
) -> String {
    scene.blobs.push(Blob {
        path: name.into(),
        bytes: bytes.len() as u64,
        sha256: sha(&bytes),
    });
    files.insert(name.into(), bytes);
    name.into()
}
fn line_pixel(x: u8) -> [u8; 4] {
    [24 + 16 * x, 40 + 8 * x, 16 + 24 * x, 255]
}
fn volume_pixel(mip: u32, x: u8, y: u8, z: u8) -> [u8; 4] {
    if mip == 1 {
        // Affine, integer-exact eight-corner means are R44/G60/B88.
        [
            16 + 32 * x + 16 * y + 8 * z,
            32 + 16 * x + 32 * y + 8 * z,
            48 + 8 * x + 16 * y + 56 * z,
            200 + x + 2 * y + 4 * z,
        ]
    } else {
        [
            128 + 3 * x + 5 * y + 7 * z,
            16 + 11 * x + 13 * y + 17 * z,
            32 + 19 * x + 7 * y + 5 * z,
            255,
        ]
    }
}
fn volume_bytes(mip: u32) -> Vec<u8> {
    let side = (4u8 >> mip).max(1);
    let mut bytes = Vec::new();
    for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                bytes.extend(volume_pixel(mip, x, y, z));
            }
        }
    }
    bytes
}
fn expected_color() -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            // Right half is overwritten by the explicit linear-filter control.
            // Left half visits every voxel in the selected 2x2x2 mip over two rows.
            let (red, green) = if x >= 4 {
                (44, 60)
            } else {
                let (vx, vy, vz) = (x % 2, (x / 2) % 2, y % 2);
                (
                    (16 + 32 * vx + 16 * vy + 8 * vz) as u8,
                    (32 + 16 * vx + 32 * vy + 8 * vz) as u8,
                )
            };
            // Independent byte-domain oracle; not a readback of a rendered image.
            bytes.extend([red, green, (16 + 24 * x) as u8, 72]);
        }
    }
    bytes
}
fn texture_layout(
    binding: u32,
    visibility: ShaderStages,
    dimension: TextureViewDimension,
) -> BindGroupLayoutEntry {
    BindGroupLayoutEntry {
        binding,
        visibility,
        ty: BindingType::Texture {
            sample_type: TextureSampleType::Float { filterable: true },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}
fn draw(pipeline: &str) -> Draw {
    Draw {
        eye: None,
        pipeline: pipeline.into(),
        bindings: vec![],
        vertex_buffers: vec![],
        index_buffer: None,
        args: DrawArgs::Direct {
            first_vertex: 0,
            vertices: 3,
            first_instance: 0,
            instances: 1,
        },
        colors: vec![],
        depth: None,
        viewport: [0., 0., WIDTH as f32, HEIGHT as f32, 0., 1.],
        scissor: [0, 0, WIDTH, HEIGHT],
        blend: [0.; 4],
        stencil: 0,
        writes: vec![],
    }
}

pub fn make(out: &Path) -> Result<()> {
    let mut scene = Scene { schema: 1,
        provenance: Provenance::Authored { description: "8x4 independent CPU oracle: D1 vertex/fragment texel loads, selected D3 mip whole-volume restore, linear3D eight-voxel blend, then valid depth-only/no-color pass. No native game proof.".into() },
        eye: None, blobs: vec![], textures: vec![], buffers: vec![], pipelines: vec![], commands: vec![], expected: vec![],
        original_submission_completed: true, original_gpu_errors: Some(0), omitted_commands: 0 };
    let mut files = BTreeMap::new();
    for (id, format, aspect) in [
        ("volume-color", TextureFormat::Rgba8Unorm, Aspect::Color),
        ("volume-depth", TextureFormat::Depth32Float, Aspect::Depth),
    ] {
        scene.textures.push(Texture {
            id: id.into(),
            native_allocation_id: None,
            descriptor: TextureDesc {
                dimension: TextureDimension::D2,
                width: WIDTH,
                height: HEIGHT,
                layers: 1,
                mips: 1,
                format,
                view_formats: vec![],
            },
            mutable_target: true,
            initial: vec![SliceInitial {
                mip: 0,
                layer: 0,
                aspect,
                content_version: 0,
                origin: Initial::Unknown {
                    reason: "Explicit ordered clear initializes authored target".into(),
                },
            }],
        });
    }
    let line_blob = blob(
        &mut scene,
        &mut files,
        "line-mip0.bin",
        (0..8).flat_map(line_pixel).collect(),
    );
    scene.textures.push(Texture {
        id: "line".into(),
        native_allocation_id: None,
        descriptor: TextureDesc {
            dimension: TextureDimension::D1,
            width: 8,
            height: 1,
            layers: 1,
            mips: 1,
            format: TextureFormat::Rgba8Unorm,
            view_formats: vec![],
        },
        mutable_target: false,
        initial: vec![SliceInitial {
            mip: 0,
            layer: 0,
            aspect: Aspect::Color,
            content_version: 1,
            origin: Initial::Captured { blob: line_blob },
        }],
    });
    let mut initial = vec![];
    for mip in 0..2 {
        let name = blob(
            &mut scene,
            &mut files,
            &format!("volume-mip{mip}.bin"),
            volume_bytes(mip),
        );
        initial.push(SliceInitial {
            mip,
            layer: 0,
            aspect: Aspect::Color,
            content_version: 1,
            origin: Initial::Captured { blob: name },
        });
    }
    scene.textures.push(Texture {
        id: "volume".into(),
        native_allocation_id: None,
        descriptor: TextureDesc {
            dimension: TextureDimension::D3,
            width: 4,
            height: 4,
            layers: 4,
            mips: 2,
            format: TextureFormat::Rgba8Unorm,
            view_formats: vec![],
        },
        mutable_target: false,
        initial,
    });
    for (id, fragment, linear) in [
        ("volume-load", LOAD_FRAGMENT, false),
        ("volume-linear", LINEAR_FRAGMENT, true),
    ] {
        let mut fragment_layout = vec![texture_layout(
            0,
            ShaderStages::FRAGMENT,
            TextureViewDimension::D3,
        )];
        if linear {
            fragment_layout.push(BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::Filtering),
                count: None,
            });
        }
        scene.pipelines.push(Pipeline {
            id: id.into(),
            vertex: shader(VERTEX),
            fragment: Some(shader(fragment)),
            layouts: vec![
                vec![texture_layout(
                    0,
                    ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                    TextureViewDimension::D1,
                )],
                fragment_layout,
            ],
            vertex_layouts: vec![],
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        });
    }
    scene.pipelines.push(Pipeline {
        id: "depth-only".into(),
        vertex: shader(DEPTH_VERTEX),
        fragment: None,
        layouts: vec![],
        vertex_layouts: vec![],
        primitive: PrimitiveState::default(),
        depth_stencil: Some(DepthStencilState {
            format: TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        targets: vec![],
    });
    scene.commands.push(Command::Clear {
        transition: Transition {
            slice: slice("volume-color", 0, Aspect::Color),
            before: 0,
            after: 1,
        },
        value: ClearValue::Color {
            rgba_bits: [0., 0., 0., 1.].map(f64::to_bits),
        },
        view_format: TextureFormat::Rgba8Unorm,
    });
    scene.commands.push(Command::Clear {
        transition: Transition {
            slice: slice("volume-depth", 0, Aspect::Depth),
            before: 0,
            after: 2,
        },
        value: ClearValue::Depth {
            value_bits: 0.875f32.to_bits(),
        },
        view_format: TextureFormat::Depth32Float,
    });
    for (index, id) in ["volume-load", "volume-linear"].into_iter().enumerate() {
        let mut d = draw(id);
        d.colors = vec![Some(Attachment {
            resource: "volume-color".into(),
            mip: 0,
            layer: 0,
            view_format: TextureFormat::Rgba8Unorm,
        })];
        d.bindings = vec![
            Binding {
                group: 0,
                binding: 0,
                value: BindingValue::Texture {
                    texture: "line".into(),
                    view: TextureViewDescriptor {
                        dimension: Some(TextureViewDimension::D1),
                        base_mip_level: 0,
                        mip_level_count: Some(1),
                        ..Default::default()
                    },
                    versions: vec![Version {
                        slice: slice("line", 0, Aspect::Color),
                        content_version: 1,
                    }],
                },
            },
            Binding {
                group: 1,
                binding: 0,
                value: BindingValue::Texture {
                    texture: "volume".into(),
                    view: TextureViewDescriptor {
                        dimension: Some(TextureViewDimension::D3),
                        base_mip_level: 1,
                        mip_level_count: Some(1),
                        ..Default::default()
                    },
                    versions: vec![Version {
                        slice: slice("volume", 1, Aspect::Color),
                        content_version: 1,
                    }],
                },
            },
        ];
        if index == 1 {
            d.scissor = [4, 0, 4, HEIGHT];
            d.bindings.push(Binding {
                group: 1,
                binding: 1,
                value: BindingValue::Sampler {
                    descriptor: SamplerDescriptor {
                        mag_filter: FilterMode::Linear,
                        min_filter: FilterMode::Linear,
                        mipmap_filter: MipmapFilterMode::Nearest,
                        ..Default::default()
                    },
                },
            });
        }
        d.writes.push(Transition {
            slice: slice("volume-color", 0, Aspect::Color),
            before: if index == 0 { 1 } else { 3 },
            after: 3 + index as u64,
        });
        scene.commands.push(Command::Draw { draw: d });
    }
    // Valid control: the original color resource persists but is NOT attached
    // to this no-fragment pipeline. Binding colors with no PS is a separate
    // rejection test; silently unbinding a captured color would change its API.
    let mut depth = draw("depth-only");
    depth.depth = Some(DepthAttachment {
        attachment: Attachment {
            resource: "volume-depth".into(),
            mip: 0,
            layer: 0,
            view_format: TextureFormat::Depth32Float,
        },
        read_only_depth: false,
        read_only_stencil: true,
    });
    depth.writes.push(Transition {
        slice: slice("volume-depth", 0, Aspect::Depth),
        before: 2,
        after: 5,
    });
    scene.commands.push(Command::Draw { draw: depth });
    let color = blob(
        &mut scene,
        &mut files,
        "expected-color.bin",
        expected_color(),
    );
    let depth = blob(
        &mut scene,
        &mut files,
        "expected-depth.bin",
        0.25f32.to_le_bytes().repeat((WIDTH * HEIGHT) as usize),
    );
    scene.expected = vec![
        Expected {
            version: Version {
                slice: slice("volume-color", 0, Aspect::Color),
                content_version: 4,
            },
            blob: color,
        },
        Expected {
            version: Version {
                slice: slice("volume-depth", 0, Aspect::Depth),
                content_version: 5,
            },
            blob: depth,
        },
    ];
    fs::create_dir(out)?;
    for (name, bytes) in files {
        fs::write(out.join(name), bytes)?;
    }
    fs::write(out.join("scene.json"), serde_json::to_vec_pretty(&scene)?)?;
    Input::load(&out.join("scene.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_volume_data_and_oracle_cover_axes_mips_and_stages() {
        assert_eq!(volume_bytes(0).len(), 4 * 4 * 4 * 4);
        assert_eq!(volume_bytes(1).len(), 2 * 2 * 2 * 4);
        let mip = volume_bytes(1);
        assert_eq!(&mip[0..4], &[16, 32, 48, 200]);
        assert_eq!(&mip[28..32], &[72, 88, 128, 207]);
        assert_eq!(
            mip.chunks_exact(4).map(|p| p[0] as u32).sum::<u32>(),
            44 * 8
        );
        assert_eq!(
            mip.chunks_exact(4).map(|p| p[1] as u32).sum::<u32>(),
            60 * 8
        );
        let expected = expected_color();
        assert_eq!(expected.len(), (WIDTH * HEIGHT * 4) as usize);
        assert_eq!(&expected[0..4], &[16, 32, 16, 72]);
        assert_eq!(&expected[16..20], &[44, 60, 112, 72]);
        assert_eq!(&expected[32..36], &[24, 40, 16, 72]);
    }
}

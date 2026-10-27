//! Resolved commands, never guest pointers or mutable guest object IDs.
use serde::{Deserialize, Serialize};
use wgpu_types::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    Native { source_sha256: String },
    LegacyCaptured { manifest_sha256: String },
    Authored { description: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EyeIdentity {
    pub epoch: u64,
    pub camera: u64,
    pub request: u64,
    pub head: u64,
    pub eye: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    Color,
    Depth,
    Stencil,
}
impl Aspect {
    pub fn gpu(&self) -> TextureAspect {
        match self {
            Self::Color => TextureAspect::All,
            Self::Depth => TextureAspect::DepthOnly,
            Self::Stencil => TextureAspect::StencilOnly,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slice {
    pub resource: String,
    pub mip: u32,
    pub layer: u32,
    pub aspect: Aspect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub slice: Slice,
    pub content_version: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transition {
    pub slice: Slice,
    pub before: u64,
    pub after: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextureDesc {
    #[serde(default = "default_texture_dimension")]
    pub dimension: TextureDimension,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub mips: u32,
    pub format: TextureFormat,
    pub view_formats: Vec<TextureFormat>,
}
fn default_texture_dimension() -> TextureDimension {
    TextureDimension::D2
}
impl TextureDesc {
    /// D3 has one versioned whole-volume subresource per mip, never array slices.
    pub fn subresource_layers(&self) -> u32 {
        if self.dimension == TextureDimension::D2 {
            self.layers
        } else {
            1
        }
    }
    pub fn mip_depth(&self, mip: u32) -> u32 {
        if self.dimension == TextureDimension::D3 {
            (self.layers >> mip).max(1)
        } else {
            1
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClearValue {
    Color { rgba_bits: [u64; 4] },
    Depth { value_bits: u32 },
    Stencil { value: u32 },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Initial {
    Unknown {
        reason: String,
    },
    Captured {
        blob: String,
    }, // Exact old-density parity only, never an analytic initializer.
    UniformClear {
        value: ClearValue,
        view_format: TextureFormat,
        operation: u64,
        /// Original-density byte reference; retained even when provenance proves a clear.
        #[serde(default)]
        reference_blob: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SliceInitial {
    pub mip: u32,
    pub layer: u32,
    pub aspect: Aspect,
    pub content_version: u64,
    pub origin: Initial,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Texture {
    pub id: String,
    pub native_allocation_id: Option<u64>,
    pub descriptor: TextureDesc,
    pub mutable_target: bool,
    pub initial: Vec<SliceInitial>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Buffer {
    pub id: String,
    pub native_allocation_id: Option<u64>,
    pub content_version: u64,
    pub source_offset: u64,
    pub blob: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VertexLayout {
    pub array_stride: u64,
    pub step_mode: VertexStepMode,
    pub attributes: Vec<VertexAttribute>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shader {
    pub wgsl: String,
    pub sha256: String,
    pub entry: String,
    pub exact_final_variant: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pipeline {
    pub id: String,
    pub vertex: Shader,
    pub fragment: Option<Shader>,
    pub layouts: Vec<Vec<BindGroupLayoutEntry>>,
    pub vertex_layouts: Vec<Option<VertexLayout>>,
    pub primitive: PrimitiveState,
    pub depth_stencil: Option<DepthStencilState>,
    pub multisample: MultisampleState,
    pub targets: Vec<Option<ColorTargetState>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComputePipeline {
    pub id: String,
    pub shader: Shader,
    pub layouts: Vec<Vec<BindGroupLayoutEntry>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dispatch {
    pub eye: Option<EyeIdentity>,
    pub pipeline: String,
    pub bindings: Vec<Binding>,
    pub workgroups: [u32; 3],
    pub writes: Vec<Transition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingValue {
    Buffer {
        buffer: String,
        offset: u64,
        size: u64,
    },
    Texture {
        texture: String,
        view: TextureViewDescriptor<Option<String>>,
        versions: Vec<Version>,
    },
    Sampler {
        descriptor: SamplerDescriptor<Option<String>>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    pub value: BindingValue,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VertexBuffer {
    pub slot: u32,
    pub buffer: String,
    pub offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexBuffer {
    pub buffer: String,
    pub offset: u64,
    pub format: IndexFormat,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrawArgs {
    Direct {
        first_vertex: u32,
        vertices: u32,
        first_instance: u32,
        instances: u32,
    },
    Indexed {
        first_index: u32,
        indices: u32,
        base_vertex: i32,
        first_instance: u32,
        instances: u32,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub resource: String,
    pub mip: u32,
    pub layer: u32,
    pub view_format: TextureFormat,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepthAttachment {
    pub attachment: Attachment,
    pub read_only_depth: bool,
    pub read_only_stencil: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draw {
    pub eye: Option<EyeIdentity>,
    pub pipeline: String,
    pub bindings: Vec<Binding>,
    pub vertex_buffers: Vec<VertexBuffer>,
    pub index_buffer: Option<IndexBuffer>,
    pub args: DrawArgs,
    pub colors: Vec<Option<Attachment>>,
    pub depth: Option<DepthAttachment>,
    pub viewport: [f32; 6],
    pub scissor: [u32; 4],
    pub blend: [f64; 4],
    pub stencil: u32,
    pub writes: Vec<Transition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    Clear {
        transition: Transition,
        value: ClearValue,
        view_format: TextureFormat,
    },
    Draw {
        draw: Draw,
    },
    Dispatch {
        dispatch: Dispatch,
    },
    Copy {
        source: Version,
        destination: Transition,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expected {
    pub version: Version,
    pub blob: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    pub schema: u32,
    pub provenance: Provenance,
    pub eye: Option<EyeIdentity>,
    pub blobs: Vec<Blob>,
    pub textures: Vec<Texture>,
    pub buffers: Vec<Buffer>,
    pub pipelines: Vec<Pipeline>,
    #[serde(default)]
    pub compute_pipelines: Vec<ComputePipeline>,
    pub commands: Vec<Command>,
    pub expected: Vec<Expected>,
    pub original_submission_completed: bool,
    pub original_gpu_errors: Option<u64>,
    pub omitted_commands: u64,
}

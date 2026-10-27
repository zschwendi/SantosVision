use crate::{model::*, require, sha, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};
use wgpu_types::*;
pub const MAX_BYTES: u64 = 256 << 20;
pub const MAX_FINISHED_PAYLOAD_BYTES:u64=1<<30;
pub fn payload_bound(p:&Provenance)->u64{if p.is_finished(){MAX_FINISHED_PAYLOAD_BYTES}else{MAX_BYTES}}
pub struct Input {
    pub scene: Scene,
    pub bytes: BTreeMap<String, Vec<u8>>,
    pub manifest_sha256: String,
    pub targets_have_resolution_independent_initialization: bool,
}
pub fn aspects(f: TextureFormat) -> Vec<Aspect> {
    if f == TextureFormat::Depth32FloatStencil8 {
        vec![Aspect::Depth, Aspect::Stencil]
    } else if f == TextureFormat::Depth32Float {
        vec![Aspect::Depth]
    } else {
        vec![Aspect::Color]
    }
}
pub fn slice_bytes(d: &TextureDesc, mip: u32, aspect: &Aspect) -> Result<(u32, u32, u32, u32)> {
    require(mip < d.mips, "mip outside allocation")?;
    let w = (d.width >> mip).max(1);
    let h = (d.height >> mip).max(1);
    let (bw, bh) = d.format.block_dimensions();
    let bpp = d
        .format
        .block_copy_size(Some(aspect.gpu()))
        .ok_or("uncopyable aspect")?;
    Ok((
        w,
        h,
        w.div_ceil(bw).checked_mul(bpp).ok_or("row overflow")?,
        h.div_ceil(bh)
            .checked_mul(d.mip_depth(mip))
            .ok_or("volume rows overflow")?,
    ))
}
impl Input {
    pub fn load(path: &Path) -> Result<Self> {
        require(fs::metadata(path)?.len() <= 32 << 20, "manifest limit")?;
        let raw = fs::read(path)?;
        require(raw.len() <= 32 << 20, "manifest limit")?;
        let scene: Scene = serde_json::from_slice(&raw)?;
        let root = path.parent().ok_or("manifest parent")?.canonicalize()?;
        let mut bytes = BTreeMap::new();
        let mut total = 0u64;
        for b in &scene.blobs {
            let rel = Path::new(&b.path);
            require(
                !rel.is_absolute() && rel.components().all(|c| matches!(c, Component::Normal(_))),
                "local blob path",
            )?;
            let p = root.join(rel).canonicalize()?;
            require(p.starts_with(&root), "blob escaped capture")?;
            total = total.checked_add(b.bytes).ok_or("size overflow")?;
            require(
                total <= payload_bound(&scene.provenance) && fs::metadata(&p)?.len() == b.bytes,
                "immutable payload bound",
            )?;
            let data = fs::read(p)?;
            require(
                data.len() as u64 == b.bytes && sha(&data) == b.sha256,
                "blob length/SHA mismatch",
            )?;
            require(
                bytes.insert(b.path.clone(), data).is_none(),
                "duplicate blob",
            )?;
        }
        let mut v = Self {
            scene,
            bytes,
            manifest_sha256: sha(&raw),
            targets_have_resolution_independent_initialization: true,
        };
        v.validate()?;
        Ok(v)
    }
    pub fn data(&self, name: &str) -> Result<&[u8]> {
        self.bytes
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("missing blob {name}").into())
    }
    pub fn texture(&self, id: &str) -> Result<&Texture> {
        self.scene
            .textures
            .iter()
            .find(|x| x.id == id)
            .ok_or_else(|| format!("missing texture {id}").into())
    }
    pub fn buffer(&self, id: &str) -> Result<&Buffer> {
        self.scene
            .buffers
            .iter()
            .find(|x| x.id == id)
            .ok_or_else(|| format!("missing buffer {id}").into())
    }
    pub fn pipeline(&self, id: &str) -> Result<&Pipeline> {
        self.scene
            .pipelines
            .iter()
            .find(|x| x.id == id)
            .ok_or_else(|| format!("missing pipeline {id}").into())
    }
    fn validate(&mut self) -> Result<()> {
        let s = &self.scene;
        require(
            s.schema == 1
                && s.commands.len() <= 8192
                && s.textures.len() <= 4096
                && s.buffers.len() <= 8192
                && s.pipelines.len() <= 4096
                && s.compute_pipelines.len() <= 4096,
            "scene bounds",
        )?;
        require(
            s.original_submission_completed
                && s.original_gpu_errors.is_none_or(|n| n == 0)
                && s.omitted_commands == 0,
            "original closure missing",
        )?;
        let native_graph = matches!(s.provenance, Provenance::NativeGraph { .. } | Provenance::NativeFinishedGraph { .. } | Provenance::NativeExecutedFinishedGraph { .. });
        let native = matches!(s.provenance, Provenance::Native { .. } | Provenance::NativeGraph { .. } | Provenance::NativeFinishedGraph { .. } | Provenance::NativeExecutedFinishedGraph { .. });
        let isolated_compute = !s.commands.is_empty()
            && s.pipelines.is_empty()
            && s.commands.iter().all(|c| matches!(c, Command::Dispatch { .. } | Command::DepthToColor { .. } | Command::Checkpoint { .. }))
            && s.commands.iter().any(|c| matches!(c, Command::Dispatch { .. } | Command::DepthToColor { .. }));
        if native {
            require(!native_graph || s.commands.iter().any(|c| matches!(c, Command::Draw { .. })),
                "native graph raster anchor absent")?;
            require(
                s.original_gpu_errors == Some(0),
                "native original GPU errors unobserved",
            )?;
            require(
                (!native_graph && isolated_compute && s.eye.is_none()) || s.eye.as_ref().is_some_and(|e| {
                    e.eye < 2 && e.epoch > 0 && e.camera > 0 && e.request > 0 && e.head > 0
                }),
                "native same-eye identity",
            )?;
        }
        if s.provenance.is_finished() {
            require(s.commands.iter().filter(|c|matches!(c,Command::FinishedEndpoint{..})).count()==1
                && matches!(s.commands.last(),Some(Command::FinishedEndpoint{..})), "finished graph endpoint missing/duplicated")?;
        } else {require(!s.commands.iter().any(|c|matches!(c,Command::FinishedEndpoint{..})), "finished endpoint requires native finished graph")?;}
        let omissions=s.commands.iter().filter_map(|c|match c{Command::OriginalOmission{omission}=>Some(omission),_=>None}).collect::<Vec<_>>();
        if s.provenance.is_executed_finished() {
            require(!omissions.is_empty()&&omissions.len()<=1024,"executed graph omission count")?;
            require(omissions.windows(2).all(|w|w[0].event_index<w[1].event_index&&w[0].command_sequence<w[1].command_sequence),"original omission order/identity")?;
        }else{require(omissions.is_empty(),"original omission requires executed finished provenance")?;}
        let mut ids = BTreeSet::new();
        let mut allocation_bytes = 0u64;
        let mut native_textures = BTreeSet::new();
        let mut native_initial_versions = BTreeSet::new();
        let mut versions = BTreeMap::<Slice, u64>::new();
        let mut initialized = BTreeSet::<Slice>::new();
        let mut independent = true;
        for t in &s.textures {
            require(ids.insert(t.id.clone()), "duplicate resource")?;
            let d = &t.descriptor;
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
                    && d.mips <= 15
                    && d.mips
                        <= 32
                            - d.width
                                .max(d.height)
                                .max(if d.dimension == TextureDimension::D3 {
                                    d.layers
                                } else {
                                    1
                                })
                                .leading_zeros()
                    && (d.dimension != TextureDimension::D1
                        || (d.height == 1 && d.layers == 1 && d.mips == 1))
                    && (d.dimension != TextureDimension::D3
                        || (d.width <= 2048 && d.height <= 2048))
                    && (!t.mutable_target || d.dimension == TextureDimension::D2)
                    && (d.dimension == TextureDimension::D2
                        || (!d.format.is_depth_stencil_format() && !d.format.is_compressed())),
                "texture dimensions",
            )?;
            require(
                !native
                    || t.native_allocation_id
                        .is_some_and(|v| v > 0 && native_textures.insert(v)),
                "native texture allocation absent/duplicated",
            )?;
            for mip in 0..d.mips {
                let (bw, bh) = d.format.block_dimensions();
                let block = d.format.block_copy_size(None).unwrap_or(8) as u64;
                let row = ((d.width >> mip).max(1).div_ceil(bw) as u64)
                    .checked_mul(block)
                    .ok_or("allocation row overflow")?
                    .div_ceil(256)
                    .checked_mul(256)
                    .ok_or("allocation row overflow")?;
                let size = row
                    .checked_mul((d.height >> mip).max(1).div_ceil(bh) as u64)
                    .and_then(|n| {
                        n.checked_mul(if d.dimension == TextureDimension::D3 {
                            d.mip_depth(mip) as u64
                        } else {
                            d.layers as u64
                        })
                    })
                    .ok_or("allocation size overflow")?;
                allocation_bytes = allocation_bytes
                    .checked_add(size)
                    .ok_or("allocation size overflow")?;
            }
            require(
                allocation_bytes <= MAX_BYTES,
                "logical texture allocation bound",
            )?;
            require(
                !t.mutable_target
                    || matches!(
                        d.format,
                        TextureFormat::Bgra8Unorm
                            | TextureFormat::Bgra8UnormSrgb
                            | TextureFormat::Rgba8Unorm
                            | TextureFormat::Rgba8UnormSrgb
                            | TextureFormat::Rgba32Float
                            | TextureFormat::Rgba16Float
                            | TextureFormat::R32Float
                            | TextureFormat::Depth32Float
                            | TextureFormat::Depth32FloatStencil8
                    ) || (d.format==TextureFormat::Rg8Uint && matches!(s.provenance,Provenance::NativeFinishedGraph{..}|Provenance::NativeExecutedFinishedGraph{..}|Provenance::Authored{..})),
                "unsupported persistent target format",
            )?;
            for i in &t.initial {
                require(
                    !native
                        || (i.content_version == 0 && matches!(i.origin, Initial::Unknown { .. }))
                        || (i.content_version > 0
                            && native_initial_versions.insert(i.content_version)),
                    "native initial version absent/duplicated",
                )?;
                let key = Slice {
                    resource: t.id.clone(),
                    mip: i.mip,
                    layer: i.layer,
                    aspect: i.aspect.clone(),
                };
                require(
                    i.layer < d.subresource_layers()
                        && aspects(d.format).contains(&i.aspect)
                        && versions.insert(key.clone(), i.content_version).is_none(),
                    "initial subresource",
                )?;
                let (_, _, row, rows) = slice_bytes(d, i.mip, &i.aspect)?;
                match &i.origin {
                    Initial::Unknown { .. } => {}
                    Initial::Captured { blob } => {
                        require(
                            self.data(blob)?.len() as u64 == row as u64 * rows as u64,
                            "exact initial byte extent",
                        )?;
                        initialized.insert(key);
                        if t.mutable_target {
                            independent = false;
                        }
                    }
                    Initial::UniformClear {
                        value,
                        view_format,
                        operation,
                        reference_blob,
                    } => {
                        require(
                            t.mutable_target && *operation > 0,
                            "only actual target clear",
                        )?;
                        validate_clear(value, &i.aspect)?;
                        validate_view_format(d, *view_format)?;
                        if let Some(blob) = reference_blob {
                            require(
                                self.data(blob)?.len() as u64 == row as u64 * rows as u64,
                                "exact clear-reference byte extent",
                            )?;
                        }
                        initialized.insert(key);
                    }
                }
            }
            for mip in 0..d.mips {
                for layer in 0..d.subresource_layers() {
                    for aspect in aspects(d.format) {
                        require(
                            versions.contains_key(&Slice {
                                resource: t.id.clone(),
                                mip,
                                layer,
                                aspect,
                            }),
                            "missing explicit subresource initialization",
                        )?;
                    }
                }
            }
        }
        for b in &s.buffers {
            allocation_bytes = allocation_bytes
                .checked_add(self.data(&b.blob)?.len() as u64)
                .ok_or("allocation size overflow")?;
            require(
                allocation_bytes <= MAX_BYTES,
                "logical resource allocation bound",
            )?;
            require(
                ids.insert(b.id.clone())
                    && (!native || b.native_allocation_id.is_some_and(|n| n > 0)),
                "buffer identity",
            )?;
            require(!self.data(&b.blob)?.is_empty(), "empty immutable buffer")?;
        }
        let mut pids = BTreeSet::new();
        let mut shader_bytes = 0;
        for p in &s.pipelines {
            require(
                pids.insert(&p.id)
                    && p.layouts.len() <= 4
                    && p.vertex_layouts.len() <= 16
                    && p.targets.len() <= 8
                    && p.multisample.count == 1
                    && (p.fragment.is_some() || p.targets.is_empty()),
                "pipeline bounds",
            )?;
            for x in std::iter::once(&p.vertex).chain(p.fragment.iter()) {
                shader_bytes += x.wgsl.len();
                require(
                    shader_bytes <= 8 << 20
                        && x.exact_final_variant
                        && sha(x.wgsl.as_bytes()) == x.sha256
                        && !x.entry.is_empty(),
                    "final shader variant source contract",
                )?;
            }
            for l in &p.layouts {
                let mut slots = BTreeSet::new();
                for e in l {
                    require(
                        slots.insert(e.binding) && e.count.is_none(),
                        "binding arrays/duplicates unsupported",
                    )?;
                    require(
                        matches!(
                            e.ty,
                            BindingType::Buffer {
                                ty: BufferBindingType::Uniform
                                    | BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                ..
                            } | BindingType::Texture {
                                multisampled: false,
                                ..
                            } | BindingType::Sampler(_)
                        ),
                        "mutable storage/dynamic binding unsupported",
                    )?;
                }
            }
        }
        for p in &s.compute_pipelines {
            require(pids.insert(&p.id) && p.layouts.len() <= 4, "compute pipeline bounds/identity")?;
            let x = &p.shader;
            shader_bytes += x.wgsl.len();
            require(shader_bytes <= 8 << 20 && x.exact_final_variant
                && sha(x.wgsl.as_bytes()) == x.sha256 && !x.entry.is_empty(),
                "final compute shader variant source contract")?;
            for l in &p.layouts {
                require(l.len() <= 256, "compute layout entry bound")?;
                let mut slots = BTreeSet::new();
                for e in l {
                    require(slots.insert(e.binding) && e.count.is_none()
                        && e.visibility == ShaderStages::COMPUTE, "compute layout slot/visibility")?;
                    let supported = match e.ty {
                        BindingType::Buffer { ty: BufferBindingType::Uniform
                            | BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, .. }
                        | BindingType::Texture { multisampled: false, .. }
                        | BindingType::Sampler(_) => true,
                        BindingType::StorageTexture { access: StorageTextureAccess::WriteOnly,
                            format, view_dimension: TextureViewDimension::D2 } => matches!(format,
                                TextureFormat::Rgba32Float | TextureFormat::Rgba16Float
                                | TextureFormat::Rgba8Unorm | TextureFormat::R32Float),
                        _ => false,
                    };
                    require(supported, "unsupported compute layout/access/format")?;
                }
            }
        }
        let mut draws = 0;
        // Immutable input snapshots can have an independent native version
        // namespace (e.g. dummy material allocations). Only ordered attachment
        // writes share this ledger ordering; each transition also checks its
        // own exact preceding subresource version.
        let mut last_write = 0;
        let mut output_bytes = 0u64;
        let mut checkpoint_count = 0usize;
        for c in &s.commands {
            match c {
                Command::Clear {
                    transition,
                    value,
                    view_format,
                } => {
                    let t = self.texture(&transition.slice.resource)?;
                    require(t.mutable_target, "clear immutable input")?;
                    validate_clear(value, &transition.slice.aspect)?;
                    validate_view_format(&t.descriptor, *view_format)?;
                    advance(transition, &mut versions, &mut last_write)?;
                    initialized.insert(transition.slice.clone());
                }
                Command::Copy {
                    source,
                    destination,
                } => {
                    check_version(source, &versions, &initialized)?;
                    let a = self.texture(&source.slice.resource)?;
                    let b = self.texture(&destination.slice.resource)?;
                    require(
                        b.mutable_target
                            && a.descriptor.dimension == TextureDimension::D2
                            && source.slice != destination.slice
                            && source.slice.aspect == Aspect::Color
                            && destination.slice.aspect == Aspect::Color
                            && a.descriptor.format == b.descriptor.format
                            && slice_bytes(&a.descriptor, source.slice.mip, &Aspect::Color)?
                                == slice_bytes(
                                    &b.descriptor,
                                    destination.slice.mip,
                                    &Aspect::Color,
                                )?,
                        "only equal full-color copies supported",
                    )?;
                    advance(destination, &mut versions, &mut last_write)?;
                    initialized.insert(destination.slice.clone());
                }
                Command::CopyRegion { sources, destinations, source_origin, destination_origin, extent } => {
                    require(!sources.is_empty() && sources.len() == destinations.len(), "copy aspect arity")?;
                    let a = self.texture(&sources[0].slice.resource)?;
                    let b = self.texture(&destinations[0].slice.resource)?;
                    let sa = &sources[0].slice;
                    let sb = &destinations[0].slice;
                    let expected = aspects(a.descriptor.format).into_iter().collect::<BTreeSet<_>>();
                    require(a.descriptor.dimension == TextureDimension::D2 && b.descriptor.dimension == TextureDimension::D2
                        && a.descriptor.format == b.descriptor.format && a.descriptor.format.block_dimensions() == (1,1)
                        && sa.mip < a.descriptor.mips && sb.mip < b.descriptor.mips
                        && sa.layer < a.descriptor.layers && sb.layer < b.descriptor.layers
                        && a.id != b.id && b.mutable_target && *extent != [0,0,0] && extent.iter().all(|x|*x>0)
                        && extent[2] == 1 && source_origin[2] == 0 && destination_origin[2] == 0
                        && sources.iter().all(|v| v.slice.resource == sa.resource && v.slice.mip == sa.mip && v.slice.layer == sa.layer)
                        && destinations.iter().all(|v|v.slice.resource == sb.resource && v.slice.mip == sb.mip && v.slice.layer == sb.layer)
                        && sources.iter().map(|v|v.slice.aspect.clone()).collect::<BTreeSet<_>>() == expected
                        && destinations.iter().map(|v|v.slice.aspect.clone()).collect::<BTreeSet<_>>() == expected
                        && sources.len() == expected.len(), "unsupported copy region/aspects")?;
                    let aw=(a.descriptor.width>>sa.mip).max(1); let ah=(a.descriptor.height>>sa.mip).max(1);
                    let bw=(b.descriptor.width>>sb.mip).max(1); let bh=(b.descriptor.height>>sb.mip).max(1);
                    require(source_origin[0].checked_add(extent[0]).is_some_and(|n|n<=aw)
                        && source_origin[1].checked_add(extent[1]).is_some_and(|n|n<=ah)
                        && destination_origin[0].checked_add(extent[0]).is_some_and(|n|n<=bw)
                        && destination_origin[1].checked_add(extent[1]).is_some_and(|n|n<=bh), "copy region outside subresource")?;
                    let full = *destination_origin == [0,0,0] && extent[..2] == [bw,bh];
                    require(expected == BTreeSet::from([Aspect::Color]) || (full && *source_origin == [0,0,0] && extent[..2] == [aw,ah]),
                        "depth stencil copy requires full equal subresources")?;
                    for source in sources {check_version(source,&versions,&initialized)?;}
                    for t in destinations {
                        require(full || initialized.contains(&t.slice), "partial copy requires initialized destination")?;
                        require(!native || !native_initial_versions.contains(&t.after), "native copy reused initial content version")?;
                        advance(t,&mut versions,&mut last_write)?; initialized.insert(t.slice.clone());
                    }
                }
                Command::GenerateMip { source, destination } => {
                    check_version(source, &versions, &initialized)?;
                    let t=self.texture(&source.slice.resource)?; let d=&t.descriptor;
                    require(t.mutable_target && d.dimension==TextureDimension::D2
                        && source.slice.resource==destination.slice.resource && source.slice.layer==destination.slice.layer
                        && source.slice.mip.checked_add(1)==Some(destination.slice.mip)
                        && source.slice.aspect==Aspect::Color && destination.slice.aspect==Aspect::Color
                        && matches!(d.format, TextureFormat::Rgba8Unorm|TextureFormat::Rgba8UnormSrgb|TextureFormat::Bgra8Unorm|TextureFormat::Bgra8UnormSrgb|TextureFormat::Rgba16Float|TextureFormat::R32Float|TextureFormat::Rg32Float|TextureFormat::Rgba32Float),
                        "unsupported native mip pass")?;
                    require(!native || !native_initial_versions.contains(&destination.after), "native mip reused initial content version")?;
                    advance(destination,&mut versions,&mut last_write)?; initialized.insert(destination.slice.clone());
                }
                Command::DepthToColor { source, destination } => {
                    check_version(source, &versions, &initialized)?;
                    let a = self.texture(&source.slice.resource)?;
                    let b = self.texture(&destination.slice.resource)?;
                    require(a.descriptor.dimension == TextureDimension::D2
                        && b.descriptor.dimension == TextureDimension::D2
                        && matches!(a.descriptor.format, TextureFormat::Depth32Float | TextureFormat::Depth32FloatStencil8)
                        && b.descriptor.format == TextureFormat::R32Float
                        && source.slice.aspect == Aspect::Depth
                        && destination.slice.aspect == Aspect::Color
                        && a.id != b.id && b.mutable_target
                        && slice_bytes(&a.descriptor, source.slice.mip, &Aspect::Depth)?
                            == slice_bytes(&b.descriptor, destination.slice.mip, &Aspect::Color)?,
                        "depth proxy requires equal D2 Float32 depth and R32Float subresources")?;
                    require(!native || !native_initial_versions.contains(&destination.after),
                        "native depth proxy reused initial content version")?;
                    advance(destination, &mut versions, &mut last_write)?;
                    initialized.insert(destination.slice.clone());
                    // A full overwrite does not make captured source pixels a
                    // resolution-independent producer. Graph closure is separate.
                    independent = false;
                }
                Command::StencilToColor { source, destination } => {
                    require(matches!(s.provenance,Provenance::NativeFinishedGraph{..}|Provenance::NativeExecutedFinishedGraph{..}|Provenance::Authored{..}),"stencil proxy requires finished graph")?;
                    check_version(source,&versions,&initialized)?;
                    let a=self.texture(&source.slice.resource)?;let b=self.texture(&destination.slice.resource)?;
                    require(a.descriptor.dimension==TextureDimension::D2 && b.descriptor.dimension==TextureDimension::D2
                        && a.descriptor.format==TextureFormat::Depth32FloatStencil8 && b.descriptor.format==TextureFormat::Rg8Uint
                        && source.slice.aspect==Aspect::Stencil && destination.slice.aspect==Aspect::Color
                        && a.id!=b.id && b.mutable_target,"stencil proxy requires distinct D32S8 stencil and RG8Uint targets")?;
                    let (sw,sh,_,_)=slice_bytes(&a.descriptor,source.slice.mip,&source.slice.aspect)?;
                    let (dw,dh,_,_)=slice_bytes(&b.descriptor,destination.slice.mip,&destination.slice.aspect)?;
                    require(sw==dw && sh==dh,"stencil proxy extent mismatch")?;
                    require(!native || !native_initial_versions.contains(&destination.after),"native stencil proxy reused initial version")?;
                    advance(destination,&mut versions,&mut last_write)?;initialized.insert(destination.slice.clone());independent=false;
                }
                Command::FloatToDepth{source,destination} => {
                    require(s.provenance.is_finished()||matches!(s.provenance,Provenance::Authored{..}),"float depth proxy requires finished graph")?;
                    check_version(source,&versions,&initialized)?;
                    let a=self.texture(&source.slice.resource)?;let b=self.texture(&destination.slice.resource)?;
                    require(a.descriptor.dimension==TextureDimension::D2&&b.descriptor.dimension==TextureDimension::D2
                        &&float_depth_source_format(a.descriptor.format)&&b.descriptor.format==TextureFormat::Depth32Float
                        &&source.slice.aspect==Aspect::Color&&destination.slice.aspect==Aspect::Depth
                        &&a.id!=b.id&&b.mutable_target&&source.slice.mip==destination.slice.mip&&source.slice.layer==destination.slice.layer
                        &&a.descriptor.width==b.descriptor.width&&a.descriptor.height==b.descriptor.height
                        &&a.descriptor.mips==b.descriptor.mips&&a.descriptor.layers==b.descriptor.layers,
                        "float depth proxy native allocation/view contract")?;
                    require(!native||!native_initial_versions.contains(&destination.after),"native float depth proxy reused initial version")?;
                    advance(destination,&mut versions,&mut last_write)?;initialized.insert(destination.slice.clone());independent=false;
                }
                Command::OriginalOmission{omission} => {
                    require(s.provenance.is_executed_finished(),"original omission requires schema8 provenance")?;
                    validate_original_omission(omission)?;
                    independent=false;
                }
                Command::OriginalOnly{operation_kind:kind,expected,metadata} => {
                    require(matches!(s.provenance,Provenance::NativeFinishedGraph{..}|Provenance::NativeExecutedFinishedGraph{..}|Provenance::Authored{..}),"original-only event requires finished graph")?;
                    validate_original_only(self,kind,expected,metadata)?;
                    for e in expected {check_version(&e.version,&versions,&initialized)?;
                        let t=self.texture(&e.version.slice.resource)?;let(_,_,row,rows)=slice_bytes(&t.descriptor,e.version.slice.mip,&e.version.slice.aspect)?;
                        let bytes=u64::from(row)*u64::from(rows);require(self.data(&e.blob)?.len()as u64==bytes,"original-only source byte extent")?;
                        output_bytes=output_bytes.checked_add(bytes).ok_or("original-only reference bound")?;require(output_bytes<=payload_bound(&s.provenance),"output readback bound")?;}
                }
                Command::FinishedEndpoint{hud_free_source,hud_world,depth_source,depth_world,publication} => {
                    validate_finished_endpoint(s,c,depth_world,publication)?;
                    require(std::ptr::eq(c,s.commands.last().ok_or("finished endpoint missing")?), "finished endpoint is not last")?;
                    require(self.data(&hud_free_source.blob)?==self.data(&hud_world.blob)?,"endpoint HUD copy reference bytes differ")?;
                    for e in [hud_free_source,hud_world,depth_source] {
                        check_version(&e.version,&versions,&initialized)?;
                        let t=self.texture(&e.version.slice.resource)?;
                        let (_,_,row,rows)=slice_bytes(&t.descriptor,e.version.slice.mip,&e.version.slice.aspect)?;
                        let bytes=u64::from(row)*u64::from(rows);
                        require(self.data(&e.blob)?.len() as u64==bytes,"finished endpoint byte extent")?;
                        output_bytes=output_bytes.checked_add(bytes).ok_or("endpoint readback size overflow")?;
                        require(output_bytes<=payload_bound(&s.provenance),"output readback bound")?;
                    }
                }
                Command::Checkpoint { expected } => {
                    require(!expected.is_empty(), "empty ordered checkpoint")?;
                    checkpoint_count = checkpoint_count.checked_add(expected.len()).ok_or("checkpoint count overflow")?;
                    require(checkpoint_count <= 16384, "checkpoint count bound")?;
                    let mut selected = BTreeSet::new();
                    for e in expected {
                        require(selected.insert(e.version.slice.clone()), "duplicate checkpoint subresource")?;
                        check_version(&e.version, &versions, &initialized)?;
                        let t = self.texture(&e.version.slice.resource)?;
                        let (_, _, row, rows) = slice_bytes(&t.descriptor, e.version.slice.mip, &e.version.slice.aspect)?;
                        let bytes = (row as u64).checked_mul(rows as u64).ok_or("checkpoint byte overflow")?;
                        require(self.data(&e.blob)?.len() as u64 == bytes, "checkpoint bytes extent")?;
                        output_bytes = output_bytes.checked_add(bytes).ok_or("checkpoint size overflow")?;
                        require(output_bytes <= payload_bound(&s.provenance), "output readback bound")?;
                    }
                }
                Command::Dispatch { dispatch } => {
                    self.validate_dispatch(dispatch, &versions, &initialized)?;
                    for t in &dispatch.writes {
                        require(!native || !native_initial_versions.contains(&t.after), "native compute reused initial content version")?;
                        advance(t, &mut versions, &mut last_write)?;
                    }
                }
                Command::Draw { draw: d } | Command::ContextDraw {draw:d,..} => {
                    if let Command::ContextDraw{context,..}=c {
                        require(d.eye.is_none(), "context draw fabricated eye ownership")?;
                        match &s.provenance {
                            Provenance::NativeFinishedGraph{native_present,..}|Provenance::NativeExecutedFinishedGraph{native_present,..} => {
                                require(context.native_present==*native_present && *native_present>0, "context draw belongs to another native present")?;
                                if !context.world_view.is_null() {
                                    let e=crate::native_import::world(&context.world_view)?;
                                    let anchor=s.eye.as_ref().ok_or("context draw lacks world anchor")?;
                                    require(e.eye==anchor.eye && e.epoch==anchor.epoch, "context draw contradicts frame eye/epoch")?;
                                }
                            },
                            Provenance::Authored{..} => require(context.native_present>0 && context.world_view.is_null(), "authored context scope")?,
                            _=>return Err("context draw requires native finished graph".into()),
                        }
                    }
                    draws += 1;
                    require(
                        draws <= 4096 && (matches!(c,Command::ContextDraw{..}) || d.eye == s.eye),
                        "draw count/same-eye changed",
                    )?;
                    let p = self.pipeline(&d.pipeline)?;
                    require(
                        p.targets.len() == d.colors.len()
                            && p.depth_stencil.is_some() == d.depth.is_some(),
                        "pipeline target shape",
                    )?;
                    let mut attached = BTreeSet::new();
                    let mut writable_attached = BTreeSet::new();
                    let mut size = None;
                    for (i, a) in d.colors.iter().enumerate() {
                        if let Some(a) = a {
                            let key = self.attachment(a, Aspect::Color, &mut size)?;
                            require(
                                p.targets[i]
                                    .as_ref()
                                    .is_some_and(|t| t.format == a.view_format),
                                "color view pipeline format",
                            )?;
                            require(
                                initialized.contains(&key) && attached.insert(key.clone()),
                                "unknown/duplicate color load",
                            )?;
                            writable_attached.insert(key);
                        } else {
                            require(p.targets[i].is_none(), "missing color output")?;
                        }
                    }
                    if let Some(ds) = &d.depth {
                        let state = p.depth_stencil.as_ref().unwrap();
                        let stencil_writes = state.stencil.write_mask != 0
                            && [state.stencil.front, state.stencil.back].iter().any(|s| {
                                s.fail_op != StencilOperation::Keep
                                    || s.depth_fail_op != StencilOperation::Keep
                                    || s.pass_op != StencilOperation::Keep
                            });
                        require(
                            !(ds.read_only_depth && state.depth_write_enabled == Some(true))
                                && !(ds.read_only_stencil && stencil_writes),
                            "pipeline writes read-only attachment",
                        )?;
                        let t = self.texture(&ds.attachment.resource)?;
                        require(
                            p.depth_stencil
                                .as_ref()
                                .is_some_and(|x| x.format == ds.attachment.view_format),
                            "depth format",
                        )?;
                        for a in aspects(t.descriptor.format) {
                            let writable = match a {
                                Aspect::Depth => !ds.read_only_depth,
                                Aspect::Stencil => !ds.read_only_stencil,
                                Aspect::Color => true,
                            };
                            let key = self.attachment(&ds.attachment, a, &mut size)?;
                            if writable {
                                writable_attached.insert(key.clone());
                            }
                            require(
                                initialized.contains(&key) && attached.insert(key),
                                "unknown depth/stencil load",
                            )?;
                        }
                    }
                    let (w, h) = size.ok_or("draw has no targets")?;
                    let v = d.viewport;
                    require(
                        v.iter().all(|n| n.is_finite())
                            && v[0] >= 0.
                            && v[1] >= 0.
                            && v[2] > 0.
                            && v[3] > 0.
                            && v[0] + v[2] <= w as f32
                            && v[1] + v[3] <= h as f32
                            && v[4] >= 0.
                            && v[4] <= v[5]
                            && v[5] <= 1.,
                        "viewport",
                    )?;
                    require(
                        d.scissor[0]
                            .checked_add(d.scissor[2])
                            .is_some_and(|n| n <= w)
                            && d.scissor[1]
                                .checked_add(d.scissor[3])
                                .is_some_and(|n| n <= h)
                            && d.blend.iter().all(|n| n.is_finite()),
                        "scissor/blend",
                    )?;
                    let mut bound = BTreeSet::new();
                    for b in &d.bindings {
                        require(bound.insert((b.group, b.binding)), "duplicate binding")?;
                        let e = p
                            .layouts
                            .get(b.group as usize)
                            .and_then(|l| l.iter().find(|x| x.binding == b.binding))
                            .ok_or("unexpected binding")?;
                        match &b.value {
                            BindingValue::Buffer {
                                buffer,
                                offset,
                                size,
                            } => {
                                require(
                                    matches!(e.ty, BindingType::Buffer { .. })
                                        && *size > 0
                                        && offset.checked_add(*size).is_some_and(|n| {
                                            self.buffer(buffer)
                                                .and_then(|x| self.data(&x.blob))
                                                .is_ok_and(|b| n <= b.len() as u64)
                                        }),
                                    "buffer bound range",
                                )?;
                            }
                            BindingValue::Sampler { .. } => {
                                require(matches!(e.ty, BindingType::Sampler(_)), "sampler layout")?;
                            }
                            BindingValue::Texture {
                                texture,
                                view,
                                versions: reads,
                            } => {
                                let t = self.texture(texture)?;
                                require(
                                    matches!(e.ty, BindingType::Texture { view_dimension, .. } if view_dimension == resolved_view_dimension(&t.descriptor, view)),
                                    "texture layout dimension",
                                )?;
                                let selected = self.view_slices(t, view)?;
                                require(
                                    selected.is_disjoint(&writable_attached),
                                    "sampled/writable attachment alias",
                                )?;
                                require(
                                    reads.len() == selected.len(),
                                    "selected texture view versions incomplete",
                                )?;
                                let mut observed = BTreeSet::new();
                                for r in reads {
                                    require(
                                        selected.contains(&r.slice)
                                            && observed.insert(r.slice.clone()),
                                        "view version mismatch",
                                    )?;
                                    check_version(r, &versions, &initialized)?;
                                }
                            }
                        }
                    }
                    for (g, l) in p.layouts.iter().enumerate() {
                        for b in l {
                            require(
                                bound.contains(&(g as u32, b.binding)),
                                "required binding absent",
                            )?;
                        }
                    }
                    let mut vbs = BTreeSet::new();
                    for b in &d.vertex_buffers {
                        require(
                            vbs.insert(b.slot)
                                && self.data(&self.buffer(&b.buffer)?.blob)?.len() as u64
                                    > b.offset,
                            "vertex buffer",
                        )?;
                    }
                    for (i, l) in p.vertex_layouts.iter().enumerate() {
                        require(
                            l.is_none() || vbs.contains(&(i as u32)),
                            "vertex slot absent",
                        )?;
                    }
                    match d.args {
                        DrawArgs::Direct {
                            vertices,
                            instances,
                            first_vertex,
                            first_instance,
                        } => {
                            require(
                                d.index_buffer.is_none()
                                    && vertices > 0
                                    && instances > 0
                                    && first_vertex.checked_add(vertices).is_some()
                                    && first_instance.checked_add(instances).is_some(),
                                "direct draw range",
                            )?;
                        }
                        DrawArgs::Indexed {
                            indices,
                            first_index,
                            instances,
                            first_instance,
                            ..
                        } => {
                            let i = d.index_buffer.as_ref().ok_or("index absent")?;
                            let stride = if i.format == IndexFormat::Uint16 {
                                2
                            } else {
                                4
                            };
                            let end = (first_index as u64)
                                .checked_add(indices as u64)
                                .and_then(|n| n.checked_mul(stride))
                                .and_then(|n| n.checked_add(i.offset))
                                .ok_or("index overflow")?;
                            require(
                                indices > 0
                                    && instances > 0
                                    && first_instance.checked_add(instances).is_some()
                                    && end
                                        <= self.data(&self.buffer(&i.buffer)?.blob)?.len() as u64,
                                "index range",
                            )?;
                        }
                    }
                    // Resolve the exact fetched vertex/instance range from immutable indices.
                    let (vertex_end, instance_end) = match d.args {
                        DrawArgs::Direct {
                            vertices,
                            first_vertex,
                            instances,
                            first_instance,
                        } => (
                            (first_vertex as u64) + vertices as u64,
                            (first_instance as u64) + instances as u64,
                        ),
                        DrawArgs::Indexed {
                            indices,
                            first_index,
                            base_vertex,
                            instances,
                            first_instance,
                        } => {
                            let ib = d.index_buffer.as_ref().unwrap();
                            let bytes = self.data(&self.buffer(&ib.buffer)?.blob)?;
                            let stride = if ib.format == IndexFormat::Uint16 {
                                2
                            } else {
                                4
                            };
                            require(ib.offset % stride == 0, "index alignment")?;
                            let mut end = 0u64;
                            for n in first_index as u64..first_index as u64 + indices as u64 {
                                let at = (ib.offset + n * stride) as usize;
                                let raw = if stride == 2 {
                                    u16::from_le_bytes(bytes[at..at + 2].try_into()?) as u32
                                } else {
                                    u32::from_le_bytes(bytes[at..at + 4].try_into()?)
                                };
                                if p.primitive.strip_index_format.is_some()
                                    && raw
                                        == if stride == 2 {
                                            u16::MAX as u32
                                        } else {
                                            u32::MAX
                                        }
                                {
                                    continue;
                                }
                                let index = raw as i64 + base_vertex as i64;
                                require(index >= 0, "negative resolved vertex index")?;
                                end = end.max(index as u64 + 1);
                            }
                            (end, first_instance as u64 + instances as u64)
                        }
                    };
                    for vb in &d.vertex_buffers {
                        let layout = p
                            .vertex_layouts
                            .get(vb.slot as usize)
                            .and_then(Option::as_ref)
                            .ok_or("unexpected vertex slot")?;
                        let count = if layout.step_mode == VertexStepMode::Instance {
                            instance_end
                        } else {
                            vertex_end
                        };
                        let last_attribute = layout
                            .attributes
                            .iter()
                            .map(|a| a.offset.checked_add(a.format.size()))
                            .collect::<Option<Vec<_>>>()
                            .ok_or("vertex attribute overflow")?
                            .into_iter()
                            .max()
                            .unwrap_or(0);
                        let bytes = if count == 0 {
                            0
                        } else {
                            (count - 1)
                                .checked_mul(layout.array_stride)
                                .and_then(|n| n.checked_add(last_attribute))
                                .ok_or("vertex range overflow")?
                        };
                        require(
                            vb.offset.checked_add(bytes).is_some_and(|n| {
                                self.data(&self.buffer(&vb.buffer).unwrap().blob)
                                    .is_ok_and(|b| n <= b.len() as u64)
                            }),
                            "vertex fetch outside immutable snapshot",
                        )?;
                    }
                    let mut writes = BTreeSet::new();
                    for x in &d.writes {
                        require(
                            writable_attached.contains(&x.slice) && writes.insert(x.slice.clone()),
                            "write transition outside writable attachments",
                        )?;
                        advance(x, &mut versions, &mut last_write)?;
                    }
                    require(
                        written_slices(d, p).is_subset(&writes),
                        "missing writable attachment version transition",
                    )?;
                }
            }
        }
        require(
            !s.expected.is_empty(),
            "original after-span references absent",
        )?;
        for e in &s.expected {
            check_version(&e.version, &versions, &initialized)?;
            let t = self.texture(&e.version.slice.resource)?;
            let (_, _, row, rows) =
                slice_bytes(&t.descriptor, e.version.slice.mip, &e.version.slice.aspect)?;
            require(
                self.data(&e.blob)?.len() as u64 == row as u64 * rows as u64,
                "expected bytes extent",
            )?;
            output_bytes = output_bytes
                .checked_add(row as u64 * rows as u64)
                .ok_or("output size overflow")?;
            require(output_bytes <= payload_bound(&s.provenance), "output readback bound")?;
        }
        self.targets_have_resolution_independent_initialization = independent;
        Ok(())
    }
    fn validate_dispatch(
        &self,
        d: &Dispatch,
        versions: &BTreeMap<Slice, u64>,
        initialized: &BTreeSet<Slice>,
    ) -> Result<()> {
        require(d.eye == self.scene.eye
            || (matches!(self.scene.provenance, Provenance::NativeGraph { .. } | Provenance::NativeFinishedGraph { .. } | Provenance::NativeExecutedFinishedGraph { .. }) && d.eye.is_none()),
            "compute eye changed")?;
        require(d.workgroups.iter().all(|n| *n > 0 && *n <= 65535)
            && d.workgroups.iter().try_fold(1u64, |n, v| n.checked_mul(*v as u64)).is_some_and(|n| n <= 1 << 20),
            "dispatch workgroup bounds")?;
        let p = self.scene.compute_pipelines.iter().find(|p| p.id == d.pipeline)
            .ok_or("missing compute pipeline")?;
        let mut bound = BTreeSet::new();
        let mut sampled = BTreeSet::new();
        let mut storage = BTreeSet::new();
        let mut writable = BTreeSet::new();
        for b in &d.bindings {
            require(bound.insert((b.group, b.binding)), "duplicate compute binding")?;
            let e = p.layouts.get(b.group as usize)
                .and_then(|l| l.iter().find(|e| e.binding == b.binding))
                .ok_or("unexpected compute binding")?;
            match &b.value {
                BindingValue::Buffer { buffer, offset, size } => {
                    let BindingType::Buffer { min_binding_size, .. } = e.ty else {
                        return Err("compute buffer layout".into());
                    };
                    require(*size > 0 && *size % 4 == 0 && *offset % 256 == 0
                        && min_binding_size.is_none_or(|n| *size >= n.get())
                        && offset.checked_add(*size).is_some_and(|n| self.buffer(buffer)
                            .and_then(|b| self.data(&b.blob)).is_ok_and(|b| n <= b.len() as u64)),
                        "compute buffer bound range/alignment")?;
                }
                BindingValue::Sampler { descriptor } => {
                    let BindingType::Sampler(kind) = e.ty else { return Err("compute sampler layout".into()); };
                    require((kind == SamplerBindingType::Comparison) == descriptor.compare.is_some(),
                        "compute comparison sampler mismatch")?;
                    if kind == SamplerBindingType::NonFiltering {
                        require(descriptor.mag_filter == FilterMode::Nearest
                            && descriptor.min_filter == FilterMode::Nearest
                            && descriptor.mipmap_filter == MipmapFilterMode::Nearest
                            && descriptor.anisotropy_clamp == 1, "compute nonfiltering sampler mismatch")?;
                    }
                }
                BindingValue::Texture { texture, view, versions: reads } => {
                    let t = self.texture(texture)?;
                    let selected = self.view_slices(t, view)?;
                    match e.ty {
                        BindingType::Texture { sample_type, view_dimension, multisampled: false } => {
                            require(view_dimension == resolved_view_dimension(&t.descriptor, view)
                                && view.usage.is_none_or(|u| u.contains(TextureUsages::TEXTURE_BINDING)),
                                "compute sampled view dimension")?;
                            let format = view.format.unwrap_or(t.descriptor.format);
                            let actual = format.sample_type(Some(view.aspect), Some(Features::FLOAT32_FILTERABLE));
                            require(actual == Some(sample_type)
                                || matches!((actual, sample_type), (Some(TextureSampleType::Float { .. }), TextureSampleType::Float { filterable: false })),
                                "compute sampled view type")?;
                            sampled.insert(texture.clone());
                        }
                        BindingType::StorageTexture { access: StorageTextureAccess::WriteOnly,
                            format, view_dimension: TextureViewDimension::D2 } => {
                            require(t.mutable_target && t.descriptor.dimension == TextureDimension::D2
                                && resolved_view_dimension(&t.descriptor, view) == TextureViewDimension::D2
                                && selected.len() == 1 && view.aspect == TextureAspect::All
                                && view.usage.is_none_or(|u| u.contains(TextureUsages::STORAGE_BINDING))
                                && view.format.unwrap_or(t.descriptor.format) == format
                                && t.descriptor.format == format && storage.insert(texture.clone()),
                                "compute storage view/duplicate allocation")?;
                            for s in &selected {
                                require(s.aspect == Aspect::Color && writable.insert(s.clone()), "compute storage aspect")?;
                            }
                        }
                        _ => return Err("unsupported compute texture binding".into()),
                    }
                    require(reads.len() == selected.len(), "compute view versions incomplete")?;
                    let mut observed = BTreeSet::new();
                    for r in reads {
                        require(selected.contains(&r.slice) && observed.insert(r.slice.clone()), "compute view version mismatch")?;
                        // Even a write-only shader can preserve texels; full original initialization is required.
                        check_version(r, versions, initialized)?;
                    }
                }
            }
        }
        require(sampled.is_disjoint(&storage), "compute sampled/storage allocation alias")?;
        for (g, l) in p.layouts.iter().enumerate() {
            for e in l { require(bound.contains(&(g as u32, e.binding)), "required compute binding absent")?; }
        }
        let writes = d.writes.iter().map(|w| w.slice.clone()).collect::<BTreeSet<_>>();
        require(!writable.is_empty() && writes.len() == d.writes.len() && writes == writable,
            "compute storage writes missing/extraneous/duplicate")?;
        Ok(())
    }
    pub fn attachment(
        &self,
        a: &Attachment,
        aspect: Aspect,
        size: &mut Option<(u32, u32)>,
    ) -> Result<Slice> {
        let t = self.texture(&a.resource)?;
        require(
            t.mutable_target
                && t.descriptor.dimension == TextureDimension::D2
                && a.layer < t.descriptor.layers
                && aspects(t.descriptor.format).contains(&aspect),
            "attachment allocation",
        )?;
        validate_view_format(&t.descriptor, a.view_format)?;
        let (w, h, _, _) = slice_bytes(&t.descriptor, a.mip, &aspect)?;
        require(size.is_none_or(|s| s == (w, h)), "attachment dimensions")?;
        *size = Some((w, h));
        Ok(Slice {
            resource: a.resource.clone(),
            mip: a.mip,
            layer: a.layer,
            aspect,
        })
    }
    pub fn view_slices(
        &self,
        t: &Texture,
        v: &TextureViewDescriptor<Option<String>>,
    ) -> Result<BTreeSet<Slice>> {
        let d = &t.descriptor;
        validate_view_format(d, v.format.unwrap_or(d.format))?;
        let m = v
            .mip_level_count
            .unwrap_or(d.mips.saturating_sub(v.base_mip_level));
        let dimension = resolved_view_dimension(d, v);
        let available_layers = d.subresource_layers();
        let layers = v.array_layer_count.unwrap_or(match dimension {
            TextureViewDimension::D1 | TextureViewDimension::D2 | TextureViewDimension::D3 => 1,
            TextureViewDimension::Cube => 6,
            _ => available_layers.saturating_sub(v.base_array_layer),
        });
        let dimension_ok = match d.dimension {
            TextureDimension::D1 => {
                dimension == TextureViewDimension::D1 && v.base_array_layer == 0 && layers == 1
            }
            TextureDimension::D3 => {
                dimension == TextureViewDimension::D3 && v.base_array_layer == 0 && layers == 1
            }
            TextureDimension::D2 => match dimension {
                TextureViewDimension::D2 => layers == 1,
                TextureViewDimension::D2Array => true,
                TextureViewDimension::Cube => layers == 6 && d.width == d.height,
                TextureViewDimension::CubeArray => layers % 6 == 0 && d.width == d.height,
                _ => false,
            },
        };
        require(
            m > 0
                && layers > 0
                && dimension_ok
                && v.base_mip_level.checked_add(m).is_some_and(|n| n <= d.mips)
                && v.base_array_layer
                    .checked_add(layers)
                    .is_some_and(|n| n <= available_layers),
            "selected view unsupported",
        )?;
        let aa = match v.aspect {
            TextureAspect::All => aspects(d.format),
            TextureAspect::DepthOnly => vec![Aspect::Depth],
            TextureAspect::StencilOnly => vec![Aspect::Stencil],
            _ => return Err("plane view unsupported".into()),
        };
        let mut result = BTreeSet::new();
        for mip in v.base_mip_level..v.base_mip_level + m {
            for layer in v.base_array_layer..v.base_array_layer + layers {
                for a in &aa {
                    require(aspects(d.format).contains(a), "wrong aspect")?;
                    result.insert(Slice {
                        resource: t.id.clone(),
                        mip,
                        layer,
                        aspect: a.clone(),
                    });
                }
            }
        }
        Ok(result)
    }
}
fn resolved_view_dimension(
    d: &TextureDesc,
    v: &TextureViewDescriptor<Option<String>>,
) -> TextureViewDimension {
    v.dimension.unwrap_or(match d.dimension {
        TextureDimension::D1 => TextureViewDimension::D1,
        TextureDimension::D3 => TextureViewDimension::D3,
        TextureDimension::D2 if d.layers == 1 => TextureViewDimension::D2,
        TextureDimension::D2 => TextureViewDimension::D2Array,
    })
}
fn validate_view_format(d: &TextureDesc, f: TextureFormat) -> Result<()> {
    require(
        f == d.format || d.view_formats.contains(&f),
        "unregistered reinterpretation",
    )
}
pub(crate) fn validate_original_only(input:&Input,kind:&str,expected:&[Expected],m:&serde_json::Value)->Result<()> {
    let num=|k:&str|m[k].as_u64().ok_or_else(||format!("original-only invalid {k}"));
    require(m["original_only"]==true&&m["replay_side_effects"]==false&&m["result_completion_recorded"]==false
        &&num("event_index")?>0&&num("command_sequence")?>0,"original-only side-effect scope")?;
    let object=m.as_object().ok_or("original-only object")?;
    match kind {
        "query"=>{
            let fields=["event_index","command_sequence","original_only","replay_side_effects","action","native_query_identity","qtype","sequence","slots","live","result_completion_recorded"];
            require(object.len()==fields.len()&&object.keys().all(|k|fields.contains(&k.as_str()))&&expected.is_empty(),"original query fields")?;
            require(matches!(m["action"].as_str(),Some("begin"|"end"|"resolve"|"pass_slot"))&&num("native_query_identity")?>0
                &&num("qtype")?<=u32::MAX as u64&&num("sequence")?<=u32::MAX as u64&&m["live"].is_boolean(),"original query identity/action")?;
            let slots=m["slots"].as_array().ok_or("query slots")?.iter().map(|s|s.as_u64().ok_or("query slot")).collect::<std::result::Result<Vec<_>,_>>()?;
            require(slots.len()<=2048&&slots.iter().all(|s|*s<2048)&&slots.windows(2).all(|w|w[0]<w[1])
                &&(matches!(num("qtype")?,1|5)||slots.is_empty()),"original query slot range")?;
        },
        "readback"=>{
            let fields=["event_index","command_sequence","original_only","replay_side_effects","sources","source_blobs","source_descriptors","source_origin","extent","row_bytes","rows","destination_row_pitch","destination_depth_pitch","destination_size","result_completion_recorded"];
            require(object.len()==fields.len()&&object.keys().all(|k|fields.contains(&k.as_str()))&&expected.len()==1,"original readback fields")?;
            let e=&expected[0];let t=input.texture(&e.version.slice.resource)?;let d=&t.descriptor;let s=&e.version.slice;
            require(t.native_allocation_id.is_some_and(|n|n>0)&&e.version.content_version>0
                &&d.dimension==TextureDimension::D2&&s.aspect==Aspect::Color&&d.format.block_dimensions()==(1,1)&&s.mip<d.mips&&s.layer<d.layers,"original readback source shape")?;
            let refs=m["sources"].as_array().ok_or("readback sources")?;let blobs=m["source_blobs"].as_array().ok_or("readback blobs")?;let descriptors=m["source_descriptors"].as_array().ok_or("readback descriptors")?;
            require(refs.len()==1&&blobs.len()==1&&descriptors.len()==1&&blobs[0]==e.blob,"original readback source arity")?;
            let r=&refs[0];require(r["allocation_id"].as_u64()==t.native_allocation_id&&r["content_version"].as_u64()==Some(e.version.content_version)
                &&r["subresource"]["mip"].as_u64()==Some(s.mip as u64)&&r["subresource"]["layer"].as_u64()==Some(s.layer as u64)&&r["subresource"]["aspect"]=="color","original readback source version differs")?;
            require(descriptors[0]==serde_json::json!({"width":d.width,"height":d.height,"depth_or_array_layers":d.layers,"mip_level_count":d.mips,"sample_count":1,"dimension":d.dimension,"format":d.format}),"original readback descriptor differs")?;
            let origin:[u32;3]=serde_json::from_value(m["source_origin"].clone())?;let extent:[u32;3]=serde_json::from_value(m["extent"].clone())?;
            let row=u64::from(d.format.block_copy_size(None).ok_or("readback format")?).checked_mul(u64::from(extent[0])).ok_or("readback row overflow")?;
            let pitch=num("destination_row_pitch")?;num("destination_depth_pitch")?;
            require(origin[2]==0&&extent[2]==1&&extent[0]>0&&extent[1]>0
                &&origin[0].checked_add(extent[0]).is_some_and(|n|n<=(d.width>>s.mip).max(1))
                &&origin[1].checked_add(extent[1]).is_some_and(|n|n<=(d.height>>s.mip).max(1))
                &&num("row_bytes")?==row&&num("rows")?==u64::from(extent[1])&&pitch>=row
                &&pitch.checked_mul(u64::from(extent[1]-1)).and_then(|n|n.checked_add(row)).is_some_and(|n|n<=num("destination_size").unwrap_or(0)),"original readback range differs")?;
        },_=>return Err("unsupported original-only operation".into()),
    }Ok(())
}
/// Production ownership predicate; projection Z is deliberately not substituted.
pub(crate) fn same_screen_rays(a:&serde_json::Value,b:&serde_json::Value)->Result<bool>{
    crate::native_import::world(a)?; crate::native_import::world(b)?;
    for k in ["provenance_epoch","optics","camera_sequence","request_frame","head_pose_frame","eye_index","view_count"] {
        if a.get(k)!=b.get(k){return Ok(false);}
    }
    for k in ["game_from_eye","game_from_stage","projection_from_eye"] {
        let aa=a[k].as_array().ok_or("world matrix")?;let bb=b[k].as_array().ok_or("world matrix")?;
        for (i,(x,y)) in aa.iter().zip(bb).enumerate(){
            if k=="projection_from_eye"&&i%4==2{continue;}
            let x=x.as_f64().ok_or("world matrix value")? as f32;let y=y.as_f64().ok_or("world matrix value")? as f32;
            if !x.is_finite()||!y.is_finite()||(x-y).abs()>0.00002{return Ok(false);}
        }
    }Ok(true)
}
fn native_id(scene:&Scene,s:&Slice)->Result<u64>{
    let t=scene.textures.iter().find(|t|t.id==s.resource).ok_or("endpoint texture missing")?;
    let id=t.native_allocation_id.ok_or("endpoint native texture identity")?;
    require(id>0&&s.resource==format!("native-texture:{id}"),"endpoint native texture identity")?;Ok(id)
}
pub(crate) fn float_depth_source_format(f:TextureFormat)->bool {
    matches!(f,TextureFormat::R32Float|TextureFormat::Rg32Float|TextureFormat::Rgba32Float
        |TextureFormat::R16Float|TextureFormat::Rg16Float|TextureFormat::Rgba16Float
        |TextureFormat::Rgba8Unorm|TextureFormat::Rgba8UnormSrgb|TextureFormat::Bgra8Unorm|TextureFormat::Bgra8UnormSrgb)
}
pub(crate) fn validate_original_omission(o:&OriginalOmission)->Result<()> {
    require(serde_json::to_vec(o)?.len()<=64<<10,"original omission metadata bound")?;
    require(o.event_index>0&&o.event_index<1<<20&&o.command_sequence>0,"original omission event identity")?;
    require(o.skip_reason=="vertex_shader_unavailable"&&o.omission_seam=="vertex_shader_program_before_draw_preparation",
        "unsupported original omission seam")?;
    let v=&o.vertex_shader;
    require(v.guest_id>0&&v.hash=="6edc9adb5e47f1e0"&&v.stage==0&&!v.index_entry_present&&!v.translated_source_present&&v.program_state=="failed",
        "original omission shader evidence")?;
    require(o.topology==35&&o.requested_raw_words==o.resolved_raw_words&&o.resolved_raw_words[0]==1
        &&o.resolved_raw_words[1]>0&&o.resolved_raw_words[2]>0,"original omission direct indexed arguments")?;
    require(o.gpu_errors==0&&o.pipeline_jobs_pending==0&&o.pipeline_cache_pending==0&&o.gpu_commands_emitted==0
        &&o.original_draw_was_not_emitted&&!o.native_behavior_changed&&!o.requested_geometry_validated&&!o.pixel_visibility_known,
        "original omission is not a proven baseline no-GPU branch")
}
pub(crate) fn validate_finished_endpoint(scene:&Scene,command:&Command,depth_world:&serde_json::Value,p:&serde_json::Value)->Result<()> {
    let (Provenance::NativeFinishedGraph{capture_id,native_present,..}|Provenance::NativeExecutedFinishedGraph{capture_id,native_present,..})=&scene.provenance else{return Err("endpoint provenance".into())};
    require(!capture_id.is_empty()&&*native_present>0,"endpoint source identity")?;
    let Command::FinishedEndpoint{hud_free_source,hud_world,depth_source,..}=command else{return Err("endpoint command".into())};
    let obj=p.as_object().ok_or("endpoint publication object")?;
    let fields=["capture_id","native_present","accepted","source_id","source_generation","provenance_epoch","eye_index","request_frame_sequence","head_sequence","camera_sequence","world_view","graph_end_before_hud","postlude_original_only","hud_world_is_ready_hud_world","ready_hud_world_allocation_id","final_color_parent_allocation_id","hud_draw_count","hud_rejected","slot_color","slot_depth"];
    require(obj.len()==fields.len()&&obj.keys().all(|k|fields.contains(&k.as_str())),"endpoint publication fields")?;
    require(p["capture_id"].as_str()==Some(capture_id)&&p["native_present"].as_u64()==Some(*native_present),"endpoint callback capture/present changed")?;
    for k in ["accepted","graph_end_before_hud","postlude_original_only","hud_world_is_ready_hud_world"] {require(p[k]==true,"endpoint original publication proof missing")?;}
    require(p["hud_rejected"]==false,"endpoint HUD rejected")?;
    let num=|k:&str|p[k].as_u64().ok_or_else(||format!("endpoint numeric {k}"));
    require(num("source_id")?>0&&num("hud_draw_count")?>0,"endpoint accepted source/HUD count absent")?;num("source_generation")?;
    let published=crate::native_import::world(&p["world_view"])?;let depth=crate::native_import::world(depth_world)?;
    require(scene.eye.as_ref()==Some(&published)&&published==depth && p["world_view"]==*depth_world,"endpoint actual published depth world differs")?;
    require(num("provenance_epoch")?==published.epoch&&num("eye_index")?==u64::from(published.eye)
        && num("request_frame_sequence")?==published.request&&num("head_sequence")?==published.head&&num("camera_sequence")?==published.camera,
        "endpoint callback identity differs")?;
    let source=native_id(scene,&hud_free_source.version.slice)?;let world=native_id(scene,&hud_world.version.slice)?;let depth=native_id(scene,&depth_source.version.slice)?;
    require(source!=world&&hud_free_source.version.slice.aspect==Aspect::Color&&hud_world.version.slice.aspect==Aspect::Color
        &&num("ready_hud_world_allocation_id")?==world&&num("final_color_parent_allocation_id")?==source,"endpoint HUD world native ownership differs")?;
    for (name,source_id) in [("slot_color",source),("slot_depth",depth)] {
        let r=p[name].as_object().ok_or("endpoint slot copy receipt")?;
        require(r.len()==4&&["kind","source_allocation_id","destination_allocation_id","source_version"].iter().all(|k|r.contains_key(*k)),"endpoint slot receipt fields")?;
        let from=r["source_allocation_id"].as_u64().ok_or("endpoint copy source")?;let to=r["destination_allocation_id"].as_u64().ok_or("endpoint copy destination")?;
        require(from==source_id&&to>0&&match r["kind"].as_str(){Some("same_native_handle")=>from==to,Some("original_copy")=>from!=to,_=>false},"endpoint slot source identity differs")?;
        if name=="slot_depth" {
            let v=&r["source_version"];let expected=&depth_source.version;
            require(v["allocation_id"].as_u64()==Some(depth)&&v["content_version"].as_u64()==Some(expected.content_version)
                &&v["subresource"]["mip"].as_u64()==Some(u64::from(expected.slice.mip))
                &&v["subresource"]["layer"].as_u64()==Some(u64::from(expected.slice.layer))
                &&v["subresource"]["aspect"]==serde_json::to_value(&expected.slice.aspect)?,"endpoint slot depth selected another version")?;
        } else {require(r["source_version"].is_null() || r["source_version"].is_object(),"endpoint slot source version shape")?;}
    }
    let a=scene.textures.iter().find(|t|t.id==hud_free_source.version.slice.resource).ok_or("endpoint source texture")?;
    let b=scene.textures.iter().find(|t|t.id==hud_world.version.slice.resource).ok_or("endpoint HUD texture")?;
    let sa=&hud_free_source.version.slice;let sb=&hud_world.version.slice;
    require(sa.mip<a.descriptor.mips&&sb.mip<b.descriptor.mips,"endpoint mip outside allocation")?;
    let aw=(a.descriptor.width>>sa.mip).max(1);let ah=(a.descriptor.height>>sa.mip).max(1);
    let bw=(b.descriptor.width>>sb.mip).max(1);let bh=(b.descriptor.height>>sb.mip).max(1);
    require(a.descriptor.dimension==TextureDimension::D2&&b.descriptor.dimension==TextureDimension::D2
        &&sa.mip==0&&sa.layer==0&&a.descriptor.layers==1
        &&matches!(a.descriptor.format,TextureFormat::Bgra8Unorm|TextureFormat::Rgba8Unorm)
        &&a.descriptor.format==b.descriptor.format&&aw==bw&&ah==bh,"endpoint HUD copy extent/format")?;
    let depth_texture=scene.textures.iter().find(|t|t.id==depth_source.version.slice.resource).ok_or("endpoint selected depth texture")?;
    let ds=&depth_source.version.slice;let dd=&depth_texture.descriptor;
    require(sb.mip==0&&sb.layer==0&&b.descriptor.mips==1&&b.descriptor.layers==1
        &&ds.mip==0&&ds.layer==0&&dd.layers==1&&ds.mip<dd.mips&&dd.dimension==TextureDimension::D2&&dd.format==TextureFormat::R32Float
        &&ds.aspect==Aspect::Color&&(dd.width>>ds.mip).max(1)==aw&&(dd.height>>ds.mip).max(1)==ah,
        "endpoint HUD world/published depth shape")?;
    require(scene.commands.iter().any(|c|match c {
        Command::CopyRegion{sources,destinations,source_origin,destination_origin,extent}=>*source_origin==[0,0,0]&&*destination_origin==[0,0,0]&&*extent==[aw,ah,1]
            &&sources.len()==1&&destinations.len()==1
            &&sources.iter().any(|v|v.slice==*sa&&v.content_version==hud_free_source.version.content_version)
            &&destinations.iter().any(|v|v.slice==*sb&&v.after==hud_world.version.content_version),
        Command::Copy{source,destination}=>source.slice==*sa&&source.content_version==hud_free_source.version.content_version&&destination.slice==*sb&&destination.after==hud_world.version.content_version,
        _=>false}),"endpoint HUD world lacks original full copy lineage")?;
    Ok(())
}
fn validate_clear(v: &ClearValue, a: &Aspect) -> Result<()> {
    require(
        match (v, a) {
            (ClearValue::Color { rgba_bits }, Aspect::Color) => {
                rgba_bits.iter().all(|b| f64::from_bits(*b).is_finite())
            }
            (ClearValue::Depth { value_bits }, Aspect::Depth) => {
                (0.0..=1.0).contains(&f32::from_bits(*value_bits))
            }
            (ClearValue::Stencil { value }, Aspect::Stencil) => *value <= 255,
            _ => false,
        },
        "clear aspect/value",
    )
}
fn advance(t: &Transition, v: &mut BTreeMap<Slice, u64>, last: &mut u64) -> Result<()> {
    require(
        v.get(&t.slice) == Some(&t.before) && t.after > t.before && t.after > *last,
        "stale/nonmonotonic content version",
    )?;
    v.insert(t.slice.clone(), t.after);
    *last = t.after;
    Ok(())
}
fn check_version(
    v: &Version,
    current: &BTreeMap<Slice, u64>,
    known: &BTreeSet<Slice>,
) -> Result<()> {
    require(
        current.get(&v.slice) == Some(&v.content_version) && known.contains(&v.slice),
        "stale version or unknown source contents",
    )
}
pub fn written_slices(d: &Draw, p: &Pipeline) -> BTreeSet<Slice> {
    let mut result = BTreeSet::new();
    if p.fragment.is_some() {
        for (a, t) in d.colors.iter().zip(&p.targets) {
            if let (Some(a), Some(t)) = (a, t) {
                if !t.write_mask.is_empty() {
                    result.insert(Slice {
                        resource: a.resource.clone(),
                        mip: a.mip,
                        layer: a.layer,
                        aspect: Aspect::Color,
                    });
                }
            }
        }
    }
    if let (Some(a), Some(state)) = (&d.depth, &p.depth_stencil) {
        if state.depth_write_enabled == Some(true) && !a.read_only_depth {
            result.insert(Slice {
                resource: a.attachment.resource.clone(),
                mip: a.attachment.mip,
                layer: a.attachment.layer,
                aspect: Aspect::Depth,
            });
        }
        let op = |s: &StencilFaceState| {
            s.fail_op != StencilOperation::Keep
                || s.depth_fail_op != StencilOperation::Keep
                || s.pass_op != StencilOperation::Keep
        };
        if state.stencil.write_mask != 0
            && !a.read_only_stencil
            && (op(&state.stencil.front) || op(&state.stencil.back))
        {
            result.insert(Slice {
                resource: a.attachment.resource.clone(),
                mip: a.attachment.mip,
                layer: a.attachment.layer,
                aspect: Aspect::Stencil,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compute_binding_versions_writes_and_dispatch_boundaries() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("compute-admission-{}", std::process::id()));
        crate::compute_fixture::make(&dir)?;
        let base = Input::load(&dir.join("scene.json"))?;
        fn dispatch(s: &mut Scene) -> &mut Dispatch {
            match &mut s.commands[0] { Command::Dispatch { dispatch } => dispatch, _ => unreachable!() }
        }
        let reject = |name: &str, change: &dyn Fn(&mut Scene)| -> Result<()> {
            let mut i = Input { scene: base.scene.clone(), bytes: base.bytes.clone(),
                manifest_sha256: base.manifest_sha256.clone(), targets_have_resolution_independent_initialization: true };
            change(&mut i.scene);
            assert!(i.validate().is_err(), "accepted {name}");
            Ok(())
        };
        reject("zero workgroups", &|s| dispatch(s).workgroups[0] = 0)?;
        reject("oversized axis", &|s| dispatch(s).workgroups[0] = 65536)?;
        reject("oversized total dispatch", &|s| dispatch(s).workgroups = [65535, 65535, 1])?;
        reject("missing compute pipeline", &|s| dispatch(s).pipeline = "absent".into())?;
        reject("changed shader hash", &|s| s.compute_pipelines[0].shader.wgsl.push(' '))?;
        reject("nonfinal shader", &|s| s.compute_pipelines[0].shader.exact_final_variant = false)?;
        reject("wrong stage visibility", &|s| s.compute_pipelines[0].layouts[0][0].visibility = ShaderStages::FRAGMENT)?;
        reject("writable storage buffer", &|s| s.compute_pipelines[0].layouts[0][1].ty = BindingType::Buffer {
            ty: BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None })?;
        reject("dynamic buffer", &|s| s.compute_pipelines[0].layouts[0][1].ty = BindingType::Buffer {
            ty: BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: None })?;
        reject("readwrite storage texture", &|s| s.compute_pipelines[0].layouts[0][2].ty = BindingType::StorageTexture {
            access: StorageTextureAccess::ReadWrite, format: TextureFormat::Rgba32Float, view_dimension: TextureViewDimension::D2 })?;
        reject("unsupported storage dimension", &|s| s.compute_pipelines[0].layouts[0][2].ty = BindingType::StorageTexture {
            access: StorageTextureAccess::WriteOnly, format: TextureFormat::Rgba32Float, view_dimension: TextureViewDimension::D3 })?;
        reject("missing binding", &|s| { dispatch(s).bindings.pop(); })?;
        reject("duplicate binding", &|s| { let b = dispatch(s).bindings[0].clone(); dispatch(s).bindings.push(b); })?;
        reject("unaligned binding offset", &|s| if let BindingValue::Buffer { offset, .. } = &mut dispatch(s).bindings[1].value { *offset = 1; })?;
        reject("outside immutable range", &|s| if let BindingValue::Buffer { size, .. } = &mut dispatch(s).bindings[1].value { *size = 32; })?;
        reject("less than minimum binding size", &|s| if let BindingValue::Buffer { size, .. } = &mut dispatch(s).bindings[1].value { *size = 4; })?;
        reject("stale source version", &|s| if let BindingValue::Texture { versions, .. } = &mut dispatch(s).bindings[0].value { versions[0].content_version = 0; })?;
        reject("missing output before version", &|s| if let BindingValue::Texture { versions, .. } = &mut dispatch(s).bindings[2].value { versions.clear(); })?;
        reject("multiple output mips", &|s| if let BindingValue::Texture { view, .. } = &mut dispatch(s).bindings[2].value { view.base_mip_level = 0; view.mip_level_count = Some(2); })?;
        reject("output view usage incompatible", &|s| if let BindingValue::Texture { view, .. } = &mut dispatch(s).bindings[2].value { view.usage = Some(TextureUsages::TEXTURE_BINDING); })?;
        reject("sampled-output alias", &|s| { let output = dispatch(s).bindings[2].value.clone(); dispatch(s).bindings[0].value = output; })?;
        reject("unknown output initial bytes", &|s| s.textures[1].initial[3].origin = Initial::Unknown { reason: "unretained".into() })?;
        reject("immutable storage output", &|s| s.textures[1].mutable_target = false)?;
        reject("missing transition", &|s| dispatch(s).writes.clear())?;
        reject("extraneous transition", &|s| { let mut x = dispatch(s).writes[0].clone(); x.slice.resource = "compute-source".into(); dispatch(s).writes.push(x); })?;
        reject("duplicate transition", &|s| { let x = dispatch(s).writes[0].clone(); dispatch(s).writes.push(x); })?;
        reject("stale transition", &|s| dispatch(s).writes[0].before = 0)?;
        reject("nonadvancing transition", &|s| dispatch(s).writes[0].after = 1)?;
        reject("stale following raster", &|s| if let Command::Draw { draw } = &mut s.commands[1] {
            if let BindingValue::Texture { versions, .. } = &mut draw.bindings[0].value { versions[0].content_version = 1; }
        })?;
        reject("eye attribution changed", &|s| dispatch(s).eye = Some(EyeIdentity { epoch: 1, camera: 1, request: 1, head: 1, eye: 0 }))?;
        // Older raster manifests remain readable with the new field absent.
        let mut legacy = serde_json::to_value(&base.scene)?;
        legacy.as_object_mut().unwrap().remove("compute_pipelines");
        assert!(serde_json::from_value::<Scene>(legacy)?.compute_pipelines.is_empty());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn native_unattributed_compute_never_establishes_raster_eye() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("native-compute-admission-{}", std::process::id()));
        crate::compute_fixture::make(&dir)?;
        let mut input = Input::load(&dir.join("scene.json"))?;
        input.scene.provenance = Provenance::Native { source_sha256: "0".repeat(64) };
        input.scene.pipelines.clear();
        input.scene.commands.truncate(1);
        input.scene.expected.retain(|e| e.version.slice.resource == "compute-storage");
        let mut versions = BTreeMap::new();
        let mut generation = 0;
        for (n, t) in input.scene.textures.iter_mut().enumerate() {
            t.native_allocation_id = Some(n as u64 + 1);
            for i in &mut t.initial {
                generation += 1; i.content_version = generation;
                versions.insert(Slice { resource: t.id.clone(), mip: i.mip, layer: i.layer, aspect: i.aspect.clone() }, generation);
            }
        }
        input.scene.buffers[0].native_allocation_id = Some(100);
        if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] {
            for b in &mut dispatch.bindings {
                if let BindingValue::Texture { versions: refs, .. } = &mut b.value {
                    for r in refs { r.content_version = versions[&r.slice]; }
                }
            }
            for t in &mut dispatch.writes { t.before = versions[&t.slice]; generation += 1; t.after = generation; versions.insert(t.slice.clone(), generation); }
        }
        for e in &mut input.scene.expected { e.version.content_version = versions[&e.version.slice]; }
        input.validate()?;
        assert!(input.scene.eye.is_none());
        let prior_after = if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] {
            let old = dispatch.writes[0].after;
            dispatch.writes[0].after = generation - 1;
            old
        } else { unreachable!() };
        assert!(input.validate().unwrap_err().to_string().contains("native compute reused initial content version"));
        if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] { dispatch.writes[0].after = prior_after; }

        input.scene.commands.push(Command::Clear { transition: Transition { slice: input.scene.expected[0].version.slice.clone(),
            before: 1, after: 100 }, value: ClearValue::Color { rgba_bits: [0; 4] }, view_format: TextureFormat::Rgba32Float });
        assert!(input.validate().unwrap_err().to_string().contains("native same-eye identity"));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn native_graph_raster_anchor_does_not_assign_compute_eye() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("native-graph-eye-{}", std::process::id()));
        crate::compute_fixture::make(&dir)?;
        let mut input = Input::load(&dir.join("scene.json"))?;
        let eye = EyeIdentity { epoch: 2, camera: 3, request: 4, head: 5, eye: 1 };
        input.scene.provenance = Provenance::NativeGraph { source_sha256: "0".repeat(64) };
        input.scene.eye = Some(eye.clone());
        let mut current = BTreeMap::new();
        let mut generation = 0;
        for (n, t) in input.scene.textures.iter_mut().enumerate() {
            t.native_allocation_id = Some(n as u64 + 1);
            for i in &mut t.initial {
                generation += 1; i.content_version = generation;
                current.insert(Slice { resource: t.id.clone(), mip: i.mip, layer: i.layer, aspect: i.aspect.clone() }, generation);
            }
        }
        input.scene.buffers[0].native_allocation_id = Some(100);
        for c in &mut input.scene.commands {
            let (bindings, writes) = match c {
                Command::Dispatch { dispatch } => (&mut dispatch.bindings, &mut dispatch.writes),
                Command::Draw { draw } => { draw.eye = Some(eye.clone()); (&mut draw.bindings, &mut draw.writes) },
                _ => unreachable!(),
            };
            for b in bindings {
                if let BindingValue::Texture { versions, .. } = &mut b.value {
                    for r in versions { r.content_version = current[&r.slice]; }
                }
            }
            for t in writes { t.before = current[&t.slice]; generation += 1; t.after = generation; current.insert(t.slice.clone(), generation); }
        }
        for e in &mut input.scene.expected { e.version.content_version = current[&e.version.slice]; }
        input.validate()?;
        let anchored_commands = input.scene.commands.clone();
        input.scene.commands.retain(|c| !matches!(c, Command::Draw { .. }));
        assert!(input.validate().unwrap_err().to_string().contains("native graph raster anchor absent"));
        input.scene.commands = anchored_commands;
        input.scene.eye = None;
        assert!(input.validate().unwrap_err().to_string().contains("native same-eye identity"));
        input.scene.eye = Some(eye.clone());
        if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] { dispatch.eye = Some(EyeIdentity { eye: 0, ..eye.clone() }); }
        assert!(input.validate().unwrap_err().to_string().contains("compute eye changed"));
        if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] { dispatch.eye = Some(eye.clone()); }
        input.validate()?;
        if let Command::Dispatch { dispatch } = &mut input.scene.commands[0] { dispatch.eye = None; }
        if let Command::Draw { draw } = &mut input.scene.commands[1] { draw.eye = None; }
        assert!(input.validate().unwrap_err().to_string().contains("draw count/same-eye changed"));
        if let Command::Draw { draw } = &mut input.scene.commands[1] { draw.eye = Some(eye); }
        input.scene.provenance = Provenance::Native { source_sha256: "0".repeat(64) };
        assert!(input.validate().unwrap_err().to_string().contains("compute eye changed"));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn clear_reference_is_optional_but_has_exact_original_extent() -> Result<()> {
        let origin = Initial::UniformClear {
            value: ClearValue::Color { rgba_bits: [0; 4] },
            view_format: TextureFormat::Rgba8Unorm,
            operation: 1,
            reference_blob: Some("clear-reference.bin".into()),
        };
        let mut old = serde_json::to_value(&origin)?;
        old.as_object_mut().unwrap().remove("reference_blob");
        assert!(matches!(
            serde_json::from_value::<Initial>(old)?,
            Initial::UniformClear {
                reference_blob: None,
                ..
            }
        ));
        let dir = std::env::temp_dir().join(format!(
            "native-clear-reference-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        crate::volume_fixture::make(&dir)?;
        let mut input = Input::load(&dir.join("scene.json"))?;
        input
            .bytes
            .insert("clear-reference.bin".into(), vec![0; 8 * 4 * 4]);
        input
            .scene
            .textures
            .iter_mut()
            .find(|t| t.id == "volume-color")
            .unwrap()
            .initial[0]
            .origin = origin;
        input.validate()?;
        input.bytes.get_mut("clear-reference.bin").unwrap().pop();
        assert!(input.validate().is_err());
        input.bytes.remove("clear-reference.bin");
        assert!(input.validate().is_err());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn authored_volume_extent_and_dimension_admission() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "native-volume-admission-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        crate::volume_fixture::make(&dir)?;
        let base = Input::load(&dir.join("scene.json"))?;
        let reject = |label: &str, change: &dyn Fn(&mut Input)| -> Result<()> {
            let mut input = Input {
                scene: base.scene.clone(),
                bytes: base.bytes.clone(),
                manifest_sha256: base.manifest_sha256.clone(),
                targets_have_resolution_independent_initialization: false,
            };
            change(&mut input);
            require(
                input.validate().is_err(),
                &format!("invalid volume fixture admitted: {label}"),
            )
        };
        reject("missing z plane", &|i| {
            i.bytes.get_mut("volume-mip1.bin").unwrap().truncate(16);
        })?;
        reject("extra z plane", &|i| {
            i.bytes.get_mut("volume-mip1.bin").unwrap().extend([0; 16]);
        })?;
        for (field, value) in [("height", 2), ("layers", 2), ("mips", 2)] {
            reject("invalid D1 allocation", &|i| {
                let d = &mut i
                    .scene
                    .textures
                    .iter_mut()
                    .find(|t| t.id == "line")
                    .unwrap()
                    .descriptor;
                match field {
                    "height" => d.height = value,
                    "layers" => d.layers = value,
                    _ => d.mips = value,
                }
            })?;
        }
        reject("D3 render target", &|i| {
            i.scene
                .textures
                .iter_mut()
                .find(|t| t.id == "volume")
                .unwrap()
                .mutable_target = true;
        })?;
        reject("D3-as-array layer", &|i| {
            i.scene
                .textures
                .iter_mut()
                .find(|t| t.id == "volume")
                .unwrap()
                .initial[1]
                .layer = 1;
        })?;
        reject("D3 shader layout dimension", &|i| {
            if let BindingType::Texture { view_dimension, .. } =
                &mut i.scene.pipelines[0].layouts[1][0].ty
            {
                *view_dimension = TextureViewDimension::D2Array;
            }
        })?;
        reject("noPS bound original color", &|i| {
            if let Command::Draw { draw } = i.scene.commands.last_mut().unwrap() {
                draw.colors = vec![Some(Attachment {
                    resource: "volume-color".into(),
                    mip: 0,
                    layer: 0,
                    view_format: TextureFormat::Rgba8Unorm,
                })];
            }
        })?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn volume_layout_and_view_contract() -> Result<()> {
        let d = TextureDesc {
            dimension: TextureDimension::D3,
            width: 4,
            height: 4,
            layers: 4,
            mips: 3,
            format: TextureFormat::Rgba8Unorm,
            view_formats: vec![],
        };
        assert_eq!(slice_bytes(&d, 0, &Aspect::Color)?, (4, 4, 16, 16));
        assert_eq!(slice_bytes(&d, 1, &Aspect::Color)?, (2, 2, 8, 4));
        assert_eq!(slice_bytes(&d, 2, &Aspect::Color)?, (1, 1, 4, 1));
        let t = Texture {
            id: "volume".into(),
            native_allocation_id: None,
            descriptor: d,
            mutable_target: false,
            initial: vec![],
        };
        let input = Input {
            scene: Scene {
                schema: 1,
                provenance: Provenance::Authored {
                    description: "CPU view contract".into(),
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
            },
            bytes: BTreeMap::new(),
            manifest_sha256: String::new(),
            targets_have_resolution_independent_initialization: false,
        };
        let view = TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D3),
            base_mip_level: 1,
            mip_level_count: Some(2),
            ..Default::default()
        };
        let slices = input.view_slices(&t, &view)?;
        assert_eq!(slices.len(), 2);
        assert!(slices.iter().all(|s| s.layer == 0 && s.mip >= 1));
        for bad in [
            TextureViewDescriptor {
                dimension: Some(TextureViewDimension::D2),
                ..view.clone()
            },
            TextureViewDescriptor {
                base_array_layer: 1,
                ..view.clone()
            },
            TextureViewDescriptor {
                array_layer_count: Some(2),
                ..view.clone()
            },
            TextureViewDescriptor {
                mip_level_count: Some(3),
                ..view.clone()
            },
        ] {
            require(
                input.view_slices(&t, &bad).is_err(),
                "invalid volume view admitted",
            )?;
        }
        let mut one = t.clone();
        one.descriptor.dimension = TextureDimension::D1;
        one.descriptor.height = 1;
        one.descriptor.layers = 1;
        one.descriptor.mips = 1;
        assert_eq!(
            slice_bytes(&one.descriptor, 0, &Aspect::Color)?,
            (4, 1, 16, 1)
        );
        assert_eq!(
            input
                .view_slices(&one, &TextureViewDescriptor::default())?
                .len(),
            1
        );
        require(
            input
                .view_slices(
                    &one,
                    &TextureViewDescriptor {
                        dimension: Some(TextureViewDimension::D2),
                        ..Default::default()
                    },
                )
                .is_err(),
            "1D/2D reinterpretation admitted",
        )?;
        Ok(())
    }
    #[test]
    fn mixed_fixture_admission_and_fail_closed_boundaries() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "native-scene-admission-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        crate::fixture::make(&dir)?;
        let base = Input::load(&dir.join("scene.json"))?;
        let rejected = |label: &str, modify: &dyn Fn(&mut Scene)| -> Result<()> {
            let mut i = Input {
                scene: base.scene.clone(),
                bytes: base.bytes.clone(),
                manifest_sha256: base.manifest_sha256.clone(),
                targets_have_resolution_independent_initialization: false,
            };
            modify(&mut i.scene);
            require(
                i.validate().is_err(),
                &format!("unexpected admission: {label}"),
            )?;
            Ok(())
        };
        rejected("no-fragment pipeline with bound colors", &|s| {
            s.pipelines[0].fragment = None;
            s.pipelines[0].targets.clear();
        })?;
        rejected("unknown initial load", &|s| {
            s.commands.remove(0);
        })?;
        rejected("incorrect clear aspect", &|s| {
            if let Command::Clear { value, .. } = &mut s.commands[0] {
                *value = ClearValue::Depth { value_bits: 0 };
            }
        })?;
        rejected("overflow dimensions", &|s| {
            s.textures[0].descriptor.width = u32::MAX;
            s.textures[0].descriptor.height = u32::MAX;
            s.textures[0].descriptor.layers = u32::MAX;
        })?;
        rejected("missing initialization", &|s| {
            s.textures[0].initial.clear();
        })?;
        rejected("missing transition", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    draw.writes.clear();
                    break;
                }
            }
        })?;
        rejected("different eye", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    draw.eye = Some(EyeIdentity {
                        epoch: 1,
                        camera: 1,
                        request: 1,
                        head: 1,
                        eye: 1,
                    });
                    break;
                }
            }
        })?;
        rejected("stale selected version", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    for b in &mut draw.bindings {
                        if let BindingValue::Texture { versions, .. } = &mut b.value {
                            versions[0].content_version += 100;
                            return;
                        }
                    }
                }
            }
        })?;
        rejected("unregistered source view", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    for b in &mut draw.bindings {
                        if let BindingValue::Texture { view, .. } = &mut b.value {
                            view.format = Some(TextureFormat::Rgba16Float);
                            return;
                        }
                    }
                }
            }
        })?;
        rejected("missing original command", &|s| {
            s.omitted_commands = 1;
        })?;
        rejected("not final shader", &|s| {
            s.pipelines[0].vertex.exact_final_variant = false;
        })?;
        rejected("out of snapshot indices", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    if let DrawArgs::Indexed { indices, .. } = &mut draw.args {
                        *indices = u32::MAX;
                        return;
                    }
                }
            }
        })?;
        rejected("out of snapshot vertex", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    if let DrawArgs::Indexed { base_vertex, .. } = &mut draw.args {
                        *base_vertex = i32::MAX;
                        return;
                    }
                }
            }
        })?;
        rejected("sampled output alias", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    let a = draw.colors[0].as_ref().unwrap().clone();
                    for b in &mut draw.bindings {
                        if let BindingValue::Texture {
                            texture,
                            view,
                            versions,
                        } = &mut b.value
                        {
                            *texture = a.resource.clone();
                            view.base_mip_level = 0;
                            view.mip_level_count = Some(1);
                            view.base_array_layer = 0;
                            view.array_layer_count = Some(1);
                            view.format = Some(a.view_format);
                            versions[0].slice = Slice {
                                resource: a.resource,
                                mip: 0,
                                layer: 0,
                                aspect: Aspect::Color,
                            };
                            return;
                        }
                    }
                }
            }
        })?;
        rejected("native allocation aliases", &|s| {
            s.provenance = Provenance::Native {
                source_sha256: "0".repeat(64),
            };
            s.eye = Some(EyeIdentity {
                epoch: 1,
                camera: 1,
                request: 1,
                head: 1,
                eye: 0,
            });
            let mut version = 1;
            for t in &mut s.textures {
                t.native_allocation_id = Some(1);
                for i in &mut t.initial {
                    i.content_version = version;
                    version += 1;
                }
            }
        })?;
        rejected("writes read-only depth", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    if matches!(draw.args, DrawArgs::Indexed { .. }) {
                        draw.depth.as_mut().unwrap().read_only_depth = true;
                        return;
                    }
                }
            }
        })?;
        rejected("self-copy", &|s| {
            for c in &mut s.commands {
                if let Command::Copy {
                    source,
                    destination,
                } = c
                {
                    destination.slice = source.slice.clone();
                    destination.before = source.content_version;
                    return;
                }
            }
        })?;
        rejected("read-only conservative transition", &|s| {
            for c in &mut s.commands {
                if let Command::Draw { draw } = c {
                    let ds = draw.depth.as_mut().unwrap();
                    ds.read_only_depth = true;
                    draw.writes.push(Transition {
                        slice: Slice {
                            resource: ds.attachment.resource.clone(),
                            mip: 0,
                            layer: 0,
                            aspect: Aspect::Depth,
                        },
                        before: 2,
                        after: 999,
                    });
                    return;
                }
            }
        })?;
        let mut conservative = Input {
            scene: base.scene.clone(),
            bytes: base.bytes.clone(),
            manifest_sha256: base.manifest_sha256.clone(),
            targets_have_resolution_independent_initialization: false,
        };
        for p in &mut conservative.scene.pipelines {
            for t in p.targets.iter_mut().flatten() {
                t.write_mask = ColorWrites::empty();
            }
        }
        conservative.validate()?; // Same native invalidations are valid even when writes preserve bytes.
                                  // Retain failed artifacts on assertion; successful authored test directory is disposable.
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

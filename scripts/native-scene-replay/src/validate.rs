use crate::{model::*, require, sha, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};
use wgpu_types::*;
pub const MAX_BYTES: u64 = 256 << 20;
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
    Ok((w, h, w.div_ceil(bw) * bpp, h.div_ceil(bh)))
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
                total <= MAX_BYTES && fs::metadata(&p)?.len() == b.bytes,
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
                && s.pipelines.len() <= 4096,
            "scene bounds",
        )?;
        require(
            s.original_submission_completed
                && s.original_gpu_errors.is_none_or(|n| n == 0)
                && s.omitted_commands == 0,
            "original closure missing",
        )?;
        let native = matches!(s.provenance, Provenance::Native { .. });
        if native {
            require(
                s.original_gpu_errors == Some(0),
                "native original GPU errors unobserved",
            )?;
            require(
                s.eye.as_ref().is_some_and(|e| {
                    e.eye < 2 && e.epoch > 0 && e.camera > 0 && e.request > 0 && e.head > 0
                }),
                "native same-eye identity",
            )?;
        }
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
                    && d.layers <= 256
                    && d.mips > 0
                    && d.mips <= 15
                    && d.mips <= 32 - d.width.max(d.height).leading_zeros(),
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
                    .and_then(|n| n.checked_mul(d.layers as u64))
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
                            | TextureFormat::Rgba16Float
                            | TextureFormat::R32Float
                            | TextureFormat::Depth32Float
                            | TextureFormat::Depth32FloatStencil8
                    ),
                "unsupported persistent target format",
            )?;
            for i in &t.initial {
                require(
                    !native
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
                    i.layer < d.layers
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
                    } => {
                        require(
                            t.mutable_target && *operation > 0,
                            "only actual target clear",
                        )?;
                        validate_clear(value, &i.aspect)?;
                        validate_view_format(d, *view_format)?;
                        initialized.insert(key);
                    }
                }
            }
            for mip in 0..d.mips {
                for layer in 0..d.layers {
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
                    && p.multisample.count == 1,
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
        let mut draws = 0;
        let mut last_write = if native {
            native_initial_versions
                .iter()
                .next_back()
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
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
                Command::Draw { draw: d } => {
                    draws += 1;
                    require(
                        draws <= 4096 && d.eye == s.eye,
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
                                    matches!(e.ty, BindingType::Texture { .. }),
                                    "texture layout",
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
                            attached.contains(&x.slice) && writes.insert(x.slice.clone()),
                            "write transition outside attachments",
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
        let mut output_bytes = 0u64;
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
            require(output_bytes <= MAX_BYTES, "output readback bound")?;
        }
        self.targets_have_resolution_independent_initialization = independent;
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
        let layers = v
            .array_layer_count
            .unwrap_or(d.layers.saturating_sub(v.base_array_layer));
        require(
            m > 0
                && layers > 0
                && v.base_mip_level.checked_add(m).is_some_and(|n| n <= d.mips)
                && v.base_array_layer
                    .checked_add(layers)
                    .is_some_and(|n| n <= d.layers)
                && matches!(
                    v.dimension,
                    Some(
                        TextureViewDimension::D2
                            | TextureViewDimension::D2Array
                            | TextureViewDimension::Cube
                            | TextureViewDimension::CubeArray
                    ) | None
                ),
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
fn validate_view_format(d: &TextureDesc, f: TextureFormat) -> Result<()> {
    require(
        f == d.format || d.view_formats.contains(&f),
        "unregistered reinterpretation",
    )
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
        // Retain failed artifacts on assertion; successful authored test directory is disposable.
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

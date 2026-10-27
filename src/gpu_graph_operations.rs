//! Exact auxiliary operations from the original native encoder. This module
//! records commands and immutable inputs; it never substitutes rendering,
//! submits an encoder, waits, or writes query/readback results into the guest.
use super::*;
use std::sync::Weak;
use texture_ledger::{Aspect, ClearValue, VersionRef};

const MAX_QUERY_IDENTITIES: usize = 4096;
const MAX_AUX_EVENTS: u64 = 8192;

#[derive(Default)]
pub(super) struct AuxState {
    queries: Vec<Weak<QueryLife>>,
    events: u64,
    // Original engine helper allocations live outside the guest object table.
    // Retained actual parent handles prevent ID reuse through pool recycling.
    external: Vec<(Texture, u64)>,
}

impl AuxState {
    fn query_id(&mut self, life: &Arc<QueryLife>) -> Result<u64, String> {
        let weak = Arc::downgrade(life);
        if let Some(i) = self.queries.iter().position(|old| old.ptr_eq(&weak)) {
            return Ok(i as u64 + 1);
        }
        // Retaining Weak prevents allocation-address reuse while assigning no
        // ownership of the guest query's actual lifetime or completion.
        if self.queries.len() >= MAX_QUERY_IDENTITIES {
            return Err("finished query identity bound".into());
        }
        self.queries.push(weak);
        Ok(self.queries.len() as u64)
    }
}

struct Slice {
    texture: Texture,
    native_id: u64,
    mip: u32,
    layer: u32,
    references: Vec<VersionRef>,
}

pub(in super::super) struct AuxToken {
    kind: &'static str,
    event: u64,
    sequence: u64,
    sources: Vec<Slice>,
    destination: Slice,
    operation: Value,
}

fn descriptor(t: &Texture) -> Value {
    json!({"width":t.width(),"height":t.height(),"depth_or_array_layers":t.depth_or_array_layers(),
        "mip_level_count":t.mip_level_count(),"sample_count":t.sample_count(),
        "dimension":t.dimension(),"format":t.format()})
}
fn aspect(a: Aspect) -> TextureAspect {
    match a {
        Aspect::Color => TextureAspect::All,
        Aspect::Depth => TextureAspect::DepthOnly,
        Aspect::Stencil => TextureAspect::StencilOnly,
    }
}
fn aspects(format: TextureFormat) -> Result<Vec<Aspect>, String> {
    match format {
        TextureFormat::Depth32Float => Ok(vec![Aspect::Depth]),
        TextureFormat::Depth32FloatStencil8 => Ok(vec![Aspect::Depth, Aspect::Stencil]),
        f if !f.is_depth_stencil_format() && f.block_dimensions() == (1, 1) => {
            Ok(vec![Aspect::Color])
        }
        _ => Err(format!(
            "finished operation unsupported exact format {format:?}"
        )),
    }
}
fn native_id(gpu: &Gpu, texture: &Texture) -> Result<u64, String> {
    let mut found = None;
    for object in gpu.objs.iter().flatten() {
        let Obj::Tex(t) = object else { continue };
        let candidate = if &t.tex == texture {
            t.trace_native_identity
        } else {
            [&t.proxy_depth, &t.proxy_stencil, &t.proxy_fdepth]
                .into_iter()
                .flatten()
                .find(|p| &p.tex == texture)
                .and_then(|p| p.trace_native_identity)
        };
        if let Some(id) = candidate {
            if id == 0 || found.is_some_and(|previous| previous != id) {
                return Err("finished operation ambiguous actual native parent identity".into());
            }
            found = Some(id);
        }
    }
    found.ok_or("finished operation actual parent has no retained native identity".into())
}
fn d2_shape(t: &Texture, mip: u32, layer: u32) -> Result<[u32; 2], String> {
    if t.dimension() != TextureDimension::D2
        || t.sample_count() != 1
        || mip >= t.mip_level_count()
        || layer >= t.depth_or_array_layers()
    {
        return Err(format!(
            "finished operation requires single-sample selected D2 subresource: {:?} {:?} {}x{}x{} mip{mip}/{} layer{layer} samples{}",
            t.dimension(),
            t.format(),
            t.width(),
            t.height(),
            t.depth_or_array_layers(),
            t.mip_level_count(),
            t.sample_count()
        ));
    }
    aspects(t.format())?;
    Ok([(t.width() >> mip).max(1), (t.height() >> mip).max(1)])
}
fn float_depth_formats(source:TextureFormat,destination:TextureFormat)->Result<(),String> {
    if !matches!(source,TextureFormat::R32Float|TextureFormat::Rg32Float|TextureFormat::Rgba32Float
        |TextureFormat::R16Float|TextureFormat::Rg16Float|TextureFormat::Rgba16Float
        |TextureFormat::Rgba8Unorm|TextureFormat::Rgba8UnormSrgb|TextureFormat::Bgra8Unorm|TextureFormat::Bgra8UnormSrgb)
        || destination!=TextureFormat::Depth32Float {
        return Err(format!("finished float-to-depth exact formats unsupported: {source:?} -> {destination:?}"));
    }
    Ok(())
}
fn copy_shape(
    source: [u32; 2],
    destination: [u32; 2],
    source_origin: [u32; 3],
    destination_origin: [u32; 3],
    extent: [u32; 3],
) -> Result<bool, String> {
    if extent[0] == 0
        || extent[1] == 0
        || extent[2] != 1
        || source_origin[2] != 0
        || destination_origin[2] != 0
        || (0..2).any(|axis| {
            source_origin[axis]
                .checked_add(extent[axis])
                .is_none_or(|end| end > source[axis])
                || destination_origin[axis]
                    .checked_add(extent[axis])
                    .is_none_or(|end| end > destination[axis])
        })
    {
        return Err("finished copy selected-subresource bounds".into());
    }
    Ok(destination_origin == [0, 0, 0] && extent[..2] == destination)
}
fn clear_aspect(
    format: TextureFormat,
    view: TextureFormat,
    value: &ClearValue,
) -> Result<Aspect, String> {
    if format.remove_srgb_suffix() != view.remove_srgb_suffix() {
        return Err("finished clear incompatible actual view format".into());
    }
    let result = match value {
        ClearValue::Color { bits } if bits.iter().all(|b| f64::from_bits(*b).is_finite()) => {
            Aspect::Color
        }
        ClearValue::Depth { bits } if (0.0..=1.0).contains(&f32::from_bits(*bits)) => Aspect::Depth,
        ClearValue::Stencil { value } if *value <= 255 => Aspect::Stencil,
        _ => return Err("finished clear invalid native value".into()),
    };
    if !aspects(format)?.contains(&result) {
        return Err("finished clear value/aspect mismatch".into());
    }
    Ok(result)
}
fn advanced(before: &[VersionRef], after: &[VersionRef]) -> Result<(), String> {
    if before.is_empty()
        || before.len() != after.len()
        || before.iter().zip(after).any(|(b, a)| {
            b.allocation_id != a.allocation_id
                || b.subresource != a.subresource
                || a.content_version <= b.content_version
        })
        || after.iter().enumerate().any(|(i, a)| {
            after[..i]
                .iter()
                .any(|b| a.content_version == b.content_version)
        })
    {
        return Err(
            "finished original write did not advance each exact native subresource version".into(),
        );
    }
    Ok(())
}

fn readback_rows(
    format: TextureFormat,
    extent: [u32; 3],
    row_bytes: u32,
    rows: u32,
    destination_row_pitch: u32,
    destination_size: u64,
) -> Result<(), String> {
    if aspects(format)? != vec![Aspect::Color]
        || format
            .block_copy_size(None)
            .and_then(|n| n.checked_mul(extent[0]))
            != Some(row_bytes)
        || extent[2] != 1
        || rows == 0
        || rows != extent[1]
        || row_bytes == 0
        || destination_row_pitch < row_bytes
        || u64::from(destination_row_pitch)
            .checked_mul(u64::from(rows - 1))
            .and_then(|n| n.checked_add(u64::from(row_bytes)))
            .is_none_or(|n| n > destination_size)
    {
        return Err(
            "finished texture readback format or exact bounded row layout unsupported".into(),
        );
    }
    Ok(())
}

impl Capture {
    pub(in super::super) fn graph_aux_unsupported(
        &self,
        gpu: &Gpu,
        reason: &str,
    ) -> Result<(), String> {
        if self.aux_active(gpu)? {
            Err(reason.into())
        } else {
            Ok(())
        }
    }
    pub(in super::super) fn finished_native_identity(
        &self,
        gpu: &Gpu,
        texture: &Texture,
    ) -> Result<u64, String> {
        native_id(gpu, texture).or_else(|_| {
            self.graph
                .aux
                .external
                .iter()
                .find(|(t, _)| t == texture)
                .map(|(_, id)| *id)
                .ok_or("finished helper parent identity unavailable".into())
        })
    }
    pub(in super::super) fn graph_aux_register_texture(
        &mut self,
        gpu: &Gpu,
        texture: &Texture,
        origin: &str,
    ) -> Result<Option<u64>, String> {
        if !self.aux_active(gpu)? {
            return Ok(None);
        }
        if let Ok(id) = self.finished_native_identity(gpu, texture) {
            return Ok(Some(id));
        }
        if !matches!(
            origin,
            "original_preui_depth_snapshot" | "original_hud_world_snapshot"
        ) || self.graph.aux.external.len() >= 32
            || texture.mip_level_count() != 1
            || texture.depth_or_array_layers() != 1
        {
            return Err("finished helper allocation origin/shape/capacity unsupported".into());
        }
        d2_shape(texture, 0, 0)?;
        let id = crate::world_command_trace::native_texture_tag(true)
            .ok_or("finished helper allocation identity failed")?;
        let allocation = texture_ledger::Allocation {
            allocation_id: id,
            guest_id: 0,
            origin: origin.into(),
            width: texture.width(),
            height: texture.height(),
            layers: 1,
            mips: 1,
            dimension: "d2".into(),
            format: format!("{:?}", texture.format()),
            aspects: aspects(texture.format())?,
        };
        self.texture_ledger
            .as_mut()
            .ok_or("finished helper ledger absent")?
            .register(allocation);
        for a in aspects(texture.format())? {
            self.finished_version(id, 0, 0, a)?;
        }
        self.graph.aux.external.push((texture.clone(), id));
        Ok(Some(id))
    }
    pub(in super::super) fn graph_aux_external_copy_written(
        &mut self,
        gpu: &Gpu,
        source: &Texture,
        destination: &Texture,
    ) -> Result<(), String> {
        if !self.aux_active(gpu)? {
            return Ok(());
        }
        let sid = self.finished_native_identity(gpu, source)?;
        let did = self.finished_native_identity(gpu, destination)?;
        if source.format() != destination.format()
            || source.width() != destination.width()
            || source.height() != destination.height()
            || sid == did
            || source.mip_level_count() != 1
            || source.depth_or_array_layers() != 1
        {
            return Err("finished helper copy requires equal complete single subresources".into());
        }
        let source_refs = aspects(source.format())?
            .into_iter()
            .map(|a| self.finished_version(sid, 0, 0, a))
            .collect::<Result<Vec<_>, _>>()?;
        let ledger = self
            .texture_ledger
            .as_mut()
            .ok_or("finished helper copy ledger absent")?;
        for r in source_refs {
            ledger.produced(
                did,
                r.subresource,
                "original_helper_copy",
                vec![r],
                vec![],
                true,
            );
        }
        Ok(())
    }
    fn aux_active(&self, gpu: &Gpu) -> Result<bool, String> {
        if !self.captures_finished_graph() || self.graph.anchor.is_none() {
            return Ok(false);
        }
        if self.graph.present != gpu.presents.checked_add(1)
            || gpu.errors.load(Ordering::SeqCst) != 0
        {
            return Err(
                "finished auxiliary operation changed native present or has GPU errors".into(),
            );
        }
        if self.graph.aux.events >= MAX_AUX_EVENTS {
            return Err("finished auxiliary event bound".into());
        }
        Ok(true)
    }
    fn aux_slice(
        &self,
        gpu: &Gpu,
        texture: &Texture,
        mip: u32,
        layer: u32,
        selected: &[Aspect],
    ) -> Result<Slice, String> {
        d2_shape(texture, mip, layer)?;
        let id = self.finished_native_identity(gpu, texture)?;
        let references = selected
            .iter()
            .map(|a| self.finished_version(id, mip, layer, *a))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Slice {
            texture: texture.clone(),
            native_id: id,
            mip,
            layer,
            references,
        })
    }
    fn aux_snapshot(&mut self, gpu: &mut Gpu, slice: &Slice) -> Result<Vec<String>, String> {
        let mut blobs = Vec::with_capacity(slice.references.len());
        for r in &slice.references {
            if self.finished_version(
                slice.native_id,
                slice.mip,
                slice.layer,
                r.subresource.aspect,
            )? != *r
            {
                return Err("finished input version changed before immutable snapshot".into());
            }
            blobs.push(self.finished_snapshot(
                gpu,
                &slice.texture,
                slice.native_id,
                slice.mip,
                slice.layer,
                aspect(r.subresource.aspect),
            )?);
        }
        Ok(blobs)
    }
    fn aux_emit(&mut self, kind: &str, operation: Value) -> Result<(), String> {
        self.finished_record_operation(json!({"kind":kind,"capture_id":self.graph.capture_id,
            "native_present":self.graph.present,"operation":operation}))?;
        self.graph.aux.events += 1;
        Ok(())
    }
    /// Called before a real clear and after the actual parent/ledger registration.
    /// Metadata only, including when invoked while ensure_pass is being built.
    pub(in super::super) fn graph_aux_begin_clear(
        &mut self,
        gpu: &Gpu,
        texture: &Texture,
        mip: u32,
        layer: u32,
        view_format: TextureFormat,
        value: ClearValue,
    ) -> Result<Option<AuxToken>, String> {
        if !self.aux_active(gpu)? {
            return Ok(None);
        }
        let a = clear_aspect(texture.format(), view_format, &value)?;
        let destination = self.aux_slice(gpu, texture, mip, layer, &[a])?;
        let event = self.graph_next_event();
        let operation = json!({"event_index":event,"command_sequence":self.sequence,
            "before":destination.references[0],"descriptor":descriptor(texture),"view_format":view_format,"value":value});
        Ok(Some(AuxToken {
            kind: "clear",
            event,
            sequence: self.sequence,
            sources: vec![],
            destination,
            operation,
        }))
    }
    /// Only call at a safe outside-pass seam. Source and partial destination
    /// inputs are copied before the original native TextureAspect::All copy.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn graph_aux_begin_copy(
        &mut self,
        gpu: &mut Gpu,
        source: &Texture,
        source_mip: u32,
        source_layer: u32,
        destination: &Texture,
        destination_mip: u32,
        destination_layer: u32,
        source_origin: [u32; 3],
        destination_origin: [u32; 3],
        extent: [u32; 3],
    ) -> Result<Option<AuxToken>, String> {
        if !self.aux_active(gpu)? {
            return Ok(None);
        }
        if self.pending.as_ref().is_some_and(|p| !p.finished) {
            return Err("finished copy began inside an unclosed raster interval".into());
        }
        if source.format() != destination.format() || source == destination {
            return Err("finished copy unsupported format conversion or same-parent alias".into());
        }
        let source_size = d2_shape(source, source_mip, source_layer)?;
        let destination_size = d2_shape(destination, destination_mip, destination_layer)?;
        let full = copy_shape(
            source_size,
            destination_size,
            source_origin,
            destination_origin,
            extent,
        )?;
        if source.format().is_depth_stencil_format()
            && (!full || source_size != destination_size || source_origin != [0, 0, 0])
        {
            return Err(
                "finished depth/stencil copy requires equal full native subresources".into(),
            );
        }
        let selected = aspects(source.format())?;
        let source = self.aux_slice(gpu, source, source_mip, source_layer, &selected)?;
        let destination = self.aux_slice(
            gpu,
            destination,
            destination_mip,
            destination_layer,
            &selected,
        )?;
        let source_blobs = self.aux_snapshot(gpu, &source)?;
        let initial_blobs = if full {
            vec![Value::Null; selected.len()]
        } else {
            self.aux_snapshot(gpu, &destination)?
                .into_iter()
                .map(Value::String)
                .collect()
        };
        let event = self.graph_next_event();
        let operation = json!({"event_index":event,"command_sequence":self.sequence,
            "sources":source.references,"before":destination.references,
            "source_descriptor":descriptor(&source.texture),"destination_descriptor":descriptor(&destination.texture),
            "source_origin":source_origin,"destination_origin":destination_origin,"extent":extent,
            "source_blobs":source_blobs,"initial_blobs":initial_blobs});
        Ok(Some(AuxToken {
            kind: "copy_region",
            event,
            sequence: self.sequence,
            sources: vec![source],
            destination,
            operation,
        }))
    }
    pub(in super::super) fn graph_aux_begin_mip(
        &mut self,
        gpu: &mut Gpu,
        texture: &Texture,
        layer: u32,
        source_mip: u32,
        destination_mip: u32,
    ) -> Result<Option<AuxToken>, String> {
        if !self.aux_active(gpu)? {
            return Ok(None);
        }
        if self.pending.as_ref().is_some_and(|p| !p.finished) {
            return Err("finished mip began inside an unclosed raster interval".into());
        }
        if source_mip.checked_add(1) != Some(destination_mip)
            || texture.format().is_depth_stencil_format()
        {
            return Err("finished mip requires consecutive native color mips".into());
        }
        let source = self.aux_slice(gpu, texture, source_mip, layer, &[Aspect::Color])?;
        let destination = self.aux_slice(gpu, texture, destination_mip, layer, &[Aspect::Color])?;
        let source_blob = self.aux_snapshot(gpu, &source)?.remove(0);
        let sampler = SamplerDescriptor {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        };
        let shader = super::super::MIPMAP_WGSL;
        let event = self.graph_next_event();
        let operation = json!({"event_index":event,"command_sequence":self.sequence,"source":source.references[0],
            "before":destination.references[0],"source_blob":source_blob,"descriptor":descriptor(texture),
            "shader":{"wgsl":shader,"sha256":sha(shader.as_bytes()),"vertex_entry":"vs","fragment_entry":"fs","compilation_options":"wgpu default"},
            "sampler":sampler,"clear_rgba_bits":[0u64,0,0,1.0f64.to_bits()],"vertices":3,"instances":1});
        Ok(Some(AuxToken {
            kind: "generate_mip",
            event,
            sequence: self.sequence,
            sources: vec![source],
            destination,
            operation,
        }))
    }
    /// One actual native helper pass, in the original mip-outer/layer-inner loop.
    pub(in super::super) fn graph_aux_begin_float_depth(
        &mut self,gpu:&mut Gpu,source:&Texture,destination:&Texture,mip:u32,layer:u32,
    )->Result<Option<AuxToken>,String> {
        if !self.aux_active(gpu)? {return Ok(None);}
        if self.pending.as_ref().is_some_and(|p|!p.finished) {
            return Err("finished float-to-depth began inside an unclosed raster interval".into());
        }
        float_depth_formats(source.format(),destination.format())?;
        if source==destination || d2_shape(source,mip,layer)?!=d2_shape(destination,mip,layer)?
            || source.width()!=destination.width() || source.height()!=destination.height()
            || source.depth_or_array_layers()!=destination.depth_or_array_layers()
            || source.mip_level_count()!=destination.mip_level_count() {
            return Err("finished float-to-depth actual parent/shape mismatch".into());
        }
        let source=self.aux_slice(gpu,source,mip,layer,&[Aspect::Color])?;
        let destination=self.aux_slice(gpu,destination,mip,layer,&[Aspect::Depth])?;
        if source.native_id==destination.native_id {return Err("finished float-to-depth native identity aliases".into());}
        let source_blob=self.aux_snapshot(gpu,&source)?.remove(0);
        let initial_blob=self.aux_snapshot(gpu,&destination)?.remove(0);
        let shader=super::super::FLOAT_TO_DEPTH_PROXY_WGSL;
        let event=self.graph_next_event();
        let operation=json!({"event_index":event,"command_sequence":self.sequence,
            "source":source.references[0],"before":destination.references[0],"source_blob":source_blob,"initial_blob":initial_blob,
            "source_descriptor":descriptor(&source.texture),"destination_descriptor":descriptor(&destination.texture),
            "shader":{"wgsl":shader,"sha256":sha(shader.as_bytes()),"vertex_entry":"vs","fragment_entry":"fs","compilation_options":"wgpu default"},
            "clear_depth_bits":1.0f32.to_bits(),"depth_compare":"always","depth_write_enabled":true,"vertices":3,"instances":1});
        Ok(Some(AuxToken{kind:"float_to_depth",event,sequence:self.sequence,sources:vec![source],destination,operation}))
    }
    /// The native pass has completed encoding. Advance only its exact depth
    /// slice; no invented write to a later mip/layer is exposed to the graph.
    pub(in super::super) fn graph_aux_end_float_depth(&mut self,gpu:&mut Gpu,token:AuxToken)->Result<(),String> {
        if token.kind!="float_to_depth" || !self.aux_active(gpu)? || token.sequence!=self.sequence
            || token.sources.len()!=1 || token.sources[0].references.len()!=1 || token.destination.references.len()!=1 {
            return Err("finished float-to-depth completion token invalid".into());
        }
        let source=&token.sources[0];let destination=&token.destination;
        for slice in [source,destination] {
            let r=slice.references[0];
            if self.finished_native_identity(gpu,&slice.texture)?!=slice.native_id
                || self.finished_version(r.allocation_id,r.subresource.mip,r.subresource.layer,r.subresource.aspect)?!=r {
                return Err("finished float-to-depth parent/version changed during original pass".into());
            }
        }
        let before=destination.references[0];
        self.texture_ledger.as_mut().filter(|l|l.active()).ok_or("finished float-to-depth ledger inactive")?
            .produced(destination.native_id,before.subresource,"original_float_to_depth_proxy",source.references.clone(),vec![],true);
        self.graph_aux_end(gpu,token)
    }
    fn aux_after(&self, gpu: &Gpu, token: &AuxToken) -> Result<Vec<VersionRef>, String> {
        if !self.aux_active(gpu)? || token.sequence != self.sequence || token.event == 0 {
            return Err("finished operation completion changed original command scope".into());
        }
        for source in &token.sources {
            if self.finished_native_identity(gpu, &source.texture)? != source.native_id {
                return Err("finished source native parent replaced".into());
            }
            for r in &source.references {
                if self.finished_version(
                    r.allocation_id,
                    r.subresource.mip,
                    r.subresource.layer,
                    r.subresource.aspect,
                )? != *r
                {
                    return Err("finished source version changed during original operation".into());
                }
            }
        }
        let d = &token.destination;
        if self.finished_native_identity(gpu, &d.texture)? != d.native_id {
            return Err("finished destination native parent replaced".into());
        }
        let after = d
            .references
            .iter()
            .map(|r| self.finished_version(d.native_id, d.mip, d.layer, r.subresource.aspect))
            .collect::<Result<Vec<_>, _>>()?;
        advanced(&d.references, &after)?;
        Ok(after)
    }
    pub(in super::super) fn graph_aux_end(
        &mut self,
        gpu: &mut Gpu,
        mut token: AuxToken,
    ) -> Result<(), String> {
        let after = self.aux_after(gpu, &token)?;
        token.destination.references = after.clone();
        let final_blobs = self.aux_snapshot(gpu, &token.destination)?;
        token.operation["after"] = if token.kind == "copy_region" {
            json!(after)
        } else {
            json!(after[0])
        };
        if token.kind == "copy_region" {
            token.operation["final_blobs"] = json!(final_blobs);
        } else {
            token.operation["final_blob"] = json!(final_blobs[0]);
        }
        token.operation["checkpoint_event_index"] = json!(self.graph_next_event());
        self.aux_emit(token.kind, token.operation)
    }
    /// For actual LoadOp clears folded into ensure_pass. Never ends that pass,
    /// copies a texture, or infers bytes from its numerical clear value.
    pub(in super::super) fn graph_aux_finish_clear_metadata(
        &mut self,
        gpu: &Gpu,
        mut token: AuxToken,
    ) -> Result<(), String> {
        if token.kind != "clear" {
            return Err("metadata-only completion requires an actual clear".into());
        }
        let after = self.aux_after(gpu, &token)?;
        token.operation["after"] = json!(after[0]);
        self.aux_emit("clear", token.operation)
    }
    /// `slots` is the exact original per-instance slot list at the named seam.
    /// No result value, guest address, or replay completion is synthesized.
    pub(in super::super) fn graph_aux_query(
        &mut self,
        gpu: &Gpu,
        life: &Arc<QueryLife>,
        qtype: u32,
        action: &str,
        result_sequence: u32,
        slots: &[u32],
    ) -> Result<(), String> {
        if !self.aux_active(gpu)? {
            return Ok(());
        }
        if !matches!(action, "begin" | "end" | "resolve" | "pass_slot")
            || slots.len() > OCCLUSION_CAPACITY as usize
            || slots.iter().any(|s| *s >= OCCLUSION_CAPACITY)
            || slots.windows(2).any(|w| w[0] >= w[1])
            || (!Gpu::is_occlusion(qtype) && !slots.is_empty())
        {
            return Err("finished query action or original slot list unsupported".into());
        }
        let id = self.graph.aux.query_id(life)?;
        let event = self.graph_next_event();
        self.aux_emit(
            "query",
            json!({"event_index":event,"command_sequence":self.sequence,
            "original_only":true,"replay_side_effects":false,"action":action,
            "native_query_identity":id,"qtype":qtype,"sequence":result_sequence,
            "slots":slots,"live":life.is_live(),"result_completion_recorded":false}),
        )
    }
    /// Must be called at the original begin_occlusion_in_pass capacity check,
    /// including its exhaustion branch (native code otherwise only logs it).
    pub(in super::super) fn graph_aux_query_capacity(&self, gpu: &Gpu) -> Result<(), String> {
        if self.aux_active(gpu)?
            && gpu.occlusion_active.is_some()
            && gpu.pass.is_some()
            && gpu.occlusion_pass_slot.is_none()
            && gpu.occlusion_next >= OCCLUSION_CAPACITY
        {
            return Err(
                "finished original occlusion slot capacity exhausted after capture pass splits"
                    .into(),
            );
        }
        Ok(())
    }
    /// Call only for the actual D2 uncompressed texture readback branch. Original
    /// row bounds and signal/write callbacks remain exclusively owned by Gpu.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn graph_aux_readback(
        &mut self,
        gpu: &mut Gpu,
        texture: &Texture,
        mip: u32,
        layer: u32,
        origin: [u32; 3],
        extent: [u32; 3],
        row_bytes: u32,
        rows: u32,
        destination_row_pitch: u32,
        destination_depth_pitch: u32,
        destination_size: u64,
    ) -> Result<(), String> {
        if !self.aux_active(gpu)? {
            return Ok(());
        }
        if self.pending.as_ref().is_some_and(|p| !p.finished) {
            return Err("finished readback began inside an unclosed raster interval".into());
        }
        let size = d2_shape(texture, mip, layer)?;
        copy_shape(size, [extent[0], extent[1]], origin, [0, 0, 0], extent)?;
        readback_rows(
            texture.format(),
            extent,
            row_bytes,
            rows,
            destination_row_pitch,
            destination_size,
        )?;
        let source = self.aux_slice(gpu, texture, mip, layer, &[Aspect::Color])?;
        let blobs = self.aux_snapshot(gpu, &source)?;
        let event = self.graph_next_event();
        self.aux_emit("readback",json!({"event_index":event,"command_sequence":self.sequence,
            "original_only":true,"replay_side_effects":false,"sources":source.references,
            "source_blobs":blobs,"source_descriptors":[descriptor(texture)],
            "source_origin":origin,"extent":extent,"row_bytes":row_bytes,"rows":rows,
            "destination_row_pitch":destination_row_pitch,"destination_depth_pitch":destination_depth_pitch,
            "destination_size":destination_size,"result_completion_recorded":false}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(version: u64, mip: u32, aspect: Aspect) -> VersionRef {
        VersionRef {
            allocation_id: 7,
            content_version: version,
            subresource: texture_ledger::Subresource {
                mip,
                layer: 0,
                aspect,
            },
        }
    }
    #[test]
    fn copy_full_overwrite_is_exact_destination_coverage() {
        assert_eq!(
            copy_shape([13, 7], [9, 5], [2, 1, 0], [0, 0, 0], [9, 5, 1]),
            Ok(true)
        );
        assert_eq!(
            copy_shape([13, 7], [9, 5], [2, 1, 0], [1, 0, 0], [8, 5, 1]),
            Ok(false)
        );
        for (s, d, e) in [
            ([u32::MAX, 0, 0], [0, 0, 0], [2, 1, 1]),
            ([0, 0, 1], [0, 0, 0], [1, 1, 1]),
            ([0, 0, 0], [0, 0, 0], [1, 1, 2]),
            ([0, 0, 0], [8, 0, 0], [2, 1, 1]),
            ([0, 0, 0], [0, 0, 0], [0, 1, 1]),
        ] {
            assert!(copy_shape([13, 7], [9, 5], s, d, e).is_err());
        }
    }
    #[test]
    fn combined_depth_stencil_requires_both_exact_advanced_versions() {
        let before = [
            reference(2, 0, Aspect::Depth),
            reference(3, 0, Aspect::Stencil),
        ];
        let after = [
            reference(5, 0, Aspect::Depth),
            reference(6, 0, Aspect::Stencil),
        ];
        assert!(advanced(&before, &after).is_ok());
        assert!(advanced(&before, &after[..1]).is_err());
        assert!(advanced(&before, &[after[0], before[1]]).is_err());
        assert!(advanced(&before, &[after[0], reference(6, 1, Aspect::Stencil)]).is_err());
        assert!(advanced(&before, &[after[0], reference(5, 0, Aspect::Stencil)]).is_err());
        assert_eq!(
            aspects(TextureFormat::Depth32FloatStencil8).unwrap(),
            vec![Aspect::Depth, Aspect::Stencil]
        );
    }
    #[test]
    fn clear_retains_native_bits_and_exact_view_compatibility() {
        let fractional = ClearValue::Color {
            bits: [0.1f64.to_bits(), 0, 0, 1f64.to_bits()],
        };
        assert_eq!(
            clear_aspect(
                TextureFormat::Bgra8Unorm,
                TextureFormat::Bgra8UnormSrgb,
                &fractional
            ),
            Ok(Aspect::Color)
        );
        assert!(
            clear_aspect(
                TextureFormat::Bgra8Unorm,
                TextureFormat::Rgba8Unorm,
                &fractional
            )
            .is_err()
        );
        assert!(
            clear_aspect(
                TextureFormat::Depth32Float,
                TextureFormat::Depth32Float,
                &fractional
            )
            .is_err()
        );
        assert!(
            clear_aspect(
                TextureFormat::Depth32Float,
                TextureFormat::Depth32Float,
                &ClearValue::Depth {
                    bits: f32::NAN.to_bits()
                }
            )
            .is_err()
        );
        assert!(
            clear_aspect(
                TextureFormat::Depth32FloatStencil8,
                TextureFormat::Depth32FloatStencil8,
                &ClearValue::Stencil { value: 256 }
            )
            .is_err()
        );
    }
    #[test]
    fn float_to_depth_contract_is_exact_and_has_no_stencil_or_integer_alias() {
        for source in [TextureFormat::R32Float,TextureFormat::Rg32Float,TextureFormat::Rgba32Float,
            TextureFormat::R16Float,TextureFormat::Rg16Float,TextureFormat::Rgba16Float,
            TextureFormat::Rgba8Unorm,TextureFormat::Rgba8UnormSrgb,TextureFormat::Bgra8Unorm,TextureFormat::Bgra8UnormSrgb] {
            assert!(float_depth_formats(source,TextureFormat::Depth32Float).is_ok());
            assert!(float_depth_formats(source,TextureFormat::Depth32FloatStencil8).is_err());
        }
        for source in [TextureFormat::Depth32Float,TextureFormat::R32Uint,TextureFormat::Rgba8Sint,TextureFormat::Bc1RgbaUnorm] {
            assert!(float_depth_formats(source,TextureFormat::Depth32Float).is_err());
        }
        assert_eq!(sha(super::super::super::FLOAT_TO_DEPTH_PROXY_WGSL.as_bytes()),
            "9bb14289ac19dadd2fad243de07016ab6ee20d02ca7a6c888a02197cca0759aa");
    }
    #[test]
    fn float_to_depth_versions_advance_in_original_selected_pass_order() {
        use texture_ledger::{Allocation,Ledger,Subresource};
        let mut ledger=Ledger::new();
        for (id,format,aspect) in [(1,"R32Float",Aspect::Color),(2,"Depth32Float",Aspect::Depth)] {
            ledger.register(Allocation{allocation_id:id,guest_id:1,origin:"test".into(),width:34,height:34,layers:2,mips:2,
                dimension:"d2".into(),format:format.into(),aspects:vec![aspect]});
        }
        let original=ledger.current(2,Subresource{mip:1,layer:1,aspect:Aspect::Depth}).unwrap();
        let mut previous=0;
        for mip in 0..2 {for layer in 0..2 {
            let src=ledger.current(1,Subresource{mip,layer,aspect:Aspect::Color}).unwrap();
            let dst=Subresource{mip,layer,aspect:Aspect::Depth};
            if (mip,layer)!=(1,1) {assert_eq!(ledger.current(2,original.subresource),Some(original));}
            ledger.produced(2,dst,"original_float_to_depth_proxy",vec![src],vec![],true);
            let after=ledger.current(2,dst).unwrap();assert!(after.content_version>previous);previous=after.content_version;
            assert_eq!(ledger.current(1,src.subresource),Some(src));
        }}
    }
    #[test]
    fn query_identity_follows_native_lifetime_without_retaining_guest_completion() {
        let mut state = AuxState::default();
        let life = QueryLife::new(8);
        assert_eq!(state.query_id(&life).unwrap(), 1);
        assert_eq!(state.query_id(&life).unwrap(), 1);
        assert_eq!(Arc::strong_count(&life), 1);
        life.retire();
        assert!(!life.is_live());
        drop(life);
        assert!(state.queries[0].upgrade().is_none());
        let replacement = QueryLife::new(8);
        assert_eq!(state.query_id(&replacement).unwrap(), 2);
        state.queries.resize_with(MAX_QUERY_IDENTITIES, Weak::new);
        assert!(state.query_id(&QueryLife::new(8)).is_err());
        assert_eq!(state.query_id(&replacement).unwrap(), 2);
    }
    #[test]
    fn original_readback_rows_preserve_padding_and_reject_truncation() {
        assert!(readback_rows(TextureFormat::Rgba32Float, [1, 1, 1], 16, 1, 16, 16).is_ok());
        assert!(readback_rows(TextureFormat::Bgra8Unorm, [13, 7, 1], 52, 7, 256, 1588).is_ok());
        assert!(readback_rows(TextureFormat::Bgra8Unorm, [13, 7, 1], 52, 7, 256, 1587).is_err());
        assert!(readback_rows(TextureFormat::Bgra8Unorm, [13, 7, 1], 52, 7, 51, 4096).is_err());
        assert!(
            readback_rows(
                TextureFormat::Bgra8Unorm,
                [u32::MAX, 1, 1],
                u32::MAX,
                1,
                u32::MAX,
                u64::MAX
            )
            .is_err()
        );
        assert!(readback_rows(TextureFormat::Depth32Float, [1, 1, 1], 4, 1, 4, 4).is_err());
        assert!(readback_rows(TextureFormat::Bc1RgbaUnorm, [4, 4, 1], 8, 1, 8, 8).is_err());
        assert!(readback_rows(TextureFormat::Rgba32Float, [1, 0, 1], 16, 0, 16, 16).is_err());
    }
}

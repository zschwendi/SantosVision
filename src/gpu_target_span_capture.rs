//! Complete fixed-target intervals for original-density native replay.
//! No successful prefix is exported when any intervening operation is missing.
use super::*;
use super::super::capture_bindings::{Resource, CapturedGroup};
use texture_ledger::{Aspect, Subresource, VersionRef};

const MAX_DRAWS:usize=4096;
const MAX_COMMANDS:usize=65536;

struct TargetContext {view:Value,domain:Value,targets:Vec<(Texture,TextureAspect,String)>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum ContextBoundary {Continue,Split,Reject}
fn context_boundary(kind:CaptureKind,same_present:bool,domain_same:bool,view_same:bool,parents_same:bool)->ContextBoundary {
    if !domain_same || !parents_same || (kind.is_finished() && !same_present) {return ContextBoundary::Reject;}
    if view_same {return ContextBoundary::Continue;}
    if kind.is_finished() && same_present {ContextBoundary::Split}else{ContextBoundary::Reject}
}
fn retained_targets_equal<T:PartialEq>(old:&[T],new:&[T])->bool {
    old.len()==new.len() && old.iter().zip(new).all(|(a,b)|a==b)
}

#[derive(Default)]
pub(super) struct SpanState {
    boundary:Option<u64>,
    started:bool,
    buffers:HashMap<Buffer,u64>,
    revisions:HashMap<Buffer,u64>,
    buffer_blobs:HashMap<(u64,u64,u64,u64),String>,
    pub(super) texture_blobs:BTreeMap<(u64,u32,u32,Aspect,u64),String>,
    material_versions:BTreeMap<(u64,u32,u32,Aspect),u64>,
    dummy_textures:HashMap<Texture,u64>,
    next_buffer:u64,
    next_snapshot:u64,
    before:Vec<VersionRef>,
    context_transition_recorded:bool,
    context_mismatch_recorded:bool,
}
impl SpanState {
    pub(super) fn clear_interval(&mut self) {*self=Self::default();}
    pub(super) fn resume_at(&mut self,sequence:u64) {self.started=false;self.before.clear();self.material_versions.clear();self.boundary=Some(sequence);}
}

fn advance_captured_revision<K:Eq+std::hash::Hash+Clone>(
    buffers:&HashMap<K,u64>,revisions:&mut HashMap<K,u64>,buffer:&K,
) {
    // Earlier writes need no history before the first immutable snapshot.
    // Keep retained revision keys a subset of the bounded captured identities.
    if !buffers.contains_key(buffer) {return;}
    let value=revisions.entry(buffer.clone()).or_default();
    *value=value.saturating_add(1);
}

pub(super) fn subresource_plan(name:String,t:&Texture,mip:u32,layer:u32,aspect:TextureAspect)->Result<Plan,String> {
    if t.sample_count()!=1 || mip>=t.mip_level_count()
        || (t.dimension()!=TextureDimension::D2 && layer!=0)
        || layer>=t.depth_or_array_layers() || !t.usage().contains(TextureUsages::COPY_SRC) {
        return Err("span sampled subresource cannot be copied".into());
    }
    let (w,h)=((t.width()>>mip).max(1),(t.height()>>mip).max(1));
    let (bw,bh)=t.format().block_dimensions();
    let block=t.format().block_copy_size(Some(aspect)).ok_or("span aspect copy size unavailable")?;
    let row=w.div_ceil(bw).checked_mul(block).ok_or("span row overflow")?;
    let image_rows=h.div_ceil(bh);
    let depth=if t.dimension()==TextureDimension::D3 {(t.depth_or_array_layers()>>mip).max(1)}else{1};
    let rows=image_rows.checked_mul(depth).ok_or("span volume rows overflow")?;
    let pitch=row.checked_add(255).ok_or("span pitch overflow")?&!255;
    let bytes=(pitch as u64).checked_mul(rows as u64).filter(|&n|n>0&&n<=MAX_BYTES).ok_or("span texture bound")?;
    Ok(Plan{name,source:Source::Texture{texture:t.clone(),mip,layer,depth,aspect,
        width:w.div_ceil(bw)*bw,height:h.div_ceil(bh)*bh},row,rows,image_rows,pitch,bytes})
}

pub(super) fn world(view:crate::xr_view::WorldView)->Result<Value,String> {
    if !view.headset_applied || view.view_count!=2 || view.eye_index>=2 || !view.depth_reverse_z {
        return Err("span requires ordinary same-eye reverse-Z world".into());
    }
    raw_world(view)
}
pub(super) fn raw_world(view:crate::xr_view::WorldView)->Result<Value,String> {
    Ok(json!({"provenance_epoch":view.provenance_epoch,"camera_sequence":view.camera_sequence,
        "request_frame":view.request_frame,"head_pose_frame":view.head_pose_frame,"eye_index":view.eye_index,
        "view_count":view.view_count,"presentation_time_seconds":view.presentation_time_seconds,
        "game_from_eye":view.game_from_eye,"game_from_stage":view.game_from_stage,
        "projection_from_eye":view.projection_from_eye,"headset_applied":view.headset_applied,
        "depth_reverse_z":view.depth_reverse_z,
        "optics":{"scope_sequence":view.optics.scope_sequence,"magnification":view.optics.magnification,
            "reference_fov_degrees":view.optics.reference_fov_degrees,"actual_fov_degrees":view.optics.actual_fov_degrees},
        "request_record":crate::xr_view::consumed_request_receipt(&view).ok_or("span consumed request missing")?}))
}

impl Capture {
    pub(super) fn capture_blob_name(&self,name:String)->String {
        if self.kind.is_graph() {format!("stage{}-{name}",self.graph.stages.len())} else {name}
    }
    pub(in super::super) fn before_command(&mut self,gpu:&mut Gpu,op:u32,buffer:Option<&Buffer>) {
        if !self.captures_target_span() {return;}
        if self.kind.is_graph() && self.graph.present.is_some_and(|n|Some(n)!=gpu.presents.checked_add(1)) {
            self.reject("connected graph crossed native present");
        }
        if self.kind.is_finished() && matches!(op,5|6|33..=43) {
            if let Err(e)=self.finished_before_aux(gpu){self.reject(&e);}
        }
        if op==26 {
            if self.pending.as_ref().is_some_and(|p|!p.finished) {self.close_target_span(gpu);}
            if self.pending.is_none() {
                if self.kind.is_graph() && self.graph.anchor.is_some() {
                    self.span.started=false;self.span.before.clear();self.span.material_versions.clear();
                } else {self.span.clear_interval();}
                self.span.boundary=Some(self.sequence.saturating_add(1));
            }
        }
        if matches!(op,12|47) {
            if let Some(buffer)=buffer {
                // Buf.version covers shadow CBs only. This independent revision
                // invalidates ordinary VB/IB/readonly-storage snapshots too.
                advance_captured_revision(&self.span.buffers,&mut self.span.revisions,buffer);
            }
        }
    }
    pub(super) fn enqueue_span(&mut self,gpu:&mut Gpu,plans:Vec<Plan>)->Vec<Readback> {
        enqueue(gpu,plans).into_iter().map(|mut rb|{
            self.live_bytes.fetch_add(rb.plan.bytes,Ordering::SeqCst);
            if self.kind.is_finished() {self.finished.peak_live_staging=self.finished.peak_live_staging.max(self.live_bytes.load(Ordering::SeqCst));}
            let reservation=Arc::new(SnapshotReservation{bytes:rb.plan.bytes,live:self.live_bytes.clone()});
            self.epoch_reservations.push(reservation.clone());rb._reservation=Some(reservation);rb
        }).collect()
    }
    pub(super) fn span_command(&mut self,op:u32) {
        if self.pending.as_ref().is_some_and(|p|p.finished) {return;}
        if op==26 {return;}
        if self.pending.is_some() {
            if !matches!(op,0|1|2|9|10|12|18..=25|27..=32|47) {
                self.reject("complete span contains unsupported command");return;
            }
            let p=self.pending.as_mut().unwrap();
            if p.intervening_commands.len()>=MAX_COMMANDS {self.reject("complete span command limit");return;}
            p.intervening_commands.push(json!({"sequence":self.sequence,"opcode":op}));
        }
    }

    fn span_buffer(&mut self,b:&Buffer,offset:u64,size:u64,force_snapshot:bool,name:String,plans:&mut Vec<Plan>)->Result<Value,String> {
        let id=match self.span.buffers.get(b) {Some(&id)=>id,None=>{
            if self.span.buffers.len()>=8192 {return Err("span native buffer identity limit".into());}
            self.span.next_buffer+=1;let id=self.span.next_buffer;self.span.buffers.insert(b.clone(),id);id
        }};
        let version=if force_snapshot {
            self.span.next_snapshot=self.span.next_snapshot.checked_add(1).ok_or("span snapshot overflow")?;
            (1u64<<63)|self.span.next_snapshot
        }else{self.span.revisions.get(b).copied().unwrap_or(0).checked_add(1).ok_or("span buffer version overflow")?};
        let key=(id,version,offset,size);
        let blob=if let Some(blob)=self.span.buffer_blobs.get(&key) {blob.clone()}else{
            plans.push(buffer_plan(name.clone(),b,offset,size)?);
            self.span.buffer_blobs.insert(key,name.clone());name
        };
        Ok(json!({"blob":blob,"size":size,"source_offset":offset,"native_allocation_id":id,
            "content_version":version,"identity_basis":"retained actual wgpu Buffer handle equality",
            "version_namespace":"capture buffer upload revision or immutable selected uniform snapshot"}))
    }

    fn span_texture_identity(&mut self,gpu:&Gpu,t:&Texture,dummy:bool)->Result<u64,String> {
        if dummy {
            if let Some(&id)=self.span.dummy_textures.get(t) {return Ok(id);}
            if self.span.dummy_textures.len()>=128 {return Err("span dummy identity bound".into());}
            let id=(1u64<<63)|(self.span.dummy_textures.len()as u64+1);
            self.span.dummy_textures.insert(t.clone(),id);return Ok(id);
        }
        for object in &gpu.objs {
            if let Some(Obj::Tex(texture))=object {
                if &texture.tex==t {return texture.trace_native_identity.ok_or("span texture native identity absent".into());}
                for proxy in [&texture.proxy_depth,&texture.proxy_stencil,&texture.proxy_fdepth].into_iter().flatten() {
                    if &proxy.tex==t {return proxy.trace_native_identity.ok_or("span proxy identity absent".into());}
                }
            }
        }
        Err("span sampled parent is not a retained native allocation".into())
    }

    pub(super) fn span_group(&mut self,gpu:&Gpu,group:usize,captured:&CapturedGroup,ordinal:usize,plans:&mut Vec<Plan>)->Result<Vec<Value>,String> {
        if let Some(reason)=&captured.unsupported {return Err(format!("span resolved bindings: {reason}"));}
        let mut result=Vec::new();
        for entry in &captured.entries {
            let mut layout=entry.layout;
            let binding=layout.binding;
            let mut out=json!({"group":group,"binding":binding});
            match &entry.resource {
                Resource::Buffer{buffer,offset,size,original_dynamic_offset,..}=>{
                    let kind=match layout.ty {BindingType::Buffer{ty:BufferBindingType::Uniform,..}=>"uniform",
                        BindingType::Buffer{ty:BufferBindingType::Storage{read_only:true},..}=>"readonly_storage",
                        _=>return Err("span writable or unsupported buffer binding".into())};
                    let metadata=self.span_buffer(buffer,*offset,*size,original_dynamic_offset.is_some(),
                        self.capture_blob_name(format!("draw{ordinal}-g{group}-b{binding}.bin")),plans)?;
                    for (k,v) in metadata.as_object().unwrap(){out[k]=v.clone();}
                    out["kind"]=kind.into();out["offset"]=0.into();
                    out["original_dynamic_offset"]=json!(original_dynamic_offset);
                    if let BindingType::Buffer{has_dynamic_offset,..}=&mut layout.ty {*has_dynamic_offset=false;}
                }
                Resource::Sampler{descriptor,..}=>{out["kind"]="sampler".into();out["descriptor"]=json!(descriptor);}
                Resource::Texture{view,descriptor,native_dummy}=>{
                    let storage=matches!(layout.ty,BindingType::StorageTexture{..});
                    if storage && !matches!(self.kind,CaptureKind::Compute|CaptureKind::Graph|CaptureKind::Finished|CaptureKind::ExecutedFinished) {return Err("target-span writable texture binding is unsupported".into());}
                    let texture=view.texture();let id=self.span_texture_identity(gpu,texture,*native_dummy)?;
                    if self.pending.as_ref().is_some_and(|p|p.targets.iter().any(|t|&t.0==texture)) {
                        return Err("span samples current attachment native parent".into());
                    }
                    let mip_total=texture.mip_level_count();
                    let layer_total=if texture.dimension()==TextureDimension::D2 {texture.depth_or_array_layers()}else{1};
                    let mips=mip_total.checked_sub(descriptor.base_mip_level).ok_or("span mip base out of range")?;
                    let layers=layer_total.checked_sub(descriptor.base_array_layer).ok_or("span layer base out of range")?;
                    let mip_end=descriptor.base_mip_level.checked_add(descriptor.mip_level_count.unwrap_or(mips)).ok_or("span mip overflow")?;
                    let layer_end=descriptor.base_array_layer.checked_add(descriptor.array_layer_count.unwrap_or(layers)).ok_or("span layer overflow")?;
                    if mip_end>texture.mip_level_count() || layer_end>texture.depth_or_array_layers() {return Err("span sampled view range invalid".into());}
                    let aspect=if is_depth(texture.format()) {match descriptor.aspect {
                        TextureAspect::StencilOnly=>Aspect::Stencil,_=>Aspect::Depth
                    }}else{Aspect::Color};
                    let gpu_aspect=match aspect{Aspect::Color=>TextureAspect::All,Aspect::Depth=>TextureAspect::DepthOnly,Aspect::Stencil=>TextureAspect::StencilOnly};
                    let mut subs=Vec::new();
                    for mip in descriptor.base_mip_level..mip_end {for layer in descriptor.base_array_layer..layer_end {
                        let s=Subresource{mip,layer,aspect};
                        let version=if *native_dummy {VersionRef{allocation_id:id,
                            content_version:(1u64<<63)|((id&!(1u64<<63))<<32)|((mip as u64)<<16)|(layer as u64)+1,subresource:s}}
                            else{self.texture_ledger.as_ref().and_then(|l|l.current(id,s)).ok_or("span sampled current version absent")?};
                        let stable=(id,mip,layer,aspect);
                        if self.span.material_versions.get(&stable).is_some_and(|v|*v!=version.content_version) {
                            return Err("span sampled input changed after its immutable snapshot".into());
                        }
                        self.span.material_versions.insert(stable,version.content_version);
                        let key=(id,mip,layer,aspect,version.content_version);
                        let name=format!("texture{id}-v{}-m{mip}-l{layer}-{aspect:?}.bin",version.content_version);
                        let plan=subresource_plan(name.clone(),texture,mip,layer,gpu_aspect)?;
                        let row=plan.row;
                        let blob=if let Some(blob)=self.span.texture_blobs.get(&key){blob.clone()}else{
                            plans.push(plan);self.span.texture_blobs.insert(key,name.clone());name
                        };
                        subs.push(json!({"mip_level":mip,"layer":layer,"aspect":aspect,"blob":blob,
                            "packed_row_bytes":row,"width":(texture.width()>>mip).max(1),"height":(texture.height()>>mip).max(1),
                            "depth":if texture.dimension()==TextureDimension::D3 {(texture.depth_or_array_layers()>>mip).max(1)}else{1},"version":version}));
                    }}
                    out["kind"]=if storage {"storage_texture"}else{"texture"}.into();out["native_allocation_id"]=id.into();out["view"]=json!(descriptor);
                    out["descriptor"]=json!({"width":texture.width(),"height":texture.height(),"depth_or_array_layers":texture.depth_or_array_layers(),
                        "mip_level_count":texture.mip_level_count(),"sample_count":texture.sample_count(),"dimension":texture.dimension(),"format":texture.format()});
                    out["subresources"]=json!(subs);out["immutable_native_dummy"]=json!(native_dummy);
                }
            }
            out["layout"]=json!(layout);result.push(out);
        }
        Ok(result)
    }

    fn target_versions(&self,domain:&Value)->Result<Vec<VersionRef>,String> {
        let ledger=self.texture_ledger.as_ref().ok_or("span ledger missing")?;
        if let Some(reason)=ledger.disabled_reason() {return Err(format!("span ledger inactive: {reason}"));}
        let mut refs=Vec::new();
        for a in domain["attachments"].as_array().ok_or("span attachments absent")? {
            let id=a["native_allocation_id"].as_u64().ok_or("span attachment identity missing")?;
            let aspects=if a["role"]=="depth_stencil" {
                if a["format"]==json!(TextureFormat::Depth32FloatStencil8) {vec![Aspect::Depth,Aspect::Stencil]} else {vec![Aspect::Depth]}
            }else{vec![Aspect::Color]};
            for aspect in aspects {refs.push(ledger.current(id,Subresource{mip:a["mip"].as_u64().ok_or("span target mip absent")? as u32,layer:a["slice"].as_u64().ok_or("span target layer absent")? as u32,aspect}).ok_or("span target current version missing")?);}
        }
        Ok(refs)
    }

    fn target_context(&mut self,gpu:&Gpu,d:&Resolved<'_>)->Result<TargetContext,String> {
        if !gpu.bind_group_prepare.enabled || gpu.errors.load(Ordering::SeqCst)!=0 || gpu.hud_draw.is_some()
            || (!self.kind.is_finished() && (gpu.occlusion_active.is_some() || gpu.occlusion_pass_slot.is_some() || !gpu.pending_resolve.is_empty())) {
            return Err("span requires valid ordinary bindings, no errors, HUD or queries".into());
        }
        if d.kind>=4 || d.count==0 || d.instances==0 {return Err("span indirect or empty draw unsupported".into());}
        let pi=gpu.pass_info.as_ref().ok_or("span native pass absent")?;
        let continuation=self.kind.is_graph() && self.graph.anchor.is_some();
        if pi.width==0 || pi.height==0 || pi.width>4096 || pi.height>4096 || pi.samples!=1
            || pi.rt_formats.len()>8 || gpu.s.ds.flags&!3!=0 {
            return Err("span target extent/sample/flags unsupported".into());
        }
        if !continuation && (pi.rt_formats!=vec![Some(TextureFormat::Bgra8Unorm);4]
            || pi.ds_format!=Some(TextureFormat::Depth32FloatStencil8) || pi.ro_depth || pi.ro_stencil || gpu.s.ds.flags!=0) {
            return Err("span requires initial full four-MRT D32S8 domain".into());
        }
        let view=if self.kind.is_finished() && continuation {
            gpu.world_view.map(raw_world).transpose()?.unwrap_or(Value::Null)
        }else{world(gpu.world_view.ok_or("first or later span draw has no exact world identity")?)?};
        if self.kind.is_graph() {self.graph_start_or_check(gpu,&view)?;}
        let mut targets=Vec::new();let mut domain=Vec::new();
        for (slot,format) in pi.rt_formats.iter().enumerate() {
            let Some(format)=format else {continue;};
            let r=&gpu.s.rt[slot];let t=gpu.tex(r.tex).ok_or("span color target missing")?;
            if (!self.kind.is_finished() && (r.mip!=0 || r.slice!=0 || t.desc.d!=1)) || r.mip>=t.desc.mips || r.slice>=t.desc.d || t.desc.samples!=1 || t.format!=*format
                || t.tex.dimension()!=TextureDimension::D2 || t.format.is_depth_stencil_format() || t.format.block_dimensions()!=(1,1)
                || (t.desc.w>>r.mip).max(1)!=pi.width || (t.desc.h>>r.mip).max(1)!=pi.height || targets.iter().any(|(p,_,_):&(Texture,TextureAspect,String)|p==&t.tex) {
                return Err("span target shape or alias unsupported".into());
            }
            targets.push((t.tex.clone(),TextureAspect::All,format!("color{slot}")));
            domain.push(json!({"role":format!("color{slot}"),"guest_id":r.tex,"format":t.format,"mip":r.mip,"slice":r.slice,
                "native_allocation_id":t.trace_native_identity.ok_or("span target native identity absent")?}));
        }
        if let Some(format)=pi.ds_format {
            let ds=&gpu.s.ds;let t=gpu.tex(ds.tex).ok_or("span depth target absent")?;
            if (!self.kind.is_finished() && (ds.mip!=0 || ds.slice!=0 || t.desc.d!=1)) || ds.mip>=t.desc.mips || ds.slice>=t.desc.d || (t.desc.w>>ds.mip).max(1)!=pi.width || (t.desc.h>>ds.mip).max(1)!=pi.height || t.format!=format
                || !matches!(format,TextureFormat::Depth32Float|TextureFormat::Depth32FloatStencil8)
                || pi.ro_depth!=(ds.flags&1!=0) || pi.ro_stencil!=(ds.flags&2!=0) {return Err("span depth domain unsupported".into());}
            targets.push((t.tex.clone(),TextureAspect::DepthOnly,"depth".into()));
            if format==TextureFormat::Depth32FloatStencil8 {targets.push((t.tex.clone(),TextureAspect::StencilOnly,"stencil".into()));}
            domain.push(json!({"role":"depth_stencil","guest_id":ds.tex,"format":t.format,"mip":ds.mip,"slice":ds.slice,
                "native_allocation_id":t.trace_native_identity.ok_or("span depth native identity absent")?}));
        }
        if targets.is_empty() {return Err("span has no bound target".into());}
        let domain=json!({"width":pi.width,"height":pi.height,"attachments":domain,"depth_flags":gpu.s.ds.flags,"retained_native_parents_compared":true});
        Ok(TargetContext{view,domain,targets})
    }
    /// Preserve raw context independently of pixel/target identity. The first
    /// diagnostic is outside the capture payload namespace and never replaced.
    fn record_context_boundary(&mut self,gpu:&Gpu,context:&TargetContext,boundary:ContextBoundary) {
        let recorded=if boundary==ContextBoundary::Split {&mut self.span.context_transition_recorded}else{&mut self.span.context_mismatch_recorded};
        if *recorded {return;}*recorded=true;
        let Some(p)=&self.pending else{return;};
        let comparisons=p.targets.iter().zip(&context.targets).enumerate().map(|(index,(old,new))|json!({
            "index":index,"native_parent_equal":old.0==new.0,"aspect_equal":old.1==new.1,"role_equal":old.2==new.2,
            "old_role":old.2,"new_role":new.2,"old_aspect":old.1,"new_aspect":new.1})).collect::<Vec<_>>();
        let value=json!({"schema":1,"capture_id":self.graph.capture_id,"command_sequence":self.sequence,
            "action":if boundary==ContextBoundary::Split {"split_context_only"}else{"reject_mismatch"},
            "capture_schema":self.kind.schema(),"native_present":gpu.presents.checked_add(1),"anchor_present":self.graph.present,
            "same_present":self.graph.present==gpu.presents.checked_add(1),"old_interval_start":p.alias.as_ref().map(|a|&a["interval_start_command"]),
            "old_world_view":p.view,"new_world_view":context.view,"old_target_domain":p.domain,"new_target_domain":context.domain,
            "domain_equal":p.domain==context.domain,"old_retained_target_count":p.targets.len(),"new_retained_target_count":context.targets.len(),
            "retained_targets_equal":retained_targets_equal(&p.targets,&context.targets),"retained_target_comparisons":comparisons,
            "normal_rendering_changed":false,"world_identity_reassigned":false});
        if let Ok(bytes)=serde_json::to_vec_pretty(&value) {if bytes.len()<=65536 {if let Some(parent)=self.directory.parent(){
            let name=if boundary==ContextBoundary::Split {"first-span-context-transition.json"}else{"first-span-context-mismatch.json"};
            if let Err(error)=publish_new(&parent.join(name),&bytes){log(&format!("span context diagnostic: {error}"));}
        }}}
    }
    fn prepare_target_span(&mut self,gpu:&Gpu,d:&Resolved<'_>,ordinal:usize,context:TargetContext)->Result<Prepared,String> {
        let TargetContext{view,domain,targets}=context;
        let (key,_)=gpu.pipelines.iter().find(|(_,e)|matches!(e,PipeEntry::Done(Some(p)) if p.uid==d.pipeline.uid)).ok_or("span pipeline absent")?;
        let pipeline=self.pipelines.get(key).ok_or("span final pipeline metadata missing")?.clone();
        let (_,failure)=shader_sources::selected_pipeline_hashes(&[json!({"pipeline":pipeline})]);
        if let Some(failure)=failure {return Err(failure);}
        self.span.before=self.target_versions(&domain)?;
        let mut plans=Vec::new();let mut vertices=Vec::new();
        for (slot,buffer,offset,guest_id) in d.vertices {
            let available=buffer.size().checked_sub(*offset).ok_or("span vertex offset out of range")?;
            let vertex_layout=&pipeline["vertex_layouts"][*slot as usize];
            let stride=vertex_layout["array_stride"].as_u64().ok_or("span vertex stride absent")?;
            let step:VertexStepMode=serde_json::from_value(vertex_layout["step_mode"].clone()).map_err(|e|e.to_string())?;
            let attributes:Vec<VertexAttribute>=serde_json::from_value(vertex_layout["attributes"].clone()).map_err(|e|e.to_string())?;
            let last=attributes.iter().map(|a|a.offset.checked_add(a.format.size()).ok_or("span attribute overflow"))
                .collect::<Result<Vec<_>,_>>()?.into_iter().max().ok_or("span attributes absent")?;
            let exact=if step==VertexStepMode::Instance {Some((d.emitted_first_instance,d.instances))}
                else if d.kind&1==0 {Some((d.emitted_first_vertex,d.count))}else{None};
            let size=if let Some((first,count))=exact {
                let end=(first as u64).checked_add(count as u64).and_then(|n|n.checked_sub(1))
                    .and_then(|n|n.checked_mul(stride)).and_then(|n|n.checked_add(last)).ok_or("span vertex extent overflow")?;
                let aligned=end.checked_add(3).ok_or("span vertex padding overflow")?&!3;
                if aligned>available {return Err("span vertex fetched range exceeds native binding".into());}aligned
            }else{available};
            let mut v=self.span_buffer(buffer,*offset,size,false,self.capture_blob_name(format!("draw{ordinal}-vb{slot}.bin")),&mut plans)?;
            v["slot"]=(*slot).into();v["offset"]=0.into();v["guest_id"]=(*guest_id).into();vertices.push(v);
        }
        let index=if d.kind&1!=0 {
            let (buffer,format,offset,id)=d.index.ok_or("span indexed binding absent")?;
            let mut v=self.span_buffer(buffer,0,buffer.size(),false,self.capture_blob_name(format!("draw{ordinal}-ib.bin")),&mut plans)?;
            v["format"]=json!(format);v["offset"]=(*offset).into();v["guest_id"]=(*id).into();v
        }else{Value::Null};
        let mut bindings=Vec::new();
        for (group,resources) in d.groups.iter().enumerate() {
            if let Some(resources)=resources {
                let captured=resources.capture.as_ref().ok_or("span actual bind group metadata unavailable")?;
                bindings.extend(self.span_group(gpu,group,captured,ordinal,&mut plans)?);
            }
        }
        // At the first draw pending is absent; check current targets as well.
        for binding in &bindings {if binding["kind"]=="texture" {
            let id=binding["native_allocation_id"].as_u64();
            if domain["attachments"].as_array().unwrap().iter().any(|a|a["native_allocation_id"].as_u64()==id) {return Err("span sampled/output parent overlap".into());}
        }}
        let draw=json!({"draw":{"kind":d.kind&1,"original_kind":d.kind,"count":d.count,"instances":d.instances,
            "first_vertex":if d.kind&1==0 {d.emitted_first_vertex}else{0},
            "start_index":if d.kind&1!=0 {d.start}else{0},"base_vertex":if d.kind&1!=0 {d.emitted_base_vertex}else{0},
            "start_instance":d.emitted_first_instance},"command_sequence":self.sequence,"pipeline":pipeline,
            "vertex_buffers":vertices,"index_buffer":index,"bindings":bindings,"viewport":d.emitted_viewport,
            "scissor":d.scissor,"blend_constant":d.blend,"stencil_reference":d.stencil,"world_view":view,"target_domain":domain,
            "writes":[]});
        Ok(Prepared{draw,view,domain,targets,plans,alias:None})
    }

    pub(super) fn before_target_span(&mut self,gpu:&mut Gpu,d:&Resolved<'_>) {
        if self.done || self.finished.postlude || self.pending.as_ref().is_some_and(|p|p.finished)
            || (self.attempts>=128 && self.pending.is_none() && self.graph.stages.is_empty()) {return;}
        if self.kind.is_finished() && self.graph.anchor.is_none() && gpu.presents<500 {return;}
        if self.pending.is_none() && (self.span.boundary.is_none() || self.span.started) {return;}
        if self.kind.is_finished() && self.graph.event>=8190 {self.reject("finished graph event bound");return;}
        let context=match self.target_context(gpu,d){Ok(v)=>v,Err(e)=>{self.reject(&e);return;}};
        if let Some(p)=&self.pending {
            let boundary=context_boundary(self.kind,self.graph.present==gpu.presents.checked_add(1),
                p.domain==context.domain,p.view==context.view,retained_targets_equal(&p.targets,&context.targets));
            if boundary!=ContextBoundary::Continue {self.record_context_boundary(gpu,&context,boundary);}
            match boundary {
                ContextBoundary::Reject=>{self.reject("span context mismatch: native domain/retained target changed or context split not admitted");return;},
                ContextBoundary::Split=>{
                    self.close_target_span(gpu);
                    if self.done || self.pending.is_some() {return;}
                    self.span.resume_at(self.sequence);
                },
                ContextBoundary::Continue=>{},
            }
        }
        let ordinal=self.pending.as_ref().map_or(0,|p|p.draws.len());
        self.span.started=true;
        if ordinal>=MAX_DRAWS {self.reject("complete span draw limit");return;}
        let mut prepared=match self.prepare_target_span(gpu,d,ordinal,context){Ok(v)=>v,Err(e)=>{self.reject(&e);return;}};
        if self.kind.is_graph() {prepared.draw["event_index"]=self.graph_next_event().into();}
        let mut initial=Vec::new();let mut attachments=Vec::new();let mut final_bytes=0;
        if ordinal==0 {
            for ((texture,aspect,role),version) in prepared.targets.iter().zip(&self.span.before) {
                let first=match subresource_plan(self.capture_blob_name(format!("initial-{role}.bin")),texture,version.subresource.mip,version.subresource.layer,*aspect){Ok(p)=>p,Err(e)=>{self.reject(&e);return;}};
                let key=(version.allocation_id,version.subresource.mip,version.subresource.layer,version.subresource.aspect,version.content_version);
                let existing=if self.kind.is_graph() {self.span.texture_blobs.get(&key).cloned()}else{None};
                let readonly=match version.subresource.aspect {
                    Aspect::Depth=>gpu.s.ds.flags&1!=0,Aspect::Stencil=>gpu.s.ds.flags&2!=0,Aspect::Color=>false
                };
                if !self.kind.is_graph() || !readonly {
                    final_bytes=match add(final_bytes,first.bytes){Ok(n)=>n,Err(e)=>{self.reject(&e);return;}};
                }
                let initial_blob=existing.unwrap_or_else(||first.name.clone());
                attachments.push(json!({"role":role,"format":texture.format(),"width":(texture.width()>>version.subresource.mip).max(1),"height":(texture.height()>>version.subresource.mip).max(1),
                    "mip":version.subresource.mip,"slice":version.subresource.layer,"initial_blob":initial_blob,"final_blob":self.capture_blob_name(format!("final-{role}.bin")),
                    "descriptor":{"width":texture.width(),"height":texture.height(),"depth_or_array_layers":texture.depth_or_array_layers(),
                        "mip_level_count":texture.mip_level_count(),"sample_count":texture.sample_count(),"dimension":texture.dimension(),"format":texture.format()},
                    "native_allocation_id":version.allocation_id,"initial_version":version}));
                if initial_blob==first.name {
                    if self.kind.is_graph() {self.span.texture_blobs.insert(key,initial_blob);}
                    initial.push(first);
                }
            }
        }
        let new_bytes=match plan_bytes(&initial).and_then(|n|add(n,plan_bytes(&prepared.plans)?)){Ok(n)=>n,Err(e)=>{self.reject(&e);return;}};
        let reserved=self.pending.as_ref().map_or(final_bytes,|p|p.final_bytes);
        let retained=if self.kind.is_finished() {self.pending.as_ref().map_or(0,|p|p.readbacks.iter().map(|r|r.plan.bytes).sum())}else{self.pending.as_ref().map_or(0,|p|p.bytes)};
        if !self.reserve_fits(retained,new_bytes,reserved) || !self.reserve_fits(self.live_bytes.load(Ordering::SeqCst),new_bytes,reserved) {
            self.reject(&format!("complete span snapshot budget: retained={retained}, live={}, added={new_bytes}, final_reserved={reserved}, limit={}",self.live_bytes.load(Ordering::SeqCst),self.staging_limit()));return;
        }
        if self.kind.is_finished() {if let Err(e)=self.finished_begin_spool(gpu){self.reject(&e);return;}}
        let errors=gpu.errors.load(Ordering::SeqCst);gpu.end_pass();
        self.epoch_bytes+=new_bytes;
        let mut readbacks=self.enqueue_span(gpu,initial);readbacks.extend(self.enqueue_span(gpu,prepared.plans));
        if let Some(p)=&mut self.pending {p.draws.push(prepared.draw);p.readbacks.extend(readbacks);p.bytes+=new_bytes;p.epoch=gpu.enc_epoch;}
        else {
            if !self.kind.is_graph() || self.graph.stages.is_empty() {self.attempts+=1;}
            self.pending=Some(Pending{kind:CaptureKind::TargetSpan,alias:Some(json!({"interval_start_command":self.span.boundary.unwrap(),
                "queries_inactive":gpu.occlusion_active.is_none() && gpu.occlusion_pass_slot.is_none() && gpu.pending_resolve.is_empty()})),
                initialization_ledger:self.texture_ledger.as_ref().map(texture_ledger::Ledger::capture_evidence),shader_sources:None,
                epoch:gpu.enc_epoch,errors,view:prepared.view,domain:prepared.domain,draws:vec![prepared.draw],attachments,
                targets:prepared.targets,readbacks,bytes:new_bytes,final_bytes,finished:false,intervening_commands:vec![],candidate_key:vec![]});
        }
    }

    pub(super) fn after_target_span(&mut self,gpu:&mut Gpu) {
        let Some(p)=self.pending.as_ref().filter(|p|!p.finished) else{return;};
        let after=match self.target_versions(&p.domain){Ok(v)=>v,Err(e)=>{self.reject(&e);return;}};
        if self.span.before.len()!=after.len() {self.reject("span target version cardinality changed");return;}
        let writes:Vec<_>=self.span.before.iter().zip(&after).filter(|(a,b)|a!=b).map(|(a,b)|json!({"before":a,"after":b})).collect();
        let p=self.pending.as_mut().unwrap();p.draws.last_mut().unwrap()["writes"]=json!(writes);p.epoch=gpu.enc_epoch;
        for (attachment,version) in p.attachments.iter_mut().zip(after) {attachment["final_version"]=json!(version);}
    }

    pub(super) fn close_target_span(&mut self,gpu:&mut Gpu) {
        let Some(p)=self.pending.as_ref().filter(|p|!p.finished) else{return;};
        let unique:std::collections::HashSet<_>=p.draws.iter().map(|d|d["pipeline"]["effective_shaders"].to_string()).collect();
        if !self.kind.is_finished() && (!self.kind.is_graph() || self.graph.stages.is_empty()) && (p.draws.len()<2 || unique.len()<2) {self.reject("complete interval is not heterogeneous");return;}
        if !self.kind.is_finished() && (gpu.occlusion_active.is_some() || gpu.occlusion_pass_slot.is_some() || !gpu.pending_resolve.is_empty()) {self.reject("span closure has query state");return;}
        let mut plans=Vec::new();let mut final_names=Vec::new();
        for ((t,a,r),attachment) in p.targets.iter().zip(&p.attachments) {
            let version:VersionRef=match serde_json::from_value(attachment["final_version"].clone()) {Ok(v)=>v,Err(_)=>{self.reject("span final version missing");return;}};
            let key=(version.allocation_id,version.subresource.mip,version.subresource.layer,version.subresource.aspect,version.content_version);
            if self.kind.is_graph() {if let Some(blob)=self.span.texture_blobs.get(&key) {final_names.push(blob.clone());continue;}}
            let name=self.capture_blob_name(format!("final-{r}.bin"));
            match subresource_plan(name.clone(),t,version.subresource.mip,version.subresource.layer,*a) {Ok(v)=>plans.push(v),Err(e)=>{self.reject(&e);return;}}
            if self.kind.is_graph() {self.span.texture_blobs.insert(key,name.clone());}
            final_names.push(name);
        }
        let bytes=match plan_bytes(&plans){Ok(v)=>v,Err(e)=>{self.reject(&e);return;}};
        if bytes>p.final_bytes {self.reject("span final snapshots exceed reserved budget");return;}
        gpu.end_pass();let readbacks=self.enqueue_span(gpu,plans);
        let checkpoint=if self.kind.is_graph() {Some(self.graph_next_event())} else {None};
        let p=self.pending.as_mut().unwrap();p.readbacks.extend(readbacks);p.bytes+=bytes;
        self.epoch_bytes+=bytes;
        for (a,name) in p.attachments.iter_mut().zip(final_names) {a["final_blob"]=name.into();}
        p.finished=true;p.epoch=gpu.enc_epoch;
        p.alias.as_mut().unwrap()["interval_end_command"]=if self.kind.is_finished() {self.sequence}else{self.sequence.saturating_add(1)}.into();
        if let Some(event)=checkpoint {p.alias.as_mut().unwrap()["checkpoint_event_index"]=event.into();}
        if self.kind.is_graph() {
            let p=self.pending.take().unwrap();
            if let Err(e)=self.graph_retain_stage(p) {self.reject(&e);}
        } else {gpu.capture_bindings.take();}

    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_only_split_is_finished_same_present_and_exact_native_targets_only() {
        for kind in [CaptureKind::Finished,CaptureKind::ExecutedFinished] {
            assert_eq!(context_boundary(kind,true,true,false,true),ContextBoundary::Split);
            assert_eq!(context_boundary(kind,true,true,true,true),ContextBoundary::Continue);
            assert_eq!(context_boundary(kind,false,true,true,true),ContextBoundary::Reject);
            for (present,domain,targets) in [(false,true,true),(true,false,true),(true,true,false)] {
                assert_eq!(context_boundary(kind,present,domain,false,targets),ContextBoundary::Reject);
            }
        }
        for kind in [CaptureKind::TwoDraw,CaptureKind::ColorNull,CaptureKind::TargetSpan,CaptureKind::Compute,CaptureKind::Graph] {
            assert_eq!(context_boundary(kind,true,true,false,true),ContextBoundary::Reject);
        }
        assert!(!retained_targets_equal(&[1,2],&[1]));
        assert!(!retained_targets_equal(&[1],&[1,2]));
        assert!(!retained_targets_equal(&[(1,"color0")],&[(1,"color1")]));
        assert!(!retained_targets_equal(&[(1,"depth")],&[(2,"depth")]));
        assert!(retained_targets_equal(&[(1,"depth"),(1,"stencil")],&[(1,"depth"),(1,"stencil")]));
        let mut span=SpanState::default();span.texture_blobs.insert((1,0,0,Aspect::Color,2),"actual.bin".into());
        span.before.push(VersionRef{allocation_id:1,content_version:2,subresource:Subresource{mip:0,layer:0,aspect:Aspect::Color}});
        span.resume_at(45);assert_eq!(span.boundary,Some(45));assert!(!span.started);assert!(span.before.is_empty());
        assert_eq!(span.texture_blobs.len(),1);
        let raw=Value::Null;assert_ne!(raw,json!({"eye_index":0}));
        assert_eq!(context_boundary(CaptureKind::ExecutedFinished,true,true,false,true),ContextBoundary::Split);
    }

    #[test]
    fn upload_revisions_retain_only_already_captured_native_identities() {
        let mut buffers=HashMap::new();
        let mut revisions=HashMap::new();
        for value in 0..10_000u64 {
            let uncaptured=Arc::new(value);
            advance_captured_revision(&buffers,&mut revisions,&uncaptured);
            assert_eq!(Arc::strong_count(&uncaptured),1);
        }
        assert!(revisions.is_empty());

        let captured=Arc::new(10_001u64);
        let other=Arc::new(10_002u64);
        buffers.insert(captured.clone(),1);
        assert_eq!(revisions.get(&captured).copied().unwrap_or(0)+1,1);
        for expected in 1..=3 {
            advance_captured_revision(&buffers,&mut revisions,&captured);
            assert_eq!(revisions[&captured],expected);
            advance_captured_revision(&buffers,&mut revisions,&other);
            assert_eq!(revisions.len(),1);
            assert_eq!(Arc::strong_count(&captured),3);
            assert_eq!(Arc::strong_count(&other),1);
        }
        assert!(revisions.keys().all(|key|buffers.contains_key(key)));
        revisions.clear();buffers.clear();
        assert_eq!(Arc::strong_count(&captured),1);
    }
}

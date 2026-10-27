//! One actual post-world compute dispatch, with exact original inputs/outputs.
//! This is a replay primitive, not a complete frame or stereo certificate.
use super::*;
use super::super::capture_bindings::{CapturedGroup,Resource};
use texture_ledger::{Aspect,VersionRef};

#[derive(Default)]
pub(super) struct ComputeState {
    main_world:Option<(u64,Value)>,
    final_plans:Vec<Plan>,
}
struct ComputePrepared {
    dispatch:Value,context:Value,attachments:Vec<Value>,
    initial:Vec<Plan>,finals:Vec<Plan>,shaders:Value,
}

fn dispatch_extent(groups:[u32;3])->Result<(),String> {
    if groups.iter().any(|&n|n==0 || n>65535)
        || groups.into_iter().try_fold(1u64,|n,v|n.checked_mul(v as u64)).is_none_or(|n|n>1<<20) {
        return Err("compute workgroup bound".into());
    }
    Ok(())
}

impl Capture {
    pub(super) fn observe_compute_world(&mut self,gpu:&Gpu,d:&Resolved<'_>) {
        if self.done || self.pending.is_some() {return;}
        let Some(pass)=&gpu.pass_info else{return;};
        if pass.rt_formats!=vec![Some(TextureFormat::Bgra8Unorm);4]
            || pass.ds_format!=Some(TextureFormat::Depth32FloatStencil8)
            || d.emitted_viewport[0]!=0. || d.emitted_viewport[1]!=0.
            || d.emitted_viewport[2]!=pass.width as f32 || d.emitted_viewport[3]!=pass.height as f32 {return;}
        if let Some(Ok(view))=gpu.world_view.map(target_span::world) {
            self.compute_state.main_world=Some((gpu.presents,view));
        }
    }
    pub(super) fn compute_command(&mut self,op:u32) {
        if op==7 {self.compute_state.main_world=None;}
    }
    fn prepare_compute(&mut self,gpu:&Gpu,program:&Program,group:&CapturedGroup,groups:[u32;3])->Result<ComputePrepared,String> {
        dispatch_extent(groups)?;
        if gpu.errors.load(Ordering::SeqCst)!=0 || !gpu.bind_group_prepare.enabled
            || (!self.kind.is_finished() && (gpu.occlusion_active.is_some() || gpu.occlusion_pass_slot.is_some() || !gpu.pending_resolve.is_empty())) {
            return Err("compute requires ordinary resolved bindings and no GPU errors or active queries".into());
        }
        let (present,world)=self.compute_state.main_world.as_ref().ok_or("compute has no preceding main-world draw")?;
        if *present!=gpu.presents {return Err("compute main-world observation is from another present".into());}
        let context=json!({"native_present":gpu.presents+1,"preceding_main_world":world,
            "qualification":"preceding actual main-target draw in this present; not a full dependency or stereo certificate"});
        if let Some(reason)=&group.unsupported {return Err(format!("compute resolved bindings: {reason}"));}
        let mut outputs=Vec::new();
        for entry in &group.entries {
            match entry.layout.ty {
                BindingType::Buffer{ty:BufferBindingType::Storage{read_only:false},..}=>
                    return Err("compute writable buffers and counters require a separate replay contract".into()),
                BindingType::StorageTexture{access,format,view_dimension}=>{
                    let Resource::Texture{view,descriptor,native_dummy}= &entry.resource else{return Err("compute storage binding has no actual texture".into());};
                    let t=view.texture();
                    if access!=StorageTextureAccess::WriteOnly || view_dimension!=TextureViewDimension::D2
                        || entry.layout.visibility!=ShaderStages::COMPUTE || *native_dummy
                        || t.dimension()!=TextureDimension::D2 || t.sample_count()!=1
                        || descriptor.mip_level_count!=Some(1) || descriptor.array_layer_count!=Some(1)
                        || format!=descriptor.format.unwrap_or(t.format()) || format.is_depth_stencil_format()
                        || format.block_dimensions()!=(1,1) {
                        return Err("compute output requires one actual single-sample D2 write-only color subresource".into());
                    }
                    if outputs.iter().any(|(_,p):&(u32,Texture)|p==t) {return Err("compute output aliases another storage binding parent".into());}
                    outputs.push((entry.layout.binding,t.clone()));
                }
                _=>{}
            }
        }
        if outputs.is_empty() || outputs.len()>8 {return Err("compute storage output count bound".into());}
        for entry in &group.entries {
            if matches!(entry.layout.ty,BindingType::Texture{..}) {
                if let Resource::Texture{view,..}=&entry.resource {
                    if outputs.iter().any(|(_,t)|t==view.texture()) {return Err("compute sampled and storage parents overlap".into());}
                }
            }
        }
        let (module,shaders)={
            let registry=self.shader_sources.as_ref().ok_or("compute shader registry missing")?.borrow();
            let module=registry.module(&program.module);
            if module["status"]!="exact" {return Err("compute final module source unresolved".into());}
            let hash=module["sha256"].as_str().ok_or("compute module hash absent")?.to_owned();
            let mut snapshot=registry.snapshot(&[hash]);
            if snapshot["complete"]!=true {return Err("compute final shader export incomplete".into());}
            snapshot["export_scope"]="single actual successful compute pipeline module".into();
            (module,snapshot)
        };
        if !self.kind.is_graph() {self.span.clear_interval();}
        let mut initial=Vec::new();
        let bindings=self.span_group(gpu,0,group,0,&mut initial)?;
        let mut finals=Vec::new();let mut attachments=Vec::new();
        for (binding,texture) in outputs {
            let b=bindings.iter().find(|b|b["binding"]==binding).ok_or("compute output binding metadata missing")?;
            if b["kind"]!="storage_texture" {return Err("compute output typed metadata mismatch".into());}
            let subs=b["subresources"].as_array().filter(|s|s.len()==1).ok_or("compute output requires one exact version")?;
            let sub=&subs[0];let reference:VersionRef=serde_json::from_value(sub["version"].clone()).map_err(|e|e.to_string())?;
            if reference.subresource.aspect!=Aspect::Color {return Err("compute output color version mismatch".into());}
            let role=format!("storage-b{binding}");let final_blob=self.capture_blob_name(format!("final-{role}.bin"));
            finals.push(target_span::subresource_plan(final_blob.clone(),&texture,reference.subresource.mip,reference.subresource.layer,TextureAspect::All)?);
            attachments.push(json!({"role":role,"binding":binding,"native_allocation_id":reference.allocation_id,
                "format":texture.format(),"descriptor":b["descriptor"],"view":b["view"],
                "width":sub["width"],"height":sub["height"],"mip":reference.subresource.mip,"slice":reference.subresource.layer,
                "initial_blob":sub["blob"],"initial_version":reference,"final_blob":final_blob}));
        }
        let dispatch=json!({"workgroups":groups,"command_sequence":self.sequence,"bindings":bindings,
            "pipeline":{"effective_shader":module,"compilation_options":"wgpu default"},"writes":[]});
        Ok(ComputePrepared{dispatch,context,attachments,initial,finals,shaders})
    }
    pub(in super::super) fn before_compute(&mut self,gpu:&mut Gpu,program:&Program,group:&BgResult,groups:[u32;3]) {
        if !matches!(self.kind,CaptureKind::Compute|CaptureKind::Graph|CaptureKind::Finished|CaptureKind::ExecutedFinished) || self.done || self.finished.postlude
            || self.pending.as_ref().is_some_and(|p|p.finished) {return;}
        if self.kind.is_graph() {
            if self.graph.anchor.is_none() {return;}
            if self.graph.dispatch_pending.take()!=Some(self.sequence) || self.pending.is_some()
                || self.graph.stages.is_empty() || self.compute_state.main_world.is_none() {
                self.reject("connected graph dispatch before raster interval closure");return;
            }
        } else if self.pending.is_some() || self.attempts>=128 || self.compute_state.main_world.is_none() {return;}
        let mut prepared=match group.capture.as_ref().ok_or("compute bind group metadata missing".to_string())
            .and_then(|g|self.prepare_compute(gpu,program,g,groups)) {
            Ok(v)=>v,Err(e)=>{self.reject(&e);return;}
        };
        if self.kind.is_graph() {
            if self.graph.present!=gpu.presents.checked_add(1) || self.graph.anchor.as_ref()!=Some(&prepared.context["preceding_main_world"]) {
                self.reject("connected compute lost raster anchor or present");return;
            }
            prepared.dispatch["event_index"]=self.graph_next_event().into();
        }
        let initial_bytes=match plan_bytes(&prepared.initial){Ok(n)=>n,Err(e)=>{self.reject(&e);return;}};
        let final_bytes=match plan_bytes(&prepared.finals){Ok(n)=>n,Err(e)=>{self.reject(&e);return;}};
        if !self.reserve_fits(0,initial_bytes,final_bytes)
            || !self.reserve_fits(self.live_bytes.load(Ordering::SeqCst),initial_bytes,final_bytes) {
            self.reject("compute immutable snapshots exceed retained or in-flight budget");return;
        }
        gpu.end_pass();if !self.kind.is_graph() {self.attempts+=1;}self.epoch_bytes+=initial_bytes;
        let readbacks=self.enqueue_span(gpu,prepared.initial);
        self.compute_state.final_plans=prepared.finals;
        self.pending=Some(Pending{kind:CaptureKind::Compute,alias:None,
            initialization_ledger:self.texture_ledger.as_ref().map(texture_ledger::Ledger::capture_evidence),
            shader_sources:Some(prepared.shaders),epoch:gpu.enc_epoch,errors:gpu.errors.load(Ordering::SeqCst),
            view:Value::Null,domain:prepared.context,draws:vec![prepared.dispatch],attachments:prepared.attachments,
            targets:vec![],readbacks,bytes:initial_bytes,final_bytes,finished:false,intervening_commands:vec![],candidate_key:vec![]});
    }
    pub(in super::super) fn after_compute(&mut self,gpu:&mut Gpu) {
        if !matches!(self.kind,CaptureKind::Compute|CaptureKind::Graph|CaptureKind::Finished|CaptureKind::ExecutedFinished) || !self.pending.as_ref().is_some_and(|p|p.kind==CaptureKind::Compute && !p.finished) {return;}
        let versions=(||->Result<Vec<(VersionRef,VersionRef)>,String>{
            let ledger=self.texture_ledger.as_ref().ok_or("compute output ledger absent")?;
            if !ledger.active(){return Err("compute output ledger inactive".into());}
            self.pending.as_ref().unwrap().attachments.iter().map(|a|{
                let before:VersionRef=serde_json::from_value(a["initial_version"].clone()).map_err(|e|e.to_string())?;
                let after=ledger.current(before.allocation_id,before.subresource).ok_or("compute output final version missing")?;
                if after.content_version<=before.content_version {return Err("compute output lacks post-dispatch version transition".into());}
                Ok((before,after))
            }).collect()
        })();
        let versions=match versions {Ok(v)=>v,Err(e)=>{self.reject(&e);self.compute_state.final_plans.clear();return;}};
        let plans=std::mem::take(&mut self.compute_state.final_plans);
        let readbacks=self.enqueue_span(gpu,plans);let p=self.pending.as_mut().unwrap();
        p.readbacks.extend(readbacks);p.bytes+=p.final_bytes;self.epoch_bytes+=p.final_bytes;
        for (attachment,(_,after)) in p.attachments.iter_mut().zip(&versions) {attachment["final_version"]=json!(after);}
        p.draws[0]["writes"]=json!(versions.iter().map(|(before,after)|json!({"before":before,"after":after})).collect::<Vec<_>>());
        p.finished=true;p.epoch=gpu.enc_epoch;
        if self.kind.is_finished() {
            if let Err(e)=self.finished_retain_compute(gpu){self.reject(&e);return;}
        } else if self.kind==CaptureKind::Graph {
            if let Err(e)=self.graph_finish_compute(gpu) {self.reject(&e);return;}
        }
        // Later finished-graph stages can reuse already cached native views;
        // their exact descriptors remain in this registry across dispatches.
        if !self.kind.is_finished() {gpu.capture_bindings.take();}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compute_extent_is_nonzero_and_bounded_without_product_overflow() {
        assert!(dispatch_extent([40,23,1]).is_ok());
        for groups in [[0,1,1],[1,65536,1],[65535,65535,65535],[1024,1024,2]] {
            assert!(dispatch_extent(groups).is_err());
        }
    }
}

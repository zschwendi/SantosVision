//! Full original finished-world capture plumbing. Enabled only by private capture.
use super::*;
use texture_ledger::{Aspect,Subresource,VersionRef};

#[derive(Default)]
pub(super) struct FinishedState {
    pub(super) spool:Option<capture_spool::Spool>,
    pub(super) extra_readbacks:Vec<Readback>,
    pub(super) operations:Vec<Value>,
    pub(super) postlude:bool,
    pub(super) peak_live_staging:u64,
    pub(super) original_omission_count:usize,
    hud_boundary:Option<HudBoundary>,
}

impl Capture {
    pub(in super::super) fn captures_finished_publication(&self)->bool {
        self.kind.is_finished() && !self.done && self.finished.postlude
    }
    pub(super) fn finished_command(&mut self,op:u32) {
        if !self.captures_finished_graph() || self.graph.anchor.is_none(){return;}
        if self.graph.dispatch_pending.take().is_some(){self.reject("finished graph dispatch was not captured");return;}
        if op==39 {self.graph.dispatch_pending=Some(self.sequence);return;}
        // Immutable later resources and buffer revisions are snapshotted at
        // actual use; texture writers require their explicit operation hook.
        if matches!(op,7|13|36|40|43) || op>47 {
            self.reject(&format!("finished graph contains unsupported opcode {op}"));return;
        }
        if matches!(op,0|1|2|9|10|12|18..=32|47){self.span_command(op);}
    }
    pub(super) fn finished_retain_compute(&mut self,gpu:&Gpu)->Result<(),String> {
        let checkpoint=self.graph_next_event();
        let mut p=self.pending.take().ok_or("finished compute pending absent")?;
        if p.kind!=CaptureKind::Compute || !p.finished || p.draws.len()!=1 || self.graph.stages.len()>=1024 {
            return Err("finished graph compute closure invalid".into());
        }
        for attachment in &p.attachments {
            let v:VersionRef=serde_json::from_value(attachment["final_version"].clone()).map_err(|e|e.to_string())?;
            let name=attachment["final_blob"].as_str().ok_or("finished compute final blob absent")?.to_owned();
            self.span.texture_blobs.insert((v.allocation_id,v.subresource.mip,v.subresource.layer,v.subresource.aspect,v.content_version),name);
        }
        p.alias=Some(json!({"checkpoint_event_index":checkpoint,"queries_inactive":gpu.occlusion_active.is_none() && gpu.occlusion_pass_slot.is_none() && gpu.pending_resolve.is_empty()}));
        self.graph.stages.push(p);
        self.span.resume_at(self.sequence);
        Ok(())
    }
    pub(in super::super) fn captures_finished_graph(&self)->bool {
        self.kind.is_finished() && !self.done && !self.finished.postlude
    }
    pub(in super::super) fn finished_before_aux(&mut self,gpu:&mut Gpu)->Result<(),String> {
        if !self.captures_finished_graph() || self.graph.anchor.is_none(){return Ok(());}
        self.close_target_span(gpu);
        if self.done {return Err("finished graph interval rejected before auxiliary operation".into());}
        self.span.resume_at(self.sequence);
        Ok(())
    }
    pub(super) fn finished_record_operation(&mut self,value:Value)->Result<(),String> {
        if !self.captures_finished_graph() || self.graph.anchor.is_none(){return Err("finished operation outside active original graph".into());}
        if self.finished.operations.len()>=8192 || !value.is_object(){return Err("finished operation bound or shape".into());}
        if value["kind"]=="query" {if let Some(p)=&mut self.pending {if let Some(alias)=&mut p.alias {alias["queries_inactive"]=false.into();}}}
        self.finished.operations.push(value);Ok(())
    }
    /// Enqueue a version's immutable bytes at an already safe outside-pass seam.
    /// No encoder submission or GPU wait is introduced by this operation.
    pub(super) fn finished_snapshot(&mut self,gpu:&mut Gpu,texture:&Texture,native_id:u64,
        mip:u32,layer:u32,aspect:TextureAspect)->Result<String,String> {
        if !self.captures_finished_graph(){return Err("finished snapshot outside graph".into());}
        let aspect_tag=match aspect{TextureAspect::DepthOnly=>Aspect::Depth,TextureAspect::StencilOnly=>Aspect::Stencil,
            TextureAspect::All if !texture.format().is_depth_stencil_format()=>Aspect::Color,
            _=>return Err("finished snapshot needs one exact aspect".into())};
        let version=self.texture_ledger.as_ref().filter(|l|l.active()).and_then(|l|
            l.current(native_id,Subresource{mip,layer,aspect:aspect_tag})).ok_or("finished snapshot current native version absent")?;
        let key=(native_id,mip,layer,aspect_tag,version.content_version);
        if let Some(name)=self.span.texture_blobs.get(&key){return Ok(name.clone());}
        let name=format!("finished-texture{native_id}-v{}-m{mip}-l{layer}-{aspect_tag:?}.bin",version.content_version);
        let plan=target_span::subresource_plan(name.clone(),texture,mip,layer,aspect)?;
        let reserved=self.pending.as_ref().map_or(0,|p|p.final_bytes);
        if !self.reserve_fits(self.live_bytes.load(Ordering::SeqCst),plan.bytes,reserved){return Err("finished snapshot live staging bound".into());}
        let bytes=plan.bytes;gpu.end_pass();
        let readbacks=self.enqueue_span(gpu,vec![plan]);
        self.finished.extra_readbacks.extend(readbacks);
        self.epoch_bytes=self.add_epoch_bytes(bytes)?;
        self.finished.peak_live_staging=self.finished.peak_live_staging.max(self.live_bytes.load(Ordering::SeqCst));
        self.span.texture_blobs.insert(key,name.clone());Ok(name)
    }
    pub(super) fn finished_begin_spool(&mut self,gpu:&Gpu)->Result<(),String>{
        if self.finished.spool.is_none(){self.finished.spool=Some(capture_spool::Spool::new(self.directory.clone(),gpu.errors.clone(),gpu.errors.load(Ordering::SeqCst),gpu.inflight.clone(),self.staging_limit())?);}
        Ok(())
    }
    /// Called only from the existing natural-submission callback seam.
    pub(super) fn finished_spool_submitted(&mut self,epoch:u64)->Result<(),String>{
        let Some(spool)=self.finished.spool.as_ref()else{return Ok(());};
        if let Some(error)=spool.failure(){return Err(error);}
        let mut readbacks=std::mem::take(&mut self.finished.extra_readbacks);
        readbacks.append(&mut self.graph.proxy_readbacks);
        for stage in &mut self.graph.stages{readbacks.append(&mut stage.readbacks);}
        if let Some(p)=&mut self.pending{readbacks.append(&mut p.readbacks);}
        self.finished.peak_live_staging=self.finished.peak_live_staging.max(self.live_bytes.load(Ordering::SeqCst));
        spool.submitted(readbacks,epoch)
    }
    pub(super) fn finished_version(&self,id:u64,mip:u32,layer:u32,aspect:Aspect)->Result<VersionRef,String>{
        self.texture_ledger.as_ref().filter(|l|l.active()).and_then(|l|l.current(id,Subresource{mip,layer,aspect}))
            .ok_or("finished operation current version absent".into())
    }
}

struct HudBoundary {
    source:Texture, source_id:u64, world:Texture, world_id:u64,
    source_ref:Value, world_ref:Value,
}
fn image_reference(texture:&Texture,version:VersionRef,blob:String)->Value {
    json!({"version":version,"descriptor":{"width":texture.width(),"height":texture.height(),
        "depth_or_array_layers":texture.depth_or_array_layers(),"mip_level_count":texture.mip_level_count(),
        "sample_count":texture.sample_count(),"dimension":texture.dimension(),"format":texture.format()},"blob":blob})
}
impl Capture {
    pub(in super::super) fn finished_hud_world_copy(&mut self,gpu:&mut Gpu,source:&Texture,source_id:u64,world:&Texture,world_id:u64)->Result<(),String> {
        if !self.captures_finished_graph() || self.graph.anchor.is_none(){return Ok(());}
        self.finished_before_aux(gpu)?;
        if source==world || source_id==world_id || self.finished.hud_boundary.is_some(){return Err("finished HUD world copy parent contract".into());}
        let source_version=self.finished_version(source_id,0,0,Aspect::Color)?;
        let world_version=self.finished_version(world_id,0,0,Aspect::Color)?;
        let source_blob=self.finished_snapshot(gpu,source,source_id,0,0,TextureAspect::All)?;
        let world_blob=self.finished_snapshot(gpu,world,world_id,0,0,TextureAspect::All)?;
        self.finished.hud_boundary=Some(HudBoundary{source:source.clone(),source_id,world:world.clone(),world_id,
            source_ref:image_reference(source,source_version,source_blob),world_ref:image_reference(world,world_version,world_blob)});
        // Subsequent original HUD/query operations are intentionally outside the
        // replay graph. Actual accepted publication must still prove this exact
        // sidecar, selected depth and same final-color parent survived.
        self.finished.postlude=true;
        Ok(())
    }
    pub(in super::super) fn finished_publication(&mut self,gpu:&mut Gpu,color:&Texture,color_id:u64,world:&Texture,world_id:u64,depth:&Texture,depth_id:u64,view:crate::xr_view::WorldView,hud_draw_count:u64)
        ->Result<Option<Arc<super::super::finished_endpoint::PublicationToken>>,String> {
        if !self.kind.is_finished() || self.done || !self.finished.postlude{return Ok(None);}
        if self.epoch_bytes!=0 || !self.epoch_reservations.is_empty(){return Err("finished publication preceded original natural snapshot submission".into());}
        let boundary=self.finished.hud_boundary.as_ref().ok_or("finished publication lacks original HUD boundary")?;
        if &boundary.source!=color || boundary.source_id!=color_id || &boundary.world!=world || boundary.world_id!=world_id {
            return Err("finished publication changed original color/HUD-world parents".into());
        }
        let depth_version=self.finished_version(depth_id,0,0,Aspect::Color)?;
        let key=(depth_id,0,0,Aspect::Color,depth_version.content_version);
        let blob=self.span.texture_blobs.get(&key).ok_or("finished selected depth was not captured inside the pre-HUD graph")?.clone();
        let world_json=target_span::raw_world(view)?;
        let token=super::super::finished_endpoint::PublicationToken::new(self.graph.capture_id.clone(),
            self.graph.present.ok_or("finished native present absent")?,color,color_id,world,world_id,depth,depth_version,view,world_json.clone(),hud_draw_count)?;
        let mut depth_ref=image_reference(depth,depth_version,blob);depth_ref["world_view"]=world_json;
        let source_ref=boundary.source_ref.clone();let world_ref=boundary.world_ref.clone();
        let endpoint_event=self.graph_next_event();
        let endpoint=json!({"kind":"finished_endpoint","capture_id":self.graph.capture_id,"native_present":self.graph.present,
            "operation":{"event_index":endpoint_event,"command_sequence":self.sequence,
                "hud_free_source":source_ref,"hud_world":world_ref,"depth_source":depth_ref}});
        let manifest=self.finished_manifest(gpu,endpoint)?;
        let spool=self.finished.spool.take().ok_or("finished original spool absent")?;
        let directory=self.directory.clone();let errors=gpu.errors.clone();let peak=self.finished.peak_live_staging;
        let schema=self.kind.schema();
        let writer_token=token.clone();self.done=true;self.texture_ledger.take();self.span.clear_interval();
        std::thread::Builder::new().name("private-finished-capture".into()).spawn(move||{
            let result=(||->Result<(),String>{
                let publication=writer_token.wait(Duration::from_secs(60))?;
                let (blobs,spool_receipt)=spool.finish()?;
                if errors.load(Ordering::SeqCst)!=0{return Err("GPU error before finished capture publication".into());}
                let mut manifest=manifest;
                let last=manifest["stages"].as_array_mut().and_then(|s|s.last_mut()).ok_or("finished endpoint stage absent")?;
                last["operation"]["publication"]=publication;
                manifest["blobs"]=json!(blobs);manifest["allocated_readback_bytes"]=spool_receipt["cumulative_padded_staging_bytes"].clone();
                manifest["spool"]=spool_receipt;manifest["peak_live_staging_bytes"]=peak.into();
                let bytes=serde_json::to_vec(&manifest).map_err(|e|e.to_string())?;
                if bytes.len()>16<<20{return Err(format!("finished manifest {} exceeds16MiB",bytes.len()));}
                publish_new(&directory.join("capture.json"),&bytes)
            })();
            if let Err(reason)=result {let _=publish_new(&directory.join("rejected.json"),&serde_json::to_vec_pretty(&json!({"schema":schema,"status":"rejected","reason":reason})).unwrap());log(&format!("finished native graph rejected: {reason}"));}
        }).map_err(|e|format!("finished writer: {e}"))?;
        Ok(Some(token))
    }
    fn finished_manifest(&mut self,gpu:&Gpu,endpoint:Value)->Result<Value,String> {
        if self.pending.is_some() || self.graph.proxy_pending.is_some() || self.graph.dispatch_pending.is_some() || self.graph.event>=8192 {
            return Err("finished graph contains an open or excessive event interval".into());
        }
        let backend=format!("{:?}",gpu.adapter.get_info().backend);
        let mut stages=std::mem::take(&mut self.finished.operations);
        for p in std::mem::take(&mut self.graph.stages) {
            if !p.finished || !p.readbacks.is_empty() || p.errors!=0{return Err("finished stage unsubmitted or failed".into());}
            let closure=p.alias.as_ref().ok_or("finished stage closure absent")?;
            let mut capture=if p.kind==CaptureKind::TargetSpan {
                json!({"schema":4,"kind":"native_target_span","status":"captured",
                    "source":{"method":"Gpu::do_draw resolved bindings","backend":backend,"encoder_epoch":p.epoch},
                    "world_view":p.view,"target_domain":p.domain,"draws":p.draws,"attachments":p.attachments,
                    "allocated_readback_bytes":p.bytes,"intervening_commands":p.intervening_commands,
                    "initialization_ledger":p.initialization_ledger,"effective_shader_sources":p.shader_sources,
                    "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,
                    "closure":{"complete_target_interval":true,"first_executed_draw_captured":true,"last_executed_draw_captured":true,
                        "interval_start_command":closure["interval_start_command"],"interval_end_command":closure["interval_end_command"],
                        "checkpoint_event_index":closure["checkpoint_event_index"],"unmodelled_commands":0,
                        "initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,
                        "queries_inactive":closure["queries_inactive"],"queries_original_only":true,"gpu_errors":0}})
            }else if p.kind==CaptureKind::Compute {
                json!({"schema":5,"kind":"native_compute_dispatch","status":"captured",
                    "source":{"method":"Gpu::do_dispatch actual final pipeline and resolved bindings","backend":backend,"encoder_epoch":p.epoch},
                    "context":p.domain,"dispatch":p.draws.first().ok_or("finished compute dispatch absent")?,"attachments":p.attachments,
                    "allocated_readback_bytes":p.bytes,"initialization_ledger":p.initialization_ledger,"effective_shader_sources":p.shader_sources,
                    "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false,
                    "closure":{"single_original_dispatch":true,"checkpoint_event_index":closure["checkpoint_event_index"],"unmodelled_commands":0,
                        "initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,
                        "queries_inactive":closure["queries_inactive"],"queries_original_only":true,"gpu_errors":0}})
            }else{return Err("finished graph unexpected stage kind".into());};
            let kind=if p.kind==CaptureKind::Compute {"compute"}else if self.graph.anchor.as_ref()==Some(&capture["world_view"]){"raster"}else{"context_raster"};
            // Legacy shared metadata requires object-valued world records. A
            // truly absent native world context remains null and uncompressed.
            if p.kind==CaptureKind::TargetSpan && !capture["world_view"].is_null(){compact_target_span_metadata(&mut capture)?;}
            stages.push(json!({"kind":kind,"capture_id":self.graph.capture_id,"native_present":self.graph.present,"capture":capture}));
        }
        for mut operation in std::mem::take(&mut self.graph.operations) {
            let kind=operation.as_object_mut().and_then(|o|o.remove("operation_kind")).unwrap_or(json!("depth_to_color"));
            stages.push(json!({"kind":kind,"capture_id":self.graph.capture_id,"native_present":self.graph.present,"operation":operation}));
        }
        stages.push(endpoint);
        let index=|s:&Value|->Option<u64>{if s["kind"]=="compute" {s["capture"]["dispatch"]["event_index"].as_u64()}
            else if matches!(s["kind"].as_str(),Some("raster"|"context_raster")){s["capture"]["draws"][0]["event_index"].as_u64()}
            else{s["operation"]["event_index"].as_u64()}};
        if stages.is_empty() || stages.len()>8192 || stages.iter().any(|s|index(s).is_none()){return Err("finished stage event index absent/bound".into());}
        stages.sort_by_key(|s|index(s).unwrap());
        if stages.last().is_none_or(|s|s["kind"]!="finished_endpoint"){return Err("finished endpoint is not last".into());}
        let executed=self.kind==CaptureKind::ExecutedFinished;
        let omissions=stages.iter().filter(|s|s["kind"]=="original_omission").count();
        if omissions!=self.finished.original_omission_count || (executed && !(1..=1024).contains(&omissions))
            || (!executed && omissions!=0) {return Err("finished original omission contract mismatch".into());}
        let queries_inactive=!stages.iter().any(|s|s["kind"]=="query");
        let mut manifest=json!({"schema":if executed{8}else{7},"kind":if executed{"native_executed_finished_graph"}else{"native_finished_graph"},"status":"captured",
            "source":{"method":if executed{"single native executed finished graph"}else{"single native finished graph"},"backend":backend,"capture_id":self.graph.capture_id,
                "native_namespace":self.graph.capture_id,"native_present":self.graph.present,"encoder_epoch":gpu.enc_epoch},
            "world_view":self.graph.anchor,"stages":stages,"prior_rejected_candidates":self.rejected,
            "closure":{"same_capture":true,"same_native_namespace":true,"same_present":true,"continuous_command_interval":true,
                "unmodelled_commands":0,"initial_contents_captured":true,"original_final_captured":true,"natural_submission_completed":true,
                "queries_inactive":queries_inactive,"queries_original_only":true,"gpu_errors":0},
            "normal_frame_publication_changed":false,"finished_frame_or_density_accepted":false,"coherent_stereo_accepted":false});
        if executed {
            manifest["all_requested_draws_executed"]=false.into();
            manifest["original_omission_count"]=omissions.into();
            manifest["execution_contract"]="executed_original_image_with_recorded_omissions".into();
        }
        json_size_bounded(&manifest,128<<20)?;Ok(manifest)
    }
}

//! Private capture certificate for the original accepted WorldFrame callback.
//! Never publishes a frame, changes rendering, or substitutes a native texture.
use super::texture_ledger::{Aspect, VersionRef};
use crate::{world_frame::WorldFrame, xr_view::WorldView};
use serde_json::{json, Value};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use wgpu::{Texture, TextureDimension, TextureFormat};

pub(crate) struct PublicationToken {
    capture_id: String,
    native_present: u64,
    color: Texture,
    color_id: u64,
    hud_world: Texture,
    hud_world_id: u64,
    depth: Texture,
    depth_ref: VersionRef,
    view: WorldView,
    world_json: Value,
    hud_draw_count: u64,
    identities: Mutex<Vec<(Texture,u64)>>,
    result: Mutex<Option<Result<Value, String>>>,
    changed: Condvar,
}

fn native(t: &Texture) -> Result<u64, String> {
    crate::world_frame::native_texture_handle(t).filter(|n| *n != 0)
        .ok_or("finished endpoint requires an actual native Metal texture".into())
}
fn view_json(v: &WorldView) -> Result<Value, String> {
    let request=crate::xr_view::consumed_request_receipt(v).ok_or("finished endpoint consumed request expired")?;
    Ok(json!({"provenance_epoch":v.provenance_epoch,"camera_sequence":v.camera_sequence,
        "request_frame":v.request_frame,"head_pose_frame":v.head_pose_frame,"eye_index":v.eye_index,
        "view_count":v.view_count,"presentation_time_seconds":v.presentation_time_seconds,
        "game_from_eye":v.game_from_eye,"game_from_stage":v.game_from_stage,"projection_from_eye":v.projection_from_eye,
        "headset_applied":v.headset_applied,"depth_reverse_z":v.depth_reverse_z,
        "optics":{"scope_sequence":v.optics.scope_sequence,"magnification":v.optics.magnification,
            "reference_fov_degrees":v.optics.reference_fov_degrees,"actual_fov_degrees":v.optics.actual_fov_degrees},"request_record":request}))
}
fn frame_matches(f: &WorldFrame, v: &WorldView, present: u64, epoch: u64, camera: u64) -> bool {
    let exact=|a:&[f32;16],b:&[f32;16]|a.iter().zip(b).all(|(x,y)|x.is_finite()&&y.is_finite()&&x.to_bits()==y.to_bits());
    present>0 && f.game_frame==present && epoch==v.provenance_epoch && camera==v.camera_sequence
        && v.headset_applied && v.view_count==2 && v.eye_index<2
        && f.request_frame==v.request_frame && f.head_frame==v.head_pose_frame
        && f.view_index==v.eye_index && f.view_count==v.view_count
        && f.presentation_time.is_finite() && f.presentation_time.to_bits()==v.presentation_time_seconds.to_bits()
        && exact(&f.projection_from_eye,&v.projection_from_eye) && exact(&f.game_from_eye,&v.game_from_eye)
        && exact(&f.game_from_stage,&v.game_from_stage)
}
impl PublicationToken {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(capture_id:String,native_present:u64,color:&Texture,color_id:u64,
        hud_world:&Texture,hud_world_id:u64,depth:&Texture,depth_ref:VersionRef,
        view:WorldView,world_json:Value,hud_draw_count:u64)->Result<Arc<Self>,String> {
        if capture_id.is_empty() || capture_id.len()>256 || native_present==0 || hud_draw_count==0
            || color_id==0 || hud_world_id==0 || depth_ref.allocation_id==0 || depth_ref.content_version==0
            || color_id==hud_world_id || color_id==depth_ref.allocation_id || hud_world_id==depth_ref.allocation_id
            || color==hud_world || color==depth || hud_world==depth
            || !matches!(color.format(),TextureFormat::Bgra8Unorm|TextureFormat::Rgba8Unorm)
            || hud_world.format()!=color.format() || hud_world.mip_level_count()!=1
            || depth.format()!=TextureFormat::R32Float
            || [color,hud_world,depth].iter().any(|t|t.dimension()!=TextureDimension::D2||t.sample_count()!=1
                ||t.depth_or_array_layers()!=1||t.width()!=color.width()||t.height()!=color.height())
            || depth_ref.subresource.mip!=0 || depth_ref.subresource.layer!=0 || depth_ref.subresource.aspect!=Aspect::Color {
            return Err("finished endpoint original native identity/layout is unsupported".into());
        }
        native(color)?;native(hud_world)?;native(depth)?;
        if !view.headset_applied || view.view_count!=2 || view.eye_index>=2 || world_json!=view_json(&view)? {
            return Err("finished endpoint exact selected-depth world metadata differs".into());
        }
        Ok(Arc::new(Self {capture_id,native_present,color:color.clone(),color_id,hud_world:hud_world.clone(),hud_world_id,
            depth:depth.clone(),depth_ref,view,world_json,hud_draw_count,
            identities:Mutex::new(vec![(color.clone(),color_id),(hud_world.clone(),hud_world_id),(depth.clone(),depth_ref.allocation_id)]),
            result:Mutex::new(None),changed:Condvar::new()}))
    }
    pub(crate) fn reject(&self,reason:impl Into<String>) {
        if let Ok(mut result)=self.result.lock(){if result.is_none(){*result=Some(Err(reason.into()));self.changed.notify_all();}}
    }
    pub(crate) fn wait(&self,timeout:Duration)->Result<Value,String> {
        let state=self.result.lock().map_err(|_|"finished endpoint result lock poisoned")?;
        let (mut state,timed)=self.changed.wait_timeout_while(state,timeout,|s|s.is_none()).map_err(|_|"finished endpoint wait lock poisoned")?;
        if timed.timed_out()&&state.is_none(){*state=Some(Err("original finished publication callback was not accepted before timeout".into()));}
        state.clone().unwrap_or_else(||Err("finished publication callback unavailable".into()))
    }
    pub(crate) fn guard(self:&Arc<Self>)->PublicationGuard {PublicationGuard{token:self.clone(),finished:false,hud:None}}
    fn identity(&self,texture:&Texture)->Result<u64,String> {
        native(texture)?;
        let mut identities=self.identities.lock().map_err(|_|"endpoint native identity registry poisoned")?;
        if let Some((_,id))=identities.iter().find(|(t,_)|t==texture){return Ok(*id);}
        if identities.len()>=5{return Err("endpoint actual native texture registry bound".into());}
        let id=crate::world_command_trace::native_texture_tag(true).ok_or("native snapshot identity unavailable")?;
        identities.push((texture.clone(),id));Ok(id)
    }
    fn validate_inputs(&self,color:&Texture,depth:&Texture,hud:Option<&crate::xr_hud::HudImages>,frame:&WorldFrame,epoch:u64,camera:u64)->Result<(),String> {
        if color!=&self.color || depth!=&self.depth || hud.is_none_or(|h|h.world!=self.hud_world)
            || !frame_matches(frame,&self.view,self.native_present,epoch,camera) {
            return Err("original publication inputs do not match the captured HUD-free endpoint and selected depth".into());
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn accepted(&self,slot_color:&Texture,slot_depth:&Texture,hud:Option<&crate::xr_hud::HudImages>,
        frame:&WorldFrame,source_id:u64,generation:u64,epoch:u64,camera:u64)->Result<(),String> {
        if source_id==0 || !frame_matches(frame,&self.view,self.native_present,epoch,camera)
            || hud.is_none_or(|h|h.world!=self.hud_world) || slot_color==&self.color || slot_color==&self.hud_world
            || slot_color==slot_depth || frame.color_texture!=native(slot_color)? || frame.depth_texture!=native(slot_depth)?
            || frame.width!=self.color.width() || frame.height!=self.color.height() {
            return Err("accepted Ready does not retain the exact captured native endpoint".into());
        }
        // IDs are assigned only to these actual returned native Texture handles.
        // Known original parents reuse their authentic ledger allocation IDs.
        let color_slot_id=self.identity(slot_color)?;
        let same_depth=slot_depth==&self.depth;
        let depth_slot_id=self.identity(slot_depth)?;
        let receipt=json!({"capture_id":self.capture_id,"native_present":self.native_present,
            "accepted":true,"source_id":source_id,"source_generation":generation,"provenance_epoch":epoch,
            "eye_index":frame.view_index,"request_frame_sequence":frame.request_frame,"head_sequence":frame.head_frame,
            "camera_sequence":camera,"world_view":self.world_json,"graph_end_before_hud":true,"postlude_original_only":true,
            "hud_world_is_ready_hud_world":true,"ready_hud_world_allocation_id":self.hud_world_id,
            "final_color_parent_allocation_id":self.color_id,"hud_draw_count":self.hud_draw_count,"hud_rejected":false,
            "slot_color":{"kind":"original_copy","source_allocation_id":self.color_id,"destination_allocation_id":color_slot_id,"source_version":null},
            "slot_depth":{"kind":if same_depth{"same_native_handle"}else{"original_copy"},"source_allocation_id":self.depth_ref.allocation_id,
                "destination_allocation_id":depth_slot_id,"source_version":self.depth_ref}});
        let mut state=self.result.lock().map_err(|_|"finished endpoint result lock poisoned")?;
        if state.is_none(){*state=Some(Ok(receipt));self.changed.notify_all();}
        Ok(())
    }
}

fn same_arc<T>(expected:Option<&Arc<T>>,actual:Option<&Arc<T>>)->bool {
    matches!((expected,actual),(Some(a),Some(b)) if Arc::ptr_eq(a,b))
}

pub(crate) struct PublicationGuard {token:Arc<PublicationToken>,finished:bool,hud:Option<Arc<crate::xr_hud::HudImages>>}
impl PublicationGuard {
    pub(crate) fn validate_inputs(&mut self,color:&Texture,depth:&Texture,hud:Option<&Arc<crate::xr_hud::HudImages>>,frame:&WorldFrame,epoch:u64,camera:u64)->Result<(),String> {
        self.token.validate_inputs(color,depth,hud.map(Arc::as_ref),frame,epoch,camera)?;
        self.hud=hud.cloned();
        Ok(())
    }
    pub(crate) fn reject(mut self,reason:impl Into<String>){self.token.reject(reason);self.finished=true;}
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(mut self,slot_color:&Texture,slot_depth:&Texture,hud:Option<&Arc<crate::xr_hud::HudImages>>,
        frame:&WorldFrame,source_id:u64,generation:u64,epoch:u64,camera:u64) {
        if !same_arc(self.hud.as_ref(),hud){self.token.reject("accepted Ready changed the exact original HUD sidecar Arc");}
        else if let Err(e)=self.token.accepted(slot_color,slot_depth,hud.map(Arc::as_ref),frame,source_id,generation,epoch,camera){self.token.reject(e);}
        self.finished=true;
    }
}
impl Drop for PublicationGuard {
    fn drop(&mut self){if !self.finished{self.token.reject("original publication exited or callback retired before Ready acceptance");}}
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn callback_requires_same_sidecar_allocation(){
        let original=Arc::new(7u32);let retained=original.clone();let replacement=Arc::new(7u32);
        assert!(same_arc(Some(&original),Some(&retained)));
        assert!(!same_arc(Some(&original),Some(&replacement)));
        assert!(!same_arc(Some(&original),None));assert!(!same_arc::<u32>(None,None));
    }
    #[test] fn callback_identity_uses_exact_frame_present_eye_and_matrices(){
        let v=WorldView{headset_applied:true,view_count:2,eye_index:1,provenance_epoch:3,
            camera_sequence:4,request_frame:5,head_pose_frame:6,presentation_time_seconds:7.,
            game_from_eye:[0.;16],game_from_stage:[0.;16],projection_from_eye:[0.;16],depth_reverse_z:true,optics:Default::default()};
        let f=WorldFrame{game_frame:8,request_frame:5,head_frame:6,presentation_time:7.,view_index:1,view_count:2,
            projection_from_eye:v.projection_from_eye,game_from_eye:v.game_from_eye,game_from_stage:v.game_from_stage,..Default::default()};
        assert!(frame_matches(&f,&v,8,3,4));
        for mode in 0..7 {let mut bad=f;match mode{0=>bad.game_frame+=1,1=>bad.view_index=0,2=>bad.request_frame+=1,
            3=>bad.head_frame+=1,4=>bad.presentation_time=f64::from_bits(bad.presentation_time.to_bits()+1),
            5=>bad.projection_from_eye[10]=f32::from_bits(1),_=>bad.game_from_eye[0]=f32::NAN};
            assert!(!frame_matches(&bad,&v,8,3,4));}
        assert!(!frame_matches(&f,&v,8,9,4));assert!(!frame_matches(&f,&v,8,3,9));
    }
}

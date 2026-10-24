//! The genuine ordinary28266 cache producer, not a desired controller ray.
//! This module performs no guest calls/writes and retains no guest addresses.
use super::*;
use std::sync::atomic::AtomicU64;
const RAY_AGE:Duration=Duration::from_millis(100);
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone)]struct Sample{serial:u64,frame:[f32;16],view:crate::xr_view::WorldView,at:Instant}
static SAMPLE:Mutex<Option<Sample>>=Mutex::new(None);
#[derive(Default)]struct Audit{calls:u64,accepted:u64,last:Option<(Instant,&'static str)>}
static AUDIT:Mutex<Audit>=Mutex::new(Audit{calls:0,accepted:0,last:None});
struct Observation(&'static str);
impl Drop for Observation{fn drop(&mut self){let mut a=AUDIT.lock().unwrap_or_else(|p|p.into_inner());a.calls=a.calls.saturating_add(1);if self.0=="accepted"{a.accepted=a.accepted.saturating_add(1);}a.last=Some((Instant::now(),self.0));}}
pub(super) fn invalidate(){*SAMPLE.lock().unwrap_or_else(|p|p.into_inner())=None;}
pub(super) fn revoke(serials:&[u64]){let mut sample=SAMPLE.lock().unwrap_or_else(|p|p.into_inner());if sample.as_ref().is_some_and(|s|serials.contains(&s.serial)){*sample=None;}}
pub(super) fn status()->serde_json::Value{let a=AUDIT.lock().unwrap_or_else(|p|p.into_inner());let audit=serde_json::json!({"producer_hooks":a.calls,"accepted":a.accepted,"last":a.last.map(|(at,gate)|serde_json::json!({"gate":gate,"age_ms":at.elapsed().as_millis()}))});drop(a);
    let sample=SAMPLE.lock().unwrap_or_else(|p|p.into_inner());serde_json::json!({"audit":audit,"sample":sample.as_ref().map(|s|serde_json::json!({"view_epoch":s.view.provenance_epoch,"camera_sequence":s.view.camera_sequence,"request_frame":s.view.request_frame,"head_frame":s.view.head_pose_frame,"scope_sequence":s.view.optics.scope_sequence,"age_ms":s.at.elapsed().as_millis()})),"firing":"native cached barrel; original correction and spread preserved","desired_pose_agreement_required":false})}
fn local_cached<M:Memory>(s:&mut Snapshot<'_,M>)->Option<(u64,u64)>{
    let factory=s.u64(20_159_720)?;if factory==0{return None;}let ped=s.u64(factory.checked_add(8)?)?;if ped==0{return None;}
    let manager=s.u64(ped.checked_add(4344)?)?;if manager==0{return None;}let physical=s.u64(manager.checked_add(120)?)?;if physical==0{return None;}
    let weapon=s.u64(physical.checked_add(864)?)?;if weapon==0||s.u64(weapon.checked_add(88)?)?!=physical{return None;}
    if s.u32((weapon.checked_add(491)?)&!3)?&(4<<24)==0{return None;}Some((weapon,physical))
}
fn cache_frame<M:Memory>(s:&mut Snapshot<'_,M>,weapon:u64)->Option<[f32;16]>{
    let mut q=[0.;4];for(i,v)in q.iter_mut().enumerate(){*v=f32::from_bits(s.u32(weapon.checked_add(16+i as u64*4)?)?);}
    let norm=q.iter().map(|v|v*v).sum::<f32>();if !norm.is_finite()||(norm-1.).abs()>0.001{return None;}
    let[x,y,z,w]=q;let mut m=[1.-2.*(y*y+z*z),2.*(x*y+z*w),2.*(x*z-y*w),0.,2.*(x*y-z*w),1.-2.*(x*x+z*z),2.*(y*z+x*w),0.,2.*(x*z+y*w),2.*(y*z-x*w),1.-2.*(x*x+y*y),0.,0.,0.,0.,1.];
    for i in 0..3{m[12+i]=f32::from_bits(s.u32(weapon.checked_add(32+i as u64*4)?)?);}rigid(&m).then_some(m)
}
fn actual_frame<M:Memory>(s:&mut Snapshot<'_,M>,expected:Option<(u64,u64)>)->Option<[f32;16]>{
    let(weapon,physical)=local_cached(s)?;if expected.is_some_and(|(w,_)|w!=weapon){return None;}
    let cache=cache_frame(s,weapon)?;let(world,_)=ordinary_muzzle_world(s,weapon,physical)?;
    if !close(&cache,&world){return None;}
    if let Some((_,temporary))=expected{if !close(&s.matrix(temporary)?,&world){return None;}}
    Some(cache)
}
fn primary_agrees<M:Memory>(s:&mut Snapshot<'_,M>,view:&crate::xr_view::WorldView)->bool{
    // Same active primary layout certified by patch5/xr_view. Canonicalize only
    // Mat34 padding; all12 actual camera coordinates must be bit-identical.
    let Some(source)=s.u64(19_237_856).filter(|p|*p!=0)else{return false;};
    let Some(camera)=source.checked_add(16+192).and_then(|p|s.matrix(p))else{return false;};
    (0..4).all(|c|(0..3).all(|r|camera[c*4+r].to_bits()==view.game_from_eye[c*4+r].to_bits()))
}
fn scope_ready()->bool{crate::xr_motion_aim::arms_enabled()&&crate::xr_view::world_camera_enabled()&&crate::pad::spatial_controls_selected()&&!crate::pad::IN_VEHICLE.load(Ordering::Relaxed)&&crate::pad::verified_guest_gameplay_context()}
pub(super) fn native_firing_fallback()->bool{
    if !scope_ready(){return false;}let Some(rt)=crate::RT.get()else{return false;};let guest=Guest(&rt.mem);let mut s=Snapshot::new(&guest);
    // All cached branches remain native. Missing controller/IK input cannot
    // starve the game's own firing path, and grants no reticle authority.
    local_cached(&mut s).is_some()&&s.unchanged()
}
fn compatible_views(sample:&crate::xr_view::WorldView,current:&crate::xr_view::WorldView)->bool{
    sample.headset_applied&&current.headset_applied&&sample.provenance_epoch!=0
        &&sample.provenance_epoch==current.provenance_epoch&&sample.camera_sequence!=0
        &&current.camera_sequence>=sample.camera_sequence&&current.optics.scope_sequence==sample.optics.scope_sequence
}
pub(super) fn current_cached_weapon_scope()->bool{
    if !scope_ready(){return false;}let Some(rt)=crate::RT.get()else{return false;};let guest=Guest(&rt.mem);let mut s=Snapshot::new(&guest);
    let ordinary=local_cached(&mut s).and_then(|(weapon,physical)|ordinary_muzzle_source(&mut s,weapon,physical));
    ordinary.is_some()&&s.unchanged()
}
pub(super) fn current_ray(epoch:u64)->Option<CurrentInputRay>{
    if !scope_ready(){return None;}let sample=SAMPLE.lock().unwrap_or_else(|p|p.into_inner()).clone()?;
    if sample.at.elapsed()>RAY_AGE||sample.view.provenance_epoch!=epoch{return None;}
    let primary=crate::xr_motion_aim::native_ray_primary()?;
    if !compatible_views(&sample.view,&primary){return None;}
    let rt=crate::RT.get()?;let guest=Guest(&rt.mem);let mut s=Snapshot::new(&guest);
    let frame=actual_frame(&mut s,None)?;
    if !close(&frame,&sample.frame)||!primary_agrees(&mut s,&primary)||!s.unchanged()
        ||crate::xr_motion_aim::native_ray_primary()!=Some(primary){return None;}
    Some(CurrentInputRay{origin:[frame[12],frame[13],frame[14]],direction:[frame[0],frame[1],frame[2]],source_age:sample.at.elapsed(),camera_sequence:sample.view.camera_sequence,native_cached:true})
}
pub(super) fn host_hook(caller:&mut wasmtime::Caller<'_,crate::HostState>,params:&[wasmtime::Val],results:&mut[wasmtime::Val],mem:&wasmtime::SharedMemory)->anyhow::Result<()>{
    use wasmtime::Val;
    anyhow::ensure!(params.len()==4&&results.is_empty(),"native cache import ABI mismatch");let mut observation=Observation("scope_or_tls");
    let(Val::I64(weapon),Val::I64(temporary),Val::I64(tls),Val::I32(first))=(&params[0],&params[1],&params[2],&params[3])else{anyhow::bail!("native cache argument type mismatch")};
    if !scope_ready()||*first!=1||*tls==0||crate::guest_call_scope::producer_store_lifetime(caller.data())==0{return Ok(());}
    //28266 allocates exactly64B at global SP into local3 for this muzzle.
    observation.0="original_stack_matrix_identity";if active_stack(caller)!=Some(*temporary as u64){return Ok(());}
    observation.0="fresh_primary_missing";let Some(primary)=crate::xr_motion_aim::native_ray_primary()else{return Ok(());};
    let guest=Guest(mem);let mut s=Snapshot::new(&guest);observation.0="cache_temporary_or_live_bone_disagree";
    let Some(frame)=actual_frame(&mut s,Some((*weapon as u64,*temporary as u64)))else{return Ok(());};
    observation.0="active_primary_or_snapshot_changed";if !primary_agrees(&mut s,&primary)||!s.unchanged()||crate::xr_motion_aim::native_ray_primary()!=Some(primary){return Ok(());}
    observation.0="producer_boundary_capacity";
    let Ok(serial)=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|v|v.checked_add(1))else{return Ok(());};
    let state=&mut caller.data_mut().motion_arms;state.cache_touched.retain(|x|x.2.elapsed()<=MAX_AGE);
    if state.cache_touched.len()>=512{return Ok(());}let Some(sequence)=state.sequence.checked_add(1)else{return Ok(());};state.sequence=sequence;state.cache_touched.push((sequence,serial,Instant::now()));
    *SAMPLE.lock().unwrap_or_else(|p|p.into_inner())=Some(Sample{serial,frame,view:primary,at:Instant::now()});observation.0="accepted";Ok(())
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn actual_cached_ray_requires_cache_original_temporary_and_current_bone_agreement(){
        let m=super::super::tests::native_fixture();let local=super::super::tests::add_cached_muzzle(&m);
        m.u64(20_159_720,64);m.u64(72,1024);let theta=0.7f32;
        for(i,v)in[0f32,0.,(theta/2.).sin(),(theta/2.).cos()].into_iter().enumerate(){m.put(24_016+i as u64*4,v.to_bits());}
        for i in 0..3{m.put(24_032+i as u64*4,local[12+i].to_bits());}m.matrix(40_000,&local);
        assert!(actual_frame(&mut Snapshot::new(&m),Some((24_000,40_000))).is_some());
        m.put(40_048,9f32.to_bits());assert!(actual_frame(&mut Snapshot::new(&m),Some((24_000,40_000))).is_none());m.matrix(40_000,&local);
        m.put(24_032,8f32.to_bits());assert!(actual_frame(&mut Snapshot::new(&m),None).is_none());assert!(ordinary_muzzle_source(&mut Snapshot::new(&m),24_000,22_000).is_some());m.put(24_032,local[12].to_bits());
        m.put(38_000+2*64+48,7f32.to_bits());assert!(actual_frame(&mut Snapshot::new(&m),None).is_none());
        m.u64(24_312,1);assert!(ordinary_muzzle_source(&mut Snapshot::new(&m),24_000,22_000).is_none());m.u64(24_312,0);m.u64(32_104,999);assert!(ordinary_muzzle_source(&mut Snapshot::new(&m),24_000,22_000).is_none());
    }
    fn view()->crate::xr_view::WorldView{crate::xr_view::WorldView{provenance_epoch:7,camera_sequence:11,request_frame:12,head_pose_frame:13,presentation_time_seconds:1.,eye_index:0,view_count:2,game_from_eye:identity(),game_from_stage:identity(),projection_from_eye:identity(),depth_reverse_z:false,headset_applied:true,optics:crate::xr_view::ScopeOptics::default()}}
    #[test]fn cached_ray_keeps_original_camera_generation_and_cannot_cross_epoch_scope_or_foreign_primary(){
        let sample=view();let mut next=sample;next.camera_sequence+=1;assert!(compatible_views(&sample,&next));next.optics.scope_sequence=1;assert!(!compatible_views(&sample,&next));next=sample;next.provenance_epoch+=1;assert!(!compatible_views(&sample,&next));next=sample;next.camera_sequence-=1;assert!(!compatible_views(&sample,&next));next=sample;next.headset_applied=false;assert!(!compatible_views(&sample,&next));
        let m=super::super::tests::native_fixture();m.u64(19_237_856,70_000);m.matrix(70_000+16+192,&identity());assert!(primary_agrees(&mut Snapshot::new(&m),&sample));let mut foreign=identity();foreign[12]=0.01;m.matrix(70_000+16+192,&foreign);assert!(!primary_agrees(&mut Snapshot::new(&m),&sample));
    }
    #[test]fn completed_nested_cache_producer_is_revoked_when_outer_guest_boundary_fails(){
        let mut state=StoreState::default();let outer=state.boundary_prefix();let child=state.boundary_prefix();let serial=u64::MAX-1;
        state.sequence+=1;state.cache_touched.push((state.sequence,serial,Instant::now()));*SAMPLE.lock().unwrap()=Some(Sample{serial,frame:identity(),view:view(),at:Instant::now()});
        state.finish_boundary(&child);assert!(!state.cache_touched.is_empty());state.recover_boundary(&outer);assert!(SAMPLE.lock().unwrap().is_none());assert!(state.cache_touched.is_empty());
    }
}

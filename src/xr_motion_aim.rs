//! Private, generation-scoped native aim inputs. This module never writes a
//! game camera, a calculated shot, or a render matrix. A producer must supply
//! the hand sample captured with the accepted original camera generation.
//!
//! The diagnostic AOT supplies invocation-owned 80-byte guest stack storage.
//! No guest address escapes its callback; calculated-shot acknowledgement is a
//! copy of the ORIGINAL successful CalculateFiringVector output, not an impact.

use std::time::{Duration, Instant};
use std::sync::{atomic::{AtomicBool,AtomicU64, Ordering}, LazyLock,Mutex};

pub const IMPORT_MODULE: &str = "muguet_motion_aim_v1";
static DIAGNOSTIC_ENABLED: AtomicBool = AtomicBool::new(false);
static CANDIDATE: Mutex<Option<CameraCandidate>> = Mutex::new(None);
static LEFT_CANDIDATE:Mutex<Option<LeftArmCandidate>>=Mutex::new(None);
// Actual primary propagation provenance is independent of controller tracking.
// Native cached rays are not desired IK targets and do not borrow a hand pose.
static NATIVE_RAY_PRIMARY:Mutex<Option<(crate::xr_view::WorldView,Instant)>>=Mutex::new(None);
static LATEST_CAMERA_SEQUENCE:AtomicU64=AtomicU64::new(0);
static LAST_ACK: Mutex<Option<ShotAcknowledgement>> = Mutex::new(None);
static NATIVE_MODE:Mutex<Option<(bool,Instant,u64,u64)>>=Mutex::new(None);
static IK_PERMIT:Mutex<Option<(u64,u64,Instant)>>=Mutex::new(None);
static FIRE_GATE:Mutex<FireGate>=Mutex::new(FireGate {release_required:true,was_motion:false});
// Value-only, process-lifetime evidence survives leaving immersion. These
// observations never grant a pose, refresh its age, or change a native gate.
#[derive(Default)]
struct CaptureAudit {attempts:u64,accepted:u64,invalidations:u64,last:Option<CaptureObservation>,last_headset:Option<CaptureObservation>}
type CaptureObservation=(Instant,&'static str,u64,u64,u64,Option<(u32,[u64;4],Option<f64>,Option<f64>)>);
static CAPTURE_AUDIT:Mutex<CaptureAudit>=Mutex::new(CaptureAudit{attempts:0,accepted:0,invalidations:0,last:None,last_headset:None});
static ARM_CANDIDATE_GATE:AtomicU64=AtomicU64::new(0);
impl CaptureAudit{fn record(&mut self,view:crate::xr_view::WorldView,reason:&'static str,observed:Option<(u32,[u64;4],Option<f64>,Option<f64>)>){
    self.attempts=self.attempts.saturating_add(1);if reason=="accepted"{self.accepted=self.accepted.saturating_add(1);}
    self.last=Some((Instant::now(),reason,view.provenance_epoch,view.camera_sequence,view.head_pose_frame,observed));
    if view.headset_applied{self.last_headset=self.last;}
}}
fn capture_observed(view:crate::xr_view::WorldView,reason:&'static str,packet:Option<(u32,[f64;3],[u64;4])>){
    let observed=packet.map(|(flags,times,ids)|(flags,ids,
        ((times[0]-times[2])*1000.).is_finite().then_some((times[0]-times[2])*1000.),
        ((times[0]-view.presentation_time_seconds)*1000.).is_finite().then_some((times[0]-view.presentation_time_seconds)*1000.)));
    CAPTURE_AUDIT.lock().unwrap_or_else(|p|p.into_inner()).record(view,reason,observed);
}
fn describe_capture_observation(observation:&CaptureObservation)->serde_json::Value{
    let(at,reason,epoch,camera,head,packet)=observation;
    serde_json::json!({"age_ms":at.elapsed().as_millis(),"decision":reason,"view_epoch":epoch,"camera_sequence":camera,"accepted_head_frame":head,
        "packet_flags":packet.map(|p|p.0),"packet_ids":packet.map(|p|p.1),"right_original_source_age_ms":packet.and_then(|p|p.2),"packet_presentation_delta_ms":packet.and_then(|p|p.3)})
}
fn packet_rejection(head:u64,presentation:f64,flags:u32,times:[f64;3],ids:[u64;4])->Option<&'static str>{
    if flags==0&&ids==[0;4]{return Some("no_exact_head_motion_packet");}
    if flags&2==0{return Some("no_fresh_right_accessory");}
    if flags&(1<<9)!=0{return Some("bare_right_wrist_is_not_accessory_aim");}
    if ids[0]!=1{return Some("motion_packet_schema");}if ids[1]==0{return Some("motion_tracking_epoch_missing");}
    if ids[2]!=head{return Some("exact_head_frame_mismatch");}if ids[3]==0{return Some("motion_packet_sequence_missing");}
    if !times.iter().all(|v|v.is_finite()){return Some("nonfinite_packet_time");}
    if (times[0]-presentation).abs()>1e-6{return Some("accepted_head_presentation_time_mismatch");}
    if times[2]>times[0]+0.01{return Some("right_source_in_future");}
    if times[0]-times[2]>0.15{return Some("right_source_stale_at_capture");}None
}
fn left_packet_valid(head:u64,presentation:f64,flags:u32,times:[f64;3],ids:[u64;4])->bool{
    flags&1!=0&&flags&(1<<8)==0&&ids[0]==1&&ids[1]!=0&&ids[2]==head&&head!=0&&ids[3]!=0
        &&times.iter().all(|v|v.is_finite())&&(times[0]-presentation).abs()<=1e-6
        &&times[1]<=times[0]+0.01&&times[0]-times[1]<=0.15
}
#[derive(Clone,Debug)]
pub(crate) struct LeftArmCandidate{pub view_epoch:u64,pub camera_sequence:u64,pub head_frame:u64,pub tracking_epoch:u64,pub hand_sequence:u64,pub game_from_stage:[f32;16],pub stage_from_grip:[f32;16],pub recorded_at:Instant,pub source_age:Duration}
impl LeftArmCandidate{fn valid(&self,now:Instant,epoch:u64)->bool{self.view_epoch==epoch&&epoch!=0&&self.camera_sequence!=0&&self.head_frame!=0&&self.tracking_epoch!=0&&self.hand_sequence!=0&&rigid(&self.game_from_stage)&&rigid(&self.stage_from_grip)&&now.checked_duration_since(self.recorded_at).and_then(|age|age.checked_add(self.source_age)).is_some_and(|age|age<=MAX_AGE)}}
#[derive(Clone,Copy)]
enum ArmModeAuthority{RecentAimRead,CurrentNativePredicate}
fn arm_mode_matches(first:bool,authority:ArmModeAuthority,history:Option<(bool,Instant,u64,u64)>,now:Instant,epoch:u64)->bool{
    first&&match authority{ArmModeAuthority::CurrentNativePredicate=>true,
        ArmModeAuthority::RecentAimRead=>history.is_some_and(|(first,at,_,e)|first&&e==epoch&&now.checked_duration_since(at).is_some_and(|age|age<=MAX_AGE))}
}
fn arm_mode_authorized(first:bool,authority:ArmModeAuthority)->bool{
    let history=match authority{ArmModeAuthority::RecentAimRead=>*NATIVE_MODE.lock().unwrap_or_else(|p|p.into_inner()),ArmModeAuthority::CurrentNativePredicate=>None};
    arm_mode_matches(first,authority,history,Instant::now(),crate::xr_view::view_epoch())
}
pub(crate) fn native_left_arm_candidate(first:bool)->Option<LeftArmCandidate>{native_left_arm_candidate_with_authority(first,ArmModeAuthority::RecentAimRead)}
/// Only the certified native arm request/target hooks may use their current
/// original45913(ped,0,0,0,1,0) result instead of another hook's mode history.
/// The caller still qualifies local owner, native stack and request identity.
pub(crate) fn native_left_arm_candidate_at_native_site(first:bool)->Option<LeftArmCandidate>{native_left_arm_candidate_with_authority(first,ArmModeAuthority::CurrentNativePredicate)}
fn native_left_arm_candidate_with_authority(first:bool,authority:ArmModeAuthority)->Option<LeftArmCandidate>{
    if !arms_enabled()||!current_supported_scope(first)||!arm_mode_authorized(first,authority){return None;}
    let c=LEFT_CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone()?;if !c.valid(Instant::now(),crate::xr_view::view_epoch())||LATEST_CAMERA_SEQUENCE.load(Ordering::Acquire)!=c.camera_sequence{return None;}
    #[cfg(target_os="visionos")]{
        extern "C"{fn sv_xr_motion_read_v1(frame:u64,f:*mut f32,fn_:u32,t:*mut f64,tn:u32,id:*mut u64,idn:u32)->u32;}
        let mut f=[0.;80];let mut t=[0.;3];let mut ids=[0;4];let flags=unsafe{sv_xr_motion_read_v1(c.head_frame,f.as_mut_ptr(),80,t.as_mut_ptr(),3,ids.as_mut_ptr(),4)};
        if flags&1==0||flags&(1<<8)!=0||ids!=[1,c.tracking_epoch,c.head_frame,c.hand_sequence]||!f[16..32].iter().zip(c.stage_from_grip).all(|(a,b)|a.to_bits()==b.to_bits()){return None;}Some(c)
    }
    #[cfg(not(target_os="visionos"))]{None}
}
pub fn diagnostic_enabled() -> bool { DIAGNOSTIC_ENABLED.load(Ordering::Acquire) }
static GAMEPLAY_REQUESTED:LazyLock<bool>=LazyLock::new(||std::env::var("MUGUET_XR_MOTION_AIM").as_deref()==Ok("1"));
fn resolve_arms_requested(runtime:Option<&str>,compiled_default:Option<&str>)->bool{
    match runtime{Some("1")=>true,Some(_)=>false,None=>compiled_default==Some("1")}
}
static ARMS_REQUESTED:LazyLock<bool>=LazyLock::new(||resolve_arms_requested(std::env::var("MUGUET_XR_MOTION_ARMS").ok().as_deref(),option_env!("MUGUET_XR_MOTION_ARMS_DEFAULT")));
pub fn arms_enabled()->bool{diagnostic_enabled()&&*ARMS_REQUESTED}
// First-person view-model correction is a render change with its own physical
// acceptance. It requires the certified separate AOT and its own launch intent.
static VIEWMODEL_REQUESTED:LazyLock<bool>=LazyLock::new(||resolve_arms_requested(std::env::var("MUGUET_XR_FP_VIEWMODEL").ok().as_deref(),option_env!("MUGUET_XR_FP_VIEWMODEL_DEFAULT")));
pub(crate) fn viewmodel_enabled()->bool{diagnostic_enabled()&&*VIEWMODEL_REQUESTED}
/// Swift acquisition follows the native certificate-qualified mode, including
/// an explicitly opted-in build's ordinary Home-screen launch without env vars.
#[no_mangle]
pub extern "C" fn sv_xr_motion_arms_enabled_v1()->u32{u32::from(arms_enabled())}
/// Certificate acceptance and gameplay activation are deliberately independent.
/// Cache only launch intent; an early status read cannot cache certificatefalse.
pub fn gameplay_enabled()->bool {diagnostic_enabled()&&(*GAMEPLAY_REQUESTED||*ARMS_REQUESTED)}
fn supported_motion_scope(first_person:bool,world_active:bool,spatial:bool,in_vehicle:bool,gameplay:bool)->bool {
    first_person&&world_active&&spatial&&!in_vehicle&&gameplay
}
fn current_supported_scope(first_person:bool)->bool {
    supported_motion_scope(first_person,crate::xr_view::world_camera_enabled(),crate::pad::spatial_controls_selected(),
        crate::pad::IN_VEHICLE.load(Ordering::Relaxed),crate::pad::verified_guest_gameplay_context())
}
/// Same accepted camera packet for native arm requests and targets. A later
/// callback cannot borrow a replacement tracking generation or extend its age.
pub(crate) fn native_arm_candidate(first_person:bool)->Option<CameraCandidate>{
    native_arm_candidate_with_authority(first_person,ArmModeAuthority::RecentAimRead)
}
/// Invocation-local mode authority, exclusively for the certified native arm
/// hooks. This does not publish NATIVE_MODE or grant firing/reticle authority.
pub(crate) fn native_arm_candidate_at_native_site(first_person:bool)->Option<CameraCandidate>{
    if !arms_enabled(){return None;}native_arm_candidate_with_authority(first_person,ArmModeAuthority::CurrentNativePredicate)
}
fn native_arm_candidate_with_authority(first_person:bool,authority:ArmModeAuthority)->Option<CameraCandidate>{
    let fail=|gate|{ARM_CANDIDATE_GATE.store(gate,Ordering::Relaxed);None};
    if !gameplay_enabled(){return fail(1);}if !current_supported_scope(first_person){return fail(2);}
    if !arm_mode_authorized(first_person,authority){return fail(3);}
    let Some(candidate)=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone()else{return fail(4);};
    if !candidate.valid(Instant::now(),crate::xr_view::view_epoch()){return fail(5);}
    if !tracking_still_live(&candidate){return fail(6);}ARM_CANDIDATE_GATE.store(7,Ordering::Relaxed);Some(candidate)
}
pub(crate) fn accept_native_arm_input(c:&CameraCandidate){
    if arms_enabled()&&c.valid(Instant::now(),crate::xr_view::view_epoch())&&tracking_still_live(c){
        *IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner())=Some((c.view_epoch,c.tracking_epoch,Instant::now()));
    }
}

#[derive(Clone,Copy)]
enum FireScope { Native, Motion {ready:bool} }
struct FireGate {release_required:bool,was_motion:bool}
impl FireGate {
    fn apply(&mut self,value:f32,scope:FireScope)->f32 {
        if matches!(scope,FireScope::Native) {self.release_required=true;self.was_motion=false;return value;}
        if !self.was_motion {self.release_required=true;self.was_motion=true;}
        if matches!(scope,FireScope::Motion{ready:false}) {self.release_required=true;return 0.;}
        if self.release_required {if value.is_finite()&&value<=0.2 {self.release_required=false;}return 0.;}
        value
    }
}
/// Called ONLY for spatial-controller on-foot gameplay fire in a motion AOT.
/// Window, menu, vehicle acceleration and the standard pad path bypass this.
pub(crate) fn filter_spatial_trigger(value:f32)->f32 {
    if !gameplay_enabled()||!crate::xr_view::world_camera_enabled(){return value;}
    if !crate::pad::spatial_controls_selected()||!crate::pad::verified_guest_gameplay_context()||crate::pad::IN_VEHICLE.load(Ordering::Relaxed){
        return FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).apply(value,FireScope::Native);
    }
    if crate::xr_motion_arms::native_fallback_active(){return FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).apply(value,FireScope::Native);}
    let now=Instant::now();let epoch=crate::xr_view::view_epoch();
    let mode=*NATIVE_MODE.lock().unwrap_or_else(|p|p.into_inner());
    let scope=match mode.filter(|(_,at,_,e)|*e==epoch&&now.duration_since(*at)<=MAX_AGE) {
        Some((false,_,_,_))=>FireScope::Native,
        _=>{
            let candidate=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone();
            let permit=*IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner());
            let ready=mode.is_some_and(|(first,at,_,e)|first&&e==epoch&&now.duration_since(at)<=MAX_AGE)
                &&candidate.as_ref().is_some_and(|c|c.valid(now,epoch)&&tracking_still_live(c)
                    &&(!arms_enabled()||crate::xr_motion_arms::camera_frame_for_candidate(c).is_some())
                    &&permit.is_some_and(|(e,tracking,at)|e==epoch&&tracking==c.tracking_epoch&&now.duration_since(at)<=MAX_AGE));
            FireScope::Motion{ready}
        }
    };
    FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).apply(value,scope)
}
fn motion_ready(candidate:&CameraCandidate,now:Instant,epoch:u64)->bool{
    let permit=*IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner());
    candidate.valid(now,epoch)&&tracking_still_live(candidate)
        &&(!arms_enabled()||crate::xr_motion_arms::camera_frame_for_candidate(candidate).is_some())
        &&permit.is_some_and(|(e,tracking,at)|e==epoch&&tracking==candidate.tracking_epoch&&now.duration_since(at)<=MAX_AGE)
        &&!FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).release_required
}

/// Only a separate host build pinned to the exact new serialized AOT can
/// activate the new imports. Normal signed AOT and normal patch5 stay unchanged.
pub(crate) fn qualify_embedded_aot(code: &[u8]) -> anyhow::Result<()> {
    let Some(expected)=option_env!("MUGUET_MOTION_AOT_SHA256") else { return Ok(()); };
    anyhow::ensure!(expected.len()==64&&expected.bytes().all(|b|b.is_ascii_hexdigit()&&!b.is_ascii_uppercase()),"invalid motion diagnostic AOT certificate");
    #[cfg(target_vendor="apple")]
    {
        anyhow::ensure!(code.len()<=u32::MAX as usize,"motion AOT is too large");
        extern "C" { fn CC_SHA256(p:*const std::ffi::c_void,n:u32,out:*mut u8)->*mut u8; }
        let mut digest=[0u8;32];unsafe{CC_SHA256(code.as_ptr().cast(),code.len() as u32,digest.as_mut_ptr());}
        let actual:String=digest.iter().map(|b|format!("{b:02x}")).collect();
        anyhow::ensure!(actual==expected,"embedded AOT does not match motion diagnostic certificate");
        DIAGNOSTIC_ENABLED.store(true,Ordering::Release);Ok(())
    }
    #[cfg(not(target_vendor="apple"))]
    {let _=code;anyhow::bail!("motion AOT certificate requires Apple host");}
}

pub(crate) fn invalidate_camera() {
    let mut audit=CAPTURE_AUDIT.lock().unwrap_or_else(|p|p.into_inner());audit.invalidations=audit.invalidations.saturating_add(1);drop(audit);
    crate::xr_motion_arms::invalidate_inputs();
    LATEST_CAMERA_SEQUENCE.store(0,Ordering::Release);
    *CANDIDATE.lock().unwrap_or_else(|p|p.into_inner())=None;
    *LEFT_CANDIDATE.lock().unwrap_or_else(|p|p.into_inner())=None;
    *NATIVE_RAY_PRIMARY.lock().unwrap_or_else(|p|p.into_inner())=None;
    *LAST_ACK.lock().unwrap_or_else(|p|p.into_inner())=None;
    *NATIVE_MODE.lock().unwrap_or_else(|p|p.into_inner())=None;
    *IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner())=None;
    FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).release_required=true;
}

/// Called after the qualified primary's native setters and accepted head pose
/// commit. The Swift ring must return the EXACT accepted head frame; no latest
/// hand snapshot is borrowed later by native IK/shot reads.
pub(crate) fn capture_camera(view: crate::xr_view::WorldView, base_camera:[f32;16]) {
    if !gameplay_enabled() { return; }
    if LATEST_CAMERA_SEQUENCE.fetch_max(view.camera_sequence,Ordering::AcqRel)>view.camera_sequence{capture_observed(view,"older_camera_callback",None);return;}
    {
        let mut candidate=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner());
        if LATEST_CAMERA_SEQUENCE.load(Ordering::Acquire)!=view.camera_sequence{capture_observed(view,"camera_replaced_before_capture",None);return;}
        *candidate=None;
    }
    *NATIVE_RAY_PRIMARY.lock().unwrap_or_else(|p|p.into_inner())=None;
    *LEFT_CANDIDATE.lock().unwrap_or_else(|p|p.into_inner())=None;
    if !view.headset_applied{capture_observed(view,"headset_not_applied_to_native_camera",None);return;}
    if !crate::xr_view::view_epoch_is_active(view.provenance_epoch){capture_observed(view,"inactive_view_epoch",None);return;}
    *NATIVE_RAY_PRIMARY.lock().unwrap_or_else(|p|p.into_inner())=Some((view,Instant::now()));
    #[cfg(target_os="visionos")]
    {
        extern "C" { fn sv_xr_motion_read_v1(expected_head_frame:u64,
            floats:*mut f32,float_count:u32,times:*mut f64,time_count:u32,ids:*mut u64,id_count:u32)->u32; }
        let mut floats=[0f32;80];let mut times=[0f64;3];let mut ids=[0u64;4];
        let flags=unsafe{sv_xr_motion_read_v1(view.head_pose_frame,floats.as_mut_ptr(),80,times.as_mut_ptr(),3,ids.as_mut_ptr(),4)};
        // Left position tracking has its own validity/source age. Losing the
        // right accessory must not discard an accepted left Sense grip packet.
        if left_packet_valid(view.head_pose_frame,view.presentation_time_seconds,flags,times,ids){
            let left=LeftArmCandidate{view_epoch:view.provenance_epoch,camera_sequence:view.camera_sequence,head_frame:view.head_pose_frame,tracking_epoch:ids[1],hand_sequence:ids[3],game_from_stage:view.game_from_stage,stage_from_grip:floats[16..32].try_into().unwrap(),recorded_at:Instant::now(),source_age:Duration::from_secs_f64((times[0]-times[1]).max(0.))};
            if left.valid(Instant::now(),view.provenance_epoch){let mut published=LEFT_CANDIDATE.lock().unwrap_or_else(|p|p.into_inner());if LATEST_CAMERA_SEQUENCE.load(Ordering::Acquire)==view.camera_sequence&&crate::xr_view::view_epoch_is_active(view.provenance_epoch){*published=Some(left);}}
        }
        // Right-handed first slice. Bare wrist poses do not constitute a
        // calibrated weapon/barrel ray. One missing left hand is harmless.
        if let Some(reason)=packet_rejection(view.head_pose_frame,view.presentation_time_seconds,flags,times,ids){capture_observed(view,reason,Some((flags,times,ids)));return;}
        let aim:[f32;16]=floats[64..80].try_into().unwrap();
        let candidate=CameraCandidate {view_epoch:view.provenance_epoch,camera_sequence:view.camera_sequence,
            head_frame:view.head_pose_frame,tracking_epoch:ids[1],hand_sequence:ids[3],
            original_game_from_camera:base_camera,game_from_stage:view.game_from_stage,
            stage_from_aim:aim,stage_from_grip:floats[48..64].try_into().unwrap(),
            left_grip:if flags&1!=0&&flags&(1<<8)==0&&times[1]<=times[0]+0.01&&times[0]-times[1]<=0.15 {
                Some((floats[16..32].try_into().unwrap(),Duration::from_secs_f64((times[0]-times[1]).max(0.))))
            }else{None},aim_from_weapon:identity(),recorded_at:Instant::now(),
            source_age_at_capture:Duration::from_secs_f64((times[0]-times[2]).max(0.))};
        if candidate.valid(Instant::now(),view.provenance_epoch) {
            let mut published=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner());
            if LATEST_CAMERA_SEQUENCE.load(Ordering::Acquire)==view.camera_sequence
                &&crate::xr_view::view_epoch_is_active(view.provenance_epoch){*published=Some(candidate);capture_observed(view,"accepted",Some((flags,times,ids)));}
            else{capture_observed(view,"camera_or_epoch_replaced_before_publication",Some((flags,times,ids)));}
        }else{capture_observed(view,"nonrigid_or_expired_candidate",Some((flags,times,ids)));}
    }
    #[cfg(not(target_os="visionos"))] {let _=base_camera;capture_observed(view,"not_visionos_host",None);}
}
pub(crate) fn native_ray_primary()->Option<crate::xr_view::WorldView>{
    if !arms_enabled()||!current_supported_scope(true){return None;}
    let(view,at)=(*NATIVE_RAY_PRIMARY.lock().unwrap_or_else(|p|p.into_inner()))?;
    (at.elapsed()<=Duration::from_millis(100)&&view.headset_applied&&view.camera_sequence!=0
        &&LATEST_CAMERA_SEQUENCE.load(Ordering::Acquire)==view.camera_sequence
        &&crate::xr_view::view_epoch_is_active(view.provenance_epoch)).then_some(view)
}

pub fn status_json()->serde_json::Value {
    let capture={let audit=CAPTURE_AUDIT.lock().unwrap_or_else(|p|p.into_inner());serde_json::json!({"attempts":audit.attempts,"accepted":audit.accepted,"camera_invalidations":audit.invalidations,
        "last":audit.last.as_ref().map(describe_capture_observation),"last_headset_applied":audit.last_headset.as_ref().map(describe_capture_observation)})};
    let epoch=crate::xr_view::view_epoch();let mode=*NATIVE_MODE.lock().unwrap_or_else(|p|p.into_inner());let permit=*IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner());
    let scope=serde_json::json!({"world_camera_active":crate::xr_view::world_camera_enabled(),"spatial_controls_selected":crate::pad::spatial_controls_selected(),"in_vehicle":crate::pad::IN_VEHICLE.load(Ordering::Relaxed),"verified_gameplay":crate::pad::verified_guest_gameplay_context(),
        "native_mode":mode.map(|(first,at,_,e)|serde_json::json!({"first_person":first,"age_ms":at.elapsed().as_millis(),"current_epoch":e==epoch,"fresh":e==epoch&&at.elapsed()<=MAX_AGE})),
        "ik_permit":permit.map(|(e,tracking,at)|serde_json::json!({"age_ms":at.elapsed().as_millis(),"tracking_epoch":tracking,"current_epoch":e==epoch,"fresh":e==epoch&&at.elapsed()<=MAX_AGE})),
        "last_arm_candidate_gate":match ARM_CANDIDATE_GATE.load(Ordering::Relaxed){0=>"not_observed",1=>"gameplay_motion_disabled",2=>"outside_supported_first_person_scope",3=>"native_first_person_mode_missing_or_stale",4=>"no_camera_candidate",5=>"camera_candidate_invalid_or_stale",6=>"exact_tracking_packet_no_longer_live",_=>"ready"}});
    let candidate=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner());
    let left_candidate=LEFT_CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone();
    let ack=LAST_ACK.lock().unwrap_or_else(|p|p.into_inner());
    serde_json::json!({"diagnostic_aot_certified":diagnostic_enabled(),"gameplay_motion_active":gameplay_enabled(),"native_arms":crate::xr_motion_arms::status_json(),
        "compiled_arms_default":option_env!("MUGUET_XR_MOTION_ARMS_DEFAULT")==Some("1"),"arms_launch_requested":*ARMS_REQUESTED,
        "capture_diagnostics":capture,"scope_diagnostics":scope,
        "left_candidate":left_candidate.as_ref().map(|c|serde_json::json!({"view_epoch":c.view_epoch,"camera_sequence":c.camera_sequence,"head_frame":c.head_frame,"hand_sequence":c.hand_sequence,"tracking_epoch":c.tracking_epoch,"age_ms":c.recorded_at.elapsed().as_millis(),"source_age_ms":c.source_age.as_millis(),"valid_now":c.valid(Instant::now(),epoch),"requires_right_tracking":false})),
        "candidate":candidate.as_ref().map(|c|serde_json::json!({"view_epoch":c.view_epoch,
            "camera_sequence":c.camera_sequence,"head_frame":c.head_frame,"hand_sequence":c.hand_sequence,
            "tracking_epoch":c.tracking_epoch,"age_ms":c.recorded_at.elapsed().as_millis(),"original_source_age_ms":c.source_age_at_capture.as_millis(),"valid_now":c.valid(Instant::now(),epoch)})),
        "historical_last_successful_native_calculation":ack.as_ref().map(|a|serde_json::json!({"token":a.token,"ped":a.ped,
            "task":a.task,"origin":a.origin,"target":a.target,"motion_input_generation":a.motion_generation,
            "age_ms":a.recorded_at.elapsed().as_millis(),"current_aim_or_impact":false})),
        "scope":"right-hand native aim-input diagnostic; not impact, gun-bone or hand interaction certification"})
}

pub(crate) fn host_hook(caller:&mut wasmtime::Caller<'_,crate::HostState>,name:&str,
    params:&[wasmtime::Val],results:&mut[wasmtime::Val],mem:&wasmtime::SharedMemory)->anyhow::Result<()> {
    use wasmtime::Val;
    anyhow::ensure!(diagnostic_enabled(),"motion imports require matching separately compiled AOT certificate");
    if !gameplay_enabled(){return inactive_hook(name,params,results);}
    let arg=|index:usize|->anyhow::Result<u64>{match params.get(index){Some(Val::I64(v))=>Ok(*v as u64),Some(Val::I32(v))=>Ok(*v as u32 as u64),_=>anyhow::bail!("motion hook argument absent/type mismatch")}};
    let make_identity=|tls:u64|Identity{store:crate::guest_call_scope::producer_store_lifetime(caller.data()),pthread:caller.data().pthread,tls};
    match name {
        "private_frame"=>{
            anyhow::ensure!(params.len()==6&&results.len()==1,"motion frame import ABI mismatch");
            let (site,ped,original,scratch,tls,first_person)=(arg(0)?,arg(1)?,arg(2)?,arg(3)?,arg(4)?,arg(5)?!=0);
            results[0]=Val::I64(original as i64);
            let Some(site)=ReadSite::from_u32(site as u32) else { return Ok(()); };
            let identity=make_identity(tls);
            caller.data_mut().motion_aim.read_started(site);
            let live_local=load64(mem,20_159_720).and_then(|factory|factory.checked_add(8))
                .and_then(|address|load64(mem,address)).unwrap_or(0);
            if ped==0||ped!=live_local { return Ok(()); }
            *NATIVE_MODE.lock().unwrap_or_else(|p|p.into_inner())=Some((first_person,Instant::now(),ped,crate::xr_view::view_epoch()));
            if !first_person {
                *IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner())=None;
                caller.data_mut().motion_aim.invalidate_inputs();return Ok(());
            }
            if !current_supported_scope(first_person){caller.data_mut().motion_aim.invalidate_inputs();return Ok(());}
            if crate::xr_motion_arms::native_fallback_active(){return Ok(());}
            // Full arm requests own the hand target. The old GunIk input would
            // additionally rotate the torso toward the controller's distant
            // point and fight that independent pose.
            if arms_enabled()&&site==ReadSite::GunIk{return Ok(());}
            let current_sp=caller.get_export("__muguet_motion_stack_pointer").and_then(|e|e.into_global())
                .and_then(|g|g.get(&mut *caller).i64()).map(|v|v as u64);
            // The SP global is intentionally not normally exported. The
            // diagnostic factory exports only this existing nonshared global.
            if current_sp.and_then(|sp|sp.checked_add(16))!=Some(scratch)
                || scratch.checked_add(FRAME_BYTES as u64).is_none_or(|end|end>mem.data_size() as u64)
                || ranges_overlap(original,FRAME_BYTES as u64,scratch,FRAME_BYTES as u64) { return Ok(()); }
            let Some(frame)=load_frame(mem,original) else { return Ok(()); };
            let candidate=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone();
            let Some(candidate)=candidate else { return Ok(()); };
            if !crate::pad::verified_guest_gameplay_context() || !tracking_still_live(&candidate) { return Ok(()); }
            // The injected hook carries the actual native query result; false
            // was observed above and returned the original pointer unchanged.
            let private=caller.data_mut().motion_aim.read(identity,ped,live_local,true,site,frame,
                &candidate,Instant::now(),crate::xr_view::view_epoch());
            if let Some(private)=private {
                // Re-read selected source words before committing the private
                // copy. A concurrent changed native camera fails through.
                if load_frame(mem,original)==Some(frame)
                    && crate::xr_view::view_epoch_is_active(candidate.view_epoch)
                    && store_frame(mem,scratch,&private) {
                    caller.data_mut().motion_aim.mark_applied(identity,ped,site);
                    if site==ReadSite::GunIk {
                        *IK_PERMIT.lock().unwrap_or_else(|p|p.into_inner())=Some((candidate.view_epoch,candidate.tracking_epoch,Instant::now()));
                    }
                    results[0]=Val::I64(scratch as i64);
                }
            }
        },
        "calculation_begin"=>{
            anyhow::ensure!(params.len()==5&&results.len()==1,"motion begin import ABI mismatch");
            let (task,ped,origin,target,tls)=(arg(0)?,arg(1)?,arg(2)?,arg(3)?,arg(4)?);
            let identity=make_identity(tls);
            let live_local=load64(mem,20_159_720).and_then(|factory|factory.checked_add(8))
                .and_then(|address|load64(mem,address)).unwrap_or(0);
            let valid=ped!=0&&ped==live_local&&load64(mem,task)==Some(5_968_184);
            let token=if valid{caller.data_mut().motion_aim.begin(identity,task,ped,origin,target)}else{0};
            results[0]=Val::I64(token as i64);
        },
        "calculation_complete"=>{
            anyhow::ensure!(params.len()==7&&results.is_empty(),"motion complete import ABI mismatch");
            let (token,task,ped,origin,target,tls,success)=(arg(0)?,arg(1)?,arg(2)?,arg(3)?,arg(4)?,arg(5)?,arg(6)?!=0);
            if token==0{return Ok(());}
            let identity=make_identity(tls);
            let vector=|address:u64|->Option<[f32;3]>{Some([f32::from_bits(load32(mem,address)?),
                f32::from_bits(load32(mem,address.checked_add(4)?)?),f32::from_bits(load32(mem,address.checked_add(8)?)?)])};
            let origin_value=vector(origin).unwrap_or([f32::NAN;3]);let target_value=vector(target).unwrap_or([f32::NAN;3]);
            if let Some(ack)=caller.data_mut().motion_aim.complete(token,identity,task,ped,origin,target,success,origin_value,target_value){
                *LAST_ACK.lock().unwrap_or_else(|p|p.into_inner())=Some(ack);
            }
        },
        "allow_fire"=>{
            anyhow::ensure!(params.len()==5&&results.len()==1,"motion fire gate ABI mismatch");
            let (phase,task,ped,tls,first_person)=(arg(0)?,arg(1)?,arg(2)?,arg(3)?,arg(4)?!=0);
            anyhow::ensure!(phase<=1,"motion fire gate phase unknown");
            let identity=make_identity(tls);let epoch=crate::xr_view::view_epoch();
            let scoped=current_supported_scope(first_person)&&!crate::xr_motion_arms::native_fallback_active();
            let candidate=CANDIDATE.lock().unwrap_or_else(|p|p.into_inner()).clone();
            let valid=scoped&&candidate.as_ref().is_some_and(|c|motion_ready(c,Instant::now(),epoch));
            let allowed=if phase==0{
                caller.data_mut().motion_aim.fire_attempt=None;
                if !scoped{true}else if valid{
                    let state=&mut caller.data_mut().motion_aim;
                    state.fire_attempt=Some((identity,task,ped,epoch,state.sequence));true
                }else{FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).release_required=true;false}
            }else{
                let state=&mut caller.data_mut().motion_aim;
                if let Some((owner,expected_task,expected_ped,expected_epoch,floor))=state.fire_attempt.take(){
                    valid&&owner==identity&&task==expected_task&&ped==expected_ped&&epoch==expected_epoch
                        &&state.latest_ack.as_ref().is_some_and(|ack|ack.token>floor&&ack.task==task&&ack.ped==ped
                            &&candidate.as_ref().is_some_and(|c|ack.motion_generation==Some((c.view_epoch,c.camera_sequence,c.hand_sequence))))
                }else{!scoped}
            };
            if !allowed{FIRE_GATE.lock().unwrap_or_else(|p|p.into_inner()).release_required=true;}
            results[0]=Val::I32(i32::from(allowed));
        },
        _=>anyhow::bail!("unknown motion diagnostic import {name}"),
    }Ok(())
}
fn inactive_hook(name:&str,params:&[wasmtime::Val],results:&mut[wasmtime::Val])->anyhow::Result<()> {
    use wasmtime::Val;
    match name {
        "private_frame"=>{
            anyhow::ensure!(params.len()==6&&results.len()==1,"inactive motion frame ABI mismatch");
            let Some(Val::I64(original))=params.get(2) else{anyhow::bail!("inactive motion frame pointer ABI mismatch")};
            results[0]=Val::I64(*original);
        },
        "calculation_begin"=>{anyhow::ensure!(params.len()==5&&results.len()==1,"inactive motion begin ABI mismatch");results[0]=Val::I64(0);},
        "calculation_complete"=>{anyhow::ensure!(params.len()==7&&results.is_empty(),"inactive motion completion ABI mismatch");},
        "allow_fire"=>{anyhow::ensure!(params.len()==5&&results.len()==1,"inactive motion fire ABI mismatch");results[0]=Val::I32(1);},
        _=>anyhow::bail!("unknown inactive motion diagnostic import {name}"),
    }Ok(())
}
fn ranges_overlap(a:u64,an:u64,b:u64,bn:u64)->bool {
    a.checked_add(an).zip(b.checked_add(bn)).is_none_or(|(ae,be)|a<be&&b<ae)
}
/// Revalidate the SAME immutable source packet so transport invalidation/loss is
/// immediately visible. Never borrow a replacement pose or restamp its age.
fn tracking_still_live(candidate:&CameraCandidate)->bool {
    #[cfg(target_os="visionos")]
    {
        extern "C" { fn sv_xr_motion_read_v1(expected_head_frame:u64,
            floats:*mut f32,float_count:u32,times:*mut f64,time_count:u32,ids:*mut u64,id_count:u32)->u32; }
        let mut floats=[0f32;80];let mut times=[0f64;3];let mut ids=[0u64;4];
        let flags=unsafe{sv_xr_motion_read_v1(candidate.head_frame,floats.as_mut_ptr(),80,times.as_mut_ptr(),3,ids.as_mut_ptr(),4)};
        flags&2!=0&&flags&(1<<9)==0&&ids==[1,candidate.tracking_epoch,candidate.head_frame,candidate.hand_sequence]
            &&floats[64..80].iter().zip(candidate.stage_from_aim).all(|(a,b)|a.to_bits()==b.to_bits())
            &&floats[48..64].iter().zip(candidate.stage_from_grip).all(|(a,b)|a.to_bits()==b.to_bits())
    }
    #[cfg(not(target_os="visionos"))] {let _=candidate;false}
}
fn load32(mem:&wasmtime::SharedMemory,address:u64)->Option<u32>{
    let offset=usize::try_from(address).ok()?;let (base,len)=crate::mem_base_len(mem);
    if offset%4!=0||offset.checked_add(4)?>len{return None;}
    Some(unsafe{&*base.add(offset).cast::<std::sync::atomic::AtomicU32>()}.load(Ordering::Relaxed))
}
fn load64(mem:&wasmtime::SharedMemory,address:u64)->Option<u64>{
    let offset=usize::try_from(address).ok()?;let (base,len)=crate::mem_base_len(mem);
    if offset%8!=0||offset.checked_add(8)?>len{return None;}
    Some(unsafe{&*base.add(offset).cast::<std::sync::atomic::AtomicU64>()}.load(Ordering::Relaxed))
}
fn load_frame(mem:&wasmtime::SharedMemory,address:u64)->Option<[u8;FRAME_BYTES]>{
    address.checked_add(FRAME_BYTES as u64)?;let mut frame=[0u8;FRAME_BYTES];
    for word in 0..FRAME_BYTES/4{frame[word*4..word*4+4].copy_from_slice(&load32(mem,address+word as u64*4)?.to_le_bytes());}Some(frame)
}
fn store_frame(mem:&wasmtime::SharedMemory,address:u64,frame:&[u8;FRAME_BYTES])->bool{
    let Ok(offset)=usize::try_from(address) else{return false;};let (base,len)=crate::mem_base_len(mem);
    if offset%16!=0||offset.checked_add(FRAME_BYTES).is_none_or(|end|end>len){return false;}
    for word in 0..FRAME_BYTES/4{let value=u32::from_le_bytes(frame[word*4..word*4+4].try_into().unwrap());
        unsafe{&*base.add(offset+word*4).cast::<std::sync::atomic::AtomicU32>()}.store(value,Ordering::Relaxed);}
    true
}

pub const FRAME_BYTES: usize = 80;
const MAX_AGE: Duration = Duration::from_millis(150);
const MAX_NESTING: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub store: u64,
    pub pthread: u64,
    pub tls: u64,
}
impl Identity {
    fn known(self) -> bool { self.store != 0 && self.pthread != 0 && self.tls != 0 }
}

/// Minted by qualified primary-camera propagation, never by an eye, reticle or
/// a later polling callback. All matrices are column-major rigid transforms.
#[derive(Clone, Debug)]
pub struct CameraCandidate {
    pub view_epoch: u64,
    pub camera_sequence: u64,
    pub head_frame: u64,
    pub tracking_epoch: u64,
    pub hand_sequence: u64,
    pub original_game_from_camera: [f32; 16],
    pub game_from_stage: [f32; 16],
    pub stage_from_aim: [f32; 16],
    pub stage_from_grip: [f32;16],
    pub left_grip:Option<([f32;16],Duration)>,
    /// Calibrated aim-location -> native weapon frame, not grip == muzzle.
    pub aim_from_weapon: [f32; 16],
    pub recorded_at: Instant,
    pub source_age_at_capture: Duration,
}
impl CameraCandidate {
    pub fn valid(&self, now: Instant, epoch: u64) -> bool {
        self.view_epoch == epoch && epoch != 0 && self.camera_sequence != 0
            && self.head_frame != 0 && self.tracking_epoch != 0 && self.hand_sequence != 0
            && now.checked_duration_since(self.recorded_at).and_then(|age|age.checked_add(self.source_age_at_capture))
                .is_some_and(|age| age <= MAX_AGE)
            && [&self.original_game_from_camera, &self.game_from_stage,
                &self.stage_from_aim, &self.stage_from_grip, &self.aim_from_weapon].into_iter().all(|m| rigid(m))
    }
    fn weapon(&self) -> [f32; 16] {
        multiply(&multiply(&self.game_from_stage, &self.stage_from_aim), &self.aim_from_weapon)
    }
}
pub(crate) fn left_grip_for_candidate(c:&CameraCandidate)->Option<[f32;16]>{
    let(grip,source_age)=c.left_grip?;
    if !rigid(&grip)||c.recorded_at.elapsed().checked_add(source_age)?>MAX_AGE{return None;}
    #[cfg(target_os="visionos")]{
        extern "C"{fn sv_xr_motion_read_v1(frame:u64,f:*mut f32,fn_:u32,t:*mut f64,tn:u32,id:*mut u64,idn:u32)->u32;}
        let mut f=[0.;80];let mut t=[0.;3];let mut id=[0;4];
        let flags=unsafe{sv_xr_motion_read_v1(c.head_frame,f.as_mut_ptr(),80,t.as_mut_ptr(),3,id.as_mut_ptr(),4)};
        if flags&1==0||flags&(1<<8)!=0||id!=[1,c.tracking_epoch,c.head_frame,c.hand_sequence]
            ||!f[16..32].iter().zip(grip).all(|(a,b)|a.to_bits()==b.to_bits()){return None;}Some(grip)
    }
    #[cfg(not(target_os="visionos"))]{None}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadSite { GunIk = 1, FireInitial = 2, FireDominant = 3 }
impl ReadSite {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value { 1 => Some(Self::GunIk), 2 => Some(Self::FireInitial), 3 => Some(Self::FireDominant), _ => None }
    }
}

#[derive(Clone, Debug)]
struct Lease {
    identity: Identity,
    ped: u64,
    candidate: CameraCandidate,
    original_frame: [u8; FRAME_BYTES],
    private_frame: [u8; FRAME_BYTES],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShotAcknowledgement {
    pub token: u64,
    pub ped: u64,
    pub task: u64,
    pub origin: [f32; 3],
    pub target: [f32; 3],
    /// None means native output was observed without a qualified hand read.
    pub motion_generation: Option<(u64, u64, u64)>,
    pub recorded_at:Instant,
}
#[derive(Clone, Debug)]
struct Calculation {
    token: u64, identity: Identity, task: u64, ped: u64,
    origin_pointer: u64, target_pointer: u64,
    motion_generation: Option<(u64, u64, u64)>,
    source_generation: Option<(u64, u64, u64)>,
    arm_frame:Option<[f32;16]>,
}
impl Calculation{
    fn arm_frame_for_read(&mut self,site:ReadSite,sample:impl FnOnce()->Option<[f32;16]>)->Option<[f32;16]>{
        match site{ReadSite::FireInitial=>{let frame=sample()?;self.arm_frame=Some(frame);Some(frame)},ReadSite::FireDominant=>self.arm_frame,ReadSite::GunIk=>None}
    }
}

#[derive(Default)]
pub struct StoreState {
    lease: Option<Lease>,
    calculations: Vec<Calculation>,
    sequence: u64,
    pub latest_ack: Option<ShotAcknowledgement>,
    fire_attempt:Option<(Identity,u64,u64,u64,u64)>,
}
impl StoreState {
    pub fn clear(&mut self) { self.lease = None; self.calculations.clear(); self.latest_ack = None;self.fire_attempt=None; }
    fn invalidate_inputs(&mut self){self.lease=None;for c in &mut self.calculations{c.motion_generation=None;c.source_generation=None;}}
    /// Caller stores the prefix before entering guest code. On Err it discards
    /// child calculations before invoke catches LongJmp / restores its stack.
    pub fn boundary_prefix(&self) -> Vec<u64> { self.calculations.iter().map(|c| c.token).collect() }
    pub fn recover_boundary(&mut self, prefix: &[u64]) {
        self.fire_attempt=None;
        if self.calculations.iter().take(prefix.len()).map(|c| c.token).eq(prefix.iter().copied())
            && self.calculations.len() >= prefix.len() { self.calculations.truncate(prefix.len()); }
        else { self.clear(); }
    }

    pub fn begin(&mut self, identity: Identity, task: u64, ped: u64,
        origin_pointer: u64, target_pointer: u64) -> u64 {
        if !identity.known() || task == 0 || ped == 0 || origin_pointer == 0 || target_pointer == 0
            || self.calculations.len() >= MAX_NESTING { self.clear(); return 0; }
        let Some(token) = self.sequence.checked_add(1) else { self.clear(); return 0; };
        self.sequence = token;
        self.calculations.push(Calculation { token, identity, task, ped, origin_pointer,
            target_pointer, motion_generation: None, source_generation:None,arm_frame:None });
        token
    }

    /// Same-call gates are native local-ped pointer equality plus the original
    /// first-person predicate (0,0,0,1,0), not a UI-selected camera mode.
    /// Exact matrix comparison binds the selected aim-frame alternative to the
    /// accepted original camera. A blend/recoil mismatch fails through.
    pub fn read(&mut self, identity: Identity, ped: u64, live_local_ped: u64,
        native_first_person: bool, site: ReadSite, frame: [u8; FRAME_BYTES],
        candidate: &CameraCandidate, now: Instant, active_epoch: u64) -> Option<[u8; FRAME_BYTES]> {
        if !identity.known() || ped == 0 || ped != live_local_ped || !native_first_person
            || !candidate.valid(now, active_epoch)
            || !frame_matches_camera(&frame, &candidate.original_game_from_camera) {
            self.lease = None; return None;
        }
        let same = self.lease.as_ref().is_some_and(|lease| lease.identity == identity && lease.ped == ped
            && lease.candidate.view_epoch == candidate.view_epoch
            && lease.candidate.camera_sequence == candidate.camera_sequence
            && lease.candidate.head_frame == candidate.head_frame
            && lease.candidate.tracking_epoch == candidate.tracking_epoch
            && lease.original_frame == frame);
        if !same || arms_enabled() {
            let mut private_frame = frame;
            let weapon=if arms_enabled(){
                let calculation=self.calculations.last_mut()?;
                calculation.arm_frame_for_read(site,||crate::xr_motion_arms::camera_frame_for_candidate(candidate))?
            }else{candidate.weapon()};
            write_native_frame(&mut private_frame, &weapon);
            self.lease = Some(Lease { identity, ped, candidate: candidate.clone(), original_frame: frame, private_frame });
        }
        let lease = self.lease.as_ref()?;
        if !lease.candidate.valid(now,active_epoch) { return None; }
        let generation=(lease.candidate.view_epoch,lease.candidate.camera_sequence,lease.candidate.hand_sequence);
        if site!=ReadSite::GunIk && self.calculations.last().is_some_and(|c|
            c.source_generation.is_some_and(|expected|expected!=generation)) { return None; }
        if site != ReadSite::GunIk && !self.calculations.last()
            .is_some_and(|c|c.identity==identity&&c.ped==ped) { return None; }
        Some(lease.private_frame)
    }

    fn mark_applied(&mut self,identity:Identity,ped:u64,site:ReadSite) {
        if site==ReadSite::GunIk { return; }
        if let Some((lease,calculation))=self.lease.as_ref().zip(self.calculations.last_mut()) {
            if lease.identity==identity&&lease.ped==ped&&calculation.identity==identity&&calculation.ped==ped {
                calculation.motion_generation=Some((lease.candidate.view_epoch,
                    lease.candidate.camera_sequence,lease.candidate.hand_sequence));
                calculation.source_generation=calculation.motion_generation;
            }
        }
    }
    fn read_started(&mut self,site:ReadSite){
        if site!=ReadSite::GunIk{if let Some(c)=self.calculations.last_mut(){c.motion_generation=None;}}
    }

    /// Copy only. No output pointer survives this call and no corrected native
    /// origin/target is overwritten. Success is the native Boolean unchanged.
    pub fn complete(&mut self, token: u64, identity: Identity, task: u64, ped: u64,
        origin_pointer: u64, target_pointer: u64, success: bool,
        origin: [f32; 3], target: [f32; 3]) -> Option<ShotAcknowledgement> {
        let calculation = self.calculations.pop()?;
        if token == 0 || calculation.token != token || calculation.identity != identity
            || calculation.task != task || calculation.ped != ped
            || calculation.origin_pointer != origin_pointer || calculation.target_pointer != target_pointer {
            self.clear(); return None;
        }
        if !success || !origin.into_iter().chain(target).all(f32::is_finite)
            || origin == target { return None; }
        let ack = ShotAcknowledgement { token, ped, task, origin, target,
            motion_generation: calculation.motion_generation,recorded_at:Instant::now() };
        self.latest_ack = Some(ack.clone()); Some(ack)
    }
}

fn component(frame: &[u8; FRAME_BYTES], offset: usize) -> f32 {
    f32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap())
}
/// Native camFrame stores right/forward/up/position at16/32/48/64. A graphics
/// camera stores right/up/back/position. The first16 bytes are opaque/preserved.
pub fn frame_matches_camera(frame: &[u8; FRAME_BYTES], camera: &[f32; 16]) -> bool {
    if !rigid(camera) { return false; }
    for (offset, column, sign) in [(16,0,1.),(32,2,-1.),(48,1,1.),(64,3,1.)] {
        for row in 0..3 {
            let value = component(frame, offset + row * 4);
            let expected = sign * camera[column * 4 + row];
            // The conversion is only permutation/sign, so demand exact numeric
            // equality (signed zeros compare equal); no freshness/tolerance gate.
            if !value.is_finite() || value != expected { return false; }
        }
    }
    true
}
fn write_native_frame(frame: &mut [u8; FRAME_BYTES], weapon: &[f32; 16]) {
    for (offset,column,sign) in [(16,0,1.),(32,2,-1.),(48,1,1.),(64,3,1.)] {
        for row in 0..3 { frame[offset + row*4..offset + row*4 + 4]
            .copy_from_slice(&(sign * weapon[column*4 + row]).to_le_bytes()); }
    }
}
pub fn identity() -> [f32; 16] { [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.] }
pub fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.;16];
    for c in 0..4 { for r in 0..4 { out[c*4+r] = (0..4).map(|k| a[k*4+r]*b[c*4+k]).sum(); } } out
}
pub fn rigid(m: &[f32; 16]) -> bool {
    if !m.iter().all(|x| x.is_finite()) || m[3].abs()>1e-5 || m[7].abs()>1e-5
        || m[11].abs()>1e-5 || (m[15]-1.).abs()>1e-5 { return false; }
    for a in 0..3 { for b in 0..3 {
        let dot: f32 = (0..3).map(|r| m[a*4+r]*m[b*4+r]).sum();
        if (dot - if a==b {1.} else {0.}).abs()>1e-3 { return false; }
    }}
    let determinant=m[0]*(m[5]*m[10]-m[9]*m[6])-m[4]*(m[1]*m[10]-m[9]*m[2])+m[8]*(m[1]*m[6]-m[5]*m[2]);
    (determinant-1.).abs()<1e-3
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn normal_launch_arm_default_is_opt_in_and_runtime_overrides_are_explicit(){
        for default in [None,Some("0"),Some("unexpected")]{assert!(!resolve_arms_requested(None,default));assert!(resolve_arms_requested(Some("1"),default));}
        assert!(resolve_arms_requested(None,Some("1")));assert!(!resolve_arms_requested(Some("0"),Some("1")));
        assert!(resolve_arms_requested(Some("1"),Some("1")));assert!(!resolve_arms_requested(Some("0"),None));
        assert!(!resolve_arms_requested(Some("unexpected"),Some("1")));assert!(!resolve_arms_requested(Some("unexpected"),None));
    }
    #[test]fn capture_diagnostics_distinguish_missing_exact_packet_from_stale_source(){
        let ids=[1,4,12,13];let times=[100.,99.99,99.98];
        assert_eq!(packet_rejection(12,100.,3,times,ids),None);
        assert_eq!(packet_rejection(12,100.,0,[0.;3],[0;4]),Some("no_exact_head_motion_packet"));
        assert_eq!(packet_rejection(12,100.,1,times,ids),Some("no_fresh_right_accessory"));
        assert_eq!(packet_rejection(12,100.,514,times,ids),Some("bare_right_wrist_is_not_accessory_aim"));
        assert_eq!(packet_rejection(13,100.,3,times,ids),Some("exact_head_frame_mismatch"));
        assert_eq!(packet_rejection(12,100.01,3,times,ids),Some("accepted_head_presentation_time_mismatch"));
        assert_eq!(packet_rejection(12,100.,3,[100.,99.99,99.8],ids),Some("right_source_stale_at_capture"));
        assert_eq!(packet_rejection(12,100.,3,[100.,99.99,100.02],ids),Some("right_source_in_future"));
    }
    #[test]fn capture_diagnostics_retain_immersive_failure_after_return_to_window(){
        let mut audit=CaptureAudit::default();let mut view=crate::xr_view::WorldView{provenance_epoch:7,camera_sequence:11,request_frame:1,head_pose_frame:12,presentation_time_seconds:100.,eye_index:0,view_count:2,
            game_from_eye:identity(),game_from_stage:identity(),projection_from_eye:identity(),depth_reverse_z:false,headset_applied:true, optics:crate::xr_view::ScopeOptics::default()};
        audit.record(view,"accepted",Some((3,[1,4,12,13],Some(20.),Some(0.))));
        audit.record(view,"no_fresh_right_accessory",Some((0,[0;4],Some(0.),Some(-100000.))));
        view.headset_applied=false;view.head_pose_frame=0;
        audit.record(view,"headset_not_applied_to_native_camera",None);
        assert_eq!((audit.attempts,audit.accepted),(3,1));assert_eq!(audit.last.unwrap().1,"headset_not_applied_to_native_camera");
        let last=audit.last_headset.unwrap();assert_eq!((last.1,last.4),("no_fresh_right_accessory",12));assert_eq!(last.5.unwrap().0,0);
    }
    fn id() -> Identity { Identity {store:1,pthread:2,tls:3} }
    fn candidate(now: Instant) -> CameraCandidate {
        let mut aim=identity();aim[12]=2.;aim[13]=3.;aim[14]=4.;
        CameraCandidate {view_epoch:7,camera_sequence:11,head_frame:12,tracking_epoch:4,hand_sequence:13,
            original_game_from_camera:identity(),game_from_stage:identity(),stage_from_aim:aim,
            stage_from_grip:identity(),left_grip:None,aim_from_weapon:identity(),recorded_at:now,source_age_at_capture:Duration::ZERO}
    }
    fn frame() -> [u8; FRAME_BYTES] { let mut f=[0x55;FRAME_BYTES];write_native_frame(&mut f,&identity());f }
    #[test]fn nested_arm_calculations_retain_their_own_actual_frame(){
        let mut state=StoreState::default();state.begin(id(),20,10,100,200);
        let mut parent=identity();parent[12]=1.;
        assert_eq!(state.calculations.last_mut().unwrap().arm_frame_for_read(ReadSite::FireInitial,||Some(parent)),Some(parent));
        state.begin(id(),21,10,101,201);let mut child=identity();child[12]=9.;
        assert_eq!(state.calculations.last_mut().unwrap().arm_frame_for_read(ReadSite::FireInitial,||Some(child)),Some(child));
        state.calculations.pop();
        assert_eq!(state.calculations.last_mut().unwrap().arm_frame_for_read(ReadSite::FireDominant,||panic!("must not reread actual frame")),Some(parent));
    }
    #[test]
    fn private_frame_preserves_opaque_words_and_uses_one_hand_generation() {
        let now=Instant::now();let c=candidate(now);let original=frame();let mut s=StoreState::default();
        let ik=s.read(id(),10,10,true,ReadSite::GunIk,original,&c,now,7).unwrap();
        assert_eq!(&ik[..16],&original[..16]);assert_eq!(component(&ik,64),2.);
        assert_eq!(component(&ik,68),3.);assert_eq!(component(&ik,72),4.);
        assert_eq!(&ik[28..32],&original[28..32]);assert_eq!(frame(),original);
        let token=s.begin(id(),20,10,100,200);assert_ne!(token,0);
        let mut later=c.clone();later.hand_sequence=99;later.stage_from_aim[12]=80.;
        assert_eq!(s.read(id(),10,10,true,ReadSite::FireInitial,original,&later,now,7),Some(ik));
        s.mark_applied(id(),10,ReadSite::FireInitial);
        assert_eq!(s.read(id(),10,10,true,ReadSite::FireDominant,original,&later,now,7),Some(ik));
        let ack=s.complete(token,id(),20,10,100,200,true,[1.,2.,3.],[7.,9.,4.]).unwrap();
        assert_eq!(ack.motion_generation,Some((7,11,13)));
        assert_eq!(ack.origin,[1.,2.,3.]);assert_eq!(ack.target,[7.,9.,4.]);
    }
    #[test]
    fn foreign_ped_mode_epoch_camera_and_nonrigid_samples_fail_through() {
        let now=Instant::now();let c=candidate(now);let f=frame();let mut s=StoreState::default();
        for (ped,local,first,epoch) in [(0,0,true,7),(10,11,true,7),(10,10,false,7),(10,10,true,8)] {
            assert!(s.read(id(),ped,local,first,ReadSite::GunIk,f,&c,now,epoch).is_none());
        }
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,f,&c,now+Duration::from_millis(151),7).is_none());
        let mut other=f;other[16..20].copy_from_slice(&0.5_f32.to_le_bytes());
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,other,&c,now,7).is_none());
        let mut bad=c.clone();bad.stage_from_aim[0]=-1.;
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,f,&bad,now,7).is_none());
    }
    #[test]
    fn returned_native_shot_is_observed_and_failed_or_foreign_results_are_not_acknowledged() {
        let mut s=StoreState::default();let t=s.begin(id(),20,10,100,200);
        assert!(s.complete(t,id(),20,10,100,200,false,[0.;3],[1.;3]).is_none());
        let t=s.begin(id(),20,10,100,200);
        let ack=s.complete(t,id(),20,10,100,200,true,[0.;3],[1.;3]).unwrap();
        assert_eq!(ack.motion_generation,None);
        let t=s.begin(id(),20,10,100,200);
        assert!(s.complete(t,id(),21,10,100,200,true,[0.;3],[1.;3]).is_none());
        assert!(s.latest_ack.is_none());
    }
    #[test]
    fn nested_recovery_discards_children_before_parent_continues() {
        let mut s=StoreState::default();let parent=s.begin(id(),20,10,100,200);
        let prefix=s.boundary_prefix();let child=s.begin(id(),30,10,300,400);assert!(child>parent);
        s.recover_boundary(&prefix);
        assert!(s.complete(parent,id(),20,10,100,200,true,[0.;3],[1.;3]).is_some());
        s.recover_boundary(&[999]);assert!(s.latest_ack.is_none());
    }
    #[test]
    fn original_anchor_age_and_frozen_lease_expiry_are_not_restarted(){
        let now=Instant::now();let mut c=candidate(now);c.source_age_at_capture=Duration::from_millis(140);
        let mut s=StoreState::default();
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,frame(),&c,now,7).is_some());
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,frame(),&c,now+Duration::from_millis(11),7).is_none());
        let mut s=StoreState::default();assert!(s.read(id(),10,10,true,ReadSite::GunIk,frame(),&c,now,7).is_some());
        let mut refreshed=c.clone();refreshed.recorded_at=now+Duration::from_millis(11);refreshed.source_age_at_capture=Duration::ZERO;
        assert!(s.read(id(),10,10,true,ReadSite::GunIk,frame(),&refreshed,now+Duration::from_millis(11),7).is_none());
    }
    #[test]
    fn tracking_loss_and_mode_reentry_require_release_before_fire(){
        let mut gate=FireGate{release_required:true,was_motion:false};
        assert_eq!(gate.apply(1.,FireScope::Native),1.);
        assert_eq!(gate.apply(1.,FireScope::Motion{ready:true}),0.);
        assert_eq!(gate.apply(0.,FireScope::Motion{ready:true}),0.);
        assert_eq!(gate.apply(1.,FireScope::Motion{ready:true}),1.);
        assert_eq!(gate.apply(1.,FireScope::Motion{ready:false}),0.);
        assert_eq!(gate.apply(1.,FireScope::Motion{ready:true}),0.);
        assert_eq!(gate.apply(0.,FireScope::Motion{ready:true}),0.);
        assert_eq!(gate.apply(1.,FireScope::Motion{ready:true}),1.);
        assert_eq!(gate.apply(1.,FireScope::Native),1.);
    }
    #[test]
    fn certified_but_unrequested_motion_hooks_preserve_native_inputs_and_firing(){
        use wasmtime::Val;
        let mut result=[Val::I64(-1)];let args=[Val::I32(1),Val::I64(77),Val::I64(999),Val::I64(-1),Val::I64(0),Val::I32(1)];
        inactive_hook("private_frame",&args,&mut result).unwrap();assert_eq!(result[0].i64(),Some(999));
        inactive_hook("calculation_begin",&vec![Val::I64(-1);5],&mut result).unwrap();assert_eq!(result[0].i64(),Some(0));
        inactive_hook("calculation_complete",&vec![Val::I64(-1);7],&mut []).unwrap();
        inactive_hook("allow_fire",&vec![Val::I64(-1);5],&mut result).unwrap();assert_eq!(result[0].i32(),Some(1));
        assert!(inactive_hook("unknown",&[],&mut []).is_err());
    }
    #[test]
    fn native_frame_and_firing_share_only_supported_spatial_onfoot_firstperson_scope(){
        assert!(supported_motion_scope(true,true,true,false,true));
        for route in [(false,true,true,false,true),(true,false,true,false,true),
            (true,true,false,false,true),(true,true,true,true,true),(true,true,true,false,false)]{
            assert!(!supported_motion_scope(route.0,route.1,route.2,route.3,route.4));
        }
    }
    #[test]
    fn native_arm_site_uses_its_current_predicate_without_restamping_tracking_or_aim_history(){
        let now=Instant::now();let epoch=7;
        // A current native arm predicate remains authoritative if a different
        // camera-reading task has not run, has expired, or last saw another mode.
        for history in [None,Some((true,now-Duration::from_millis(151),10,epoch)),
            Some((false,now,10,epoch)),Some((true,now,10,epoch-1))]{
            assert!(!arm_mode_matches(true,ArmModeAuthority::RecentAimRead,history,now,epoch));
            assert!(arm_mode_matches(true,ArmModeAuthority::CurrentNativePredicate,history,now,epoch));
            assert!(!arm_mode_matches(false,ArmModeAuthority::CurrentNativePredicate,history,now,epoch));
        }
        let fresh=Some((true,now,10,epoch));
        assert!(arm_mode_matches(true,ArmModeAuthority::RecentAimRead,fresh,now,epoch));
        assert!(!arm_mode_matches(false,ArmModeAuthority::RecentAimRead,fresh,now,epoch));
        // Mode authority does not replace the independent packet-age, epoch or
        // head/hand identity checks used by both production candidate paths.
        let c=candidate(now);assert!(c.valid(now,epoch));
        assert!(!c.valid(now+Duration::from_millis(151),epoch));
        assert!(!c.valid(now,epoch+1));
        for field in 0..3{let mut missing=c.clone();match field{0=>missing.head_frame=0,1=>missing.tracking_epoch=0,_=>missing.hand_sequence=0};assert!(!missing.valid(now,epoch));}
        let mut old_source=c.clone();old_source.source_age_at_capture=Duration::from_millis(151);assert!(!old_source.valid(now,epoch));
        let left=LeftArmCandidate{view_epoch:epoch,camera_sequence:11,head_frame:12,tracking_epoch:4,hand_sequence:13,
            game_from_stage:identity(),stage_from_grip:identity(),recorded_at:now,source_age:Duration::ZERO};
        assert!(left.valid(now,epoch));assert!(!left.valid(now+Duration::from_millis(151),epoch));assert!(!left.valid(now,epoch+1));
    }
    #[test]fn left_packet_and_position_candidate_remain_valid_when_right_accessory_is_lost(){
        let times=[10.,9.98,1.];let ids=[1,4,12,13];assert!(left_packet_valid(12,10.,1,times,ids));assert_eq!(packet_rejection(12,10.,1,times,ids),Some("no_fresh_right_accessory"));
        assert!(!left_packet_valid(12,10.,1|(1<<8),times,ids));assert!(!left_packet_valid(11,10.,1,times,ids));assert!(!left_packet_valid(12,10.,1,[10.,9.8,1.],ids));
        let now=Instant::now();let mut left=LeftArmCandidate{view_epoch:7,camera_sequence:11,head_frame:12,tracking_epoch:4,hand_sequence:13,game_from_stage:identity(),stage_from_grip:identity(),recorded_at:now,source_age:Duration::from_millis(20)};
        assert!(left.valid(now+Duration::from_millis(129),7));assert!(!left.valid(now+Duration::from_millis(131),7));assert!(!left.valid(now,8));left.stage_from_grip[0]=2.;assert!(!left.valid(now,7));
    }
}

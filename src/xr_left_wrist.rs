//! Explicit left grip-to-native-hand calibration for GTA's own full-pose arm
//! solver. While a calibration session is open no left arm request is issued,
//! so the rendered left hand is the game's own (animation and stock IK). The
//! player aligns the real left Sense grip with that hand and confirms. The
//! post-IK cache stage then records grip_from_hand = inverse(G)*H over several
//! still, consecutive, fresh camera generations, where G = game_from_stage *
//! stage_from_grip from the live exact left packet and H is the actual native
//! hand bone in the same skeleton reference. Translation is the measured real
//! offset, bounded by the actual native forearm; orientation is measured, never
//! an assumed axis map. A calibrated target G*grip_from_hand is transported into
//! original38548 with native request flags256 only while the controller
//! fingerprint, rig and left-flags artifact all match. The fingerprint is the
//! GameController product category, vendor name and fixed left hand: it
//! distinguishes controller models, NOT physical units (no per-unit serial is
//! exposed). A successful capture stays pending until the card explicitly
//! commits it; any other session end discards it. Otherwise the existing
//! position-only path (flags0, native wrist) remains. No bone, goal, request
//! or solver state is written here; this module retains values only.
use super::*;
use std::sync::OnceLock;

pub(super) const FLAGS_IMPORT:&str=super::LEFT_REQUEST_FLAGS_IMPORT;
pub(super) const REQUIRED_SAMPLES:usize=12;
const CAPTURE_TIMEOUT:Duration=Duration::from_secs(4);
/// The calibration card polls state; without it the native hand is not held.
const SESSION_HEARTBEAT:Duration=Duration::from_millis(1500);
const OBSERVATION_AGE:Duration=Duration::from_millis(500);
// Stillness reuses the established actual-pose agreement bounds (3 cm, 3 deg).
const STILL_TRANSLATION_M:f64=0.03;
const STILL_ROTATION_DEG:f64=3.;
type W=[f64;16];

#[repr(u32)]
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum Reason{None=0,NoFirstPersonWeaponFrame=1,LeftControllerNotTracked=2,LeftHandRigUnqualified=3,LeftIkBlendingOut=4,
    ControllerIdentityMissing=5,HoldStill=6,CaptureTimeout=7,GripTooFarFromHand=8,TrackingChanged=9,SessionExpired=10,Unsupported=11,RigChanged=12,NotRigid=13,
    NoPostIkObservation=14,ControllerChanged=15}
impl Reason{fn name(self)->&'static str{match self{Self::None=>"none",Self::NoFirstPersonWeaponFrame=>"no_first_person_weapon_frame",
    Self::LeftControllerNotTracked=>"left_controller_not_tracked",Self::LeftHandRigUnqualified=>"left_hand_rig_unqualified",Self::LeftIkBlendingOut=>"left_ik_still_blending_out",
    Self::ControllerIdentityMissing=>"controller_identity_missing",Self::HoldStill=>"hold_still",Self::CaptureTimeout=>"capture_timeout",
    Self::GripTooFarFromHand=>"grip_farther_than_native_forearm",Self::TrackingChanged=>"tracking_changed",Self::SessionExpired=>"session_expired",
    Self::Unsupported=>"unsupported_artifact",Self::RigChanged=>"rig_changed",Self::NotRigid=>"non_rigid_pose",
    Self::NoPostIkObservation=>"no_post_ik_observation_before_deadline",Self::ControllerChanged=>"controller_fingerprint_changed"}}}

/// Native rig identity: skeleton size, converted left hand/auxiliary bones and
/// the live native hand->auxiliary transform used by every left request.
#[derive(Clone,Copy,Debug,PartialEq)]
pub(super) struct Rig{pub bone_count:u32,pub hand:u16,pub aux:u16,pub hand_from_aux:[f32;16]}
impl Rig{pub(super) fn matches(&self,o:&Rig)->bool{self.bone_count==o.bone_count&&self.hand==o.hand&&self.aux==o.aux&&close(&self.hand_from_aux,&o.hand_from_aux)}}
#[derive(Clone,Copy,Debug,PartialEq)]
/// `controller` is the product/vendor/left-hand fingerprint, not a unit serial.
pub(super) struct Calibration{pub grip_from_hand:[f32;16],pub controller:u64,pub rig:Rig,pub forearm_m:f32,pub samples:u32}
#[derive(Clone,Copy,Debug)]
pub(super) struct NativeSample{pub grip:[f32;16],pub hand:[f32;16],pub forearm_m:f64,pub rig:Rig,pub native_hand:bool,pub camera_sequence:u64,pub tracking:u64,pub view_epoch:u64}

fn wide(m:&[f32;16])->W{canonical(*m).map(f64::from)}
fn mul(a:&W,b:&W)->W{std::array::from_fn(|i|{let(c,r)=(i/4,i%4);(0..4).map(|k|a[k*4+r]*b[c*4+k]).sum()})}
fn rigid64(m:&W)->bool{
    if m.iter().any(|v|!v.is_finite())||[3,7,11].iter().any(|&i|m[i]!=0.)||m[15]!=1.{return false;}
    let dot=|a:usize,b:usize|(0..3).map(|r|m[a*4+r]*m[b*4+r]).sum::<f64>();
    let det=m[0]*(m[5]*m[10]-m[9]*m[6])-m[4]*(m[1]*m[10]-m[9]*m[2])+m[8]*(m[1]*m[6]-m[5]*m[2]);
    (0..3).all(|a|(0..3).all(|b|(dot(a,b)-f64::from(u8::from(a==b))).abs()<=1e-4))&&(det-1.).abs()<=1e-4
}
fn inverse64(m:&W)->W{let mut o=[0.;16];o[15]=1.;for c in 0..3{for r in 0..3{o[c*4+r]=m[r*4+c];}}for r in 0..3{o[12+r]=-(0..3).map(|k|o[k*4+r]*m[12+k]).sum::<f64>();}o}
fn translation(m:&W)->f64{(m[12]*m[12]+m[13]*m[13]+m[14]*m[14]).sqrt()}
fn rotation_deg(a:&W,b:&W)->f64{let t:f64=(0..3).map(|c|(0..3).map(|r|a[c*4+r]*b[c*4+r]).sum::<f64>()).sum();((t-1.)/2.).clamp(-1.,1.).acos().to_degrees()}
fn agree(a:&W,b:&W)->bool{(0..3).map(|i|(a[12+i]-b[12+i]).powi(2)).sum::<f64>().sqrt()<=STILL_TRANSLATION_M&&rotation_deg(a,b)<=STILL_ROTATION_DEG}
fn quaternion(m:&W)->[f64;4]{
    let t=m[0]+m[5]+m[10];
    if t>0.{let s=(t+1.).sqrt()*2.;[(m[6]-m[9])/s,(m[8]-m[2])/s,(m[1]-m[4])/s,0.25*s]}
    else if m[0]>m[5]&&m[0]>m[10]{let s=(1.+m[0]-m[5]-m[10]).sqrt()*2.;[0.25*s,(m[4]+m[1])/s,(m[8]+m[2])/s,(m[6]-m[9])/s]}
    else if m[5]>m[10]{let s=(1.+m[5]-m[0]-m[10]).sqrt()*2.;[(m[4]+m[1])/s,0.25*s,(m[9]+m[6])/s,(m[8]-m[2])/s]}
    else{let s=(1.+m[10]-m[0]-m[5]).sqrt()*2.;[(m[8]+m[2])/s,(m[9]+m[6])/s,0.25*s,(m[1]-m[4])/s]}
}
fn from_quaternion([x,y,z,w]:[f64;4],t:[f64;3])->W{
    [1.-2.*(y*y+z*z),2.*(x*y+z*w),2.*(x*z-y*w),0., 2.*(x*y-z*w),1.-2.*(x*x+z*z),2.*(y*z+x*w),0., 2.*(x*z+y*w),2.*(y*z-x*w),1.-2.*(x*x+y*y),0., t[0],t[1],t[2],1.]
}
/// Sign-aligned quaternion mean and translation mean of mutually still samples.
pub(super) fn average(samples:&[W])->Option<W>{
    let first=quaternion(samples.first()?);let mut q=[0.;4];let mut t=[0.;3];
    for m in samples{let mut s=quaternion(m);if (0..4).map(|i|s[i]*first[i]).sum::<f64>()<0.{s=s.map(|v|-v);}for i in 0..4{q[i]+=s[i];}for i in 0..3{t[i]+=m[12+i];}}
    let n=(q.iter().map(|v|v*v).sum::<f64>()).sqrt();if !(n.is_finite()&&n>0.){return None;}
    let out=from_quaternion(q.map(|v|v/n),t.map(|v|v/samples.len()as f64));rigid64(&out).then_some(out)
}

/// One capture window, pinned to the controller fingerprint it started with.
#[derive(Default)]
pub(super) struct Capture{controller:u64,samples:Vec<W>,first:Option<(u64,u64,Rig)>,last_sequence:u64,pub restarts:u32,pub last_reason:Option<Reason>}
#[derive(Debug,PartialEq)]
pub(super) enum Step{Waiting(Reason),Collected(usize),Done(Calibration),Failed(Reason)}
impl Capture{
    pub(super) fn new(controller:u64)->Self{Self{controller,..Self::default()}}
    fn reset(&mut self,reason:Reason){if !self.samples.is_empty(){self.restarts+=1;}self.samples.clear();self.first=None;self.last_reason=Some(reason);}
    pub(super) fn count(&self)->usize{self.samples.len()}
    /// One post-IK observation. Only a new camera generation adds a sample;
    /// motion, tracking/view epoch or rig change restarts the still window.
    pub(super) fn add(&mut self,s:&NativeSample,controller:u64)->Step{
        if controller==0{return Step::Failed(Reason::ControllerIdentityMissing);}
        // Never mix samples across controller fingerprints: restart on change.
        if controller!=self.controller{self.reset(Reason::ControllerChanged);self.controller=controller;}
        if !s.native_hand{self.reset(Reason::LeftIkBlendingOut);return Step::Waiting(Reason::LeftIkBlendingOut);}
        let(g,h)=(wide(&s.grip),wide(&s.hand));if !rigid64(&g)||!rigid64(&h){self.reset(Reason::NotRigid);return Step::Waiting(Reason::NotRigid);}
        let gh=mul(&inverse64(&g),&h);
        if !(s.forearm_m>0.)||translation(&gh)>s.forearm_m{self.reset(Reason::GripTooFarFromHand);return Step::Waiting(Reason::GripTooFarFromHand);}
        if let Some((tracking,epoch,rig))=self.first{
            if tracking!=s.tracking||epoch!=s.view_epoch{self.reset(Reason::TrackingChanged);}
            else if !rig.matches(&s.rig){self.reset(Reason::RigChanged);}
            else if s.camera_sequence<=self.last_sequence{return Step::Collected(self.samples.len());}
            else if !agree(&self.samples[0],&gh){self.reset(Reason::HoldStill);}
        }
        if self.samples.is_empty(){self.first=Some((s.tracking,s.view_epoch,s.rig));}
        self.samples.push(gh);self.last_sequence=s.camera_sequence;
        if self.samples.len()<REQUIRED_SAMPLES{return Step::Collected(self.samples.len());}
        let Some(mean)=average(&self.samples)else{self.reset(Reason::NotRigid);return Step::Waiting(Reason::NotRigid)};
        let rig=self.first.map_or(s.rig,|f|f.2);
        Step::Done(Calibration{grip_from_hand:mean.map(|v|v as f32),controller:self.controller,rig,forearm_m:s.forearm_m as f32,samples:self.samples.len()as u32})
    }
}

/// A successful capture is only `pending` until the card commits it.
struct Session{serial:u64,heartbeat:Instant,capture:Option<(Instant,Capture)>,pending:Option<Calibration>}
#[derive(Default)]
pub(super) struct State{controller:u64,calibration:Option<Calibration>,session:Option<Session>,serial:u64,
    observed:Option<(Instant,Result<bool,Reason>)>,result:Option<(u64,Result<(),Reason>)>,last_left_token:u32,applied:Option<Instant>}
impl State{
    /// Session heartbeat and the capture deadline are enforced on every state
    /// access, not only when a post-IK observation arrives. An expired session
    /// drops any uncommitted capture with it.
    fn expire(&mut self,now:Instant){
        if self.session.as_ref().is_some_and(|s|now.saturating_duration_since(s.heartbeat)>SESSION_HEARTBEAT){
            let serial=self.session.take().map_or(0,|s|s.serial);self.result=Some((serial,Err(Reason::SessionExpired)));
        }
        let observed=self.observed.map(|o|o.0);
        let Some(session)=self.session.as_mut()else{return};
        let Some((started,capture))=session.capture.as_ref()else{return};
        if now.saturating_duration_since(*started)<=CAPTURE_TIMEOUT{return;}
        // The observer stopped (no first-person gun frame, mode change) when no
        // fresh post-IK observation exists at the deadline.
        let stalled=observed.is_none_or(|at|at<*started||now.saturating_duration_since(at)>OBSERVATION_AGE);
        let reason=if stalled{Reason::NoPostIkObservation}else{match capture.last_reason{Some(Reason::None)|None=>Reason::CaptureTimeout,Some(r)=>r}};
        let serial=session.serial;session.capture=None;self.result=Some((serial,Err(reason)));
    }
    pub(super) fn open(&mut self,now:Instant,supported:bool)->u64{
        if !supported{return 0;}self.expire(now);
        self.serial=self.serial.wrapping_add(1).max(1);self.session=Some(Session{serial:self.serial,heartbeat:now,capture:None,pending:None});self.serial
    }
    pub(super) fn capture(&mut self,session:u64,now:Instant)->i32{
        self.expire(now);let controller=self.controller;let ready=self.native_hand_ready(now);
        let Some(s)=self.session.as_mut().filter(|s|s.serial==session&&session!=0)else{return -1};
        if controller==0{return -4;}if !ready{return -2;}
        s.heartbeat=now;s.capture=Some((now,Capture::new(controller)));s.pending=None;self.result=None;0
    }
    /// Ends the session, discarding any uncommitted capture.
    pub(super) fn close(&mut self,session:u64)->i32{
        if self.session.as_ref().is_some_and(|s|session==0||s.serial==session){self.session=None;0}else{-1}
    }
    /// Adopts this session's successful capture and ends the session.
    /// 0 committed; -1 no such session (nothing changes); -2 nothing to commit
    /// (session stays open).
    pub(super) fn commit(&mut self,session:u64,now:Instant)->i32{
        self.expire(now);
        let Some(s)=self.session.as_mut().filter(|s|s.serial==session&&session!=0)else{return -1};
        let Some(calibration)=s.pending.take()else{return -2};
        self.calibration=Some(calibration);self.session=None;0
    }
    /// Fingerprint changes restart, and loss fails, an active capture. A
    /// captured but uncommitted result is pinned the same way: a different or
    /// lost fingerprint discards it, so commit can never adopt it.
    pub(super) fn set_controller(&mut self,fingerprint:u64,now:Instant){
        self.expire(now);self.controller=fingerprint;
        let Some(session)=self.session.as_mut()else{return};let serial=session.serial;
        if session.pending.is_some_and(|c|c.controller!=fingerprint){
            session.pending=None;
            self.result=Some((serial,Err(if fingerprint==0{Reason::ControllerIdentityMissing}else{Reason::ControllerChanged})));
        }
        let Some((_,capture))=session.capture.as_mut()else{return};
        if fingerprint==0{session.capture=None;self.result=Some((serial,Err(Reason::ControllerIdentityMissing)));}
        else if fingerprint!=capture.controller{capture.reset(Reason::ControllerChanged);capture.controller=fingerprint;}
    }
    pub(super) fn suspends(&mut self,now:Instant)->bool{self.expire(now);self.session.is_some()}
    fn native_hand_ready(&self,now:Instant)->bool{self.observed.is_some_and(|(at,r)|now.saturating_duration_since(at)<=OBSERVATION_AGE&&r==Ok(true))}
    fn eligible(&self,now:Instant)->bool{self.observed.is_some_and(|(at,r)|now.saturating_duration_since(at)<=OBSERVATION_AGE&&r.is_ok())}
    /// Calibrated target only for the matching controller and native rig.
    pub(super) fn calibrated_hand(&mut self,grip:&[f32;16],rig:&Rig,supported:bool,now:Instant)->Option<[f32;16]>{
        if !supported||self.suspends(now)||self.controller==0{return None;}
        let c=self.calibration.filter(|c|c.controller==self.controller&&c.rig.matches(rig))?;
        let hand=multiply(grip,&c.grip_from_hand);rigid(&hand).then_some(hand)
    }
    pub(super) fn observe(&mut self,observed:Result<NativeSample,Reason>,now:Instant){
        self.expire(now);self.observed=Some((now,observed.as_ref().map(|s|s.native_hand).map_err(|r|*r)));
        let controller=self.controller;
        let Some(session)=self.session.as_mut()else{return};let serial=session.serial;
        let Some((_,capture))=session.capture.as_mut()else{return};
        let step=match &observed{Ok(s)=>capture.add(s,controller),Err(r)=>{capture.reset(*r);Step::Waiting(*r)}};
        let finished=match step{
            Step::Done(c)=>{session.pending=Some(c);Some(Ok(()))}
            Step::Failed(r)=>Some(Err(r)),
            _=>None};
        if let Some(result)=finished{session.capture=None;self.result=Some((serial,result));}
    }
    pub(super) fn snapshot(&mut self,now:Instant,supported:bool)->SVLeftWristState{
        self.expire(now);if let Some(s)=self.session.as_mut(){s.heartbeat=now;}
        let session=self.session.as_ref();
        let capturing=session.and_then(|s|s.capture.as_ref());
        let result=self.result.filter(|(serial,_)|session.is_some_and(|s|s.serial==*serial));
        let observed_reason=self.observed.filter(|(at,_)|now.saturating_duration_since(*at)<=OBSERVATION_AGE)
            .map_or(Reason::NoFirstPersonWeaponFrame,|(_,r)|match r{Ok(true)=>Reason::None,Ok(false)=>Reason::LeftIkBlendingOut,Err(e)=>e});
        // A failed capture leaves the card open; readiness stays live so the
        // player can confirm again, and the failure is reported separately.
        let(phase,reason)=match(session,capturing,result){
            (None,_,_)=>(0,if supported{observed_reason}else{Reason::Unsupported}),
            (Some(_),Some((_,c)),_)=>(3,c.last_reason.unwrap_or(Reason::None)),
            (Some(_),None,Some((_,Ok(()))))=>(4,Reason::None),
            (Some(_),None,_)=>(if self.native_hand_ready(now)&&self.controller!=0{2}else{1},if self.controller==0{Reason::ControllerIdentityMissing}else{observed_reason}),
        };
        let failure=match result{Some((_,Err(r)))=>r as u32,_=>0};
        let summary=self.calibration.map(|c|{let m=wide(&c.grip_from_hand);(translation(&m) as f32,rotation_deg(&m,&wide(&identity())) as f32)});
        let applicable=self.calibration.is_some_and(|c|c.controller==self.controller&&self.controller!=0);
        SVLeftWristState{version:1,size:std::mem::size_of::<SVLeftWristState>()as u32,session:session.map_or(0,|s|s.serial),
            result_session:self.result.map_or(0,|r|r.0),phase,reason:reason as u32,samples:capturing.map_or(0,|c|c.1.count()as u32),
            required_samples:REQUIRED_SAMPLES as u32,
            flags:u32::from(supported)|u32::from(self.eligible(now))<<1|u32::from(self.calibration.is_some())<<2|u32::from(applicable)<<3
                |u32::from(self.native_hand_ready(now))<<4|u32::from(self.applied.is_some_and(|at|now.saturating_duration_since(at)<=MAX_AGE))<<5,
            translation_m:summary.map_or(0.,|s|s.0),rotation_degrees:summary.map_or(0.,|s|s.1),failure}
    }
}
static STATE:std::sync::LazyLock<Mutex<State>>=std::sync::LazyLock::new(||Mutex::new(State::default()));
fn state()->std::sync::MutexGuard<'static,State>{STATE.lock().unwrap_or_else(|p|p.into_inner())}

/// The opted-in artifact must transport host-chosen left flags into 38506.
fn flags_import_present()->bool{
    static PRESENT:OnceLock<bool>=OnceLock::new();
    if let Some(v)=PRESENT.get(){return *v;}
    let Some(rt)=crate::RT.get()else{return false};
    *PRESENT.get_or_init(||rt.imports.iter().any(|i|i.module==IMPORT_MODULE&&i.name==FLAGS_IMPORT))
}
pub(super) fn supported()->bool{crate::xr_motion_aim::arms_enabled()&&flags_import_present()}
pub(super) fn suspends_left_requests()->bool{state().suspends(Instant::now())}
pub(super) fn note_left_request(token:u32){state().last_left_token=token;}
pub(super) fn note_calibrated_applied(){state().applied=Some(Instant::now());}
pub(super) fn calibrated_hand(grip:&[f32;16],rig:&Rig)->Option<[f32;16]>{let supported=supported();state().calibrated_hand(grip,rig,supported,Instant::now())}
pub(super) fn policy_name()->&'static str{
    let mut s=state();let now=Instant::now();
    if s.applied.is_some_and(|at|now.saturating_duration_since(at)<=MAX_AGE)&&!s.suspends(now){"calibrated grip-to-native-hand full pose; native flags256"}
    else{"native wrist; tracked grip translation only"}
}

/// Post-IK native left hand. Native means this frame's left request is not
/// ours, or our previous one has fully blended out (original38481 clears
/// request bytes in38488 and blends +92 toward zero when not renewed).
pub(super) fn native_hand<M:Memory>(s:&mut Snapshot<'_,M>,last_token:u32)->Result<(Rig,[f32;16],f64,bool),Reason>{
    let rig=Reason::LeftHandRigUnqualified;
    let ped=observed_local_ped(s.mem).ok_or(rig)?;let(owner,skel)=manager(s,ped.checked_add(3888).ok_or(rig)?).ok_or(rig)?;
    let solver=s.u64(ped+3888+272).filter(|p|*p!=0).ok_or(rig)?;
    if owner!=ped||s.u64(solver)!=Some(SOLVER_VTABLE)||s.u64(solver+480)!=Some(ped){return Err(rig);}
    let(goal,request)=(solver+32,solver+224);
    let mut bones=[0u16;5];for(slot,offset)in bones.iter_mut().zip([72,74,76,78,88]){*slot=s.u16(goal+offset).ok_or(rig)?;}
    let[hand,forearm,upper,aux,end]=bones;
    let count=s.u32(skel+32).ok_or(rig)?;
    if [hand,forearm,aux].iter().any(|b|u32::from(*b)>=count)||aux<=upper||aux>=end||u32::from(end)>count{return Err(rig);}
    let k=hand_chain(s,skel,hand,aux).ok_or(rig)?;
    let world=trace::bone_world(s,skel,hand).ok_or(rig)?;let elbow=trace::bone_world(s,skel,forearm).ok_or(rig)?;
    let forearm_m=(0..3).map(|i|f64::from(world[12+i]-elbow[12+i]).powi(2)).sum::<f64>().sqrt();
    let blend=s.u32(request+92).map(f32::from_bits).filter(|b|b.is_finite()).ok_or(rig)?;
    let ours=last_token!=0&&s.u32(request+12)==Some(cookie(last_token));
    Ok((Rig{bone_count:count,hand,aux,hand_from_aux:k},world,forearm_m,blend==0.||!ours))
}
/// Called from the genuine local first-person weapon render-list producer,
/// after the frame's IK solve. Values only; no guest writes or calls.
pub(super) fn observe<M:Memory>(mem:&M){
    if !supported(){return;}
    let token=state().last_left_token;let mut s=Snapshot::new(mem);
    let sample=native_hand(&mut s,token).and_then(|(rig,hand,forearm_m,native)|{
        let c=crate::xr_motion_aim::native_left_arm_candidate(true).ok_or(Reason::LeftControllerNotTracked)?;
        let grip=multiply(&c.game_from_stage,&c.stage_from_grip);if !rigid(&grip){return Err(Reason::NotRigid);}
        if !s.unchanged(){return Err(Reason::LeftHandRigUnqualified);}
        Ok(NativeSample{grip,hand,forearm_m,rig,native_hand:native,camera_sequence:c.camera_sequence,tracking:c.tracking_epoch,view_epoch:c.view_epoch})
    });
    state().observe(sample,Instant::now());
}
pub(super) fn status()->serde_json::Value{
    let supported=supported();let mut s=state();let now=Instant::now();let snap=s.snapshot_without_heartbeat(now,supported);
    serde_json::json!({"supported":supported,"phase":snap.phase,"reason":reason_name(snap.reason),"last_failure":reason_name(snap.failure),"calibration_present":snap.flags&4!=0,
        "controller_fingerprint_matches":snap.flags&8!=0,"controller_fingerprint_scope":"GameController product category, vendor and fixed left hand; not a per-unit serial","eligible":snap.flags&2!=0,"native_hand_shown":snap.flags&16!=0,"calibrated_target_applied":snap.flags&32!=0,
        "grip_to_hand_translation_m":snap.translation_m,"grip_to_hand_rotation_degrees":snap.rotation_degrees,"samples":s.calibration.map(|c|c.samples),
        "authority":"left target transport only; not visible alignment acceptance"})
}
fn reason_name(code:u32)->&'static str{
    [Reason::None,Reason::NoFirstPersonWeaponFrame,Reason::LeftControllerNotTracked,Reason::LeftHandRigUnqualified,Reason::LeftIkBlendingOut,Reason::ControllerIdentityMissing,
     Reason::HoldStill,Reason::CaptureTimeout,Reason::GripTooFarFromHand,Reason::TrackingChanged,Reason::SessionExpired,Reason::Unsupported,Reason::RigChanged,Reason::NotRigid,
     Reason::NoPostIkObservation,Reason::ControllerChanged]
        .into_iter().find(|r|*r as u32==code).map_or("unknown",Reason::name)
}
impl State{
    /// Report path: never refreshes the card heartbeat.
    fn snapshot_without_heartbeat(&mut self,now:Instant,supported:bool)->SVLeftWristState{
        let heartbeat=self.session.as_ref().map(|s|s.heartbeat);let out=self.snapshot(now,supported);
        if let(Some(s),Some(h))=(self.session.as_mut(),heartbeat){s.heartbeat=h;}out
    }
}

#[repr(C)]
#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub struct SVLeftWristState{pub version:u32,pub size:u32,pub session:u64,pub result_session:u64,pub phase:u32,pub reason:u32,
    pub samples:u32,pub required_samples:u32,pub flags:u32,pub translation_m:f32,pub rotation_degrees:f32,pub failure:u32}
const _:()=assert!(std::mem::size_of::<SVLeftWristState>()==56);
#[repr(C)]
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct SVLeftWristCalibration{pub version:u32,pub size:u32,pub controller:u64,pub bone_count:u32,pub hand:u16,pub aux:u16,
    pub samples:u32,pub forearm_m:f32,pub hand_from_aux:[f32;12],pub grip_from_hand:[f32;12]}
const _:()=assert!(std::mem::size_of::<SVLeftWristCalibration>()==128);
fn pack(m:&[f32;16])->[f32;12]{std::array::from_fn(|i|m[(i/3)*4+i%3])}
fn unpack(v:&[f32;12])->[f32;16]{let mut m=identity();for i in 0..12{m[(i/3)*4+i%3]=v[i];}m}
pub(super) fn export(c:&Calibration)->SVLeftWristCalibration{
    SVLeftWristCalibration{version:1,size:128,controller:c.controller,bone_count:c.rig.bone_count,hand:c.rig.hand,aux:c.rig.aux,samples:c.samples,
        forearm_m:c.forearm_m,hand_from_aux:pack(&c.rig.hand_from_aux),grip_from_hand:pack(&c.grip_from_hand)}
}
/// Persisted values are re-validated: rigid, nonzero controller fingerprint and a measured
/// offset no longer than the measured native forearm it was captured with.
pub(super) fn import(v:&SVLeftWristCalibration)->Option<Calibration>{
    if v.version!=1||v.size!=128||v.controller==0||v.bone_count==0||v.bone_count>MAX_BONES||u32::from(v.hand)>=v.bone_count
        ||u32::from(v.aux)>=v.bone_count||v.hand==v.aux||!(v.forearm_m.is_finite()&&v.forearm_m>0.&&v.forearm_m<=1.)||v.samples<REQUIRED_SAMPLES as u32{return None;}
    let(k,g)=(unpack(&v.hand_from_aux),unpack(&v.grip_from_hand));
    if !rigid(&k)||!rigid(&g)||!rigid64(&wide(&g))||translation(&wide(&g))>f64::from(v.forearm_m){return None;}
    Some(Calibration{grip_from_hand:g,controller:v.controller,rig:Rig{bone_count:v.bone_count,hand:v.hand,aux:v.aux,hand_from_aux:k},forearm_m:v.forearm_m,samples:v.samples})
}

/// Product/vendor/fixed-hand fingerprint of the current left controller; 0 none.
#[no_mangle]
pub extern "C" fn sv_xr_left_wrist_controller_v1(fingerprint:u64)->i32{state().set_controller(fingerprint,Instant::now());0}
#[no_mangle]
pub extern "C" fn sv_xr_left_wrist_open_v1()->u64{let supported=supported();state().open(Instant::now(),supported)}
#[no_mangle]
pub extern "C" fn sv_xr_left_wrist_capture_v1(session:u64)->i32{if !supported(){return -3;}state().capture(session,Instant::now())}
/// Ends the session and discards any uncommitted capture.
#[no_mangle]
pub extern "C" fn sv_xr_left_wrist_close_v1(session:u64)->i32{state().close(session)}
/// Adopts this session's successful capture, then ends the session.
#[no_mangle]
pub extern "C" fn sv_xr_left_wrist_commit_v1(session:u64)->i32{state().commit(session,Instant::now())}
/// # Safety
/// `out` must point to writable storage of at least `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_left_wrist_state_v1(out:*mut SVLeftWristState,size:u32)->i32{
    if out.is_null()||size as usize!=std::mem::size_of::<SVLeftWristState>(){return -1;}
    let supported=supported();let value=state().snapshot(Instant::now(),supported);unsafe{out.write_unaligned(value)};0
}
/// # Safety
/// `out` must point to writable storage of at least `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_left_wrist_export_v1(out:*mut SVLeftWristCalibration,size:u32)->i32{
    if out.is_null()||size!=128{return -1;}let Some(c)=state().calibration else{return 1};unsafe{out.write_unaligned(export(&c))};0
}
/// A null pointer clears the calibration and returns to position-only tracking.
/// # Safety
/// `input`, when non-null, must point to readable storage of at least `size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_left_wrist_import_v1(input:*const SVLeftWristCalibration,size:u32)->i32{
    if input.is_null(){state().calibration=None;return 0;}
    if size!=128{return -1;}let value=unsafe{input.read_unaligned()};
    match import(&value){Some(c)=>{state().calibration=Some(c);0}None=>-2}
}

#[cfg(test)]mod tests{
    use super::*;
    use super::super::tests::native_fixture;
    fn rot(axis:[f64;3],deg:f64)->W{let n=(axis.iter().map(|v|v*v).sum::<f64>()).sqrt();let[x,y,z]=axis.map(|v|v/n);let(s,c)=deg.to_radians().sin_cos();let t=1.-c;
        [t*x*x+c,t*x*y+s*z,t*x*z-s*y,0., t*x*y-s*z,t*y*y+c,t*y*z+s*x,0., t*x*z+s*y,t*y*z-s*x,t*z*z+c,0., 0.,0.,0.,1.]}
    fn at(mut m:W,p:[f64;3])->W{m[12..15].copy_from_slice(&p);m}
    fn narrow(m:&W)->[f32;16]{m.map(|v|v as f32)}
    fn rig()->Rig{let mut k=identity();k[12]=0.09;k[13]=0.01;Rig{bone_count:100,hand:10,aux:12,hand_from_aux:k}}
    /// The physical relation the player actually holds; not known to the code.
    fn held()->W{at(mul(&rot([0.3,-1.,0.2],47.),&rot([1.,0.,0.],-21.)),[0.012,-0.064,0.031])}
    fn sample(i:u64,grip:&W,noise:f64)->NativeSample{
        let wobble=at(rot([1.,1.,0.],noise),[noise*1e-3,0.,0.]);let hand=mul(&mul(grip,&held()),&wobble);
        NativeSample{grip:narrow(grip),hand:narrow(&hand),forearm_m:0.26,rig:rig(),native_hand:true,camera_sequence:100+i,tracking:7,view_epoch:3}
    }
    fn grip(i:u64)->W{at(mul(&rot([0.,0.,1.],131.+i as f64*0.01),&rot([1.,0.,0.],12.)),[1210.4,-1353.2,29.3])}
    fn capture_all(c:&mut Capture,from:u64)->Step{let mut last=Step::Waiting(Reason::None);for i in from..from+REQUIRED_SAMPLES as u64{last=c.add(&sample(i,&grip(i),if i%2==0{0.4}else{-0.4}),55);}last}
    #[test]fn still_aligned_samples_recover_the_measured_grip_to_hand_relation(){
        let mut c=Capture::default();let Step::Done(cal)=capture_all(&mut c,0)else{panic!("not done")};
        let m=wide(&cal.grip_from_hand);assert!(rotation_deg(&m,&held())<0.05,"{}",rotation_deg(&m,&held()));
        assert!((0..3).all(|i|(m[12+i]-held()[12+i]).abs()<2e-3));assert_eq!((cal.controller,cal.samples),(55,REQUIRED_SAMPLES as u32));assert!(cal.rig.matches(&rig()));
        // Transported later: target G'*grip_from_hand reproduces the held hand.
        let g=at(rot([0.2,0.9,0.1],-64.),[1211.,-1352.,30.1]);let mut st=State{controller:55,calibration:Some(cal),..State::default()};
        let target=wide(&st.calibrated_hand(&narrow(&g),&rig(),true,Instant::now()).unwrap());let expected=mul(&g,&held());
        assert!(rotation_deg(&target,&expected)<0.06);assert!((0..3).all(|i|(target[12+i]-expected[12+i]).abs()<3e-3));
    }
    #[test]fn motion_epoch_rig_and_blending_reject_or_restart_instead_of_averaging(){
        let mut c=Capture::default();for i in 0..5{assert_eq!(c.add(&sample(i,&grip(i),0.),55),Step::Collected(i as usize+1));}
        // The same camera generation is not a new sample.
        assert_eq!(c.add(&sample(4,&grip(4),0.),55),Step::Collected(5));
        let mut moved=sample(5,&grip(5),0.);moved.hand=narrow(&mul(&wide(&moved.hand),&rot([0.,1.,0.],4.)));assert_eq!(c.add(&moved,55),Step::Collected(1));assert_eq!(c.restarts,1);assert_eq!(c.last_reason,Some(Reason::HoldStill));
        let mut lost=sample(6,&grip(6),0.);lost.tracking=8;c.add(&lost,55);assert_eq!(c.last_reason,Some(Reason::TrackingChanged));assert_eq!(c.count(),1);
        let mut other=sample(7,&grip(7),0.);other.tracking=8;other.rig.hand_from_aux[12]=0.12;c.add(&other,55);assert_eq!(c.last_reason,Some(Reason::RigChanged));
        let mut blending=sample(8,&grip(8),0.);blending.native_hand=false;assert_eq!(c.add(&blending,55),Step::Waiting(Reason::LeftIkBlendingOut));assert_eq!(c.count(),0);
        let mut far=sample(9,&grip(9),0.);far.forearm_m=0.05;assert_eq!(c.add(&far,55),Step::Waiting(Reason::GripTooFarFromHand));
        assert_eq!(Capture::default().add(&sample(1,&grip(1),0.),0),Step::Failed(Reason::ControllerIdentityMissing));
    }
    #[test]fn quaternion_mean_is_sign_invariant_and_exact_for_identical_samples(){
        let m=held();assert!(rotation_deg(&average(&[m,m,m]).unwrap(),&m)<1e-9);
        let q=quaternion(&m);assert!(rotation_deg(&from_quaternion(q.map(|v|-v),[m[12],m[13],m[14]]),&m)<1e-9);
        let a=mul(&m,&rot([0.,0.,1.],1.));let b=mul(&m,&rot([0.,0.,1.],-1.));assert!(rotation_deg(&average(&[a,b]).unwrap(),&m)<1e-6);
    }
    #[test]fn session_suspends_left_requests_and_only_a_native_capture_acknowledges(){
        let now=Instant::now();let mut st=State{controller:55,..State::default()};assert_eq!(st.open(now,false),0);
        let session=st.open(now,true);assert!(st.suspends(now));assert_eq!(st.snapshot(now,true).phase,1);
        // Confirm is refused until the post-IK observation shows GTA's own hand.
        assert_eq!(st.capture(session,now),-2);
        st.observe(Ok(NativeSample{native_hand:false,..sample(0,&grip(0),0.)}),now);assert_eq!(st.capture(session,now),-2);assert_eq!(st.snapshot(now,true).reason,Reason::LeftIkBlendingOut as u32);
        st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.snapshot(now,true).phase,2);assert_eq!(st.capture(session+1,now),-1);assert_eq!(st.capture(session,now),0);
        for i in 1..=REQUIRED_SAMPLES as u64{assert_ne!(st.snapshot(now,true).phase,4);st.observe(Ok(sample(i,&grip(i),0.2)),now);}
        // Captured but still pending: nothing is applied or reported present.
        let snap=st.snapshot(now,true);assert_eq!((snap.phase,snap.result_session,snap.flags&4!=0),(4,session,false));
        assert!(st.calibrated_hand(&narrow(&grip(0)),&rig(),true,now).is_none(),"card open: no calibrated target");
        assert_eq!(st.commit(session+1,now),-1);assert_eq!(st.commit(session,now),0);assert!(!st.suspends(now),"commit ends the session");
        assert!(st.snapshot(now,true).flags&4!=0);assert!(st.calibrated_hand(&narrow(&grip(0)),&rig(),true,now).is_some());
        assert_eq!(st.commit(session,now),-1,"a commit cannot be repeated");
    }
    /// Success immediately followed by Crown/Window/expiry before the card
    /// acknowledged it: the capture is discarded and nothing changes.
    #[test]fn uncommitted_success_is_discarded_by_close_or_expiry_and_keeps_previous_calibration(){
        let now=Instant::now();let previous={let mut c=Capture::new(55);let Step::Done(cal)=capture_all(&mut c,40)else{panic!()};cal};
        let run=|st:&mut State,session:u64|{st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
            for i in 1..=REQUIRED_SAMPLES as u64{st.observe(Ok(sample(i,&mul(&grip(i),&rot([0.,0.,1.],20.)),0.)),now);}assert_eq!(st.snapshot(now,true).phase,4);};
        let mut st=State{controller:55,calibration:Some(previous),..State::default()};let session=st.open(now,true);run(&mut st,session);
        assert_eq!(st.close(session),0);assert_eq!(st.calibration,Some(previous),"Crown/Window close discards the uncommitted capture");
        assert_eq!(st.commit(session,now),-1,"a closed session cannot be committed afterward");
        let mut st=State{controller:55,..State::default()};let session=st.open(now,true);run(&mut st,session);
        let later=now+SESSION_HEARTBEAT+Duration::from_millis(10);assert!(!st.suspends(later));assert!(st.calibration.is_none(),"expiry discards it");
        assert_eq!(st.commit(session,later),-1);
        let mut st=State{controller:55,..State::default()};let session=st.open(now,true);assert_eq!(st.commit(session,now),-2,"nothing to commit");assert!(st.suspends(now));
    }
    /// The observer can stop (gun holstered, mode change) while the card keeps
    /// polling; the deadline still ends the capture with a meaningful reason.
    #[test]fn capture_deadline_fires_without_any_new_post_ik_observation(){
        let now=Instant::now();let mut st=State{controller:55,..State::default()};let session=st.open(now,true);
        st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
        for i in 1..=4u64{st.observe(Ok(sample(i,&grip(i),0.)),now+Duration::from_millis(30*i));}
        for step in 1..=8u64{assert_eq!(st.snapshot(now+Duration::from_millis(500*step),true).phase,3,"heartbeat only");}
        let snap=st.snapshot(now+CAPTURE_TIMEOUT+Duration::from_millis(1),true);
        assert_eq!((snap.phase,snap.failure,snap.result_session,snap.samples),(1,Reason::NoPostIkObservation as u32,session,0));
        assert!(st.calibration.is_none());assert!(st.session.as_ref().is_some_and(|s|s.pending.is_none()));
        // Retry needs a fresh native hand; Cancel still works.
        let t=now+CAPTURE_TIMEOUT+Duration::from_millis(100);assert_eq!(st.capture(session,t),-2);
        st.observe(Ok(sample(9,&grip(9),0.)),t);assert_eq!(st.capture(session,t),0);assert_eq!(st.snapshot(t,true).phase,3);assert_eq!(st.close(session),0);
    }
    /// Samples are never mixed across controller fingerprints; loss fails.
    #[test]fn controller_fingerprint_change_restarts_and_loss_fails_the_capture(){
        let now=Instant::now();let mut st=State{controller:55,..State::default()};let session=st.open(now,true);
        st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
        for i in 1..=8u64{st.observe(Ok(sample(i,&grip(i),0.)),now);}assert_eq!(st.snapshot(now,true).samples,8);
        st.set_controller(56,now);let snap=st.snapshot(now,true);assert_eq!((snap.phase,snap.samples,snap.reason),(3,0,Reason::ControllerChanged as u32));
        for i in 9..9+REQUIRED_SAMPLES as u64{st.observe(Ok(sample(i,&grip(i),0.)),now);}
        assert_eq!(st.snapshot(now,true).phase,4);assert_eq!(st.commit(session,now),0);assert_eq!(st.calibration.map(|c|c.controller),Some(56));
        // Capture::add also re-pins defensively if the fingerprint arrives with a sample.
        let mut c=Capture::new(55);for i in 0..5{c.add(&sample(i,&grip(i),0.),55);}assert_eq!(c.count(),5);
        c.add(&sample(5,&grip(5),0.),57);assert_eq!((c.count(),c.last_reason,c.controller),(1,Some(Reason::ControllerChanged),57));
        let session=st.open(now,true);st.observe(Ok(sample(30,&grip(30),0.)),now);assert_eq!(st.capture(session,now),0);
        for i in 31..35u64{st.observe(Ok(sample(i,&grip(i),0.)),now);}
        st.set_controller(0,now);let snap=st.snapshot(now,true);
        assert_eq!((snap.phase,snap.failure,snap.reason),(1,Reason::ControllerIdentityMissing as u32,Reason::ControllerIdentityMissing as u32),"not ready without a fingerprint");
        assert_eq!(st.capture(session,now),-4,"no retry without a recognised controller");assert_eq!(st.commit(session,now),-2);
        assert_eq!(st.calibration.map(|c|c.controller),Some(56),"previous calibration unchanged");
    }
    /// A fingerprint change or loss after a successful capture but before the
    /// card's commit discards the pending capture; commit cannot adopt it.
    #[test]fn pending_success_is_discarded_by_fingerprint_change_or_loss_before_commit(){
        let now=Instant::now();
        let previous={let mut c=Capture::new(56);let mut last=Step::Waiting(Reason::None);
            for i in 0..REQUIRED_SAMPLES as u64{last=c.add(&sample(i,&grip(i),0.),56);}let Step::Done(cal)=last else{panic!("no previous")};cal};
        for(fingerprint,reason)in[(56u64,Reason::ControllerChanged),(0,Reason::ControllerIdentityMissing)]{
            let mut st=State{controller:55,calibration:Some(previous),..State::default()};let session=st.open(now,true);
            st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
            for i in 1..=REQUIRED_SAMPLES as u64{st.observe(Ok(sample(i,&grip(i),0.)),now);}assert_eq!(st.snapshot(now,true).phase,4);
            st.set_controller(fingerprint,now);let snap=st.snapshot(now,true);
            assert_ne!(snap.phase,4,"no success is shown once the pinned fingerprint is gone");
            assert_eq!((snap.failure,snap.result_session),(reason as u32,session));
            assert_eq!(st.commit(session,now),-2,"commit cannot adopt a capture whose fingerprint changed or was lost");
            assert_eq!(st.calibration,Some(previous),"previous calibration unchanged");assert!(st.suspends(now),"card stays open for retry or Cancel");
        }
        let mut st=State{controller:55,..State::default()};let session=st.open(now,true);
        st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
        for i in 1..=REQUIRED_SAMPLES as u64{st.observe(Ok(sample(i,&grip(i),0.)),now);}
        st.set_controller(55,now);assert_eq!(st.commit(session,now),0,"re-announcing the same fingerprint keeps the capture");
        assert_eq!(st.calibration.map(|c|c.controller),Some(55));
        assert_eq!(reason_name(Reason::NoPostIkObservation as u32),"no_post_ik_observation_before_deadline");
        assert_eq!(reason_name(Reason::ControllerChanged as u32),"controller_fingerprint_changed");
    }
    #[test]fn expiry_failure_timeout_and_mismatches_keep_position_fallback(){
        let now=Instant::now();let mut st=State{controller:55,..State::default()};let session=st.open(now,true);
        st.observe(Ok(sample(0,&grip(0),0.)),now);assert_eq!(st.capture(session,now),0);
        // The card keeps polling (heartbeat) while tracking is lost.
        for step in 1..=8u64{let t=now+Duration::from_millis(500*step);assert_eq!(st.snapshot(t,true).phase,3);st.observe(Err(Reason::LeftControllerNotTracked),t);}
        let snap=st.snapshot(now+Duration::from_millis(4500),true);assert_eq!((snap.phase,snap.failure,snap.result_session),(1,Reason::LeftControllerNotTracked as u32,session));
        assert!(st.calibration.is_none());let later=now+CAPTURE_TIMEOUT+SESSION_HEARTBEAT+Duration::from_secs(1);assert!(!st.suspends(later));
        assert_eq!(st.result.map(|r|r.1),Some(Err(Reason::SessionExpired)));
        let mut c=Capture::default();let Step::Done(cal)=capture_all(&mut c,0)else{panic!()};let mut st=State{controller:55,calibration:Some(cal),..State::default()};
        assert!(st.calibrated_hand(&narrow(&grip(0)),&rig(),false,now).is_none(),"artifact without left flags");
        let mut changed=rig();changed.hand_from_aux[13]=0.05;assert!(st.calibrated_hand(&narrow(&grip(0)),&changed,true,now).is_none(),"different character rig");
        st.controller=56;assert!(st.calibrated_hand(&narrow(&grip(0)),&rig(),true,now).is_none(),"different controller");st.controller=0;assert!(st.calibrated_hand(&narrow(&grip(0)),&rig(),true,now).is_none());
    }
    #[test]fn persisted_calibration_round_trips_and_rejects_corruption(){
        let mut c=Capture::default();let Step::Done(cal)=capture_all(&mut c,0)else{panic!()};let v=export(&cal);assert_eq!(import(&v),Some(cal));
        for corrupt in [SVLeftWristCalibration{version:2,..v},SVLeftWristCalibration{controller:0,..v},SVLeftWristCalibration{hand:12,..v},
            SVLeftWristCalibration{forearm_m:0.01,..v},SVLeftWristCalibration{samples:3,..v},SVLeftWristCalibration{grip_from_hand:{let mut g=v.grip_from_hand;g[0]*=1.1;g},..v}]{assert!(import(&corrupt).is_none());}
        unsafe{assert_eq!(sv_xr_left_wrist_import_v1(std::ptr::null(),0),0);}
    }
    /// Exact original38548 from the 38487-TRUE branch through the arm goal writes
    /// (ordinals278..=3457), arm0, blend1, identity request offset. Direct and
    /// indirect calls are type-shaped stubs that must never run on this path.
    #[cfg(feature="jit")]
    #[test]#[ignore="reads exact private original; executes only isolated original38548 goal arithmetic"]
    fn original_left_goal_takes_calibrated_orientation_only_with_native_flags256(){
        use wasm_encoder::{Instruction as I,BlockType,MemArg};use wasmtime::{Config,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder,Val,ValType as T};
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();
        const FRAME:u64=100_000;const SOLVER:u64=200_000;const SKEL:u64=300_000;const BANK:u64=400_000;
        let v=|o:u64|MemArg{offset:o,align:4,memory_index:0};
        let mut prologue=vec![I::I64Const(FRAME as i64),I::LocalSet(26),I::I64Const((SOLVER+32)as i64),I::LocalSet(27),I::I64Const((SOLVER+224)as i64),I::LocalSet(31),
            I::I64Const(0),I::LocalSet(28),I::LocalGet(2),I::LocalSet(29),I::I64Const((SOLVER+488)as i64),I::LocalSet(35),I::I64Const((SOLVER+224)as i64),I::LocalSet(36),
            I::I64Const((SOLVER+32)as i64),I::LocalSet(37)];
        for(offset,local)in[(224,3),(240,7),(256,4),(272,6)]{prologue.extend([I::I64Const(FRAME as i64),I::V128Load(v(offset)),I::LocalSet(local)]);}
        // The single outward exit (hand index >= bone count) lands after these.
        for _ in 0..4{prologue.push(I::Block(BlockType::Empty));}prologue.push(I::I32Const(1));
        let epilogue=vec![I::End,I::End,I::End,I::End,I::End];
        let slice=crate::xr_viewmodel_patch::slice::build(&original,38548,278,3457,278,&prologue,&epilogue,8).unwrap();
        let reference=at(mul(&rot([0.,0.,1.],131.),&rot([1.,0.,0.],4.)),[1210.3,-1353.4,28.9]);
        let target=mul(&at(mul(&rot([0.3,0.2,1.],-77.),&rot([1.,0.,0.],38.)),[1210.62,-1353.05,29.41]),&held());
        for flags in [256u32,0]{
            let mut c=Config::new();c.wasm_memory64(true).wasm_threads(true).shared_memory(true);let engine=Engine::new(&c).unwrap();
            let module=wasmtime::Module::new(&engine,&slice.wasm).unwrap();
            let memory=SharedMemory::new(&engine,MemoryTypeBuilder::new().min(8).max(Some(8)).memory64(true).shared(true).build().unwrap()).unwrap();
            let put=|a:u64,m:&W|{for c in 0..4{for r in 0..4{assert!(crate::write_u32(&memory,a+c*16+r*4,(m[(c*4+r)as usize]as f32).to_bits()));}}};
            put(FRAME+224,&reference);put(FRAME+96,&target);
            let current=at(rot([0.,1.,0.],20.),[0.3,-0.2,0.1]);put(BANK+5*64,&current);
            for(i,value)in[0f32,0.,0.,1.].into_iter().enumerate(){assert!(crate::write_u32(&memory,SOLVER+224+16+i as u64*4,value.to_bits()));}
            assert!(crate::write_u32(&memory,SOLVER+224+92,1f32.to_bits()));assert!(crate::write_u32(&memory,SOLVER+224+112,flags));
            assert!(crate::write_u32(&memory,SOLVER+32+72,5));assert!(crate::write_u64(&memory,SKEL+24,BANK));assert!(crate::write_u32(&memory,SKEL+32,16));
            let mut linker=Linker::new(&engine);let mut store=Store::new(&engine,());linker.define(&mut store,"env","memory",memory.clone()).unwrap();
            let vt=|t:&wasmparser::ValType|match t{wasmparser::ValType::I32=>T::I32,wasmparser::ValType::I64=>T::I64,wasmparser::ValType::F32=>T::F32,wasmparser::ValType::F64=>T::F64,_=>panic!("stub type")};
            for(index,ty)in &slice.calls{let ft=wasmtime::FuncType::new(&engine,ty.params().iter().map(vt),ty.results().iter().map(vt));let index=*index;
                linker.func_new("env",&format!("f{index}"),ft,move|_,_,_|panic!("original call {index} is outside the flags0/256 blend1 path")).unwrap();}
            let instance=linker.instantiate(&mut store,&module).unwrap();
            instance.get_func(&mut store,"slice").unwrap().call(&mut store,&[Val::I64(SOLVER as i64),Val::F32(0.016f32.to_bits()),Val::I64(SKEL as i64)],&mut []).unwrap();
            let goal:[f32;16]=std::array::from_fn(|i|f32::from_bits(crate::read_u32(&memory,SOLVER+32+i as u64*4).unwrap()));
            let world=mul(&reference,&wide(&goal));
            assert!(rotation_deg(&world,&target)<0.01,"flags {flags}");assert!((0..3).all(|i|(world[12+i]-target[12+i]).abs()<5e-4),"flags {flags}");
            let goal_flags=crate::read_u32(&memory,SOLVER+32+88).unwrap()>>16;assert_eq!(goal_flags&1,u32::from(flags==256),"flags {flags}: orientation bit");
            assert_eq!(crate::read_u32(&memory,SOLVER+32+64).unwrap()&255,1);
        }
    }
    #[test]fn native_hand_requires_blended_out_or_foreign_request_and_live_rig(){
        let m=native_fixture();let solver=50_000;m.u64(1024+3888+272,solver);m.u64(solver,SOLVER_VTABLE);m.u64(solver+480,1024);
        m.u64(20_159_720,64);m.u64(72,1024);m.u64(1024+3888,MANAGER_VTABLE);m.u64(1024+3888+352,1024);m.u64(1024+3888+264,10_000);m.put(1024+5324,0);
        m.put(solver+32+72,3|(2<<16));m.put(solver+32+76,1|(4<<16));m.put(solver+32+88,6);
        m.put(12_008,3);let mut k=identity();k[13]=0.08;m.matrix(13_000+4*64,&k);m.matrix(15_000+3*64,&identity());m.matrix(15_000+4*64,&k);
        let mut elbow=identity();elbow[12]=-0.26;m.matrix(15_000+2*64,&elbow);m.u64(10_008,44_000);m.matrix(44_000,&identity());
        m.put(solver+224+92,0f32.to_bits());m.put(solver+224+12,cookie(9));
        let(r,_,forearm,native)=native_hand(&mut Snapshot::new(&m),9).unwrap();assert_eq!((r.hand,r.aux,r.bone_count),(3,4,16));assert!((forearm-0.26).abs()<1e-6);assert!(native);
        m.put(solver+224+92,0.5f32.to_bits());assert!(!native_hand(&mut Snapshot::new(&m),9).unwrap().3,"our request still blending out");
        m.put(solver+224+12,1f32.to_bits());assert!(native_hand(&mut Snapshot::new(&m),9).unwrap().3,"stock request owns the slot");
        m.put(solver+32+76,4|(4<<16));assert_eq!(native_hand(&mut Snapshot::new(&m),9).err(),Some(Reason::LeftHandRigUnqualified));
    }
}

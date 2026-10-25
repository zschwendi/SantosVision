//! Immersive first-person view-model correction at the two certified completed
//! skinned-model seams: original 44415 CEntityDrawDataStreamPed::Draw and 44355
//! CDrawSkinnedEntityDC::ExecuteStatic. Source proof and seam liveness:
//! target/verification/immersive-recording-20261013/viewmodel-transform-report.md
//! and device-session-20261024/barrel-reticle21/completed-model-seams.json.
//!
//! The original first-person branch clones the current viewport, zeroes its
//! translation (Bz) and builds M = Bz*S*inverse(O) from the camera-relative
//! extension O. Its view*M = S*inverse(O): headset rotation, head translation
//! and eye offset all cancel, so tracked arms and the gun stay locked to the
//! face. The world already renders from Beye = B0*R, R = reference-relative head
//! times head-from-eye. Applying that same R gives M' = Bz*inverse(R)*inverse(Bz)*M
//! and view*M' = inverse(R)*S*inverse(O): the native first-person model, exactly
//! where the flat game places it relative to the game camera B0, seen from the
//! physical eye. Only this invocation's own completed stack model changes; flat,
//! third person, unmatched views and every other draw family keep the original.
//! This is no depth, coverage or displayed-pixel certificate.
use super::*;
use std::sync::atomic::AtomicU64;

const MODEL:u64=1072;
/// Both functions construct the cloned grcViewport at frame+128; camera at +192.
const CLONE_CAMERA:u64=128+192;
const VIEWPORT_CAMERA:u64=192;
const COORDINATES:[u64;12]=[0,4,8,16,20,24,32,36,40,48,52,56];
// B(renderer right/up/back) * S = C(game right/forward/up).
const GAME_TO_RENDERER:W=[1.,0.,0.,0., 0.,0.,-1.,0., 0.,1.,0.,0., 0.,0.,0.,1.];
/// Recomputed original arithmetic versus the completed stack model.
const NATIVE_MODEL_TOLERANCE:f64=2e-3;
/// pose.relative is bounded to 5 m by xr_view; allow the eye offset on top.
const MAX_HEAD_TRANSLATION_M:f64=5.5;
type W=[f64;16];

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum Family{StreamPed,Skinned}
impl Family{
    pub(super) fn from_index(index:i32)->Option<Self>{match index{44415=>Some(Self::StreamPed),44355=>Some(Self::Skinned),_=>None}}
    pub(super) fn index(self)->i32{match self{Self::StreamPed=>44415,Self::Skinned=>44355}}
    fn slot(self)->usize{match self{Self::StreamPed=>0,Self::Skinned=>1}}
    /// O = entity_from_game_camera (44355 includes the composed attachment).
    fn camera_relative(self)->u64{match self{Self::StreamPed=>80,Self::Skinned=>64}}
    /// The model the original NON-first-person branch draws. Observation only.
    fn draw_world(self)->u64{match self{Self::StreamPed=>16,Self::Skinned=>0}}
    /// Same draw-data predicate as the original branch (with context 7/19).
    fn first_person<M:Memory>(self,s:&mut Snapshot<'_,M>,draw:u64)->Option<bool>{match self{
        Self::StreamPed=>Some(s.u64(draw.checked_add(160)?)?&(1<<59)!=0),
        Self::Skinned=>Some(s.u32(draw.checked_add(136)?)?&64!=0)}}
}

fn wide(m:&[f32;16])->W{canonical(*m).map(f64::from)}
fn mul(a:&W,b:&W)->W{std::array::from_fn(|i|{let(c,r)=(i/4,i%4);(0..4).map(|k|a[k*4+r]*b[c*4+k]).sum()})}
fn rigid64(m:&W,eps:f64)->bool{
    if m.iter().any(|v|!v.is_finite())||[3,7,11].iter().any(|&i|m[i]!=0.)||m[15]!=1.{return false;}
    let dot=|a:usize,b:usize|(0..3).map(|r|m[a*4+r]*m[b*4+r]).sum::<f64>();
    let det=m[0]*(m[5]*m[10]-m[9]*m[6])-m[4]*(m[1]*m[10]-m[9]*m[2])+m[8]*(m[1]*m[6]-m[5]*m[2]);
    (0..3).all(|a|(0..3).all(|b|(dot(a,b)-f64::from(u8::from(a==b))).abs()<=eps))&&(det-1.).abs()<=eps
}
fn inverse64(m:&W)->W{
    let mut out=[0.;16];out[15]=1.;
    for c in 0..3{for r in 0..3{out[c*4+r]=m[r*4+c];}}
    for r in 0..3{out[12+r]=-(0..3).map(|k|out[k*4+r]*m[12+k]).sum::<f64>();}out
}
fn rotation_only(m:&W)->W{let mut out=*m;out[12]=0.;out[13]=0.;out[14]=0.;out}
fn translation_m(m:&W)->f64{(m[12]*m[12]+m[13]*m[13]+m[14]*m[14]).sqrt()}
fn rotation_deg(a:&W,b:&W)->f64{
    let trace:f64=(0..3).map(|c|(0..3).map(|r|a[c*4+r]*b[c*4+r]).sum::<f64>()).sum();
    ((trace-1.)/2.).clamp(-1.,1.).acos().to_degrees()
}
fn max_error(a:&W,b:&W)->f64{COORDINATES.iter().map(|&o|(a[(o/4)as usize]-b[(o/4)as usize]).abs()).fold(0.,f64::max)}

/// inverse(B0)*Beye: the reference-relative head and eye transform the world
/// view received at the qualified primary camera callback.
pub(super) fn base_from_eye(base:&[f32;16],eye:&[f32;16])->Option<W>{
    let(base,eye)=(wide(base),wide(eye));if !rigid64(&base,1e-4)||!rigid64(&eye,1e-4){return None;}
    let r=mul(&inverse64(&base),&eye);
    (rigid64(&r,1e-4)&&translation_m(&r)<=MAX_HEAD_TRANSLATION_M).then_some(r)
}
/// Bz*S*inverse(O): the original first-person model arithmetic.
pub(super) fn native_model(clone:&W,o:&[f32;16])->Option<W>{
    let o=wide(o);if !rigid64(&o,1e-3){return None;}
    Some(mul(&mul(&rotation_only(clone),&GAME_TO_RENDERER),&inverse64(&o)))
}
/// M' = Bz*inverse(R)*inverse(Bz)*M; padding is left to the caller.
pub(super) fn corrected_model(clone:&W,base_from_eye:&W,model:&W)->Option<[f32;16]>{
    let bz=rotation_only(clone);let t=mul(&mul(&bz,&inverse64(base_from_eye)),&inverse64(&bz));
    let out=mul(&t,model).map(|v|v as f32);out.iter().all(|v|v.is_finite()).then_some(out)
}

#[derive(Clone,Copy,Debug)]
pub(super) struct Applied{pub family:Family,pub view:crate::xr_view::WorldView,pub head_translation_m:f64,pub head_rotation_deg:f64,
    pub model_shift_m:f64,pub draw_world:Option<(f64,f64)>}
fn words<M:Memory>(s:&mut Snapshot<'_,M>,address:u64)->Option<[u32;16]>{
    let mut out=[0;16];for(i,v)in out.iter_mut().enumerate(){*v=s.u32(address.checked_add(i as u64*4)?)?;}Some(out)
}
fn floats(w:&[u32;16])->Option<[f32;16]>{let m=w.map(f32::from_bits);COORDINATES.iter().all(|&o|m[(o/4)as usize].is_finite()).then_some(m)}

/// Read-only qualification, then one write of the invocation's own 12 model
/// coordinates. Every rejection leaves the original native model untouched.
pub(super) fn qualify_and_correct<M:Memory>(mem:&M,family:Family,draw:u64,stack:u64,parent:u64,qualified:bool,
    lookup:impl Fn(&[f32;16])->Option<(crate::xr_view::WorldView,[f32;16])>)->Result<Applied,&'static str>{
    if !qualified||parent==0{return Err("original_first_person_branch_not_taken");}
    let mut s=Snapshot::new(mem);
    if !family.first_person(&mut s,draw).ok_or("draw_data_unreadable")?{return Err("draw_data_not_first_person");}
    let clone=words(&mut s,stack.checked_add(CLONE_CAMERA).ok_or("frame_range")?).ok_or("clone_viewport_unreadable")?;
    let camera=words(&mut s,parent.checked_add(VIEWPORT_CAMERA).ok_or("parent_range")?).ok_or("parent_viewport_unreadable")?;
    // The original clone copies the parent's basis and stores a zero position.
    if (0..3).any(|c|(0..3).any(|r|clone[c*4+r]!=camera[c*4+r]))||clone[12..15]!=[0;3]{return Err("clone_is_not_zeroed_parent_basis");}
    let camera=floats(&camera).ok_or("parent_camera_nonfinite")?;
    let(view,base)=lookup(&camera).ok_or("parent_is_not_installed_headset_eye")?;
    if !view.headset_applied{return Err("parent_is_not_installed_headset_eye");}
    let relative=base_from_eye(&base,&view.game_from_eye).ok_or("eye_transform_not_rigid_or_bounded")?;
    let o=floats(&words(&mut s,draw.checked_add(family.camera_relative()).ok_or("draw_range")?).ok_or("camera_relative_unreadable")?).ok_or("camera_relative_nonfinite")?;
    let raw=words(&mut s,stack.checked_add(MODEL).ok_or("frame_range")?).ok_or("model_unreadable")?;
    let model=wide(&floats(&raw).ok_or("model_nonfinite")?);
    let bz=wide(&floats(&clone).ok_or("clone_nonfinite")?);if !rigid64(&rotation_only(&bz),1e-4){return Err("clone_basis_not_rigid");}
    let native=native_model(&bz,&o).ok_or("camera_relative_not_rigid")?;
    if max_error(&native,&model)>NATIVE_MODEL_TOLERANCE{return Err("model_is_not_original_camera_relative_result");}
    let corrected=corrected_model(&bz,&relative,&model).ok_or("corrected_model_nonfinite")?;
    // Observation only: compare the camera-relative world placement B0*S*inv(O)
    // with the draw data's own world matrix. Never selects or alters a model.
    let draw_world=words(&mut s,draw.saturating_add(family.draw_world())).and_then(|w|floats(&w)).map(|m|wide(&m)).filter(|m|rigid64(m,1e-3))
        .map(|world|{let placed=mul(&mul(&wide(&base),&GAME_TO_RENDERER),&inverse64(&wide(&o)));
            ((0..3).map(|i|(placed[12+i]-world[12+i]).powi(2)).sum::<f64>().sqrt(),rotation_deg(&placed,&world))});
    if !s.unchanged(){return Err("snapshot_changed");}
    let applied=Applied{family,view,head_translation_m:translation_m(&relative),head_rotation_deg:rotation_deg(&relative,&[1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.]),
        model_shift_m:(0..3).map(|i|(f64::from(corrected[12+i])-model[12+i]).powi(2)).sum::<f64>().sqrt(),draw_world};
    if COORDINATES.iter().all(|&o|corrected[(o/4)as usize].to_bits()==raw[(o/4)as usize]){return Ok(applied);}
    for o in COORDINATES{if !mem.put(stack+MODEL+o,corrected[(o/4)as usize].to_bits()){return Err("model_write_failed");}}
    Ok(applied)
}

static HOOKS:[AtomicU64;2]=[AtomicU64::new(0),AtomicU64::new(0)];
static APPLIED:[AtomicU64;2]=[AtomicU64::new(0),AtomicU64::new(0)];
static LAST_GATE:Mutex<[Option<(Instant,&'static str)>;2]>=Mutex::new([None,None]);
#[derive(Clone,Copy,Default)]
struct AppliedObservations{latest:Option<(Instant,Applied)>,families:[Option<(Instant,Applied)>;2]}
impl AppliedObservations{
    fn record(&mut self,at:Instant,applied:Applied){
        self.latest=Some((at,applied));
        self.families[applied.family.slot()]=Some((at,applied));
    }
}
static LAST_APPLIED:Mutex<AppliedObservations>=Mutex::new(AppliedObservations{latest:None,families:[None,None]});
fn describe_applied(sample:Option<(Instant,Applied)>)->Option<serde_json::Value>{
    sample.map(|(at,a)|serde_json::json!({"age_ms":at.elapsed().as_millis(),"family":a.family.index(),"view_epoch":a.view.provenance_epoch,
        "camera_sequence":a.view.camera_sequence,"request_frame":a.view.request_frame,"head_pose_frame":a.view.head_pose_frame,
        "eye_index":a.view.eye_index,"head_translation_m":a.head_translation_m,"head_rotation_degrees":a.head_rotation_deg,
        "model_translation_shift_m":a.model_shift_m,"camera_relative_vs_draw_world":a.draw_world.map(|(t,r)|serde_json::json!({"translation_m":t,"rotation_degrees":r}))}))
}

pub(super) fn host_hook(caller:&mut wasmtime::Caller<'_,crate::HostState>,params:&[wasmtime::Val],results:&mut[wasmtime::Val],mem:&wasmtime::SharedMemory)->anyhow::Result<()>{
    use wasmtime::Val;
    anyhow::ensure!(params.len()==6&&results.is_empty(),"viewmodel import ABI mismatch");
    let(Val::I32(family),Val::I64(draw),Val::I64(stack),Val::I64(parent),Val::I32(qualified),Val::I64(tls))=(&params[0],&params[1],&params[2],&params[3],&params[4],&params[5])
        else{anyhow::bail!("viewmodel argument type mismatch")};
    let family=Family::from_index(*family).ok_or_else(||anyhow::anyhow!("unknown viewmodel family"))?;
    HOOKS[family.slot()].fetch_add(1,Ordering::Relaxed);
    let gate=if !crate::xr_motion_aim::viewmodel_enabled(){"viewmodel_not_requested"}
        else if !crate::xr_view::world_camera_enabled(){"immersive_world_camera_inactive"}
        else if *tls==0||active_stack(caller)!=Some(*stack as u64){"frame_or_tls_identity"}
        else{match qualify_and_correct(&Guest(mem),family,*draw as u64,*stack as u64,*parent as u64,*qualified!=0,crate::xr_view::installed_headset_eye){
            Ok(applied)=>{APPLIED[family.slot()].fetch_add(1,Ordering::Relaxed);LAST_APPLIED.lock().unwrap_or_else(|p|p.into_inner()).record(Instant::now(),applied);
                // Only a corrected completed model may attribute this invocation's
                // emissions to its displaced parent eye (default off; host reads only).
                let _=crate::xr_view::foreground_depth::bind_after_correction(caller.data(),mem,family.index() as u32,*stack as u64,*parent as u64,&applied.view);
                "applied"}
            Err(gate)=>gate}};
    LAST_GATE.lock().unwrap_or_else(|p|p.into_inner())[family.slot()]=Some((Instant::now(),gate));Ok(())
}
pub(super) fn status()->serde_json::Value{
    let gates=*LAST_GATE.lock().unwrap_or_else(|p|p.into_inner());let last=*LAST_APPLIED.lock().unwrap_or_else(|p|p.into_inner());
    let family=|f:Family|serde_json::json!({"first_person_hooks":HOOKS[f.slot()].load(Ordering::Relaxed),"applied":APPLIED[f.slot()].load(Ordering::Relaxed),
        "last_gate":gates[f.slot()].map(|(at,gate)|serde_json::json!({"gate":gate,"age_ms":at.elapsed().as_millis()})),
        "last_applied":describe_applied(last.families[f.slot()])});
    serde_json::json!({"requested":crate::xr_motion_aim::viewmodel_enabled(),"stream_ped_44415":family(Family::StreamPed),"skinned_44355":family(Family::Skinned),
        "last_applied":describe_applied(last.latest),
        "authority":"completed stack model matrix only; no displayed-pixel, depth, coverage or barrel-alignment certificate",
        "foreground_depth":crate::xr_view::foreground_depth::status_json()})
}

#[cfg(test)]mod tests{
    use super::*;
    use super::super::tests::TestMemory;
    fn rot(axis:[f64;3],deg:f64)->W{let n=(axis.iter().map(|v|v*v).sum::<f64>()).sqrt();let[x,y,z]=axis.map(|v|v/n);let(s,c)=deg.to_radians().sin_cos();let t=1.-c;
        [t*x*x+c,t*x*y+s*z,t*x*z-s*y,0., t*x*y-s*z,t*y*y+c,t*y*z+s*x,0., t*x*z+s*y,t*y*z-s*x,t*z*z+c,0., 0.,0.,0.,1.]}
    fn at(mut m:W,p:[f64;3])->W{m[12..15].copy_from_slice(&p);m}
    fn narrow(m:&W)->[f32;16]{m.map(|v|v as f32)}
    fn point(m:&W,p:[f64;3])->[f64;3]{std::array::from_fn(|r|(0..3).map(|k|m[k*4+r]*p[k]).sum::<f64>()+m[12+r])}
    fn close3(a:[f64;3],b:[f64;3],eps:f64)->bool{(0..3).all(|i|(a[i]-b[i]).abs()<=eps)}
    /// One coherent frame: game camera B0 (renderer axes), entity E, camera-relative
    /// O=inverse(E)*C with C=B0*S, and a physical head/eye R so Beye=B0*R.
    struct Scene{b0:W,e:W,o:W,r:W,beye:W}
    fn scene(head_deg:f64)->Scene{
        let b0=at(mul(&rot([0.,1.,0.],37.),&rot([1.,0.,0.],-8.)),[1210.5,-1353.25,29.75]);
        let e=at(rot([0.,0.,1.],131.),[1210.3,-1353.4,28.9]);
        let o=mul(&inverse64(&e),&mul(&b0,&GAME_TO_RENDERER));
        let r=at(mul(&rot([1.,0.,0.],-head_deg),&rot([0.,1.,0.],0.6*head_deg)),[0.031,-0.07,0.04]);
        Scene{beye:mul(&b0,&r),b0,e,o,r}
    }
    #[test]fn original_model_cancels_head_and_corrected_model_sees_world_fixed_hand_from_the_eye(){
        for head in [0.,12.,-35.,70.]{
            let sc=scene(head);let bz=rotation_only(&sc.beye);
            let native=native_model(&bz,&narrow(&sc.o)).unwrap();
            // Native: view*model = S*inverse(O), independent of the head.
            let native_view=mul(&inverse64(&bz),&native);let fixed=mul(&GAME_TO_RENDERER,&inverse64(&sc.o));
            assert!(max_error(&native_view,&fixed)<1e-4,"head {head}");
            let relative=base_from_eye(&narrow(&sc.b0),&narrow(&sc.beye)).unwrap();
            let corrected=wide(&corrected_model(&bz,&relative,&native).unwrap());
            // Corrected: view*M' = inverse(R)*S*inverse(O) = inverse(Beye)*B0*S*inverse(O).
            let view=mul(&inverse64(&bz),&corrected);let expected=mul(&inverse64(&sc.r),&fixed);
            assert!(max_error(&view,&expected)<2e-4,"head {head}");
            // A hand bone at a world pose W sits in entity space at inverse(E)*W.
            // Its corrected eye-space position equals inverse(Beye)*W, the
            // physical location; the native one stays at inverse(B0)*W.
            let hand_world=[1210.62,-1353.05,29.41];let local=point(&inverse64(&sc.e),hand_world);
            let eye=point(&view,local);assert!(close3(eye,point(&inverse64(&sc.beye),hand_world),5e-4),"head {head}");
            let head_locked=point(&native_view,local);assert!(close3(head_locked,point(&inverse64(&sc.b0),hand_world),5e-4));
            if head.abs()>1.{assert!(!close3(eye,head_locked,0.01));}
        }
    }
    #[test]fn tracked_grip_from_game_from_stage_appears_at_the_physical_controller_in_the_eye(){
        // xr_view: game_from_stage = Beye*inverse(stage_from_eye); desired hand =
        // game_from_stage*stage_from_grip. After native IK places the hand there,
        // the corrected first-person draw shows it at eye_from_grip exactly.
        let sc=scene(-40.);let stage_from_eye=at(rot([0.,1.,0.],25.),[0.2,1.6,-0.1]);
        let stage_from_grip=at(rot([1.,0.,0.],-60.),[0.35,1.05,-0.45]);
        let game_from_stage=mul(&sc.beye,&inverse64(&stage_from_eye));let hand=mul(&game_from_stage,&stage_from_grip);
        let local=point(&inverse64(&sc.e),[hand[12],hand[13],hand[14]]);
        let bz=rotation_only(&sc.beye);let native=native_model(&bz,&narrow(&sc.o)).unwrap();
        let relative=base_from_eye(&narrow(&sc.b0),&narrow(&sc.beye)).unwrap();
        let corrected=wide(&corrected_model(&bz,&relative,&native).unwrap());
        let eye=point(&mul(&inverse64(&bz),&corrected),local);
        let physical=mul(&inverse64(&stage_from_eye),&stage_from_grip);
        assert!(close3(eye,[physical[12],physical[13],physical[14]],1e-3),"{eye:?} {physical:?}");
    }
    #[test]fn identity_eye_transform_preserves_original_model(){
        let sc=scene(0.);let bz=rotation_only(&sc.b0);let native=narrow(&native_model(&bz,&narrow(&sc.o)).unwrap());
        let identity:W=[1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.];
        let out=corrected_model(&bz,&identity,&wide(&native)).unwrap();
        for o in COORDINATES{assert_eq!(out[(o/4)as usize].to_bits(),native[(o/4)as usize].to_bits());}
    }
    #[test]fn unbounded_or_nonrigid_eye_transforms_are_rejected(){
        let sc=scene(10.);let mut far=sc.beye;far[12]+=6.;assert!(base_from_eye(&narrow(&sc.b0),&narrow(&far)).is_none());
        let mut skew=sc.beye;skew[0]*=1.01;assert!(base_from_eye(&narrow(&sc.b0),&narrow(&skew)).is_none());
        assert!(base_from_eye(&narrow(&sc.b0),&narrow(&sc.beye)).is_some());
    }

    const DRAW:u64=60_000;const STACK:u64=70_000;const PARENT:u64=80_000;
    fn put_words(m:&TestMemory,a:u64,v:&[f32;16]){for(i,x)in v.iter().enumerate(){m.put(a+i as u64*4,x.to_bits());}}
    fn fixture(family:Family,head:f64)->(TestMemory,Scene,crate::xr_view::WorldView){
        let sc=scene(head);let m=TestMemory::default();let mut camera=narrow(&sc.beye);camera[3]=f32::from_bits(0x7fc0_0001);
        put_words(&m,PARENT+VIEWPORT_CAMERA,&camera);
        let mut clone=camera;clone[12]=0.;clone[13]=0.;clone[14]=0.;put_words(&m,STACK+CLONE_CAMERA,&clone);
        put_words(&m,DRAW+family.camera_relative(),&narrow(&sc.o));put_words(&m,DRAW+family.draw_world(),&narrow(&sc.e));
        let mut model=narrow(&native_model(&rotation_only(&sc.beye),&narrow(&sc.o)).unwrap());for i in [3,7,11,15]{model[i]=f32::from_bits(0xdead_0000+i as u32);}
        put_words(&m,STACK+MODEL,&model);
        match family{Family::StreamPed=>m.u64(DRAW+160,1<<59),Family::Skinned=>{m.put(DRAW+136,64);}}
        let view=crate::xr_view::WorldView{provenance_epoch:3,camera_sequence:9,request_frame:4,head_pose_frame:5,presentation_time_seconds:1.,eye_index:1,view_count:2,
            game_from_eye:narrow(&sc.beye),game_from_stage:identity(),projection_from_eye:identity(),depth_reverse_z:true,headset_applied:true,optics:crate::xr_view::ScopeOptics::default()};
        (m,sc,view)
    }
    #[test]fn arm_report_does_not_replace_gun_observation_or_its_view_identity(){
        let mut samples=AppliedObservations::default();
        for(family,camera,error)in [(Family::Skinned,71,(0.46,158.)),(Family::StreamPed,72,(0.,0.))]{
            let(m,sc,mut view)=fixture(family,15.);view.camera_sequence=camera;
            let mut applied=qualify_and_correct(&m,family,DRAW,STACK,PARENT,true,|_|Some((view,narrow(&sc.b0)))).unwrap();
            applied.draw_world=Some(error);
            samples.record(Instant::now(),applied);
        }
        let gun=describe_applied(samples.families[Family::Skinned.slot()]).unwrap();
        let arms=describe_applied(samples.families[Family::StreamPed.slot()]).unwrap();
        assert_eq!(gun["family"],44355);assert_eq!(gun["camera_sequence"],71);
        assert_eq!(gun["camera_relative_vs_draw_world"]["rotation_degrees"],158.);
        assert_eq!(arms["family"],44415);assert_eq!(arms["camera_sequence"],72);
        assert_eq!(arms["camera_relative_vs_draw_world"]["rotation_degrees"],0.);
        assert_eq!(describe_applied(samples.latest).unwrap()["family"],44415);
    }
    #[test]fn both_seams_rewrite_only_the_twelve_model_coordinates_after_full_qualification(){
        for family in [Family::StreamPed,Family::Skinned]{
            let(m,sc,view)=fixture(family,-30.);let before=m.0.borrow().clone();let base=narrow(&sc.b0);
            let lookup=|c:&[f32;16]|((0..4).all(|i|(0..3).all(|r|c[i*4+r].to_bits()==view.game_from_eye[i*4+r].to_bits()))).then_some((view,base));
            let applied=qualify_and_correct(&m,family,DRAW,STACK,PARENT,true,lookup).unwrap();
            assert!((applied.head_rotation_deg-rotation_deg(&sc.r,&[1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.])).abs()<1e-3);
            let (t,r)=applied.draw_world.unwrap();assert!(t<1e-3&&r<0.05,"coherent frame draw-world disagreement {t} {r}");
            let after=m.0.borrow().clone();
            for(address,value)in &after{if !(STACK+MODEL..STACK+MODEL+64).contains(address)||[12,28,44,60].contains(&(address-STACK-MODEL)){assert_eq!(before.get(address),Some(value),"{address}");}}
            let mut model=[0f32;16];for(i,v)in model.iter_mut().enumerate(){*v=f32::from_bits(after[&(STACK+MODEL+i as u64*4)]);}
            let view_model=mul(&inverse64(&rotation_only(&sc.beye)),&wide(&model));
            assert!(max_error(&view_model,&mul(&inverse64(&sc.r),&mul(&GAME_TO_RENDERER,&inverse64(&sc.o))))<2e-4);
        }
    }
    #[test]fn every_unqualified_branch_preserves_the_native_model(){
        let family=Family::Skinned;
        let cases:[(&str,fn(&TestMemory),bool,u64);7]=[
            ("original_first_person_branch_not_taken",|_|{},false,PARENT),
            ("original_first_person_branch_not_taken",|_|{},true,0),
            ("draw_data_not_first_person",|m|{m.put(DRAW+136,0);},true,PARENT),
            ("clone_is_not_zeroed_parent_basis",|m|{m.put(STACK+CLONE_CAMERA+48,0.01f32.to_bits());},true,PARENT),
            ("clone_is_not_zeroed_parent_basis",|m|{let w=m.word(STACK+CLONE_CAMERA).unwrap();m.put(STACK+CLONE_CAMERA,w^1);},true,PARENT),
            ("parent_is_not_installed_headset_eye",|m|{let w=m.word(PARENT+VIEWPORT_CAMERA+48).unwrap();m.put(PARENT+VIEWPORT_CAMERA+48,w^1);},true,PARENT),
            ("model_is_not_original_camera_relative_result",|m|{m.put(STACK+MODEL+48,(f32::from_bits(m.word(STACK+MODEL+48).unwrap())+0.01).to_bits());},true,PARENT)];
        for(expected,mutate,qualified,parent)in cases{
            let(m,sc,view)=fixture(family,25.);mutate(&m);let before=m.0.borrow().clone();let base=narrow(&sc.b0);
            let lookup=|c:&[f32;16]|((0..4).all(|i|(0..3).all(|r|c[i*4+r].to_bits()==view.game_from_eye[i*4+r].to_bits()))).then_some((view,base));
            assert_eq!(qualify_and_correct(&m,family,DRAW,STACK,parent,qualified,lookup).err(),Some(expected));
            assert_eq!(*m.0.borrow(),before,"{expected} wrote guest memory");
        }
        let(m,_,mut view)=fixture(family,25.);view.headset_applied=false;let before=m.0.borrow().clone();
        assert_eq!(qualify_and_correct(&m,family,DRAW,STACK,PARENT,true,|_|Some((view,identity()))).err(),Some("parent_is_not_installed_headset_eye"));assert_eq!(*m.0.borrow(),before);
        let(m,sc,view)=fixture(family,25.);let mut skew=narrow(&sc.o);skew[0]*=1.05;put_words(&m,DRAW+64,&skew);let base=narrow(&sc.b0);
        assert_eq!(qualify_and_correct(&m,family,DRAW,STACK,PARENT,true,|_|Some((view,base))).err(),Some("camera_relative_not_rigid"));
    }

    /// Executes the EXACT original operator ranges that build the completed model
    /// (44415 ordinals217..=499, 44355 408..=735 including its LOD distance),
    /// with each direct call replaced by an import of the identical original type,
    /// then the production seam instrumentation calling this production host code.
    #[cfg(feature="jit")]
    mod original_slices{
        use super::*;
        use std::sync::{Arc,Mutex as M};
        use wasm_encoder::Instruction as I;
        const FRAME:u64=100_000;const PARENT_VP:u64=200_000;const DRAW_DATA:u64=300_000;const LOD:u64=400_000;const CURRENT:u64=12_374_976;
        type Gates=Arc<M<Vec<Result<(),&'static str>>>>;
        fn mat34(memory:&wasmtime::SharedMemory,address:u64,m:&[f32;16]){for c in 0..4{for r in 0..4{assert!(crate::write_u32(memory,address+c*16+r*4,m[(c*4+r)as usize].to_bits()));}}}
        fn read34(memory:&wasmtime::SharedMemory,address:u64)->[f32;16]{std::array::from_fn(|i|f32::from_bits(crate::read_u32(memory,address+i as u64*4).unwrap()))}
        fn run(family:Family,head:f64,qualified:bool,first_person_flag:bool,installed:bool)->(wasmtime::SharedMemory,Scene,Gates,usize){
            use wasmtime::{Config,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder,Val,ValType as T};
            let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();
            let(function,first,last,seam,pages,prologue,epilogue,site):(u32,usize,usize,usize,u64,Vec<I<'static>>,Vec<I<'static>>,_)=match family{
                Family::StreamPed=>(44415,217,499,500,200,vec![I::I64Const(FRAME as i64),I::LocalSet(5),I::I64Const((FRAME+128)as i64),I::LocalSet(11),I::LocalGet(3),I::LocalSet(34)],
                    vec![I::Nop,I::End],(0u32,5u32,18u32,34u32,false)),
                Family::Skinned=>(44355,408,735,686,340,vec![I::LocalGet(0),I::I32WrapI64,I::LocalSet(8),I::I64Const(FRAME as i64),I::LocalSet(0),I::I64Const(DRAW_DATA as i64),I::LocalSet(1),I::I64Const((FRAME+128)as i64),I::LocalSet(2)],
                    vec![I::LocalGet(0),I::LocalGet(18),I::F32Store(wasm_encoder::MemArg{offset:2000,align:2,memory_index:0}),I::End],(1,0,6,8,true)),
            };
            let slice=crate::xr_viewmodel_patch::slice::build(&original,function,first,last,seam,&prologue,&epilogue,pages).unwrap();
            let calls=slice.calls.len()as u32;
            let sites=[crate::xr_viewmodel_patch::Site{function:calls,ordinal:slice.seam,family:family.index(),draw:site.0,stack:site.1,parent:site.2,qualified:site.3,reload:site.4}];
            let wasm=crate::xr_viewmodel_patch::instrument(&slice.wasm,&sites,0,None).unwrap();
            let mut c=Config::new();c.wasm_memory64(true).wasm_threads(true).shared_memory(true).wasm_simd(true);let engine=Engine::new(&c).unwrap();
            let module=wasmtime::Module::new(&engine,&wasm).unwrap();
            let memory=SharedMemory::new(&engine,MemoryTypeBuilder::new().min(pages).max(Some(pages)).memory64(true).shared(true).build().unwrap()).unwrap();
            let sc=scene(head);let mut beye=narrow(&sc.beye);beye[15]=1.;
            mat34(&memory,PARENT_VP+VIEWPORT_CAMERA,&beye);assert!(crate::write_u64(&memory,CURRENT,PARENT_VP));
            let (o,world)=(narrow(&sc.o),narrow(&sc.e));
            match family{Family::StreamPed=>{mat34(&memory,DRAW_DATA+80,&o);mat34(&memory,DRAW_DATA+16,&world);assert!(crate::write_u64(&memory,DRAW_DATA+160,if first_person_flag{1<<59}else{0}));}
                Family::Skinned=>{mat34(&memory,DRAW_DATA+64,&o);mat34(&memory,DRAW_DATA,&world);assert!(crate::write_u32(&memory,DRAW_DATA+136,if first_person_flag{64}else{0}));
                    assert!(crate::write_u64(&memory,11_012_152,LOD));for(i,v)in[0.4f32,-0.3,0.2].into_iter().enumerate(){assert!(crate::write_u32(&memory,LOD+96+i as u64*4,v.to_bits()));}}}
            let mut linker=Linker::new(&engine);let mut store=Store::new(&engine,());linker.define(&mut store,"env","memory",memory.clone()).unwrap();
            let vt=|t:&wasmparser::ValType|match t{wasmparser::ValType::I32=>T::I32,wasmparser::ValType::I64=>T::I64,wasmparser::ValType::F32=>T::F32,wasmparser::ValType::F64=>T::F64,_=>panic!("stub type")};
            let stubs=Arc::new(M::new(Vec::<u32>::new()));
            for(index,ty)in slice.calls.iter(){let ft=wasmtime::FuncType::new(&engine,ty.params().iter().map(vt),ty.results().iter().map(vt));let mem=memory.clone();let index=*index;let seen=stubs.clone();
                linker.func_new("env",&format!("f{index}"),ft,move|_,p,r|{seen.lock().unwrap().push(index);let a=|i:usize|p[i].unwrap_i64()as u64;match index{
                    // grcViewport::operator= copies the source viewport, including its camera.
                    9653=>{let bytes=crate::read_bytes(&mem,a(1),944);assert!(crate::write_bytes(&mem,a(0),&bytes));if let Some(v)=r.get_mut(0){*v=Val::I64(a(0)as i64);}}
                    9603|64700=>{}
                    9636=>{let bytes=crate::read_bytes(&mem,a(1),64);assert!(crate::write_bytes(&mem,a(0)+192,&bytes));}
                    9614=>{let old=crate::read_u64(&mem,CURRENT).unwrap();assert!(crate::write_u64(&mem,CURRENT,a(0)));r[0]=Val::I64(old as i64);}
                    1062=>{let m=read34(&mem,a(1));let inv=inverse64(&wide(&m)).map(|v|v as f32);mat34(&mem,a(0),&inv);}
                    other=>panic!("unexpected original call {other}")}Ok(())}).unwrap();}
            let gates:Gates=Arc::default();let sink=gates.clone();let mem=memory.clone();let view=crate::xr_view::WorldView{provenance_epoch:3,camera_sequence:9,request_frame:4,head_pose_frame:5,
                presentation_time_seconds:1.,eye_index:0,view_count:2,game_from_eye:beye,game_from_stage:identity(),projection_from_eye:identity(),depth_reverse_z:true,headset_applied:true,optics:crate::xr_view::ScopeOptics::default()};
            let base=narrow(&sc.b0);
            linker.func_wrap(IMPORT_MODULE,crate::xr_viewmodel_patch::IMPORT,move|f:i32,draw:i64,frame:i64,parent:i64,qualified:i32,_tls:i64|{
                let lookup=|c:&[f32;16]|(installed&&(0..4).all(|i|(0..3).all(|r|c[i*4+r].to_bits()==view.game_from_eye[i*4+r].to_bits()))).then_some((view,base));
                sink.lock().unwrap().push(qualify_and_correct(&Guest(&mem),Family::from_index(f).unwrap(),draw as u64,frame as u64,parent as u64,qualified!=0,lookup).map(|_|()));}).unwrap();
            let instance=linker.instantiate(&mut store,&module).unwrap();let slice_fn=instance.get_func(&mut store,"slice").unwrap();
            let args:Vec<Val>=match family{Family::StreamPed=>vec![Val::I64(DRAW_DATA as i64),Val::I64(0),Val::I64(0),Val::I32(i32::from(qualified)),Val::I32(0)],Family::Skinned=>vec![Val::I64(i64::from(qualified))]};
            slice_fn.call(&mut store,&args,&mut []).unwrap();let n=stubs.lock().unwrap().len();(memory,sc,gates,n)
        }
        fn eye_space(memory:&wasmtime::SharedMemory,sc:&Scene)->W{mul(&inverse64(&rotation_only(&sc.beye)),&wide(&read34(memory,FRAME+MODEL)))}
        #[test]#[ignore="reads exact private original; executes only isolated original model arithmetic"]
        fn original_first_person_models_are_corrected_and_feed_native_continuations(){
            for family in [Family::StreamPed,Family::Skinned]{for head in [-35.,0.5,48.]{
                let(memory,sc,gates,calls)=run(family,head,true,true,true);assert_eq!(*gates.lock().unwrap(),vec![Ok(())],"{family:?} {head}");assert!(calls>=5);
                // Original arithmetic produced Bz*S*inverse(O) (else qualification rejects);
                // the corrected model now gives view*M' = inverse(R)*S*inverse(O).
                let expected=mul(&inverse64(&sc.r),&mul(&GAME_TO_RENDERER,&inverse64(&sc.o)));
                assert!(max_error(&eye_space(&memory,&sc),&expected)<5e-4,"{family:?} {head}");
                if family==Family::Skinned{
                    // Native LOD distance consumes the RELOADED corrected translation.
                    let m=read34(&memory,FRAME+MODEL);let lod=[0.4f32,-0.3,0.2];
                    let corrected=((m[12]-lod[0]).powi(2)+(m[13]-lod[1]).powi(2)+(m[14]-lod[2]).powi(2)).sqrt();
                    let distance=f32::from_bits(crate::read_u32(&memory,FRAME+2000).unwrap());assert!((distance-corrected).abs()<1e-5,"{distance} {corrected}");
                    let native=native_model(&rotation_only(&wide(&narrow(&sc.beye))),&narrow(&sc.o)).unwrap();
                    let stale=((native[12]as f32-lod[0]).powi(2)+(native[13]as f32-lod[1]).powi(2)+(native[14]as f32-lod[2]).powi(2)).sqrt();
                    if head.abs()>1.{assert!((distance-stale).abs()>1e-3);}
                }
            }}
        }
        #[test]#[ignore="reads exact private original; executes only isolated original model arithmetic"]
        fn original_non_first_person_and_unmatched_views_keep_native_models(){
            for family in [Family::StreamPed,Family::Skinned]{
                // Context or draw flag absent: the original world branch runs, its
                // parent local stays zero and the guest never calls the host.
                for(qualified,flag)in[(false,true),(true,false)]{
                    let(memory,sc,gates,_)=run(family,30.,qualified,flag,true);assert!(gates.lock().unwrap().is_empty());
                    let m=read34(&memory,FRAME+MODEL);let e=narrow(&sc.e);for o in COORDINATES{assert_eq!(m[(o/4)as usize].to_bits(),e[(o/4)as usize].to_bits());}
                }
                // First-person branch without an installed headset eye keeps the native head-locked model.
                let(memory,sc,gates,_)=run(family,30.,true,true,false);assert_eq!(*gates.lock().unwrap(),vec![Err("parent_is_not_installed_headset_eye")]);
                let native=mul(&GAME_TO_RENDERER,&inverse64(&sc.o));assert!(max_error(&eye_space(&memory,&sc),&native)<5e-4);
            }
        }
    }
}

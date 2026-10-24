//! Bounded observations only. No eligibility, source age, lease, guest state or
//! firing authority is changed. Last immersive values survive leaving the view.
use super::*;
#[derive(Clone)]
struct Target{at:Instant,token:u32,generation:(u64,u64,u64),desired:[f32;16],reference:[f32;16],stock:[f32;16],blend:Option<f32>,before:Option<Bones>,capability:PoseCapability}
#[derive(Clone,Copy)]
struct Bones{actual:[f32;16],goal:[f32;16],upper:[f32;16],active:u32,orientation:u16,blend:f32}
#[derive(Clone)]
struct Rendered{at:Instant,target:Target,bones:Bones,drawable:Option<([f32;16],bool)>,same_cookie:bool}
static TARGETS:Mutex<[Option<Target>;2]>=Mutex::new([None,None]);
static RENDERED:Mutex<[Option<Rendered>;2]>=Mutex::new([None,None]);
fn position(m:&[f32;16])->[f32;3]{[m[12],m[13],m[14]]}
fn distance(a:&[f32;16],b:&[f32;16])->f32{(0..3).map(|i|(a[12+i]-b[12+i]).powi(2)).sum::<f32>().sqrt()}
fn finite(value:u32)->Option<f32>{let f=f32::from_bits(value);f.is_finite().then_some(f)}
fn bone_world<M:Memory>(s:&mut Snapshot<'_,M>,skel:u64,bone:u16)->Option<[f32;16]>{
    let count=s.u32(skel.checked_add(32)?)?;if count==0||count>MAX_BONES||u32::from(bone)>=count{return None;}
    let reference=s.u64(skel.checked_add(8)?)?;if reference==0{return None;}let reference=s.matrix(reference)?;
    let bank=s.u64(skel.checked_add(24)?)?;if bank==0{return None;}
    Some(multiply(&reference,&s.matrix(bank.checked_add(u64::from(bone)*64)?)?))
}
fn bones<M:Memory>(s:&mut Snapshot<'_,M>,skel:u64,goal:u64,request:u64)->Option<Bones>{
    let hand=s.u16(goal.checked_add(72)?)?;let upper=s.u16(goal.checked_add(76)?)?;
    let actual=bone_world(s,skel,hand)?;let upper=bone_world(s,skel,upper)?;
    let reference=s.u64(skel.checked_add(8)?)?;let reference=s.matrix(reference)?;
    Some(Bones{actual,upper,goal:multiply(&reference,&s.matrix(goal)?),active:s.u32(goal.checked_add(64)?)?&255,
        orientation:s.u16(goal.checked_add(90)?)?,blend:finite(s.u32(request.checked_add(92)?)?)?})
}
pub(super) fn target<M:Memory>(mem:&M,_ped:u64,skel:u64,goal:u64,request:u64,lease:&Lease,stock:[f32;16],reference:[f32;16]){
    let mut s=Snapshot::new(mem);let before=bones(&mut s,skel,goal,request);
    let blend=s.u32(request.saturating_add(92)).and_then(finite);
    if !s.unchanged(){return;}
    TARGETS.lock().unwrap_or_else(|p|p.into_inner())[lease.arm as usize]=Some(Target{at:Instant::now(),token:lease.token,generation:lease.generation,
        desired:lease.hand,reference,stock,blend,before,capability:lease.capability});
}
fn render_one<M:Memory>(s:&mut Snapshot<'_,M>,ped:u64,skel:u64,solver:u64,arm:usize,target:Target)->Option<Rendered>{
    let goal=solver.checked_add(32+96*arm as u64)?;let request=solver.checked_add(224+128*arm as u64)?;
    let same_cookie=s.u32(request.checked_add(12)?)?==cookie(target.token);
    let output=bones(s,skel,goal,request)?;
    // Optional independent normal drawable route. Require matching skeleton
    // metadata before reusing this hand index; report distinct bank identity.
    let drawable=(||{
        let drawable=s.u64(ped.checked_add(80)?)?;if drawable==0{return None;}
        let draw_skel=s.u64(drawable.checked_add(40)?)?;if draw_skel==0||s.u64(draw_skel)?!=s.u64(skel)?{return None;}
        let hand=s.u16(goal.checked_add(72)?)?;Some((bone_world(s,draw_skel,hand)?,draw_skel==skel))
    })();
    Some(Rendered{at:Instant::now(),target,bones:output,drawable,same_cookie})
}
pub(super) fn render<M:Memory>(mem:&M,epoch:u64){
    let targets=TARGETS.lock().unwrap_or_else(|p|p.into_inner()).clone();
    let mut s=Snapshot::new(mem);
    let Some(ped)=observed_local_ped(mem)else{return;};let Some((_,skel))=manager(&mut s,ped.saturating_add(3888))else{return;};
    let Some(solver)=s.u64(ped.saturating_add(3888+272)).filter(|p|*p!=0)else{return;};
    if s.u64(solver)!=Some(SOLVER_VTABLE)||s.u64(solver.saturating_add(480))!=Some(ped){return;}
    let observed=std::array::from_fn::<_,2,_>(|arm|targets[arm].clone().filter(|t|t.generation.0==epoch)
        .and_then(|t|render_one(&mut s,ped,skel,solver,arm,t)));
    if !s.unchanged(){return;}
    let mut retained=RENDERED.lock().unwrap_or_else(|p|p.into_inner());for arm in 0..2{if observed[arm].is_some(){retained[arm]=observed[arm].clone();}}
}
fn describe_bones(b:Bones,target:&[f32;16])->serde_json::Value{serde_json::json!({
    "actual_hand_world":position(&b.actual),"actual_hand_x_axis":&b.actual[..3],"native_goal_world":position(&b.goal),"upper_arm_world":position(&b.upper),
    "actual_to_desired_m":distance(&b.actual,target),"goal_to_desired_m":distance(&b.goal,target),"upper_to_desired_m":distance(&b.upper,target),
    "goal_active_byte":b.active,"goal_orientation_flags":b.orientation,"request_blend_weight":b.blend})}
fn describe_target(t:&Target)->serde_json::Value{serde_json::json!({"age_ms":t.at.elapsed().as_millis(),"view_epoch":t.generation.0,"camera_sequence":t.generation.1,"hand_sequence":t.generation.2,
    "desired_hand_world":position(&t.desired),"desired_hand_x_axis":t.capability.full_pose().then_some(&t.desired[..3]),"orientation_qualified":t.capability.full_pose(),"stock_target_world":position(&t.stock),"reference_world":position(&t.reference),
    "request_blend_weight":t.blend,"capability":t.capability.name(),"before_native_blend":t.before.map(|b|describe_bones(b,&t.desired))})}
pub(super) fn status()->serde_json::Value{
    let targets=TARGETS.lock().unwrap_or_else(|p|p.into_inner()).clone();let rendered=RENDERED.lock().unwrap_or_else(|p|p.into_inner()).clone();
    let arm=|index:usize|serde_json::json!({"last_applied_target":targets[index].as_ref().map(describe_target),"last_native_cache_stage":rendered[index].as_ref().map(|r:&Rendered|serde_json::json!({
        "age_ms":r.at.elapsed().as_millis(),"applied_target":describe_target(&r.target),"request_cookie_still_matches":r.same_cookie,"bones":describe_bones(r.bones,&r.target.desired),
        "drawable_skeleton_hand_world":r.drawable.map(|(m,_)|position(&m)),"drawable_skeleton_matches_manager":r.drawable.map(|(_,same)|same)}))});
    serde_json::json!({"left":arm(0usize),"right":arm(1usize),"authority":"historical numeric observations only; no pose or render acceptance"})
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn observed_native_goal_and_real_hand_remain_distinct_in_rotated_reference(){
        let m=super::super::tests::native_fixture();let mut reference=identity();
        reference[0]=0.;reference[1]=1.;reference[4]=-1.;reference[5]=0.;reference[12]=10.;reference[13]=20.;reference[14]=30.;
        m.u64(10_008,44_000);m.matrix(44_000,&reference);
        let goal=50_000;let request=52_000;let mut desired=identity();desired[12]=3.;desired[13]=4.;desired[14]=5.;m.matrix(goal,&desired);
        m.put(goal+64,1);m.put(goal+72,5);m.put(goal+76,3);m.put(goal+88,1<<16);m.put(request+92,0.25f32.to_bits());
        m.matrix(15_000+3*64,&identity());
        let original=bones(&mut Snapshot::new(&m),10_000,goal,request).unwrap();
        assert_eq!(position(&original.actual),[8.,20.,30.]);assert_eq!(position(&original.goal),[6.,23.,35.]);
        assert_eq!((original.active,original.orientation,original.blend),(1,1,0.25));
        // Distinguish real global output moving from a goal that remains fixed.
        let mut moved=identity();moved[13]=2.2;m.matrix(15_000+5*64,&moved);
        let changed=bones(&mut Snapshot::new(&m),10_000,goal,request).unwrap();
        assert!((distance(&changed.actual,&original.actual)-0.2).abs()<1e-5);assert_eq!(changed.goal,original.goal);
        m.put(request+92,f32::NAN.to_bits());assert!(bones(&mut Snapshot::new(&m),10_000,goal,request).is_none());
    }
}

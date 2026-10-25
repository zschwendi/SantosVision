//! Bounded observations only. No eligibility, source age, lease, guest state or
//! firing authority is changed. Last immersive values survive leaving the view.
use super::*;
#[derive(Clone)]
struct Target{at:Instant,token:u32,generation:(u64,u64,u64),tracking:u64,desired:[f32;16],reference:[f32;16],stock:[f32;16],blend:Option<f32>,before:Option<Bones>,capability:PoseCapability}
#[derive(Clone,Copy)]
struct Bones{actual:[f32;16],goal:[f32;16],upper:[f32;16],active:u32,orientation:u16,blend:f32}
#[derive(Clone)]
struct Rendered{at:Instant,target:Target,bones:Bones,drawable:Option<([f32;16],bool)>,same_cookie:bool}
static TARGETS:Mutex<[Option<Target>;2]>=Mutex::new([None,None]);
static RENDERED:Mutex<[Option<Rendered>;2]>=Mutex::new([None,None]);
/// Only a transient, per-Store diagnostic pairing; never used for eligibility
/// or dereferenced later without revalidating the current native owner chain.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct GoalSite{pub solver:u64,pub passed:u64,pub resolved:u64,pub arm:u64,pub goal:u64,pub request:u64,pub scratch:u64,pub tls:u64}
#[derive(Clone)]pub(super) struct PendingGoal{site:GoalSite,ped:u64,target:Target,flags:u32}
#[derive(Default)]struct FpCandidate{skeleton:Option<u64>,metadata_matches:Option<bool>,bone_count:Option<u32>,global_bank:Option<u64>,manager_bone_count:Option<u32>,manager_global_bank:Option<u64>,hand:Option<[f32;16]>,manager_hand:Option<[f32;16]>}
struct CompletedGoal{at:Instant,elapsed:Duration,pending:PendingGoal,relative:[f32;16],world:[f32;16],active:u32,orientation:u16,blend:f32,fp:FpCandidate}
#[derive(Default)]struct GoalAudit{calls:u64,accepted:u64,last:Option<(Instant,&'static str)>,sample:Option<CompletedGoal>}
static COMPLETED:std::sync::LazyLock<Mutex<[GoalAudit;2]>>=std::sync::LazyLock::new(||Mutex::new([GoalAudit::default(),GoalAudit::default()]));
fn position(m:&[f32;16])->[f32;3]{[m[12],m[13],m[14]]}
fn distance(a:&[f32;16],b:&[f32;16])->f32{(0..3).map(|i|(a[12+i]-b[12+i]).powi(2)).sum::<f32>().sqrt()}
fn finite(value:u32)->Option<f32>{let f=f32::from_bits(value);f.is_finite().then_some(f)}
pub(super) fn bone_world<M:Memory>(s:&mut Snapshot<'_,M>,skel:u64,bone:u16)->Option<[f32;16]>{
    let count=s.u32(skel.checked_add(32)?)?;if count==0||count>MAX_BONES||u32::from(bone)>=count{return None;}
    let reference=s.u64(skel.checked_add(8)?)?;if reference==0{return None;}let reference=s.matrix(reference)?;
    let bank=s.u64(skel.checked_add(24)?)?;if bank==0{return None;}
    Some(multiply(&reference,&s.matrix(bank.checked_add(u64::from(bone)*64)?)?))
}
fn first_person_candidate<M:Memory>(mem:&M,ped:u64,skel:u64,goal:u64)->FpCandidate{
    // Optional and independently coherent: missing/invalid/changing FP data
    // cannot reject the exact completed-goal observation. This is a candidate
    // arm palette source; actual44414 draw selection remains UNKNOWN here.
    let mut s=Snapshot::new(mem);let mut out=FpCandidate::default();
    out.manager_bone_count=skel.checked_add(32).and_then(|a|s.u32(a));out.manager_global_bank=skel.checked_add(24).and_then(|a|s.u64(a));
    let hand=goal.checked_add(72).and_then(|a|s.u16(a));
    out.manager_hand=hand.and_then(|h|bone_world(&mut s,skel,h));
    out.skeleton=ped.checked_add(4256).and_then(|a|s.u64(a)).filter(|a|*a!=0);
    if let Some(fp)=out.skeleton{
        out.metadata_matches=s.u64(fp).zip(s.u64(skel)).map(|(a,b)|a!=0&&a==b);
        out.bone_count=fp.checked_add(32).and_then(|a|s.u32(a));
        out.global_bank=fp.checked_add(24).and_then(|a|s.u64(a)).filter(|a|*a!=0);
        if out.metadata_matches==Some(true)&&out.bone_count.is_some()&&out.bone_count==out.manager_bone_count{out.hand=hand.and_then(|h|bone_world(&mut s,fp,h));}
    }
    if s.unchanged(){out}else{FpCandidate::default()}
}
fn bones<M:Memory>(s:&mut Snapshot<'_,M>,skel:u64,goal:u64,request:u64)->Option<Bones>{
    let hand=s.u16(goal.checked_add(72)?)?;let upper=s.u16(goal.checked_add(76)?)?;
    let actual=bone_world(s,skel,hand)?;let upper=bone_world(s,skel,upper)?;
    let reference=s.u64(skel.checked_add(8)?)?;let reference=s.matrix(reference)?;
    Some(Bones{actual,upper,goal:multiply(&reference,&s.matrix(goal)?),active:s.u32(goal.checked_add(64)?)?&255,
        orientation:s.u16(goal.checked_add(90)?)?,blend:finite(s.u32(request.checked_add(92)?)?)?})
}
pub(super) fn target<M:Memory>(mem:&M,ped:u64,site:GoalSite,lease:&Lease,stock:[f32;16],reference:[f32;16])->Option<PendingGoal>{
    let(skel,goal,request)=(site.resolved,site.goal,site.request);
    let mut s=Snapshot::new(mem);let before=bones(&mut s,skel,goal,request);
    let blend=s.u32(request.saturating_add(92)).and_then(finite);
    if !s.unchanged(){return None;}
    let target=Target{at:Instant::now(),token:lease.token,generation:lease.generation,tracking:lease.tracking,
        desired:lease.hand,reference,stock,blend,before,capability:lease.capability};
    TARGETS.lock().unwrap_or_else(|p|p.into_inner())[lease.arm as usize]=Some(target.clone());
    Some(PendingGoal{site,ped,target,flags:lease.flags})
}
fn completed_goal<M:Memory>(mem:&M,pending:Option<PendingGoal>,site:GoalSite,generation:Option<((u64,u64,u64),u64)>,stack_matches:bool)->Result<CompletedGoal,&'static str>{
    let p=pending.ok_or("no_applied_target_in_this_invocation")?;
    if !stack_matches||p.site!=site{return Err("invocation_identity_changed");}
    if p.target.at.elapsed()>MAX_AGE||generation!=Some((p.target.generation,p.target.tracking)){return Err("applied_target_stale_or_generation_changed");}
    let mut s=Snapshot::new(mem);
    let(owner,skel)=manager(&mut s,p.ped.checked_add(3888).ok_or("owner_overflow")?).ok_or("local_manager_skeleton_changed")?;
    if owner!=p.ped||skel!=site.passed||skel!=site.resolved||s.u64(p.ped.checked_add(4160).ok_or("owner_overflow")?)!=Some(site.solver)
        ||s.u64(site.solver)!=Some(SOLVER_VTABLE)||s.u64(site.solver.checked_add(480).ok_or("solver_overflow")?)!=Some(p.ped)
        ||site.arm>1||site.solver.checked_add(32+96*site.arm)!=Some(site.goal)||site.solver.checked_add(224+128*site.arm)!=Some(site.request){return Err("native_owner_or_goal_identity_changed");}
    if s.u32(site.request.checked_add(12).ok_or("request_overflow")?)!=Some(cookie(p.target.token))
        ||s.u32(site.request.checked_add(112).ok_or("request_overflow")?)!=Some(p.flags)
        ||s.u64(site.request.checked_add(80).ok_or("request_overflow")?)!=Some(0){return Err("request_cookie_or_flags_changed");}
    for i in 0..3{if s.u32(site.request.checked_add(i*4).ok_or("request_overflow")?)!=Some(p.target.desired[12+i as usize].to_bits()){return Err("request_position_changed");}}
    let reference=s.u64(skel.checked_add(8).ok_or("skeleton_overflow")?).and_then(|a|s.matrix(a)).ok_or("reference_invalid")?;
    if reference!=p.target.reference{return Err("reference_changed");}
    let relative=s.matrix(site.goal).ok_or("completed_goal_invalid")?;
    let active=s.u32(site.goal.checked_add(64).ok_or("goal_overflow")?).ok_or("goal_active_missing")?&255;
    let orientation=s.u16(site.goal.checked_add(90).ok_or("goal_overflow")?).ok_or("goal_flags_missing")?;
    let blend=s.u32(site.request.checked_add(92).ok_or("request_overflow")?).and_then(finite).ok_or("request_blend_invalid")?;
    if active!=1{return Err("native_goal_not_active");}
    if !s.unchanged(){return Err("post_goal_snapshot_changed");}
    let elapsed=p.target.at.elapsed();let world=multiply(&reference,&relative);
    let fp=first_person_candidate(mem,p.ped,skel,site.goal);
    if !s.unchanged(){return Err("post_goal_snapshot_changed");}
    Ok(CompletedGoal{at:Instant::now(),elapsed,pending:p,relative,world,active,orientation,blend,fp})
}
pub(super) fn post_goal<M:Memory>(mem:&M,pending:Option<PendingGoal>,site:GoalSite,generation:Option<((u64,u64,u64),u64)>,stack_matches:bool){
    if site.arm>1{return;}let result=completed_goal(mem,pending,site,generation,stack_matches);
    let mut audit=COMPLETED.lock().unwrap_or_else(|p|p.into_inner());let a=&mut audit[site.arm as usize];a.calls=a.calls.saturating_add(1);
    match result{Ok(sample)=>{a.accepted=a.accepted.saturating_add(1);a.last=Some((Instant::now(),"observed_completed_native_goal"));a.sample=Some(sample);},Err(gate)=>a.last=Some((Instant::now(),gate))}
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
fn describe_target(t:&Target)->serde_json::Value{serde_json::json!({"age_ms":t.at.elapsed().as_millis(),"request_token":t.token,"view_epoch":t.generation.0,"camera_sequence":t.generation.1,"hand_sequence":t.generation.2,"tracking_epoch":t.tracking,
    "desired_hand_world":position(&t.desired),"desired_hand_x_axis":t.capability.full_pose().then_some(&t.desired[..3]),"orientation_qualified":t.capability.full_pose(),"stock_target_world":position(&t.stock),"reference_world":position(&t.reference),
    "request_blend_weight":t.blend,"capability":t.capability.name(),"before_native_blend":t.before.map(|b|describe_bones(b,&t.desired))})}
pub(super) fn status()->serde_json::Value{
    let targets=TARGETS.lock().unwrap_or_else(|p|p.into_inner()).clone();let rendered=RENDERED.lock().unwrap_or_else(|p|p.into_inner()).clone();
    let completed=COMPLETED.lock().unwrap_or_else(|p|p.into_inner());
    let arm=|index:usize|serde_json::json!({"last_applied_target":targets[index].as_ref().map(describe_target),"post_native_goal":{"calls":completed[index].calls,"accepted":completed[index].accepted,
        "last_gate":completed[index].last.map(|(at,gate)|serde_json::json!({"age_ms":at.elapsed().as_millis(),"gate":gate})),
        "last_sample":completed[index].sample.as_ref().map(|s|{let p=&s.pending;serde_json::json!({"age_ms":s.at.elapsed().as_millis(),"target_to_goal_ms":s.elapsed.as_secs_f64()*1000.,
            "applied_target":describe_target(&p.target),"native_goal_reference_matrix":s.relative,"native_goal_world_matrix":s.world,"goal_to_desired_m":distance(&s.world,&p.target.desired),
            "goal_active_byte":s.active,"goal_orientation_flags":s.orientation,"request_flags":p.flags,"request_blend_weight":s.blend,
            "identity":{"ped":p.ped,"manager":p.ped+3888,"solver":p.site.solver,"passed_skeleton":p.site.passed,"resolved_skeleton":p.site.resolved,"goal":p.site.goal,"request":p.site.request},
            "first_person_candidate":{"draw_selection":"unknown","skeleton":s.fp.skeleton,"skeleton_matches_manager":s.fp.skeleton.map(|skel|skel==p.site.resolved),
                "metadata_matches_manager":s.fp.metadata_matches,"bone_count":s.fp.bone_count,"global_bank":s.fp.global_bank,
                "manager_bone_count":s.fp.manager_bone_count,"manager_global_bank":s.fp.manager_global_bank,
                "hand_world_matrix":s.fp.hand,"manager_hand_world_matrix":s.fp.manager_hand,
                "hand_to_desired_m":s.fp.hand.map(|m|distance(&m,&p.target.desired)),"hand_to_native_goal_m":s.fp.hand.map(|m|distance(&m,&s.world)),
                "hand_to_manager_hand_m":s.fp.hand.zip(s.fp.manager_hand).map(|(a,b)|distance(&a,&b))},
            "same_invocation_and_request":true,"scope":"completed native goal before Solve; no solved-hand, skin or displayed-pixel acceptance"})})},"last_native_cache_stage":rendered[index].as_ref().map(|r:&Rendered|serde_json::json!({
        "age_ms":r.at.elapsed().as_millis(),"applied_target":describe_target(&r.target),"request_cookie_still_matches":r.same_cookie,"bones":describe_bones(r.bones,&r.target.desired),
        "drawable_skeleton_hand_world":r.drawable.map(|(m,_)|position(&m)),"drawable_skeleton_matches_manager":r.drawable.map(|(_,same)|same)}))});
    serde_json::json!({"left":arm(0usize),"right":arm(1usize),"authority":"historical numeric observations only; no pose or render acceptance"})
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn post_goal_observation_consumes_exact_owner_cookie_and_never_writes_guest(){
        let m=super::super::tests::native_fixture();let(ped,skel,solver)=(1024,10_000,50_000);let goal=solver+128;let request=solver+352;
        for(a,v)in[(20_159_720,60_000),(60_008,ped),(ped+3888,MANAGER_VTABLE),(ped+3888+352,ped),
            (ped+3888+264,skel),(ped+3888+272,solver),(solver,SOLVER_VTABLE),(solver+480,ped),(skel+8,44_000),(request+80,0),(ped+4256,70_000)]{m.u64(a,v);}
        m.put(ped+5324,0);m.matrix(44_000,&identity());m.matrix(15_000+3*64,&identity());
        let mut desired=identity();desired[12]=0.2;desired[13]=0.3;desired[14]=0.4;m.matrix(goal,&desired);
        m.put(goal+64,1);m.put(goal+72,5);m.put(goal+76,3);m.put(goal+88,1<<16);m.put(request+92,1f32.to_bits());
        for i in 0..3{m.put(request+i*4,desired[12+i as usize].to_bits());}m.put(request+12,cookie(77));m.put(request+112,256);
        let lease=Lease{token:77,arm:1,generation:(1,2,3),tracking:4,hand:desired,attachment:identity(),chain:identity(),muzzle:None,
            capability:PoseCapability::PhysicalRootRay,bones:(5,6),at:Instant::now(),committed:true,flags:256};
        for(a,v)in[(70_000,11_000),(70_008,44_000),(70_024,71_000)]{m.u64(a,v);}m.put(70_032,16);m.matrix(71_000+5*64,&desired);
        let site=GoalSite{solver,passed:skel,resolved:skel,arm:1,goal,request,scratch:80_000,tls:123};
        let pending=target(&m,ped,site,&lease,desired,identity()).unwrap();let generation=Some(((1,2,3),4));
        let before=m.0.borrow().clone();let sample=completed_goal(&m,Some(pending.clone()),site,generation,true).unwrap();
        assert_eq!(sample.relative,desired);assert_eq!((sample.active,sample.orientation,sample.blend),(1,1,1.));
        assert_eq!(sample.fp.skeleton,Some(70_000));assert_eq!(sample.fp.hand,Some(desired));assert_eq!(*m.0.borrow(),before);
        m.u64(70_000,99);let unsupported=completed_goal(&m,Some(pending.clone()),site,generation,true).unwrap();assert!(unsupported.fp.hand.is_none());m.u64(70_000,11_000);
        m.put(70_032,4);assert!(completed_goal(&m,Some(pending.clone()),site,generation,true).unwrap().fp.hand.is_none());m.put(70_032,16);
        m.u64(ped+4256,0);assert!(completed_goal(&m,Some(pending.clone()),site,generation,true).unwrap().fp.skeleton.is_none());m.u64(ped+4256,70_000);
        // The actual dispatch entry records accepted and rejected observations;
        // pending is taken once by the same StoreState path used by host_hook.
        let mut state=StoreState::default();state.post_goal_pending[1]=Some(pending.clone());
        post_goal(&m,state.post_goal_pending[1].take(),site,generation,true);
        assert_eq!(COMPLETED.lock().unwrap()[1].last.unwrap().1,"observed_completed_native_goal");
        post_goal(&m,state.post_goal_pending[1].take(),site,generation,true);
        assert_eq!(COMPLETED.lock().unwrap()[1].last.unwrap().1,"no_applied_target_in_this_invocation");
        assert_eq!(*m.0.borrow(),before);
        let mut wrong=site;wrong.solver+=8;assert_eq!(completed_goal(&m,Some(pending.clone()),wrong,generation,true).err(),Some("invocation_identity_changed"));
        wrong=site;wrong.resolved+=8;assert_eq!(completed_goal(&m,Some(pending.clone()),wrong,generation,true).err(),Some("invocation_identity_changed"));
        assert_eq!(completed_goal(&m,Some(pending.clone()),site,Some(((1,2,99),4)),true).err(),Some("applied_target_stale_or_generation_changed"));
        let mut old=pending.clone();old.target.at=Instant::now()-MAX_AGE-Duration::from_millis(1);
        assert_eq!(completed_goal(&m,Some(old),site,generation,true).err(),Some("applied_target_stale_or_generation_changed"));
        m.put(request+12,cookie(78));assert_eq!(completed_goal(&m,Some(pending.clone()),site,generation,true).err(),Some("request_cookie_or_flags_changed"));m.put(request+12,cookie(77));
        m.u64(solver+480,ped+8);assert_eq!(completed_goal(&m,Some(pending.clone()),site,generation,true).err(),Some("native_owner_or_goal_identity_changed"));m.u64(solver+480,ped);
        m.u64(ped+3888+264,skel+8);assert!(completed_goal(&m,Some(pending.clone()),site,generation,true).is_err());m.u64(ped+3888+264,skel);
        struct ChangedRead<'a>{base:&'a super::super::tests::TestMemory,address:u64,reads:std::cell::Cell<u32>}
        impl Memory for ChangedRead<'_>{
            fn word(&self,a:u64)->Option<u32>{let value=self.base.word(a);if a!=self.address{return value;}let n=self.reads.get();self.reads.set(n+1);value.map(|v|if n==0{v}else{v^1})}
            fn put(&self,_:u64,_:u32)->bool{panic!("post-goal observation must never write guest memory")}
        }
        let changing=ChangedRead{base:&m,address:goal+64,reads:std::cell::Cell::new(0)};
        assert_eq!(completed_goal(&changing,Some(pending.clone()),site,generation,true).err(),Some("post_goal_snapshot_changed"));
        state.post_goal_pending[1]=Some(pending.clone());let prefix=state.boundary_prefix();state.finish_boundary(&prefix);assert!(state.post_goal_pending[1].is_none());
        state.post_goal_pending[1]=Some(pending);let prefix=state.boundary_prefix();state.recover_boundary(&prefix);assert!(state.post_goal_pending[1].is_none());
        assert_eq!(*m.0.borrow(),before);
    }
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

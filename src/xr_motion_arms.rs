//! Native arm INPUT bridge. The game still converts, blends and solves every
//! goal, then updates the weapon through its original attachment machinery.
//! Guest addresses are used only during a hook; retained leases contain values.
use std::{collections::VecDeque, sync::{atomic::{AtomicU32, Ordering}, Mutex}, time::{Duration, Instant}};
use crate::xr_motion_aim::{identity, multiply, rigid, CameraCandidate};

pub const IMPORT_MODULE: &str = "muguet_motion_arms_v1";
const MAX_AGE: Duration = Duration::from_millis(150);
const MANAGER_VTABLE:u64=5_169_184;
const SOLVER_VTABLE:u64=5_169_304;
const MAX_BONES:u32=4096;
// A finite, non-recycled mantissa in the otherwise opaque Vector3 padding.
// Exhaustion disables requests for this process rather than reusing a token.
static NEXT_TOKEN:AtomicU32=AtomicU32::new(1);
static LEASES:Mutex<VecDeque<Lease>>=Mutex::new(VecDeque::new());
static INPUT:Mutex<Option<WeaponInput>>=Mutex::new(None);
static LEFT_INPUT:Mutex<Option<(u32,(u64,u64,u64),Instant)>>=Mutex::new(None);
static STATUS:AtomicU32=AtomicU32::new(0);
static SUPPORT:Mutex<Option<(bool,u64,Instant)>>=Mutex::new(None);
pub(crate) fn native_fallback_active()->bool{
    crate::xr_motion_aim::arms_enabled()&&SUPPORT.lock().unwrap_or_else(|p|p.into_inner()).is_some_and(|(supported,e,at)|!supported&&e==crate::xr_view::view_epoch()&&at.elapsed()<=MAX_AGE)
}
#[derive(Clone)]struct WeaponInput{token:u32,generation:(u64,u64,u64),tracking:u64,weapon:[f32;16],at:Instant}
pub(crate) fn invalidate_inputs(){LEASES.lock().unwrap_or_else(|p|p.into_inner()).clear();*INPUT.lock().unwrap_or_else(|p|p.into_inner())=None;*LEFT_INPUT.lock().unwrap_or_else(|p|p.into_inner())=None;*SUPPORT.lock().unwrap_or_else(|p|p.into_inner())=None;STATUS.store(0,Ordering::Relaxed);}
pub fn status_json()->serde_json::Value{
    let input=INPUT.lock().unwrap_or_else(|p|p.into_inner()).clone();let left=*LEFT_INPUT.lock().unwrap_or_else(|p|p.into_inner());
    serde_json::json!({"enabled":crate::xr_motion_aim::arms_enabled(),"right_target_active":input.as_ref().is_some_and(|i|i.at.elapsed()<=MAX_AGE),
        "left_position_active":left.is_some_and(|(_,g,at)|at.elapsed()<=MAX_AGE&&g.0==crate::xr_view::view_epoch()),
        "left_orientation_policy":"native wrist; tracked grip translation only","unsupported_weapon_native_fallback":native_fallback_active(),
        "right_input_state":match STATUS.load(Ordering::Relaxed){0=>"inactive",1=>"waiting for qualified request and exact tracking packet",2=>"native target accepted; waiting for weapon pose agreement",3=>"actual weapon ray ready",_=>"live weapon or packet qualification pending"},
        "pose_agreement_translation_m":0.03,"pose_agreement_rotation_degrees":3,"ray_is_impact":false})
}
fn input_for_candidate(c:&CameraCandidate)->Option<WeaponInput>{
    if native_fallback_active(){return None;}
    let input=INPUT.lock().unwrap_or_else(|p|p.into_inner()).clone()?;
    if input.generation!=generation(c)||input.tracking!=c.tracking_epoch||input.at.elapsed()>MAX_AGE{return None;}
    Some(input)
}
/// Actual native weapon geometry after target agreement. This is an input
/// direction, not an intersection, impact or guarantee about renderer latency.
pub(crate) struct CurrentInputRay{pub origin:[f32;3],pub direction:[f32;3],pub source_age:Duration,pub camera_sequence:u64}
pub(crate) fn current_actual_input_ray(epoch:u64)->Option<CurrentInputRay>{
    let c=current(true)?;if c.view_epoch!=epoch{return None;}let frame=camera_frame_for_candidate(&c)?;
    Some(CurrentInputRay{origin:[frame[12],frame[13],frame[14]],direction:[-frame[8],-frame[9],-frame[10]],
        source_age:c.recorded_at.elapsed().checked_add(c.source_age_at_capture)?,camera_sequence:c.camera_sequence})
}
fn pose_agrees(actual:&[f32;16],desired:&[f32;16])->bool{
    let distance=(0..3).map(|i|(actual[12+i]-desired[12+i]).powi(2)).sum::<f32>();
    distance<=0.03f32.powi(2)&&(0..3).all(|c|(0..3).map(|r|actual[c*4+r]*desired[c*4+r]).sum::<f32>()>=3f32.to_radians().cos())
}
/// Re-derive all live addresses for each query. The physical matrix may precede
/// an animation update; agreement prevents a ray getting ahead of visible IK.
pub(crate) fn camera_frame_for_candidate(c:&CameraCandidate)->Option<[f32;16]>{
    STATUS.store(4,Ordering::Relaxed);
    let input=input_for_candidate(c)?;let rt=crate::RT.get()?;let guest=Guest(&rt.mem);let mut read=Snapshot::new(&guest);
    let factory=read.u64(20_159_720)?;if factory==0{return None;}let ped=read.u64(factory.checked_add(8)?)?;
    let(_,skel)=manager(&mut read,ped.checked_add(3888)?)?;
    let solver=read.u64(ped.checked_add(3888+272)?)?;
    if solver==0||read.u64(solver)?!=SOLVER_VTABLE||read.u64(solver+480)?!=ped{return None;}
    let goal=solver.checked_add(128)?;let request=solver.checked_add(352)?;
    if read.u32(request+12)?!=cookie(input.token)||read.u64(request+80)?!=0||read.u32(request+112)?!=256{return None;}
    let hand=read.u16(goal+72)?;let bone=read.u16(goal+78)?;let a=attachment(&mut read,ped,skel,hand,bone)?;
    let lease=LEASES.lock().unwrap_or_else(|p|p.into_inner()).iter().find(|l|l.token==input.token&&l.committed).cloned()?;
    if lease.generation!=generation(c)||lease.bones!=(hand,bone)||lease.attachment!=a.a||!close(&lease.chain,&a.k)
        ||!read.unchanged(){return None;}
    if !pose_agrees(&a.actual,&input.weapon){STATUS.store(2,Ordering::Relaxed);return None;}
    if current(true).is_none_or(|live|generation(&live)!=generation(c)){return None;}
    STATUS.store(3,Ordering::Relaxed);
    Some(multiply(&a.actual,&inverse(&aim_from_weapon())?))
}

#[derive(Clone)]
struct Lease {token:u32,arm:u32,generation:(u64,u64,u64), tracking:u64, hand:[f32;16],
    attachment:[f32;16], chain:[f32;16], bones:(u16,u16), at:Instant, committed:bool}
#[derive(Default)]
pub struct StoreState {pending:Vec<(u32,u64)>,touched:Vec<(u64,u32,Instant)>,sequence:u64,depth:u64,pub requests:u64,pub applied:u64,pub fallback:u64}
pub struct BoundaryPrefix{floor:u64,depth:u64}
impl StoreState {
    pub fn boundary_prefix(&mut self)->BoundaryPrefix{let prefix=BoundaryPrefix{floor:self.sequence,depth:self.depth};self.depth=self.depth.saturating_add(1);prefix}
    pub fn finish_boundary(&mut self,prefix:&BoundaryPrefix){
        if self.depth!=prefix.depth.saturating_add(1){self.recover_boundary(prefix);return;}
        self.depth=prefix.depth;if self.depth==0{self.touched.clear();}
    }
    fn touch(&mut self,token:u32)->bool{
        self.touched.retain(|x|x.2.elapsed()<=MAX_AGE);
        if self.touched.len()>=512{return false;}
        let Some(next)=self.sequence.checked_add(1)else{return false;};self.sequence=next;
        // Only current leases matter to rollback. Expired tokens cannot be
        // refreshed or reused; bound a long-running outer guest invocation.
        self.touched.push((next,token,Instant::now()));true
    }
    pub fn recover_boundary(&mut self,prefix:&BoundaryPrefix) {
        let invalid:Vec<_>=self.touched.iter().filter(|x|x.0>prefix.floor).map(|x|x.1).collect();
        self.touched.retain(|x|x.0<=prefix.floor);self.depth=prefix.depth;self.pending.clear();
        LEASES.lock().unwrap_or_else(|p|p.into_inner()).retain(|l|!invalid.contains(&l.token));
        let mut input=INPUT.lock().unwrap_or_else(|p|p.into_inner());if input.as_ref().is_some_and(|i|invalid.contains(&i.token)){*input=None;}
        let mut left=LEFT_INPUT.lock().unwrap_or_else(|p|p.into_inner());if left.as_ref().is_some_and(|i|invalid.contains(&i.0)){*left=None;}
    }
}
fn generation(c:&CameraCandidate)->(u64,u64,u64){(c.view_epoch,c.camera_sequence,c.hand_sequence)}
fn cookie(token:u32)->u32{0x3f80_0000|token}
fn decode_cookie(word:u32)->Option<u32>{let token=word&0x7f_ffff;(word&0xff80_0000==0x3f80_0000&&token!=0).then_some(token)}

/// Native Mat34V padding is deliberately ignored in arithmetic copies.
pub fn canonical(mut m:[f32;16])->[f32;16]{for i in [3,7,11]{m[i]=0.;}m[15]=1.;m}
pub fn inverse(m:&[f32;16])->Option<[f32;16]>{
    if !rigid(m){return None;}let mut out=identity();
    for c in 0..3{for r in 0..3{out[c*4+r]=m[r*4+c];}}
    for r in 0..3{out[12+r]=-(0..3).map(|k|out[k*4+r]*m[12+k]).sum::<f32>();}Some(out)
}
fn close(a:&[f32;16],b:&[f32;16])->bool{(0..4).all(|c|(0..3).all(|r|(a[c*4+r]-b[c*4+r]).abs()<0.002))}
/// 82185's ordinary native orientation path fires along weapon +X. Map that
/// axis to accessory aim -Z and native +Z up to accessory +Y, preserving roll.
pub fn aim_from_weapon()->[f32;16]{[0.,0.,-1.,0., -1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,0.,1.]}
/// Pin the hand origin to the tracked GRIP. The orientation comes from the
/// actual hand->attachment->weapon transform, not identity grip/model axes.
pub fn desired_hand(c:&CameraCandidate,hand_from_weapon:&[f32;16])->Option<[f32;16]>{
    let weapon=multiply(&multiply(&c.game_from_stage,&c.stage_from_aim),&aim_from_weapon());
    let mut hand=multiply(&weapon,&inverse(hand_from_weapon)?);
    let grip=multiply(&c.game_from_stage,&c.stage_from_grip);
    hand[12..15].copy_from_slice(&grip[12..15]);rigid(&hand).then_some(hand)
}

trait Memory {fn word(&self,address:u64)->Option<u32>;fn put(&self,address:u64,word:u32)->bool;}
fn store_target<M:Memory>(mem:&M,target:u64,hand:&[f32;16],full_pose:bool)->bool{
    for c in if full_pose{0..4}else{3..4}{for r in 0..3{if !mem.put(target+(c*16+r*4)as u64,hand[c*4+r].to_bits()){return false;}}}true
}
struct Guest<'a>(&'a wasmtime::SharedMemory);
impl Memory for Guest<'_>{
    fn word(&self,address:u64)->Option<u32>{let offset=usize::try_from(address).ok()?;let(base,len)=crate::mem_base_len(self.0);
        if offset%4!=0||offset.checked_add(4)?>len{return None;}Some(unsafe{&*base.add(offset).cast::<AtomicU32>()}.load(Ordering::Relaxed))}
    fn put(&self,address:u64,word:u32)->bool{let Ok(offset)=usize::try_from(address)else{return false};let(base,len)=crate::mem_base_len(self.0);
        if offset%4!=0||offset.checked_add(4).is_none_or(|e|e>len){return false;}unsafe{&*base.add(offset).cast::<AtomicU32>()}.store(word,Ordering::Relaxed);true}
}
struct Snapshot<'a,M:Memory>{mem:&'a M,words:Vec<(u64,u32)>}
impl<'a,M:Memory> Snapshot<'a,M>{
    fn new(mem:&'a M)->Self{Self{mem,words:vec![]}}
    fn u32(&mut self,a:u64)->Option<u32>{let v=self.mem.word(a)?;self.words.push((a,v));Some(v)}
    fn u64(&mut self,a:u64)->Option<u64>{Some(self.u32(a)? as u64|((self.u32(a.checked_add(4)?)? as u64)<<32))}
    fn u16(&mut self,a:u64)->Option<u16>{let word=self.u32(a&!3)?;Some((word>>((a&3)*8))as u16)}
    fn matrix(&mut self,a:u64)->Option<[f32;16]>{let mut m=[0.;16];for(i,v)in m.iter_mut().enumerate(){*v=f32::from_bits(self.u32(a.checked_add(i as u64*4)?)?);}let m=canonical(m);rigid(&m).then_some(m)}
    fn unchanged(&self)->bool{self.words.iter().all(|(a,v)|self.mem.word(*a)==Some(*v))}
}
#[derive(Clone)]struct Attachment {a:[f32;16],k:[f32;16],actual:[f32;16],hand:u16,bone:u16}
fn hand_chain<M:Memory>(s:&mut Snapshot<'_,M>,skel:u64,hand:u16,bone:u16)->Option<[f32;16]>{
    let count=s.u32(skel.checked_add(32)?)?;if count==0||count>MAX_BONES||u32::from(hand)>=count||u32::from(bone)>=count||hand==bone{return None;}
    let data=s.u64(skel)?;let parents=s.u64(data.checked_add(56)?)?;
    let locals=s.u64(skel.checked_add(16)?)?;let globals=s.u64(skel.checked_add(24)?)?;
    if [data,parents,locals,globals].contains(&0){return None;}
    let mut child=bone;let mut chain=identity();let mut visited=vec![];
    while child!=hand {
        if visited.len()>=16||visited.contains(&child){return None;}visited.push(child);
        chain=multiply(&s.matrix(locals.checked_add(u64::from(child)*64)?)?,&chain);
        let parent=s.u16(parents.checked_add(u64::from(child)*2)?)?;
        if parent==65535||u32::from(parent)>=count{return None;}child=parent;
    }
    let hg=s.matrix(globals.checked_add(u64::from(hand)*64)?)?;
    let ag=s.matrix(globals.checked_add(u64::from(bone)*64)?)?;
    let k=multiply(&inverse(&hg)?,&ag);if !close(&k,&chain){return None;}Some(k)
}
fn attachment<M:Memory>(s:&mut Snapshot<'_,M>,ped:u64,skel:u64,hand:u16,bone:u16)->Option<Attachment>{
    let k=hand_chain(s,skel,hand,bone)?;
    let wm=s.u64(ped.checked_add(4344)?)?;let equipped=s.u64(wm.checked_add(120)?)?;
    if wm==0||equipped==0{return None;}
    let weapon=s.u64(equipped.checked_add(864)?)?;if weapon==0{return None;}
    // The cached muzzle orientation branch needs a separate model certificate.
    if s.u32((weapon.checked_add(491)?)&!3)?&(4<<24)!=0{return None;}
    let physical=s.u64(weapon.checked_add(88)?)?;
    if physical!=equipped||s.u64(physical.checked_add(400)?)?!=physical.checked_add(96)?{return None;}
    let drawable=s.u64(physical.checked_add(80)?)?;if drawable==0{return None;}
    let extension=s.u64(drawable.checked_add(72)?)?;
    if extension==0||s.u64(extension)?!=ped||s.u16(extension.checked_add(120)?)?!=bone{return None;}
    let flags=s.u32(extension.checked_add(124)?)?;
    if flags&15!=2||flags&0x8000!=0{return None;}
    let mut q=[0.;4];for(i,v)in q.iter_mut().enumerate(){*v=f32::from_bits(s.u32(extension.checked_add(96+i as u64*4)?)?);}
    let norm=q.iter().map(|v|v*v).sum::<f32>();if !norm.is_finite()||(norm-1.).abs()>0.001{return None;}
    let[x,y,z,w]=q;let mut a=[1.-2.*(y*y+z*z),2.*(x*y+z*w),2.*(x*z-y*w),0.,
        2.*(x*y-z*w),1.-2.*(x*x+z*z),2.*(y*z+x*w),0.,
        2.*(x*z+y*w),2.*(y*z-x*w),1.-2.*(x*x+y*y),0.,0.,0.,0.,1.];
    for i in 0..3{a[12+i]=f32::from_bits(s.u32(extension.checked_add(64+i as u64*4)?)?);}
    let actual=s.matrix(physical.checked_add(96)?)?;
    rigid(&a).then_some(Attachment{a,k,actual,hand,bone})
}
fn arm_attachment<M:Memory>(s:&mut Snapshot<'_,M>,ped:u64,skel:u64,arm:u32,hand:u16,bone:u16)->Option<Attachment>{
    if arm==1{return attachment(s,ped,skel,hand,bone);}if arm!=0{return None;}
    let solver=s.u64(ped.checked_add(3888+272)?)?;if solver==0||s.u64(solver)?!=SOLVER_VTABLE||s.u64(solver+480)?!=ped{return None;}
    let right_hand=s.u16(solver+128+72)?;let right_aux=s.u16(solver+128+78)?;
    attachment(s,ped,skel,right_hand,right_aux)?;
    Some(Attachment{a:identity(),k:hand_chain(s,skel,hand,bone)?,actual:identity(),hand,bone})
}
/// Distinguish a stable unsupported native branch from a temporarily changing
/// pose bank. Only this narrow observation permits ordinary firing fallback.
fn unsupported_weapon<M:Memory>(s:&mut Snapshot<'_,M>,ped:u64,bone:u16)->Option<bool>{
    let wm=s.u64(ped.checked_add(4344)?)?;if wm==0{return Some(true);}
    let physical=s.u64(wm.checked_add(120)?)?;if physical==0{return Some(true);}
    let weapon=s.u64(physical.checked_add(864)?)?;if weapon==0{return Some(true);}
    if s.u64(weapon.checked_add(88)?)?!=physical{return Some(true);}
    if s.u32((weapon.checked_add(491)?)&!3)?&(4<<24)!=0{return Some(true);}
    if s.u64(physical.checked_add(400)?)?!=physical.checked_add(96)?{return Some(true);}
    let drawable=s.u64(physical.checked_add(80)?)?;if drawable==0{return Some(true);}
    let extension=s.u64(drawable.checked_add(72)?)?;if extension==0{return Some(true);}
    if s.u64(extension)?!=ped||s.u16(extension.checked_add(120)?)?!=bone{return Some(true);}
    let flags=s.u32(extension.checked_add(124)?)?;Some(flags&15!=2||flags&0x8000!=0)
}
fn manager<M:Memory>(s:&mut Snapshot<'_,M>,m:u64)->Option<(u64,u64)>{
    let factory=s.u64(20_159_720)?;let ped=s.u64(factory.checked_add(8)?)?;
    if factory==0||ped==0||m!=ped.checked_add(3888)?||s.u64(m)?!=MANAGER_VTABLE||s.u64(m.checked_add(352)?)?!=ped{return None;}
    if s.u32(ped.checked_add(5324)?)?&4!=0{return None;}
    let skel=s.u64(m.checked_add(264)?)?;if skel==0{return None;}
    // Manager+264 is initialized by the exact native Init path; Iterate also
    // supplies and independently resolves this same native skeleton.
    let reference=s.u64(skel.checked_add(8)?)?;if reference==0{return None;}s.matrix(reference)?;Some((ped,skel))
}
fn current(first:bool)->Option<CameraCandidate>{if !crate::xr_motion_aim::arms_enabled(){return None;}crate::xr_motion_aim::native_arm_candidate(first)}
fn active_stack(caller:&mut wasmtime::Caller<'_,crate::HostState>)->Option<u64>{caller.get_export("__muguet_motion_stack_pointer")?.into_global()?.get(caller).i64().map(|v|v as u64)}

pub(crate) fn host_hook(caller:&mut wasmtime::Caller<'_,crate::HostState>,name:&str,params:&[wasmtime::Val],results:&mut[wasmtime::Val],mem:&wasmtime::SharedMemory)->anyhow::Result<()>{
    use wasmtime::Val;
    anyhow::ensure!(crate::xr_motion_aim::diagnostic_enabled(),"arm imports require matching separate AOT certificate");
    let arg=|i:usize|->anyhow::Result<u64>{match params.get(i){Some(Val::I64(v))=>Ok(*v as u64),Some(Val::I32(v))=>Ok(*v as u32 as u64),_=>anyhow::bail!("native arm argument ABI mismatch")}};
    match name {
        "request"=>{
            anyhow::ensure!(params.len()==6&&results.len()==1,"native arm request ABI mismatch");results[0]=Val::I64(0);
            let(manager_pointer,position,tls,first)=(arg(0)?,arg(2)?,arg(3)?,arg(4)?!=0);
            let arm=arg(5)? as u32;if arm>1{return Ok(());}
            let dt=params[1].f32().unwrap_or(f32::NAN);if !dt.is_finite()||dt<=0.||tls==0||active_stack(caller)!=Some(position){return Ok(());}
            let Some(candidate)=current(first)else{return Ok(())};let guest=Guest(mem);let mut read=Snapshot::new(&guest);
            STATUS.store(1,Ordering::Relaxed);
            let Some((ped,skel))=manager(&mut read,manager_pointer)else{return Ok(())};
            let Some((hand,bone))=read.u32(position+16).zip(read.u32(position+20)).filter(|(h,b)|*h<65535&&*b<65535)else{return Ok(())};
            let Some(attachment)=arm_attachment(&mut read,ped,skel,arm,hand as u16,bone as u16)else{
                if arm==1&&unsupported_weapon(&mut read,ped,bone as u16)==Some(true)&&read.unchanged(){*SUPPORT.lock().unwrap_or_else(|p|p.into_inner())=Some((false,candidate.view_epoch,Instant::now()));}return Ok(());
            };
            if arm==1{*SUPPORT.lock().unwrap_or_else(|p|p.into_inner())=Some((true,candidate.view_epoch,Instant::now()));}
            let hand=if arm==1{let b=multiply(&attachment.k,&attachment.a);let Some(h)=desired_hand(&candidate,&b)else{return Ok(())};h}
                else{let Some(grip)=crate::xr_motion_aim::left_grip_for_candidate(&candidate)else{return Ok(())};let mut h=identity();let g=multiply(&candidate.game_from_stage,&grip);h[12..15].copy_from_slice(&g[12..15]);h};
            if !read.unchanged()||current(first).is_none_or(|c|generation(&c)!=generation(&candidate)){return Ok(());}
            let Ok(token)=NEXT_TOKEN.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|t|(t<0x7f_ffff).then_some(t+1))else{return Ok(())};
            if caller.data().motion_arms.pending.len()>=16{return Ok(());}
            for i in 0..3{if !guest.put(position+i as u64*4,hand[12+i].to_bits()){return Ok(());}}
            if !guest.put(position+12,cookie(token)){return Ok(());}
            let mut leases=LEASES.lock().unwrap_or_else(|p|p.into_inner());leases.retain(|l|l.at.elapsed()<=MAX_AGE);
            if leases.len()>=64{leases.pop_front();}
            leases.push_back(Lease{token,arm,generation:generation(&candidate),tracking:candidate.tracking_epoch,hand,
                attachment:attachment.a,chain:attachment.k,bones:(attachment.hand,attachment.bone),at:Instant::now(),committed:false});
            if !caller.data_mut().motion_arms.touch(token){leases.retain(|l|l.token!=token);return Ok(());}
            caller.data_mut().motion_arms.pending.push((token,tls));caller.data_mut().motion_arms.requests+=1;results[0]=Val::I64(token as i64);
        },
        "request_complete"=>{
            anyhow::ensure!(params.len()==2&&results.is_empty(),"native arm completion ABI mismatch");let(token,tls)=(arg(0)? as u32,arg(1)?);
            let owned=caller.data_mut().motion_arms.pending.pop()==Some((token,tls));
            let mut leases=LEASES.lock().unwrap_or_else(|p|p.into_inner());
            if owned{if let Some(lease)=leases.iter_mut().find(|l|l.token==token){lease.committed=true;}}else{leases.retain(|l|l.token!=token);}
        },
        "target"=>{
            anyhow::ensure!(params.len()==9&&results.is_empty(),"native arm target ABI mismatch");
            let(solver,passed,resolved,arm,goal,request,target,tls,first)=(arg(0)?,arg(1)?,arg(2)?,arg(3)?,arg(4)?,arg(5)?,arg(6)?,arg(7)?,arg(8)?!=0);
            let Some(candidate)=current(first)else{return Ok(())};
            if arm>1||tls==0||active_stack(caller).and_then(|sp|sp.checked_add(96))!=Some(target){return Ok(());}
            let guest=Guest(mem);let mut read=Snapshot::new(&guest);
            let Some(ped)=read.u64(solver+480)else{return Ok(())};let Some((owner,skel))=manager(&mut read,ped.saturating_add(3888))else{return Ok(())};
            if ped!=owner||passed!=skel||resolved!=skel||read.u64(solver)!=Some(SOLVER_VTABLE)||read.u64(ped+3888+272)!=Some(solver)
                ||goal!=solver+32+96*arm||request!=solver+224+128*arm||read.u64(request+80)!=Some(0)||read.u32(request+112)!=Some(if arm==1{256}else{0}){return Ok(());}
            let Some(token)=read.u32(request+12).and_then(decode_cookie)else{return Ok(())};
            if read.u32(target+60)!=Some(cookie(token)){return Ok(());}
            let lease=LEASES.lock().unwrap_or_else(|p|p.into_inner()).iter().find(|l|l.token==token).cloned();
            let Some(lease)=lease.filter(|l|l.arm==arm as u32&&l.committed&&l.generation==generation(&candidate)&&l.tracking==candidate.tracking_epoch&&l.at.elapsed()<=MAX_AGE)else{return Ok(())};
            if arm==0&&crate::xr_motion_aim::left_grip_for_candidate(&candidate).is_none(){return Ok(());}
            let Some((hand,bone))=read.u16(goal+72).zip(read.u16(goal+78))else{return Ok(())};
            let Some((upper,end))=read.u16(goal+76).zip(read.u16(goal+88))else{return Ok(())};
            if (hand,bone)!=lease.bones||bone<=upper||bone>=end||read.u32(skel+32).is_none_or(|n|u32::from(end)>n){return Ok(());}
            let Some(a)=arm_attachment(&mut read,ped,skel,arm as u32,hand,bone)else{return Ok(())};
            if a.a!=lease.attachment||!close(&a.k,&lease.chain){return Ok(());}
            for i in 0..3{if read.u32(request+i as u64*4)!=Some(lease.hand[12+i].to_bits()){return Ok(());}}
            let Some(reference)=read.u64(skel+8).and_then(|p|read.matrix(p))else{return Ok(())};
            if read.matrix(target+128)!=Some(reference){return Ok(());}
            // Record all original target words, including padding. Only XYZ is
            // replaced, then the literal native inverse/blend code continues.
            let Some(original)=read.matrix(target)else{return Ok(())};let _=original;
            if !read.unchanged()||current(first).is_none_or(|c|generation(&c)!=lease.generation){return Ok(());}
            if !caller.data_mut().motion_arms.touch(token){return Ok(());}
            anyhow::ensure!(store_target(&guest,target,&lease.hand,arm==1),"qualified private arm target range changed");
            if arm==1{
            let weapon=multiply(&lease.hand,&multiply(&lease.chain,&lease.attachment));
            *INPUT.lock().unwrap_or_else(|p|p.into_inner())=Some(WeaponInput{token:lease.token,generation:lease.generation,tracking:lease.tracking,weapon,at:Instant::now()});
            crate::xr_motion_aim::accept_native_arm_input(&candidate);
            STATUS.store(2,Ordering::Relaxed);
            }else{*LEFT_INPUT.lock().unwrap_or_else(|p|p.into_inner())=Some((token,lease.generation,Instant::now()));}
            caller.data_mut().motion_arms.applied+=1;
        },
        _=>anyhow::bail!("unknown native arm import {name}"),
    }Ok(())
}

#[cfg(test)]mod tests{
    use super::*;
    use std::{cell::RefCell,collections::BTreeMap};
    #[derive(Default)]struct TestMemory(RefCell<BTreeMap<u64,u32>>);
    impl Memory for TestMemory{fn word(&self,a:u64)->Option<u32>{self.0.borrow().get(&a).copied()}fn put(&self,a:u64,v:u32)->bool{self.0.borrow_mut().insert(a,v);true}}
    impl TestMemory{fn u64(&self,a:u64,v:u64){self.put(a,v as u32);self.put(a+4,(v>>32)as u32);}fn matrix(&self,a:u64,m:&[f32;16]){for(i,v)in m.iter().enumerate(){self.put(a+i as u64*4,v.to_bits());}}}
    fn native_fixture()->TestMemory{
        let m=TestMemory::default();
        for(a,v)in[(10_000,11_000),(10_016,13_000),(10_024,15_000),(11_056,12_000),(1024+4344,20_000),
            (20_120,22_000),(22_864,24_000),(24_088,22_000),(22_400,22_096),(22_080,26_000),(26_072,28_000),(28_000,1024)]{m.u64(a,v);}
        m.put(10_032,16);m.put(12_012,5);m.put(24_488,0);m.put(28_120,6);m.put(28_124,0x1002);
        let mut k=identity();k[12]=0.1;m.matrix(13_000+6*64,&k);let mut h=identity();h[13]=2.;m.matrix(15_000+5*64,&h);m.matrix(15_000+6*64,&multiply(&h,&k));
        for(i,v)in[0f32,0.,0.,1.].into_iter().enumerate(){m.put(28_096+i as u64*4,v.to_bits());}
        for(i,v)in[0.03f32,0.02,-0.05].into_iter().enumerate(){m.put(28_064+i as u64*4,v.to_bits());}m.matrix(22_096,&identity());m
    }
    fn candidate()->CameraCandidate{CameraCandidate{view_epoch:1,camera_sequence:2,head_frame:3,tracking_epoch:4,hand_sequence:5,
        original_game_from_camera:identity(),game_from_stage:identity(),stage_from_aim:identity(),stage_from_grip:identity(),left_grip:None,aim_from_weapon:identity(),recorded_at:Instant::now(),source_age_at_capture:Duration::ZERO}}
    #[test]fn actual_attachment_inverse_preserves_grip_position_and_barrel_roll(){
        let mut c=candidate();c.stage_from_grip[12]=1.5;c.stage_from_grip[13]=-0.2;c.stage_from_grip[14]=3.;
        c.stage_from_aim=[0.,1.,0.,0.,-1.,0.,0.,0.,0.,0.,1.,0.,0.,0.,0.,1.];
        let mut k=identity();k[12]=0.15;let mut a=identity();a[13]=0.03;a[14]=-0.07;
        let b=multiply(&k,&a);let h=desired_hand(&c,&b).unwrap();let w=multiply(&h,&b);
        assert_eq!(&h[12..15],&c.stage_from_grip[12..15]);
        let desired=multiply(&c.stage_from_aim,&aim_from_weapon());
        for i in 0..12{assert!((w[i]-desired[i]).abs()<1e-6);}
        assert_ne!(&w[12..15],&c.stage_from_aim[12..15]);
        let target=multiply(&w,&inverse(&b).unwrap());assert!(close(&target,&h));
    }
    #[test]fn padding_is_canonicalized_only_in_arithmetic_copies(){let mut m=identity();m[3]=f32::NAN;m[7]=77.;m[11]=-1.;m[15]=0.;assert!(inverse(&m).is_none());assert_eq!(inverse(&canonical(m)),Some(identity()));}
    #[test]fn finite_cookies_are_unique_and_unknown_formats_fail(){for token in [1,2,0x12345,0x7ffffe]{let w=cookie(token);assert!(f32::from_bits(w).is_finite());assert_eq!(decode_cookie(w),Some(token));}assert_eq!(decode_cookie(1f32.to_bits()),None);assert_eq!(decode_cookie(f32::NAN.to_bits()),None);}
    #[test]fn live_native_attachment_requires_exact_parent_bone_chain_and_standard_branch(){
        let mem=native_fixture();let mut s=Snapshot::new(&mem);let a=attachment(&mut s,1024,10_000,5,6).unwrap();assert!((a.k[12]-0.1).abs()<1e-6);assert_eq!(&a.a[12..15],&[0.03,0.02,-0.05]);assert!(s.unchanged());
        mem.put(28_124,0x9002);assert!(attachment(&mut Snapshot::new(&mem),1024,10_000,5,6).is_none());mem.put(28_124,0x1002);
        mem.put(24_488,4<<24);assert!(attachment(&mut Snapshot::new(&mem),1024,10_000,5,6).is_none());mem.put(24_488,0);
        mem.u64(22_400,22_112);assert!(attachment(&mut Snapshot::new(&mem),1024,10_000,5,6).is_none());mem.u64(22_400,22_096);
        mem.put(28_120,7);assert!(attachment(&mut Snapshot::new(&mem),1024,10_000,5,6).is_none());mem.put(28_120,6);
        mem.put(15_000+6*64+48,8f32.to_bits());assert!(attachment(&mut Snapshot::new(&mem),1024,10_000,5,6).is_none());assert_eq!(unsupported_weapon(&mut Snapshot::new(&mem),1024,6),Some(false));assert!(!s.unchanged());
    }
    #[test]fn native_private_target_preserves_padding_metadata_and_left_basis(){
        let m=TestMemory::default();for i in 0..24{m.put(4000+i*4,0xa5a5_0000+i as u32);}
        let original=m.0.borrow().clone();let mut h=identity();h[12]=1.;h[13]=2.;h[14]=3.;assert!(store_target(&m,4000,&h,true));
        for i in [3,7,11,15,16,17,18,19,20,21,22,23]{assert_eq!(m.word(4000+i*4),original.get(&(4000+i*4)).copied());}
        *m.0.borrow_mut()=original.clone();assert!(store_target(&m,4000,&h,false));
        for i in 0..12{assert_eq!(m.word(4000+i*4),original.get(&(4000+i*4)).copied());}
        assert_eq!(m.word(4000+48),Some(1f32.to_bits()));assert_eq!(m.word(4000+60),original.get(&(4000+60)).copied());
    }
    #[test]fn actual_pose_agreement_rejects_unreached_or_rotated_guns(){let a=identity();let mut b=a;b[12]=0.031;assert!(!pose_agrees(&a,&b));b[12]=0.02;assert!(pose_agrees(&a,&b));let t=4f32.to_radians();b=identity();b[0]=t.cos();b[1]=t.sin();b[4]=-t.sin();b[5]=t.cos();assert!(!pose_agrees(&a,&b));}
    #[test]fn failed_boundary_discards_uncommitted_and_committed_child_leases(){let mut s=StoreState::default();let root=s.boundary_prefix();s.pending.push((1,7));s.touch(1);let p=s.boundary_prefix();s.pending.push((2,7));s.touch(2);s.touch(3);s.recover_boundary(&p);assert!(s.pending.is_empty());assert_eq!(s.touched.len(),1);s.recover_boundary(&root);assert!(s.pending.is_empty());}
    #[test]fn successful_child_then_failed_outer_revokes_committed_and_applied_values(){
        let mut state=StoreState::default();let outer=state.boundary_prefix();let inner=state.boundary_prefix();let token=0x7e1234;
        let c=candidate();let lease=Lease{token,arm:1,generation:generation(&c),tracking:c.tracking_epoch,hand:identity(),attachment:identity(),chain:identity(),bones:(5,6),at:Instant::now(),committed:true};
        LEASES.lock().unwrap().push_back(lease);*INPUT.lock().unwrap()=Some(WeaponInput{token,generation:generation(&c),tracking:c.tracking_epoch,weapon:identity(),at:Instant::now()});
        state.touch(token);state.finish_boundary(&inner);assert_eq!(state.touched.len(),1);state.recover_boundary(&outer);
        assert!(!LEASES.lock().unwrap().iter().any(|l|l.token==token));assert!(INPUT.lock().unwrap().is_none());assert_eq!(state.depth,0);
    }
}

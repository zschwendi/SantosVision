//! Bounded admission evidence during the certified original's native death
//! effect. This observes existing decisions; it never supplies HUD authority.
use super::ShaderKind;
use crate::xr_view::WorldView;
use serde_json::{json,Value};
use std::{collections::VecDeque,sync::{Mutex,atomic::{AtomicU64,Ordering}},time::Instant};

const TAIL:usize=16;
const RETAINED:usize=7; // six eye/outcome slots plus one unqualified present
const MAX_JSON:usize=8*1024;

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) struct Owner{state:u32,manager:u64,context:u64,effect:u64,metadata_hash:u32}
/// The shared readers use unaligned loads, so alignment is unrestricted. Guard
/// their COMPLETE width and usize conversion before their unchecked addition.
fn checked_address(address:u64,width:u64,length:usize)->Option<usize>{
    if width==0{return None;}let start=usize::try_from(address).ok()?;
    let end=usize::try_from(address.checked_add(width)?).ok()?;(end<=length).then_some(start)
}
fn checked_read<T>(address:u64,width:u64,length:usize,read:&impl Fn(u64)->Option<T>)->Option<T>{
    checked_address(address,width,length)?;read(address)
}
fn checked_owner(length:usize,read32:impl Fn(u64)->Option<u32>,read64:impl Fn(u64)->Option<u64>)->Option<Owner>{
    owner(|a|checked_read(a,4,length,&read32),|a|checked_read(a,8,length,&read64))
}
pub(crate) fn shared_owner(mem:&wasmtime::SharedMemory)->Option<Owner>{
    let length=crate::mem_base_len(mem).1;
    checked_owner(length,|a|crate::read_u32(mem,a),|a|crate::read_u64(mem,a))
}
/// Source26183 reads this manager/context/name before creating manager+976 and
/// writes state1/2 at19235384. State alone does not establish a live effect.
fn owner(read32:impl Fn(u64)->Option<u32>,read64:impl Fn(u64)->Option<u64>)->Option<Owner>{
    let state=read32(19_235_384)?;if !matches!(state,1|2){return None;}
    let manager=read64(19_235_320)?;if manager==0{return None;}
    let context=read64(manager.checked_add(752)?)?;let effect=read64(manager.checked_add(976)?)?;
    if context==0||effect==0{return None;}let metadata_hash=read32(context.checked_add(if state==1{140}else{144})?)?;
    if metadata_hash==0||read32(19_235_384)?!=state||read64(19_235_320)?!=manager{return None;}
    Some(Owner{state,manager,context,effect,metadata_hash})
}

#[derive(Clone,Copy)]
pub(crate) struct Text{bytes:[u8;64],len:u8,truncated:bool}
impl Text{
    pub(crate) fn new(s:&str)->Self{let mut n=s.len().min(64);while !s.is_char_boundary(n){n-=1;}
        let mut bytes=[0;64];bytes[..n].copy_from_slice(&s.as_bytes()[..n]);Self{bytes,len:n as u8,truncated:n<s.len()}}
    fn value(&self)->Value{json!({"text":std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("invalid"),"truncated":self.truncated})}
}
#[derive(Clone,Copy)]
pub(crate) struct Draw{
    pub ordinal:u64,pub target:u32,pub hashes:[Option<u64>;2],pub labels:[Text;2],pub kinds:[Option<ShaderKind>;2],
    pub targets:[u32;4],pub other_targets:bool,pub depth:u32,pub depth_flags:u32,pub writes_depth:bool,
    pub target_layout:[u32;6],pub target_format:Option<wgpu::TextureFormat>,
    pub blend_words:[u32;12],pub blend_len:usize,pub color_mask:u32,pub viewport:[f32;6],pub scissor:[u32;4],
    pub texture_reads:[u32;8],pub reads_count:usize,pub reads_omitted:usize,pub certified_scratch_bits:u8,
}
impl Draw{fn value(&self)->Value{json!({"draw":self.ordinal,"target":self.target,
    "shader_hashes":self.hashes.map(|v|v.map(|h|format!("{h:016x}"))),"labels":self.labels.map(|s|s.value()),
    "certified_kinds":self.kinds.map(|k|k.map(|k|format!("{k:?}"))),"targets":self.targets,"other_targets":self.other_targets,
    "depth":self.depth,"depth_flags":self.depth_flags,"writes_depth":self.writes_depth,
    "target_layout_width_height_samples_layers_mip_slice":self.target_layout,"target_format":self.target_format.map(|f|format!("{f:?}")),
    "blend_words":&self.blend_words[..self.blend_len.min(12)],"blend_words_omitted":self.blend_len.saturating_sub(12),
    "color_mask":self.color_mask,"viewport":self.viewport,"scissor":self.scissor,
    "texture_reads":&self.texture_reads[..self.reads_count.min(8)],"texture_reads_omitted":self.reads_omitted,
    "certified_scratch_bits":self.certified_scratch_bits})}}

#[derive(Default)]
pub(crate) struct PresentAudit{
    present:u64,owner:Option<Owner>,attempt:Option<Draw>,tail:VecDeque<Draw>,completed:u64,
    lens_seen:u64,lens_gate:Option<&'static str>,first_rejection:Option<(String,Option<Draw>)>,entry:u64,was_active:bool,
    resource_command:Option<(u32,[u32;8],usize,[u32;8],usize)>,
}
impl PresentAudit{
    pub(crate) fn needs_begin(&self,present:u64)->bool{self.present!=present}
    pub(crate) fn begin(&mut self,present:u64,owner:Option<Owner>){
        if owner.is_some()&&!self.was_active{self.entry=self.entry.saturating_add(1);}
        self.was_active=owner.is_some();self.present=present;self.owner=owner;self.attempt=None;self.tail.clear();
        self.completed=0;self.lens_seen=0;self.lens_gate=None;self.first_rejection=None;self.resource_command=None;
    }
    pub(crate) fn active(&self)->bool{self.owner.is_some()}
    pub(crate) fn attempted(&mut self,draw:Draw){if self.active(){self.attempt=Some(draw);}}
    pub(crate) fn completed(&mut self){if !self.active(){return;}self.completed+=1;
        if let Some(draw)=self.attempt.take(){if self.tail.len()==TAIL{self.tail.pop_front();}self.tail.push_back(draw);}}
    pub(crate) fn lens(&mut self,gate:&'static str){if self.active(){self.lens_seen+=1;self.lens_gate=Some(gate);}}
    pub(crate) fn reject(&mut self,reason:&str){if self.active()&&self.first_rejection.is_none(){
        self.first_rejection=Some((reason.chars().take(192).collect(),self.attempt));}}
    pub(crate) fn resource_command(&mut self,op:u32,writes:&[u32],reads:&[u32]){if self.active()&&self.resource_command.is_none(){
        let mut w=[0;8];let mut r=[0;8];for(a,b)in w.iter_mut().zip(writes){*a=*b;}for(a,b)in r.iter_mut().zip(reads){*a=*b;}
        self.resource_command=Some((op,w,writes.len(),r,reads.len()));}}
    pub(crate) fn finish(&mut self,final_id:u32,view:Option<&WorldView>,outcome:&'static str,reason:&str,owner:Option<Owner>,candidate:Value){
        let Some(before)=self.owner.take()else{return;};
        let final_draws:Vec<_>=self.tail.iter().filter(|d|d.target==final_id).collect();
        let omitted=final_draws.len().saturating_sub(2);
        let record=json!({"stage":"CPU_HUD_admission_before_world_depth_and_GPU_completion","present":self.present,
            "death_entry":self.entry,"native_effect_state":before.state,"expected_effect_metadata_hash":format!("{:08x}",before.metadata_hash),
            "owner_revalidated":owner==Some(before),"owner_scope":"certified26183 manager/context/effect chain; not a WASTED movie or pixel certificate",
            "view":view.map(crate::world_ui::marker),"final_target":final_id,"outcome":outcome,"reason":reason,
            "candidate":candidate,"completed_draws":self.completed,"draw_tail_limit":TAIL,
            "earlier_completed_draws_not_retained":self.completed.saturating_sub(self.tail.len()as u64),
            "lens_pair_seen":self.lens_seen,"last_lens_gate":self.lens_gate,
            "first_rejection":self.first_rejection.as_ref().map(|(r,d)|json!({"reason":r,"attempted_draw":d.map(|d|d.value())})),
            "first_final_resource_hazard":self.resource_command.map(|(op,w,wn,r,rn)|json!({"opcode":op,"writes":&w[..wn.min(8)],"reads":&r[..rn.min(8)],
                "writes_omitted":wn.saturating_sub(8),"reads_omitted":rn.saturating_sub(8)})),
            "last_final_target_draws":final_draws.iter().rev().take(2).rev().map(|d|d.value()).collect::<Vec<_>>(),
            "final_target_draws_omitted_from_status":omitted});
        record_outcome(view.map(|v|v.eye_index),outcome,record);
    }
}
struct Record{at:Instant,eye:Option<u32>,outcome:&'static str,value:Value}
static RECORDS:Mutex<Vec<Record>>=Mutex::new(Vec::new());
static EVICTED:AtomicU64=AtomicU64::new(0);
fn record_outcome(eye:Option<u32>,outcome:&'static str,value:Value){
    let eye=eye.filter(|eye|*eye<2);
    let mut records=RECORDS.lock().unwrap_or_else(|p|p.into_inner());
    if let Some(r)=records.iter_mut().find(|r|r.eye==eye&&(eye.is_none()||r.outcome==outcome)){*r=Record{at:Instant::now(),eye,outcome,value};}
    else{if records.len()==RETAINED{records.remove(0);EVICTED.fetch_add(1,Ordering::Relaxed);}records.push(Record{at:Instant::now(),eye,outcome,value});}
}
pub(crate) fn status()->Value{
    let records=RECORDS.lock().unwrap_or_else(|p|p.into_inner());
    let mut values:Vec<_>=records.iter().map(|r|{let mut value=r.value.clone();value["age_ms"]=json!(r.at.elapsed().as_millis());value}).collect();
    let mut omitted=0;
    loop{let out=json!({"scope":"native death/fail effect-state1/2 admission evidence only; no rendering change or visibility acceptance",
        "record_limit":RETAINED,"status_bytes_limit":MAX_JSON,"record_key_evictions":EVICTED.load(Ordering::Relaxed),
        "draw_packets_omitted_for_status_bound":omitted,"records":values});
        if serde_json::to_vec(&out).is_ok_and(|v|v.len()<=MAX_JSON){return out;}
        // Keep every outcome summary: later successes cannot hide a failure.
        // Prefer dropping successful draw packets, then older redundant tails.
        let index=values.iter().position(|v|v["outcome"]=="admitted_candidate"&&v["last_final_target_draws"].as_array().is_some_and(|v|!v.is_empty()))
            .or_else(||values.iter().position(|v|v["last_final_target_draws"].as_array().is_some_and(|tail|
                tail.len()>1||(!tail.is_empty()&&v["first_rejection"]["attempted_draw"].is_object()))));
        if let Some(i)=index{values[i]["last_final_target_draws"].as_array_mut().unwrap().remove(0);omitted+=1;continue;}
        let summaries:Vec<_>=values.iter().map(|v|json!({"present":v["present"],"view":v["view"],"outcome":v["outcome"],"reason":v["reason"],
            "age_ms":v["age_ms"],"owner_revalidated":v["owner_revalidated"],"lens_pair_seen":v["lens_pair_seen"],"last_lens_gate":v["last_lens_gate"],
            "first_rejection_reason":v["first_rejection"]["reason"],"detail_omitted":"status_byte_bound"})).collect();
        let out=json!({"record_limit":RETAINED,"status_bytes_limit":MAX_JSON,"detail_omitted":"status_byte_bound","records":summaries});
        if serde_json::to_vec(&out).is_ok_and(|v|v.len()<=MAX_JSON){return out;}
        return json!({"detail_omitted":"status_byte_bound","records":values.iter().map(|v|json!({"present":v["present"],
            "outcome":v["outcome"],"reason":v["reason"].as_str().map(|s|s.chars().take(96).collect::<String>()),"reason_may_be_truncated":true,
            "eye":v["view"]["eye"],"epoch":v["view"]["epoch"],"camera":v["view"]["camera"]})).collect::<Vec<_>>()});
    }
}

#[cfg(test)]mod tests{
    use super::*;
    static TEST_RECORDS:Mutex<()>=Mutex::new(());
    #[test]fn native_death_owner_requires_live_chain_and_coherent_state(){
        let r32=|a|match a{19_235_384=>Some(1),2_140=>Some(123),_=>None};
        let r64=|a|match a{19_235_320=>Some(1_000),1_752=>Some(2_000),1_976=>Some(3_000),_=>None};
        const LENGTH:usize=32*1024*1024;
        assert!(checked_owner(LENGTH,r32,r64).is_some());assert!(checked_owner(LENGTH,|_|Some(0),r64).is_none());assert!(checked_owner(LENGTH,r32,|_|Some(0)).is_none());
        assert!(checked_owner(LENGTH,|_|Some(99),r64).is_none());
        assert!(checked_owner(LENGTH,r32,|a|if a==1_976{Some(0)}else{r64(a)}).is_none());
        let second=checked_owner(LENGTH,|a|match a{19_235_384=>Some(2),2_144=>Some(456),_=>None},r64).unwrap();assert_eq!(second.metadata_hash,456);
        let reads=std::cell::Cell::new(0);assert!(checked_owner(LENGTH,|a|{if a==19_235_384{let n=reads.get();reads.set(n+1);Some(if n==0{1}else{0})}else{r32(a)}},r64).is_none());
    }
    #[test]fn complete_read_width_is_checked_before_any_shared_reader_call(){
        assert_eq!(checked_address(24,8,32),Some(24));assert_eq!(checked_address(28,4,32),Some(28));
        assert_eq!(checked_address(25,8,32),None);assert_eq!(checked_address(29,4,32),None);
        assert_eq!(checked_address(25,4,32),Some(25)); // existing load is unaligned
        assert_eq!(checked_address(u64::MAX-3,8,usize::MAX),None);assert_eq!(checked_address(u64::MAX-1,4,usize::MAX),None);
        assert_eq!(checked_address(0,0,32),None);
        let calls=std::cell::Cell::new(0);let read=|_|{calls.set(calls.get()+1);Some(7)};
        assert_eq!(checked_read(u64::MAX-3,8,usize::MAX,&read),None);assert_eq!(calls.get(),0);
        assert_eq!(checked_read(24,8,32,&read),Some(7));assert_eq!(calls.get(),1);
    }
    #[test]fn malformed_owner_pointers_near_maximum_and_memory_end_fail_before_unsafe_readers(){
        // Reproduces the review's manager+752 == MAX-3 case. The poisoned
        // address must never reach the underlying read callback.
        assert!(checked_owner(usize::MAX,|a|if a==19_235_384{Some(1)}else{panic!("unexpected read32")},
            |a|if a==19_235_320{Some(u64::MAX-755)}else{panic!("overflowed read64 reached callback: {a}")}).is_none());
        const LENGTH:usize=32*1024*1024;let end=LENGTH as u64;
        // A valid field address whose full64-bit width crosses memory end.
        let manager=end-980;
        assert!(checked_owner(LENGTH,|a|if a==19_235_384{Some(1)}else{panic!("unexpected read32")},
            |a|if a==19_235_320{Some(manager)}else if a==manager+752{Some(2_000)}else{panic!("near-end read64 reached callback: {a}")}).is_none());
        // A valid context+140 address whose32-bit width crosses memory end.
        assert!(checked_owner(LENGTH,|a|if a==19_235_384{Some(1)}else{panic!("near-end read32 reached callback: {a}")},
            |a|match a{19_235_320=>Some(1_000),1_752=>Some(end-142),1_976=>Some(3_000),_=>None}).is_none());
    }
    #[test]fn admitted_success_does_not_clear_absent_or_rejected_death_evidence(){
        let _guard=TEST_RECORDS.lock().unwrap();
        let mut records=RECORDS.lock().unwrap();records.clear();drop(records);
        record_outcome(Some(0),"absent_candidate",json!({"outcome":"absent_candidate"}));
        record_outcome(Some(0),"rejected_candidate",json!({"outcome":"rejected_candidate"}));
        record_outcome(Some(0),"admitted_candidate",json!({"outcome":"admitted_candidate"}));
        let value=status();assert_eq!(value["records"].as_array().unwrap().len(),3);
    }
    #[test]fn audit_status_and_unicode_labels_are_bounded(){
        let _guard=TEST_RECORDS.lock().unwrap();
        let text=Text::new(&"é".repeat(100));assert!(text.truncated);assert!(text.value()["text"].as_str().unwrap().len()<=64);
        for i in 0..20{record_outcome(Some(i),"rejected_candidate",json!({"payload":"x".repeat(5_000)}));}
        let value=status();assert!(serde_json::to_vec(&value).unwrap().len()<=MAX_JSON);assert!(value["records"].as_array().unwrap().len()<=RETAINED);
    }
    #[test]fn present_trace_keeps_exact_rejection_and_bounded_final_draws_without_conflating_gpu_completion(){
        let _guard=TEST_RECORDS.lock().unwrap();RECORDS.lock().unwrap().clear();
        let owner=Owner{state:1,manager:1,context:2,effect:3,metadata_hash:4};let mut a=PresentAudit::default();a.begin(7,Some(owner));
        let packet=Draw{ordinal:0,target:9,hashes:[Some(1),Some(2)],labels:[Text::new("v"),Text::new("p")],kinds:[None,None],
            targets:[9,0,0,0],other_targets:false,depth:0,depth_flags:0,writes_depth:false,target_layout:[2,2,1,1,0,0],target_format:None,
            blend_words:[0;12],blend_len:0,color_mask:15,viewport:[0.,0.,2.,2.,0.,1.],scissor:[0,0,2,2],texture_reads:[0;8],reads_count:0,reads_omitted:0,certified_scratch_bits:0};
        for i in 0..100{let mut p=packet;p.ordinal=i;a.attempted(p);if i==10{a.reject("exact_writer");}a.completed();}
        a.lens("pinned_lens_layout_viewport_scissor_or_blend_rejected");a.resource_command(35,&[9],&[8]);
        a.finish(9,None,"rejected_candidate","exact_writer",None,Value::Null);
        let s=status();let r=&s["records"][0];assert_eq!(r["first_rejection"]["attempted_draw"]["draw"],10);
        assert_eq!(r["last_final_target_draws"].as_array().unwrap().len(),2);assert_eq!(r["earlier_completed_draws_not_retained"],84);
        assert_eq!(r["owner_revalidated"],false);assert_eq!(r["first_final_resource_hazard"]["opcode"],35);
        assert!(r["stage"].as_str().unwrap().contains("before_world_depth_and_GPU_completion"));
    }
}

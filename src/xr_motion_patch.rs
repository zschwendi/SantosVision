//! Exact-original, separately requested motion-aim diagnostic artifact factory.
//! Default patch5/AOT is unchanged. This redirects selected native frame READS
//! to invocation-owned guest stack storage; it never swaps shared camera data.
use anyhow::{ensure, Context, Result};
use std::{borrow::Cow, collections::HashMap};
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasmparser::{FuncType, FunctionBody, KnownCustom, Name, Operator as O, Parser, Payload, TypeRef, ValType};

pub const IMPORT_MODULE: &str = "muguet_motion_aim_v1";
pub const READ: &str = "private_frame";
pub const BEGIN: &str = "calculation_begin";
pub const COMPLETE: &str = "calculation_complete";
pub const ALLOW_FIRE: &str = "allow_fire";
const IMPORT_COUNT:u32=4;
const ORIGINAL: &str = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0";
const MARKER: &str = "muguet.motion-aim.v1";
const IK: u32 = 76735;
const FIRE: u32 = 82184;
const CALC: u32 = 76770;
const FIRE_GUN:u32=76778;
const FIRST_PERSON: u32 = 45913;
const KNOWN: &[(u32, &str, usize, &str)] = &[
    (41056,"FindPlayerPed()",13,"577edea9348eddd86685f071a6b0d1364edbb7be34eacfbe8f29db2ae8971b9c"),
    (FIRST_PERSON,"CPed::IsFirstPersonShooterModeEnabledForPlayer(bool, bool, bool, bool, bool) const",427,"0c52de0a13d64cf094c3da241bd05c2b3cdfa40a25fbced068e478ce509bb13e"),
    (IK,"CTaskAimGunOnFoot::PostCameraUpdate(CPed*, bool)",746,"3e0771d2b40a830f7ab16eb0cd890e7c86a650635f0babe1e8dfb8fbbbce0027"),
    (FIRE,"CWeapon::CalcFireVecFromAimCamera(CEntity const*, rage::Matrix34 const&, rage::Vector3&, rage::Vector3&, bool) const",550,"68477fa1244c73e614a29a519fe6381b9de2d386b320db89d5a76eae5975c047"),
    (CALC,"CTaskAimGun::CalculateFiringVector(CPed*, rage::Vector3&, rage::Vector3&, CEntity*, float*, bool) const",3655,"d88e85b62377f1a164ebe32a4cbfcf3cc12e3a2c6b1407b049b70e82b7a29c8d"),
    (FIRE_GUN,"CTaskAimGun::FireGun(CPed*, bool, bool)",4515,"a4797cd00ecc7701b813aef4fab892328c7def8f7c3cfff59d0d8c837351872b"),
];
pub struct Artifact { pub wasm: Vec<u8>, pub manifest: serde_json::Value }

pub fn generate(original: &[u8], wanted: &[(&str, &str)]) -> Result<Artifact> {
    ensure!(sha256(original)? == ORIGINAL, "motion artifact requires exact original SHA256");
    let before = Info::parse(original)?;
    for &(index,name,size,hash) in KNOWN {
        let body=before.body(index)?;let range=body.range();
        ensure!(range.end-range.start==size as u64 && sha256(&original[range.start as usize..range.end as usize])?==hash,"motion source body mismatch: {name}");
        ensure!(before.names.get(name).is_some_and(|ids|ids.as_slice()==[index]),"motion source name/index mismatch: {name}");
    }
    ensure!(before.ty(41056)?.params().is_empty()&&before.ty(41056)?.results()==[ValType::I64],"local ped ABI changed");
    ensure!(before.ty(FIRST_PERSON)?.params()==[ValType::I64,ValType::I32,ValType::I32,ValType::I32,ValType::I32,ValType::I32]
        &&before.ty(FIRST_PERSON)?.results()==[ValType::I32],"native first-person ABI changed");
    ensure!(before.ty(IK)?.params()==[ValType::I64,ValType::I32]&&before.ty(IK)?.results().is_empty(),"IK ABI changed");
    ensure!(before.ty(FIRE)?.params()==[ValType::I64,ValType::I64,ValType::I64,ValType::I64,ValType::I64,ValType::I32]
        &&before.ty(FIRE)?.results()==[ValType::I32],"aim camera fire ABI changed");
    ensure!(before.ty(CALC)?.params()==[ValType::I64,ValType::I64,ValType::I64,ValType::I64,ValType::I64,ValType::I64,ValType::I32]
        &&before.ty(CALC)?.results()==[ValType::I32],"calculated shot ABI changed");
    ensure!(before.ty(FIRE_GUN)?.params()==[ValType::I64,ValType::I64,ValType::I32,ValType::I32]
        &&before.ty(FIRE_GUN)?.results()==[ValType::I32],"FireGun ABI changed");
    let tls=before.named_global("__tls_base")?;let sp=before.named_global("__stack_pointer")?;
    ensure!(sp==0&&tls!=sp,"motion requires exact per-instance stack/TLS globals");
    let patched=crate::patch::patch(original,wanted)?;
    let after=Info::parse(&patched)?;
    let shift=after.imports.checked_sub(before.imports).context("motion patch removed imports")?;
    ensure!(crate::patch::VERSION==5&&shift==8,"motion requires unchanged patch5 base");
    let mut targets=HashMap::new();
    for (index,param_count,stack_local,ped_local,reads,constants) in [
        (IK,2,4,0,vec![(120,1)],vec![11,737]),
        (FIRE,6,12,1,vec![(60,2),(184,3)],vec![11,542]),
    ] {
        let old=before.body(index)?;let new=after.body(index+shift)?;
        ensure!(canonical(old,before.imports,shift)?==canonical(new,after.imports,0)?,"patch5 changed motion helper body");
        let mut read_ordinals=HashMap::new();let mut constant_ordinals=Vec::new();
        let mut reader=old.get_operators_reader()?;let mut ordinal=0;
        while !reader.eof(){let offset=reader.original_position()-old.range().start;let op=reader.read()?;
            if let Some((_,site))=reads.iter().find(|(at,_)|*at==offset){
                ensure!(matches!(op,O::LocalSet{local_index:3})&&index==IK||matches!(op,O::LocalTee{local_index:2})&&index==FIRE,"motion frame read opcode mismatch");
                read_ordinals.insert(ordinal,*site);
            }
            if constants.contains(&offset){ensure!(matches!(op,O::I64Const{value:16}),"motion stack constant mismatch");constant_ordinals.push(ordinal);}
            ordinal+=1;
        }
        ensure!(read_ordinals.len()==reads.len()&&constant_ordinals.len()==2,"motion exact stack/read seams absent");
        targets.insert(index+shift,FrameTarget{params:param_count,stack_local,ped_local,reads:read_ordinals,constants:constant_ordinals});
    }
    ensure!(canonical(before.body(CALC)?,before.imports,shift)?==canonical(after.body(CALC+shift)?,after.imports,0)?,"patch5 changed CalculateFiringVector");
    ensure!(canonical(before.body(FIRE_GUN)?,before.imports,shift)?==canonical(after.body(FIRE_GUN+shift)?,after.imports,0)?,"patch5 changed FireGun");
    let mut fire_ops=before.body(FIRE_GUN)?.get_operators_reader()?;let mut ordinal=0;let mut late=None;
    while !fire_ops.eof(){let offset=fire_ops.original_position()-before.body(FIRE_GUN)?.range().start;let op=fire_ops.read()?;
        if offset==642{ensure!(matches!(op,O::CallIndirect{type_index:168,table_index:0,..}),"FireGun calculated-shot seam changed");late=Some(ordinal);}ordinal+=1;}
    let late=late.context("FireGun late firing seam missing")?;
    let manifest=serde_json::json!({"schema":"muguet.motion-aim","version":1,"base_patch_version":5,
        "original_sha256":ORIGINAL,"patch5_sha256":sha256(&patched)?,"private_frame_bytes":80,
        "stack_bytes_before":16,"stack_bytes_after":96,"native_camera_writes":false,"shot_output_writes":false,
        "local_ped_source":"load64(load64(20159720)+8), original41056",
        "first_person_predicate":"original45913(ped,0,0,0,1,0)",
        "read_sites":["76735+120","82184+60","82184+184"],"ack":"original successful76770 outputs, not impact",
        "fire_gate":"76778 entry before side effects and after calculated-shot call+642, native failure branch",
        "changed_original_bodies":[IK,FIRE,CALC,FIRE_GUN],
        "runtime_activation":"separate diagnostic AOT and matching compiled host certificate only",
        "physical_acceptance":false});
    let wasm=instrument(&patched,targets,CALC+shift,FIRST_PERSON+shift,tls,sp,Some((FIRE_GUN+shift,late)),Some(&manifest))?;
    Ok(Artifact{wasm,manifest})
}

pub fn sha256(bytes:&[u8])->Result<String>{
    ensure!(bytes.len()<=u32::MAX as usize,"motion hash input too large");
    #[cfg(target_vendor="apple")]
    {extern "C"{fn CC_SHA256(p:*const std::ffi::c_void,n:u32,out:*mut u8)->*mut u8;}
     let mut out=[0u8;32];unsafe{CC_SHA256(bytes.as_ptr().cast(),bytes.len() as u32,out.as_mut_ptr());}
     Ok(out.iter().map(|b|format!("{b:02x}")).collect())}
    #[cfg(not(target_vendor="apple"))] anyhow::bail!("motion artifact hashing requires Apple host");
}
struct Info<'a>{types:Vec<FuncType>,function_types:Vec<u32>,imports:u32,import_entries:Vec<(String,String,TypeRef)>,names:HashMap<String,Vec<u32>>,
    global_names:HashMap<String,Vec<u32>>,globals:Vec<wasmparser::GlobalType>,bodies:Vec<FunctionBody<'a>>}
impl<'a>Info<'a>{
    fn parse(bytes:&'a[u8])->Result<Self>{
        Self::parse_mode(bytes,false)
    }
    fn parse_mode(bytes:&'a[u8],allow_motion:bool)->Result<Self>{
        let mut s=Self{types:vec![],function_types:vec![],imports:0,import_entries:vec![],names:HashMap::new(),global_names:HashMap::new(),globals:vec![],bodies:vec![]};
        for p in Parser::new(0).parse_all(bytes){match p?{
            Payload::TypeSection(t)=>for t in t.into_iter_err_on_gc_types(){s.types.push(t?);},
            Payload::ImportSection(i)=>for i in i.into_imports(){let i=i?;ensure!(allow_motion||i.module!=IMPORT_MODULE,"motion artifact already instrumented");
                s.import_entries.push((i.module.into(),i.name.into(),i.ty));match i.ty{
                TypeRef::Func(t)=>{s.imports+=1;s.function_types.push(t);},TypeRef::Global(g)=>s.globals.push(g),_=>{}}},
            Payload::FunctionSection(f)=>for t in f{s.function_types.push(t?);},
            Payload::GlobalSection(g)=>for g in g{s.globals.push(g?.ty);},
            Payload::CodeSectionEntry(b)=>s.bodies.push(b),
            Payload::CustomSection(c)=>{ensure!(allow_motion||c.name()!=MARKER,"motion artifact already marked");if let KnownCustom::Name(n)=c.as_known(){for n in n{match n?{
                Name::Function(map)=>for n in map{let n=n?;s.names.entry(n.name.into()).or_default().push(n.index);},
                Name::Global(map)=>for n in map{let n=n?;s.global_names.entry(n.name.into()).or_default().push(n.index);},_=>{}}}}},_=>{}
        }}Ok(s)
    }
    fn ty(&self,index:u32)->Result<&FuncType>{self.types.get(*self.function_types.get(index as usize).context("motion function type missing")? as usize).context("motion type absent")}
    fn body(&self,index:u32)->Result<&FunctionBody<'a>>{self.bodies.get(index.checked_sub(self.imports).context("motion target imported")? as usize).context("motion target body missing")}
    fn named_global(&self,name:&str)->Result<u32>{let n=self.global_names.get(name).context("motion global name missing")?;ensure!(n.len()==1,"motion global name ambiguous");let index=n[0];
        let g=self.globals.get(index as usize).context("motion global index missing")?;ensure!(g.content_type==ValType::I64&&g.mutable&&!g.shared,"motion global must be mutable nonshared i64");Ok(index)}
}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>>{Ok(if index>=self.imports{index+self.shift}else{index})}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>>{
    let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("motion locals: {e:?}"))?;
    let mut reader=body.get_operators_reader()?;while !reader.eof(){f.instruction(&r.parse_instruction(&mut reader).map_err(|e|anyhow::anyhow!("motion canonical: {e:?}"))?);}Ok(f.into_raw_body())
}
#[derive(Clone)]struct FrameTarget{params:u32,stack_local:u32,ped_local:u32,reads:HashMap<usize,i32>,constants:Vec<usize>}
fn instrument(bytes:&[u8],targets:HashMap<u32,FrameTarget>,calc:u32,predicate:u32,tls:u32,sp:u32,fire_gate:Option<(u32,usize)>,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let info=Info::parse(bytes)?;let changed=targets.keys().copied().chain([calc]).chain(fire_gate.map(|(f,_)|f)).collect::<Vec<_>>();
    let expected=changed.len() as u32;
    let mut r=MotionReencoder{imports:info.imports,types:info.types.len() as u32,next:info.imports,targets,calc,predicate,tls,sp,fire_gate,wrapped:0};
    let mut m=wasm_encoder::Module::new();r.parse_core_module(&mut m,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("motion reencode: {e:?}"))?;
    ensure!(r.wrapped==expected,"motion helpers not wrapped exactly once");
    if let Some(manifest)=manifest{m.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MARKER),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let result=m.finish();wasmparser::Validator::new().validate_all(&result).context("motion artifact validation")?;
    verify_preservation(bytes,&result,&changed,sp)?;Ok(result)
}
/// Check EVERY untouched function and all non-code sections against ordinary
/// Reencode import-index remapping. This is stronger than trusting the three
/// intended target choices: data/globals/tables/element semantics stay pinned.
fn verify_preservation(before:&[u8],after:&[u8],changed:&[u32],sp:u32)->Result<()> {
    let a=Info::parse(before)?;let b=Info::parse_mode(after,true)?;
    ensure!(b.imports==a.imports+IMPORT_COUNT&&b.import_entries.len()==a.import_entries.len()+IMPORT_COUNT as usize
        &&b.import_entries[..a.import_entries.len()]==a.import_entries,"motion changed original imports");
    ensure!(b.types.len()==a.types.len()+IMPORT_COUNT as usize&&b.types[..a.types.len()]==a.types,"motion changed original types");
    ensure!(b.bodies.len()==a.bodies.len()&&b.function_types[a.imports as usize+IMPORT_COUNT as usize..]==a.function_types[a.imports as usize..],"motion changed original function/type relationships");
    for index in a.imports..a.function_types.len() as u32 {
        if !changed.contains(&index){ensure!(canonical(a.body(index)?,a.imports,IMPORT_COUNT)?==canonical(b.body(index+IMPORT_COUNT)?,b.imports,0)?,"motion changed nontarget function{index}");}
    }
    let mut remap=Remap{imports:a.imports,shift:IMPORT_COUNT};let mut expected=wasm_encoder::Module::new();
    remap.parse_core_module(&mut expected,Parser::new(0),before).map_err(|e|anyhow::anyhow!("motion preservation remap: {e:?}"))?;
    let expected=expected.finish();
    let sections=|bytes:&[u8]|->Result<Vec<(u8,Vec<u8>)>>{
        let mut out=vec![];for p in Parser::new(0).parse_all(bytes){let p=p?;
            if matches!(&p,Payload::CustomSection(c) if c.name()=="name"||c.name()==MARKER){continue;}
            if let Some((id,range))=p.as_section(){if ![1,2,7,10].contains(&id){out.push((id,bytes[range.start as usize..range.end as usize].to_vec()));}}
        }Ok(out)
    };
    ensure!(sections(&expected)?==sections(after)?,"motion changed data/global/table/element or other preserved sections");
    let exports=|bytes:&[u8]|->Result<Vec<(String,wasmparser::ExternalKind,u32)>>{
        let mut out=vec![];for p in Parser::new(0).parse_all(bytes){if let Payload::ExportSection(s)=p?{for e in s{let e=e?;out.push((e.name.into(),e.kind,e.index));}}}Ok(out)};
    let mut expected_exports=exports(&expected)?;expected_exports.push(("__muguet_motion_stack_pointer".into(),wasmparser::ExternalKind::Global,sp));
    ensure!(expected_exports==exports(after)?,"motion changed existing exports");Ok(())
}
struct MotionReencoder{imports:u32,types:u32,next:u32,targets:HashMap<u32,FrameTarget>,calc:u32,predicate:u32,tls:u32,sp:u32,fire_gate:Option<(u32,usize)>,wrapped:u32}
impl Reencode for MotionReencoder{
    type Error=anyhow::Error;
    fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>>{index.checked_add(if index>=self.imports{IMPORT_COUNT}else{0}).ok_or_else(||Error::UserError(anyhow::anyhow!("motion remap overflow")))}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{
        use wasm_encoder::ValType::{I32,I64};utils::parse_type_section(self,out,s)?;
        out.ty().function([I32,I64,I64,I64,I64,I32],[I64]);out.ty().function([I64;5],[I64]);out.ty().function([I64,I64,I64,I64,I64,I64,I32],[]);
        out.ty().function([I32,I64,I64,I64,I32],[I32]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{
        utils::parse_import_section(self,out,s)?;for (i,name) in [READ,BEGIN,COMPLETE,ALLOW_FIRE].into_iter().enumerate(){out.import(IMPORT_MODULE,name,wasm_encoder::EntityType::Function(self.types+i as u32));}Ok(())}
    fn parse_export_section(&mut self,out:&mut wasm_encoder::ExportSection,s:wasmparser::ExportSectionReader<'_>)->Result<(),Error<Self::Error>>{
        for export in s {let export=export?;if export.name=="__muguet_motion_stack_pointer"{return Err(Error::UserError(anyhow::anyhow!("motion stack export already exists")));}
            let kind=self.export_kind(export.kind)?;let index=self.external_index(export.kind,export.index)?;out.export(export.name,kind,index);}
        out.export("__muguet_motion_stack_pointer",wasm_encoder::ExportKind::Global,self.sp);Ok(())}
    fn parse_custom_name_subsection(&mut self,out:&mut wasm_encoder::NameSection,s:Name<'_>)->Result<(),Error<Self::Error>>{
        if let Name::Label(map)=s{let mut labels=wasm_encoder::IndirectNameMap::new();for entry in map{let entry=entry?;let mut names=wasm_encoder::NameMap::new();
            for name in entry.names{let name=name?;names.append(name.index+u32::from(entry.index==self.calc),name.name);}labels.append(self.function_index(entry.index)?,&names);}out.labels(&labels);Ok(())}
        else{utils::parse_custom_name_subsection(self,out,s)}}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{
        let index=self.next;self.next+=1;if let Some(target)=self.targets.get(&index).cloned(){self.frame_body(out,body,target)?;self.wrapped+=1;Ok(())}
        else if index==self.calc{self.calc_body(out,body)?;self.wrapped+=1;Ok(())}
        else if let Some((_,late))=self.fire_gate.filter(|(f,_)|*f==index){self.fire_body(out,body,late)?;self.wrapped+=1;Ok(())}
        else{utils::parse_function_body(self,out,body)}}
}
impl MotionReencoder{
    fn emit_fire_allow(&mut self,f:&mut wasm_encoder::Function,phase:i32,predicate_local:u32)->Result<(),Error<anyhow::Error>>{
        use wasm_encoder::{Instruction as I,ValType::I32,BlockType};
        let mem=|offset|wasm_encoder::MemArg{offset,align:3,memory_index:0};
        f.instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(mem(0))).instruction(&I::I64Eqz);
        f.instruction(&I::If(BlockType::Result(I32))).instruction(&I::I32Const(1)).instruction(&I::Else);
        f.instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(mem(0))).instruction(&I::I64Load(mem(8)));
        f.instruction(&I::LocalGet(1)).instruction(&I::I64Eq).instruction(&I::LocalGet(1)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::I32And);
        f.instruction(&I::LocalGet(0)).instruction(&I::I64Load(mem(0))).instruction(&I::I64Const(5_968_184)).instruction(&I::I64Eq).instruction(&I::I32And);
        f.instruction(&I::If(BlockType::Result(I32)));
        f.instruction(&I::LocalGet(1));for value in [0,0,0,1,0]{f.instruction(&I::I32Const(value));}
        let predicate=self.function_index(self.predicate)?;f.instruction(&I::Call(predicate)).instruction(&I::LocalSet(predicate_local));
        f.instruction(&I::I32Const(phase)).instruction(&I::LocalGet(0)).instruction(&I::LocalGet(1)).instruction(&I::GlobalGet(self.tls)).instruction(&I::LocalGet(predicate_local));
        f.instruction(&I::Call(self.imports+3)).instruction(&I::Else).instruction(&I::I32Const(1)).instruction(&I::End).instruction(&I::End);Ok(())
    }
    fn fire_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>,late:usize)->Result<(),Error<anyhow::Error>>{
        use wasm_encoder::{Instruction as I,ValType::I32,BlockType};
        let (mut f,fresh)=self.locals(&body,4,&[(2,I32)])?;
        // This entry gate precedes the original stack allocation and every
        // original task/weapon write. Denial is the native false result.
        self.emit_fire_allow(&mut f,0,fresh+1)?;
        f.instruction(&I::I32Eqz).instruction(&I::If(BlockType::Empty)).instruction(&I::I32Const(0)).instruction(&I::Return).instruction(&I::End);
        let mut reader=body.get_operators_reader()?;let mut ordinal=0;let mut checked=0;
        while !reader.eof(){let op=reader.read()?;
            let boundary=ordinal==late;
            if boundary&&!matches!(op,O::CallIndirect{..}|O::Call{..}){return Err(Error::UserError(anyhow::anyhow!("FireGun late call opcode mismatch")));}
            f.instruction(&self.instruction(op)?);
            if boundary{
                f.instruction(&I::LocalSet(fresh));self.emit_fire_allow(&mut f,1,fresh+1)?;
                f.instruction(&I::LocalSet(fresh+1)).instruction(&I::LocalGet(fresh)).instruction(&I::I32Const(0)).instruction(&I::LocalGet(fresh+1)).instruction(&I::Select);
                checked+=1;
            }ordinal+=1;
        }
        if checked!=1{return Err(Error::UserError(anyhow::anyhow!("FireGun late gate count mismatch")));}out.function(&f);Ok(())
    }
    fn locals(&mut self,body:&FunctionBody<'_>,params:u32,extra:&[(u32,wasm_encoder::ValType)])->Result<(wasm_encoder::Function,u32),Error<anyhow::Error>>{
        let mut locals=vec![];let mut fresh=params;for l in body.get_locals_reader()?{let (count,ty)=l?;fresh=fresh.checked_add(count).ok_or_else(||Error::UserError(anyhow::anyhow!("motion local overflow")))?;locals.push((count,self.val_type(ty)?));}
        for &(count,ty) in extra{locals.push((count,ty));}Ok((wasm_encoder::Function::new(locals),fresh))
    }
    fn frame_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>,target:FrameTarget)->Result<(),Error<anyhow::Error>>{
        use wasm_encoder::{Instruction as I,ValType::{I32,I64},BlockType};
        let (mut f,fresh)=self.locals(&body,target.params,&[(2,I64),(1,I32)])?;
        let mut reader=body.get_operators_reader()?;let mut ordinal=0;
        while !reader.eof(){let op=reader.read()?;
            if target.constants.contains(&ordinal){if !matches!(op,O::I64Const{value:16}){return Err(Error::UserError(anyhow::anyhow!("motion stack seam changed")));}f.instruction(&I::I64Const(96));}
            else if let Some(site)=target.reads.get(&ordinal){
                // Consume the actual selected frame before its first load. The
                // additional80bytes are above the helper's original16byte frame.
                f.instruction(&I::LocalSet(fresh));
                f.instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(wasm_encoder::MemArg{offset:0,align:3,memory_index:0})).instruction(&I::LocalTee(fresh+1)).instruction(&I::I64Eqz);
                f.instruction(&I::If(BlockType::Result(I64))).instruction(&I::LocalGet(fresh)).instruction(&I::Else);
                f.instruction(&I::LocalGet(fresh+1)).instruction(&I::I64Load(wasm_encoder::MemArg{offset:8,align:3,memory_index:0}));
                f.instruction(&I::LocalGet(target.ped_local)).instruction(&I::I64Eq).instruction(&I::LocalGet(target.ped_local)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::I32And);
                f.instruction(&I::If(BlockType::Result(I64)));
                f.instruction(&I::LocalGet(target.ped_local));for value in [0,0,0,1,0]{f.instruction(&I::I32Const(value));}
                let predicate=self.function_index(self.predicate)?;f.instruction(&I::Call(predicate)).instruction(&I::LocalSet(fresh+2));
                f.instruction(&I::I32Const(*site)).instruction(&I::LocalGet(target.ped_local)).instruction(&I::LocalGet(fresh));
                f.instruction(&I::LocalGet(target.stack_local)).instruction(&I::I64Const(16)).instruction(&I::I64Add).instruction(&I::GlobalGet(self.tls));
                f.instruction(&I::LocalGet(fresh+2)).instruction(&I::Call(self.imports));
                f.instruction(&I::Else).instruction(&I::LocalGet(fresh)).instruction(&I::End);
                f.instruction(&I::End);f.instruction(&self.instruction(op)?);
            }else{f.instruction(&self.instruction(op)?);}ordinal+=1;
        }out.function(&f);Ok(())
    }
    fn calc_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<anyhow::Error>>{
        use wasm_encoder::{Instruction as I,ValType::{I32,I64},BlockType};
        let (mut f,fresh)=self.locals(&body,7,&[(6,I64),(1,I32)])?;
        for local in 0..4{f.instruction(&I::LocalGet(local)).instruction(&I::LocalSet(fresh+local));}
        f.instruction(&I::GlobalGet(self.tls)).instruction(&I::LocalSet(fresh+4));
        for local in 0..5{f.instruction(&I::LocalGet(fresh+local));}
        f.instruction(&I::Call(self.imports+1)).instruction(&I::LocalSet(fresh+5));
        f.instruction(&I::Block(BlockType::Result(I32)));
        let mut depth=0;let mut reader=body.get_operators_reader()?;
        while !reader.eof(){match reader.read()?{
            O::Return=>{f.instruction(&I::Br(depth));},
            O::End if depth==0=>{if !reader.eof(){return Err(Error::UserError(anyhow::anyhow!("motion calc trailing operators")));}
                f.instruction(&I::End).instruction(&I::LocalSet(fresh+6));
                f.instruction(&I::LocalGet(fresh+5));for local in 0..5{f.instruction(&I::LocalGet(fresh+local));}
                f.instruction(&I::LocalGet(fresh+6)).instruction(&I::Call(self.imports+2));
                f.instruction(&I::LocalGet(fresh+6)).instruction(&I::End);},
            O::End=>{depth-=1;f.instruction(&I::End);},
            op @ (O::Block{..}|O::Loop{..}|O::If{..})=>{depth+=1;f.instruction(&self.instruction(op)?);},
            O::ReturnCall{..}|O::ReturnCallIndirect{..}|O::ReturnCallRef{..}|O::Try{..}|O::TryTable{..}|O::Catch{..}|O::CatchAll|O::Throw{..}|O::ThrowRef|O::Rethrow{..}|O::Delegate{..}=>return Err(Error::UserError(anyhow::anyhow!("unqualified motion calc exception/tail flow"))),
            op=>{f.instruction(&self.instruction(op)?);}
        }}out.function(&f);Ok(())
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use wasm_encoder::{Instruction as I,ValType as V,Function,ExportKind};
    use std::sync::{Arc,Mutex};
    fn fixture()->(Vec<u8>,HashMap<u32,FrameTarget>,usize){
        let mut m=wasm_encoder::Module::new();let mut types=wasm_encoder::TypeSection::new();
        types.ty().function([],[V::I64]);
        types.ty().function([V::I64,V::I32,V::I32,V::I32,V::I32,V::I32],[V::I32]);
        types.ty().function([V::I64,V::I32],[]);
        types.ty().function([V::I64,V::I64,V::I64,V::I64,V::I64,V::I32],[V::I32]);
        types.ty().function([V::I64,V::I64,V::I64,V::I64,V::I64,V::I64,V::I32],[V::I32]);
        types.ty().function([V::I64,V::I64,V::I32,V::I32],[V::I32]);m.section(&types);
        let mut imports=wasm_encoder::ImportSection::new();
        imports.import("env","frame",wasm_encoder::EntityType::Function(0));imports.import("env","predicate",wasm_encoder::EntityType::Function(1));
        imports.import("env","memory",wasm_encoder::EntityType::Memory(wasm_encoder::MemoryType{minimum:310,maximum:Some(310),memory64:true,shared:true,page_size_log2:None}));m.section(&imports);
        let mut functions=wasm_encoder::FunctionSection::new();for ty in [2,3,4,5]{functions.function(ty);}m.section(&functions);
        let mut globals=wasm_encoder::GlobalSection::new();for value in [4096,123]{globals.global(wasm_encoder::GlobalType{val_type:V::I64,mutable:true,shared:false},&wasm_encoder::ConstExpr::i64_const(value));}m.section(&globals);
        let mut exports=wasm_encoder::ExportSection::new();for (name,index) in [("ik",2),("fire",3),("calc",4),("firegun",5)]{exports.export(name,ExportKind::Func,index);}exports.export("sp",ExportKind::Global,0);m.section(&exports);
        let mut code=wasm_encoder::CodeSection::new();let mut targets=HashMap::new();
        let mem=|offset|wasm_encoder::MemArg{offset,align:3,memory_index:0};
        for (function,params,sp,ped,locals,sites) in [(2,2,4,0,3,vec![1]),(3,6,12,1,7,vec![2,3])]{
            let mut ops=vec![I::GlobalGet(0),I::I64Const(16),I::I64Sub,I::LocalTee(sp),I::GlobalSet(0)];
            let mut reads=HashMap::new();let mut constants=vec![1];
            for site in sites{
                if site==3{ops.push(I::I64Const(512));}else{ops.push(I::Call(0));}
                reads.insert(ops.len(),site);ops.push(if function==2{I::LocalSet(3)}else{I::LocalTee(2)});
                if function==3{ops.extend([I::I64Load(mem(64)),I::Drop]);}
                if function==2{ops.extend([I::I64Const(800),I::LocalGet(3),I::I64Load(mem(64)),I::I64Store(mem(0))]);}
                else{ops.extend([I::LocalGet(3),I::LocalGet(2),I::I64Load(mem(64)),I::I64Store(mem(0))]);}
            }
            if function==3{
                // A stand-in for native post-input correction; the injected
                // observer must report780 rather than forcing the input777.
                ops.extend([I::LocalGet(3),I::LocalGet(3),I::I64Load(mem(0)),I::I64Const(3),I::I64Add,I::I64Store(mem(0)),
                    I::LocalGet(4),I::I64Const(993),I::I64Store(mem(0))]);
            }
            ops.push(I::LocalGet(sp));constants.push(ops.len());ops.extend([I::I64Const(16),I::I64Add,I::GlobalSet(0)]);
            if function==3{ops.push(I::I32Const(37));}ops.push(I::End);
            let mut f=Function::new([(locals,V::I64)]);for op in ops{f.instruction(&op);}code.function(&f);
            targets.insert(function,FrameTarget{params,stack_local:sp,ped_local:ped,reads,constants});
        }
        let mut calc=Function::new([]);
        // Preserve incoming output-pointer arguments while the original helper
        // reuses parameter locals. Return37, not normalized Boolean1.
        for local in [0,1,2,2,3]{calc.instruction(&I::LocalGet(local));}
        calc.instruction(&I::I32Const(0)).instruction(&I::Call(3)).instruction(&I::Return).instruction(&I::End);code.function(&calc);
        let mut ops=vec![I::LocalGet(0),I::LocalGet(1),I::I64Const(1200),I::I64Const(1300),I::I64Const(0),I::I64Const(0),I::I32Const(0)];
        let late=ops.len();ops.extend([I::Call(4),I::I32Eqz,I::If(wasm_encoder::BlockType::Result(V::I32)),I::I32Const(0),I::Else,
            I::I64Const(1400),I::I64Const(99),I::I64Store(mem(0)),I::I32Const(99),I::End,I::End]);
        let mut firegun=Function::new([]);for op in ops{firegun.instruction(&op);}code.function(&firegun);m.section(&code);
        (m.finish(),targets,late)
    }
    #[test]
    fn executable_private_stack_and_original_calculated_outputs(){
        use wasmtime::{Engine,Config,SharedMemory,Store,Linker};
        use std::sync::atomic::{AtomicBool,AtomicI32,Ordering};
        let (bytes,targets,late)=fixture();let bytes=instrument(&bytes,targets,4,1,1,0,Some((5,late)),None).unwrap();
        let mut config=Config::new();config.wasm_memory64(true);config.wasm_threads(true);config.shared_memory(true);
        let engine=Engine::new(&config).unwrap();let module=wasmtime::Module::new(&engine,&bytes).unwrap();
        let memory_type=wasmtime::MemoryTypeBuilder::new().min(310).max(Some(310)).memory64(true).shared(true).build().unwrap();
        let memory=SharedMemory::new(&engine,memory_type).unwrap();
        let write=|address:u64,value:u64|{assert!(crate::write_u64(&memory,address,value));};
        write(20_159_720,1024);write(1032,77);write(256+64,5);write(512+64,6);write(900,5_968_184);
        let events=Arc::new(Mutex::new(Vec::<String>::new()));let sink=events.clone();
        let mut store=Store::new(&engine,());let mut linker=Linker::new(&engine);
        linker.define(&mut store,"env","memory",memory.clone()).unwrap();
        linker.func_wrap("env","frame",||256i64).unwrap();
        let predicate=Arc::new(AtomicI32::new(1));let predicate_value=predicate.clone();
        linker.func_wrap("env","predicate",move|ped:i64,a:i32,b:i32,c:i32,d:i32,e:i32|{
            assert_eq!((ped,a,b,c,d,e),(77,0,0,0,1,0));sink.lock().unwrap().push("predicate".into());predicate_value.load(Ordering::Relaxed)}).unwrap();
        let mem=memory.clone();let sink=events.clone();
        linker.func_wrap(IMPORT_MODULE,READ,move|site:i32,ped:i64,original:i64,scratch:i64,tls:i64,first_person:i32|{
            assert_eq!((ped,tls),(77,123));assert_eq!(scratch,4016);assert!(site>=1&&site<=3);
            sink.lock().unwrap().push(format!("read{site}"));if first_person==0{return original;}
            let mut data=crate::read_bytes(&mem,original as u64,80);
            data[64..72].copy_from_slice(&777u64.to_le_bytes());assert!(crate::write_bytes(&mem,scratch as u64,&data));scratch}).unwrap();
        let sink=events.clone();linker.func_wrap(IMPORT_MODULE,BEGIN,move|task:i64,ped:i64,origin:i64,target:i64,tls:i64|{
            assert_eq!((task,ped,origin,target,tls),(900,77,1200,1300,123));sink.lock().unwrap().push("begin".into());88i64}).unwrap();
        let allow=Arc::new(AtomicBool::new(true));let loss_after_calc=Arc::new(AtomicBool::new(false));
        let allowed=allow.clone();let lose=loss_after_calc.clone();let first=predicate.clone();
        let mem=memory.clone();let sink=events.clone();linker.func_wrap(IMPORT_MODULE,COMPLETE,move|token:i64,task:i64,ped:i64,origin:i64,target:i64,tls:i64,result:i32|{
            assert_eq!((token,task,ped,origin,target,tls,result),(88,900,77,1200,1300,123,37));
            assert_eq!(crate::read_u64(&mem,origin as u64),Some(if first.load(Ordering::Relaxed)!=0{780}else{9}));assert_eq!(crate::read_u64(&mem,target as u64),Some(993));
            sink.lock().unwrap().push("ack".into());if lose.load(Ordering::Relaxed){allowed.store(false,Ordering::Relaxed);}}).unwrap();
        let allowed=allow.clone();let sink=events.clone();
        linker.func_wrap(IMPORT_MODULE,ALLOW_FIRE,move|phase:i32,task:i64,ped:i64,tls:i64,first:i32|{
            assert_eq!((task,ped,tls),(900,77,123));sink.lock().unwrap().push(format!("allow{phase}"));
            i32::from(first==0||allowed.load(Ordering::Relaxed))}).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();
        instance.get_typed_func::<(i64,i32),()>(&mut store,"ik").unwrap().call(&mut store,(77,0)).unwrap();
        let sp=instance.get_global(&mut store,"sp").unwrap();assert_eq!(sp.get(&mut store).i64(),Some(4096));
        assert_eq!(crate::read_u64(&memory,800),Some(777));
        let result=instance.get_typed_func::<(i64,i64,i64,i64,i64,i64,i32),i32>(&mut store,"calc").unwrap()
            .call(&mut store,(900,77,1200,1300,0,0,0)).unwrap();assert_eq!(result,37);
        assert_eq!(sp.get(&mut store).i64(),Some(4096));
        assert_eq!(crate::read_u64(&memory,256+64),Some(5));assert_eq!(crate::read_u64(&memory,512+64),Some(6));
        assert_eq!(*events.lock().unwrap(),["predicate","read1","begin","predicate","read2","predicate","read3","ack"]);
        events.lock().unwrap().clear();
        instance.get_typed_func::<(i64,i32),()>(&mut store,"ik").unwrap().call(&mut store,(78,0)).unwrap();
        assert!(events.lock().unwrap().is_empty());assert_eq!(crate::read_u64(&memory,800),Some(5));
        let firegun=instance.get_typed_func::<(i64,i64,i32,i32),i32>(&mut store,"firegun").unwrap();
        assert_eq!(firegun.call(&mut store,(900,77,0,0)).unwrap(),99);assert_eq!(crate::read_u64(&memory,1400),Some(99));
        // Loss AFTER the controller poll and original calculation: ACK remains
        // original37 and corrected780/993, but native firing side effect never runs.
        write(1400,0);loss_after_calc.store(true,Ordering::Relaxed);
        assert_eq!(firegun.call(&mut store,(900,77,0,0)).unwrap(),0);assert_eq!(crate::read_u64(&memory,1400),Some(0));
        assert_eq!(sp.get(&mut store).i64(),Some(4096));
        // Entry denial performs no native calculation/ACK/side effect.
        events.lock().unwrap().clear();assert_eq!(firegun.call(&mut store,(900,77,0,0)).unwrap(),0);
        assert_eq!(*events.lock().unwrap(),["predicate","allow0"]);
        // Same-call native false first-person preserves ordinary third person.
        predicate.store(0,Ordering::Relaxed);assert_eq!(firegun.call(&mut store,(900,77,0,0)).unwrap(),99);
        assert_eq!(crate::read_u64(&memory,1400),Some(99));
    }
    #[test]
    #[ignore="exact private original, parser/reencoder only; never instantiates game"]
    fn exact_original_three_private_reads_and_native_calculated_ack(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();
        let before=crate::patch::patch(&original,crate::mp::COMMANDS).unwrap();
        let artifact=generate(&original,crate::mp::COMMANDS).unwrap();
        assert_eq!(crate::patch::patch(&original,crate::mp::COMMANDS).unwrap(),before);
        assert_eq!(artifact.manifest["native_camera_writes"],false);
        assert_eq!(artifact.manifest["shot_output_writes"],false);
        if let Ok(path)=std::env::var("MUGUET_MOTION_ARTIFACT_OUT"){
            std::fs::write(&path,&artifact.wasm).unwrap();std::fs::write(format!("{path}.json"),serde_json::to_vec_pretty(&artifact.manifest).unwrap()).unwrap();
        }
        eprintln!("motion artifact {} bytes SHA256 {}",artifact.wasm.len(),sha256(&artifact.wasm).unwrap());
    }
}

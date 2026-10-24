//! Separately requested seven-body artifact. The six-body factory is preserved.
use anyhow::{ensure,Context,Result};
use std::borrow::Cow;
use wasm_encoder::{reencode::{utils,Error,Reencode},Instruction as I,ValType as V,BlockType as B};
use wasmparser::{Parser,Payload,TypeRef,FuncType,FunctionBody,Operator as O};
const PRODUCER:u32=28266;
const MARKER:&str="muguet.motion-cache.v1";
const MODULE:&str=crate::xr_motion_arms::IMPORT_MODULE;
pub struct Artifact{pub wasm:Vec<u8>,pub manifest:serde_json::Value}
pub fn generate(original:&[u8],wanted:&[(&str,&str)])->Result<Artifact>{
    let base=crate::xr_motion_arms_patch::generate(original,wanted)?;
    let old=Info::parse(original)?;let before=Info::parse(&base.wasm)?;let shift=before.imports.checked_sub(old.imports).context("cache base imports")?;
    ensure!(shift==15,"cache requires exact six-body base");let body=old.body(PRODUCER)?;let range=body.range();
    ensure!(range.end-range.start==114083&&crate::xr_motion_patch::sha256(&original[range.start as usize..range.end as usize])?=="1737fcc3d4e62377b2a62207d55b265e6a61e96bddc9f7291de53f5b5bd3f745","cache producer source mismatch");
    ensure!(old.ty(PRODUCER)?.params().is_empty()&&old.ty(PRODUCER)?.results().is_empty(),"cache producer ABI changed");
    ensure!(canonical(body,old.imports,shift)?==canonical(before.body(PRODUCER+shift)?,before.imports,0)?,"six-body base changed cache producer");
    let mut reader=body.get_operators_reader()?;let mut n=0;let mut seam=None;
    while !reader.eof(){let at=reader.original_position();let op=reader.read()?;if at==0x1071f02{ensure!(matches!(op,O::I32Store16{memarg}if memarg.offset==491&&memarg.memory==0),"cache source seam changed");seam=Some(n);}n+=1;}
    let config=Config{producer:PRODUCER+shift,predicate:45913+shift,tls:crate::xr_motion_arms_patch::named_global(&base.wasm,"__tls_base")?,ordinal:seam.context("cache seam absent")?};
    let manifest=serde_json::json!({"schema":"muguet.motion-cache","version":1,"base":base.manifest,"changed_original_bodies":[76735,82184,76770,76778,38481,38548,PRODUCER],"producer":"ordinary28266 after original491|4 store0x1071f02; local4 weapon/local3 genuine64B bone world","hook_abi":"muguet_motion_arms_v1.cache(I64 weapon,I64 bone_world,I64 TLS,I32 first_person)->()","native_cache_writes":"original only","host_guest_calls":false,"host_guest_writes":false,"firing":"original cached firing preserved","reticle":"actual cache and live gun_muzzle agreement, active accepted primary camera","physical_acceptance":false});
    let wasm=instrument(&base.wasm,config,Some(&manifest))?;Ok(Artifact{wasm,manifest})
}
#[derive(Clone,Copy)]struct Config{producer:u32,predicate:u32,tls:u32,ordinal:usize}
struct Info<'a>{imports:u32,entries:Vec<(String,String,TypeRef)>,types:Vec<FuncType>,funcs:Vec<u32>,bodies:Vec<FunctionBody<'a>>}
impl<'a>Info<'a>{fn parse(bytes:&'a[u8])->Result<Self>{let mut s=Self{imports:0,entries:vec![],types:vec![],funcs:vec![],bodies:vec![]};for p in Parser::new(0).parse_all(bytes){match p?{
    Payload::TypeSection(t)=>for t in t.into_iter_err_on_gc_types(){s.types.push(t?);},Payload::ImportSection(i)=>for i in i.into_imports(){let i=i?;s.entries.push((i.module.into(),i.name.into(),i.ty));if let TypeRef::Func(t)=i.ty{s.imports+=1;s.funcs.push(t);}},Payload::FunctionSection(f)=>for t in f{s.funcs.push(t?);},Payload::CodeSectionEntry(b)=>s.bodies.push(b),_=>{}}}Ok(s)}
    fn body(&self,i:u32)->Result<&FunctionBody<'a>>{self.bodies.get(i.checked_sub(self.imports).context("cache target imported")?as usize).context("cache body missing")}
    fn ty(&self,i:u32)->Result<&FuncType>{self.types.get(*self.funcs.get(i as usize).context("cache function type missing")?as usize).context("cache type missing")}}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+if i>=self.imports{self.shift}else{0})}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>>{let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("cache locals {e:?}"))?;let mut reader=body.get_operators_reader()?;while !reader.eof(){f.instruction(&r.parse_instruction(&mut reader).map_err(|e|anyhow::anyhow!("cache canonical {e:?}"))?);}Ok(f.into_raw_body())}
fn instrument(bytes:&[u8],config:Config,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let before=Info::parse(bytes)?;ensure!(!before.entries.iter().any(|e|e.0==MODULE&&e.1=="cache"),"cache already instrumented");
    let mut r=Cache{imports:before.imports,types:before.types.len()as u32,next:before.imports,config,changed:0};let mut module=wasm_encoder::Module::new();
    r.parse_core_module(&mut module,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("cache reencode {e:?}"))?;ensure!(r.changed==1,"cache target count");
    if let Some(manifest)=manifest{module.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MARKER),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let out=module.finish();wasmparser::Validator::new().validate_all(&out).context("cache artifact validation")?;
    let after=Info::parse(&out)?;ensure!(after.entries.len()==before.entries.len()+1&&after.entries[..before.entries.len()]==before.entries&&after.imports==before.imports+1,"cache existing imports changed");
    ensure!(after.types.len()==before.types.len()+1&&after.types[..before.types.len()]==before.types,"cache existing types changed");
    ensure!(after.bodies.len()==before.bodies.len()&&after.funcs[after.imports as usize..]==before.funcs[before.imports as usize..],"cache functions changed");
    for i in before.imports..before.funcs.len()as u32{if i!=config.producer{ensure!(canonical(before.body(i)?,before.imports,1)?==canonical(after.body(i+1)?,after.imports,0)?,"cache changed nontarget{i}");}}
    let mut remap=Remap{imports:before.imports,shift:1};let mut expected=wasm_encoder::Module::new();remap.parse_core_module(&mut expected,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("cache preserve {e:?}"))?;
    let sections=|bytes:&[u8]|->Result<Vec<(u8,Vec<u8>)>>{let mut out=vec![];for p in Parser::new(0).parse_all(bytes){let p=p?;if matches!(&p,Payload::CustomSection(c)if c.name()=="name"||c.name()==MARKER){continue;}if let Some((id,r))=p.as_section(){if ![1,2,10].contains(&id){out.push((id,bytes[r.start as usize..r.end as usize].to_vec()));}}}Ok(out)};
    ensure!(sections(&expected.finish())?==sections(&out)?,"cache changed native data/globals/tables/elements/exports");Ok(out)
}
struct Cache{imports:u32,types:u32,next:u32,config:Config,changed:u32}
fn mem(offset:u64)->wasm_encoder::MemArg{wasm_encoder::MemArg{offset,align:3,memory_index:0}}
impl Reencode for Cache{type Error=anyhow::Error;
    fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+u32::from(i>=self.imports))}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_type_section(self,out,s)?;out.ty().function([V::I64,V::I64,V::I64,V::I32],[]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_import_section(self,out,s)?;out.import(MODULE,"cache",wasm_encoder::EntityType::Function(self.types));Ok(())}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{
        let index=self.next;self.next+=1;if index!=self.config.producer{return utils::parse_function_body(self,out,body);}
        let mut locals=vec![];let mut fresh=0;for l in body.get_locals_reader()?{let(n,t)=l?;fresh+=n;locals.push((n,self.val_type(t)?));}let(ped,manager)=(fresh,fresh+1);locals.push((2,V::I64));let mut f=wasm_encoder::Function::new(locals);
        let mut reader=body.get_operators_reader()?;let mut ordinal=0;let mut inserted=0;
        while !reader.eof(){let op=reader.read()?;f.instruction(&self.instruction(op)?);if ordinal==self.config.ordinal{
            f.instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(mem(0))).instruction(&I::LocalTee(ped)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(ped)).instruction(&I::I64Load(mem(8))).instruction(&I::LocalTee(ped)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(ped)).instruction(&I::I64Load(mem(4344))).instruction(&I::LocalTee(manager)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(manager)).instruction(&I::I64Load(mem(120))).instruction(&I::LocalGet(4)).instruction(&I::I64Load(mem(88))).instruction(&I::I64Eq).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(ped));for v in [0,0,0,1,0]{f.instruction(&I::I32Const(v));}f.instruction(&I::Call(self.function_index(self.config.predicate)?)).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(4)).instruction(&I::LocalGet(3)).instruction(&I::GlobalGet(self.config.tls)).instruction(&I::I32Const(1)).instruction(&I::Call(self.imports));
            for _ in 0..5{f.instruction(&I::End);}inserted+=1;
        }ordinal+=1;}
        if inserted!=1{return Err(Error::UserError(anyhow::anyhow!("cache insertion count")));}out.function(&f);self.changed+=1;Ok(())
    }
}

#[cfg(test)]mod tests{
    use super::*;
    fn module(producer:wasm_encoder::Function)->Vec<u8>{use wasm_encoder::*;let mut m=Module::new();let mut types=TypeSection::new();types.ty().function([V::I64,V::I32,V::I32,V::I32,V::I32,V::I32],[V::I32]);types.ty().function([V::I32],[]);types.ty().function([],[]);m.section(&types);
        let mut imports=ImportSection::new();imports.import("env","predicate",EntityType::Function(0)).import("env","event",EntityType::Function(1)).import("env","memory",EntityType::Memory(MemoryType{minimum:310,maximum:Some(310),memory64:true,shared:true,page_size_log2:None}));m.section(&imports);
        let mut funcs=FunctionSection::new();funcs.function(2);m.section(&funcs);let mut globals=GlobalSection::new();for value in [8000,123]{globals.global(GlobalType{val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(value));}m.section(&globals);
        let mut exports=ExportSection::new();exports.export("producer",ExportKind::Func,2).export("sp",ExportKind::Global,0);m.section(&exports);let mut code=CodeSection::new();code.function(&producer);m.section(&code);m.finish()}
    fn fixture()->(Vec<u8>,Config){let mut f=wasm_encoder::Function::new([(5,V::I64)]);let ops=[I::I64Const(8000),I::LocalSet(3),I::I64Const(5000),I::LocalSet(4),I::I32Const(1),I::Call(1),I::LocalGet(4),I::I32Const(4),I::I32Store16(wasm_encoder::MemArg{offset:491,align:0,memory_index:0}),I::I32Const(2),I::Call(1),I::End];for op in ops{f.instruction(&op);} (module(f),Config{producer:2,predicate:0,tls:1,ordinal:8})}
    fn execute(bytes:&[u8],native_fragment:bool){
        use std::sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}};use wasmtime::{Config as EC,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder};
        let mut ec=EC::new();ec.wasm_memory64(true).wasm_threads(true).shared_memory(true);let engine=Engine::new(&ec).unwrap();let module=wasmtime::Module::new(&engine,bytes).unwrap();let memory=SharedMemory::new(&engine,MemoryTypeBuilder::new().min(310).max(Some(310)).memory64(true).shared(true).build().unwrap()).unwrap();
        for(a,v)in[(20_159_720,64),(72,1024),(1024+4344,2000),(2120,4000),(5088,4000)]{assert!(crate::write_u64(&memory,a,v));}
        let events=Arc::new(Mutex::new(Vec::<String>::new()));let first=Arc::new(AtomicBool::new(true));let trap=Arc::new(AtomicBool::new(false));let mut linker=Linker::new(&engine);let mut store=Store::new(&engine,());linker.define(&mut store,"env","memory",memory.clone()).unwrap();
        let sink=events.clone();let flag=first.clone();linker.func_wrap("env","predicate",move|ped:i64,a:i32,b:i32,c:i32,d:i32,e:i32|{assert_eq!((ped,a,b,c,d,e),(1024,0,0,0,1,0));sink.lock().unwrap().push("predicate".into());i32::from(flag.load(Ordering::Relaxed))}).unwrap();
        let sink=events.clone();linker.func_wrap("env","event",move|n:i32|sink.lock().unwrap().push(format!("stock{n}"))).unwrap();
        let sink=events.clone();let mem=memory.clone();let fail=trap.clone();linker.func_wrap(MODULE,"cache",move|weapon:i64,temp:i64,tls:i64,first:i32|->wasmtime::Result<()>{assert_eq!((weapon,temp,tls,first),(5000,8000,123,1));assert_ne!(crate::read_u32(&mem,5488).unwrap()&(4<<24),0);sink.lock().unwrap().push("cache".into());
            if native_fragment{let q:[f32;4]=std::array::from_fn(|i|f32::from_bits(crate::read_u32(&mem,5016+i as u64*4).unwrap()));let[x,y,z,w]=q;let axis=[1.-2.*(y*y+z*z),2.*(x*y+z*w),2.*(x*z-y*w)];for i in 0..3{let original=f32::from_bits(crate::read_u32(&mem,8000+i as u64*4).unwrap());assert!((axis[i]-original).abs()<2e-5);assert_eq!(crate::read_u32(&mem,5032+i as u64*4),crate::read_u32(&mem,8048+i as u64*4));}}
            if fail.load(Ordering::Relaxed){return Err(wasmtime::Error::msg("synthetic posthook trap"));}Ok(())}).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();let f=instance.get_typed_func::<(),()>(&mut store,"producer").unwrap();
        for n in 0..if native_fragment{32}else{1}{let t=n as f32*0.19;let axis=match n%4{0=>[1.,0.,0.],1=>[0.,1.,0.],2=>[0.,0.,1.],_=>{let k=14f32.sqrt();[1./k,2./k,-3./k]}};let[x,y,z]=axis.map(|v|v*(t/2.).sin());let w=(t/2.).cos();let mut matrix=[1.-2.*(y*y+z*z),2.*(x*y+z*w),2.*(x*z-y*w),0.,2.*(x*y-z*w),1.-2.*(x*x+z*z),2.*(y*z+x*w),0.,2.*(x*z+y*w),2.*(y*z-x*w),1.-2.*(x*x+y*y),0.,0.,0.,0.,1.];matrix[12]=n as f32*0.03;matrix[13]=-1.;for(i,v)in matrix.into_iter().enumerate(){assert!(crate::write_u32(&memory,8000+i as u64*4,v.to_bits()));}
            events.lock().unwrap().clear();f.call(&mut store,()).unwrap();let expected=if native_fragment{vec!["predicate","cache","stock2"]}else{vec!["stock1","predicate","cache","stock2"]};assert_eq!(*events.lock().unwrap(),expected);assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8000));}
        first.store(false,Ordering::Relaxed);events.lock().unwrap().clear();f.call(&mut store,()).unwrap();assert!(!events.lock().unwrap().iter().any(|e|e=="cache"));
        first.store(true,Ordering::Relaxed);assert!(crate::write_u64(&memory,2120,9999));events.lock().unwrap().clear();f.call(&mut store,()).unwrap();assert!(!events.lock().unwrap().iter().any(|e|e=="predicate"||e=="cache"));
        assert!(crate::write_u64(&memory,2120,4000));trap.store(true,Ordering::Relaxed);events.lock().unwrap().clear();assert!(f.call(&mut store,()).is_err());assert!(!events.lock().unwrap().iter().any(|e|e=="stock2"));assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8000));
    }
    #[test]fn producer_hook_runs_after_original_cache_store_and_preserves_continuation_filters_and_stack(){let(bytes,config)=fixture();let out=instrument(&bytes,config,None).unwrap();execute(&out,false);}
    #[test]#[ignore="reads exact original; executes isolated native producer arithmetic only, never game lifecycle"]
    fn original_native_cache_arithmetic_and_posthook_execute32_cases(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();assert_eq!(crate::xr_motion_patch::sha256(&original).unwrap(),"11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0");let info=Info::parse(&original).unwrap();let body=info.body(PRODUCER).unwrap();let mut r=Remap{imports:0,shift:0};let mut f=r.new_function_with_parsed_locals(body).unwrap();for op in [I::I64Const(8000),I::LocalSet(3),I::I64Const(5000),I::LocalSet(4)]{f.instruction(&op);}let mut ordinal=4;let mut seam=None;let mut reader=body.get_operators_reader().unwrap();while !reader.eof(){let at=reader.original_position();let op=reader.read().unwrap();if (0x1071b1f..0x1071f06).contains(&at){if at==0x1071f02{seam=Some(ordinal);}f.instruction(&r.instruction(op).unwrap());ordinal+=1;}}
        for op in [I::I32Const(2),I::Call(1),I::End]{f.instruction(&op);}let bytes=module(f);let out=instrument(&bytes,Config{producer:2,predicate:0,tls:1,ordinal:seam.unwrap()},None).unwrap();execute(&out,true);
    }
    #[test]#[ignore="exact separate seven-body parser/reencoder only; never instantiates game"]
    fn exact_original_seven_bodies_preserve_every_other_body_and_native_data(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();let artifact=generate(&original,crate::mp::COMMANDS).unwrap();
        if let Ok(path)=std::env::var("MUGUET_MOTION_CACHE_ARTIFACT_OUT"){std::fs::write(&path,&artifact.wasm).unwrap();std::fs::write(format!("{path}.json"),serde_json::to_vec_pretty(&artifact.manifest).unwrap()).unwrap();}
        eprintln!("seven-body motion WASM {} bytes SHA256 {}",artifact.wasm.len(),crate::xr_motion_patch::sha256(&artifact.wasm).unwrap());
    }
    #[test]#[ignore="explicit held-artifact visionOS cross compilation only; no game instantiation or device"]
    fn precompile_held_seven_body_artifact_to_new_visionos_aot(){
        use std::io::Write;
        let path=std::env::var("MUGUET_MOTION_CACHE_HELD_WASM").unwrap();let out=std::env::var("MUGUET_MOTION_CACHE_AOT_OUT").unwrap();let expected=std::env::var("MUGUET_MOTION_CACHE_EXPECTED_WASM_SHA256").unwrap();let wasm=std::fs::read(&path).unwrap();let sha=crate::xr_motion_patch::sha256(&wasm).unwrap();assert_eq!(sha,expected);
        let mut manifest:serde_json::Value=serde_json::from_slice(&std::fs::read(format!("{path}.json")).unwrap()).unwrap();assert_eq!(manifest["changed_original_bodies"],serde_json::json!([76735,82184,76770,76778,38481,38548,28266]));
        let mut config=crate::engine_config_for(true);config.target("aarch64-apple-visionos").unwrap();unsafe{config.cranelift_flag_enable("has_lse")};let engine=wasmtime::Engine::new(&config).unwrap();let code=engine.precompile_module(&wasm).unwrap();let aot_sha=crate::xr_motion_patch::sha256(&code).unwrap();
        manifest["aot_sha256"]=aot_sha.clone().into();manifest["patched_wasm_sha256"]=sha.into();manifest["host_build_requirement"]="MUGUET_MOTION_AOT_SHA256 must equal aot_sha256".into();
        std::fs::OpenOptions::new().write(true).create_new(true).open(&out).unwrap().write_all(&code).unwrap();std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{out}.motion.json")).unwrap().write_all(&serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();eprintln!("separate seven-body visionOS AOT {} bytes SHA256 {aot_sha}",code.len());
    }
}

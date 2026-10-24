//! Separately requested left-wrist artifact layered on the preserved nine-body
//! view-model artifact. In the already-instrumented original38481 PreIkUpdate,
//! the injected LEFT native38506 request takes its flags (0 or 256) from the
//! host's exact pending lease instead of the baked constant 0; the RIGHT
//! request keeps its baked 256. Exactly one import is appended. Every other
//! body, native data, global, table, element and export is preserved. Native
//! flags256 maps through original38548 to ArmGoal+90 bit0, which original3551
//! uses to copy the target basis; flags0 keeps the native wrist.
use anyhow::{ensure,Context,Result};
use std::borrow::Cow;
use wasm_encoder::{reencode::{utils,Error,Reencode},Instruction as I,ValType as V};
use wasmparser::{Parser,Payload,TypeRef,FuncType,FunctionBody,Operator as O,ValType,BlockType};
const MARKER:&str="muguet.motion-left-wrist.v1";
const MODULE:&str=crate::xr_motion_arms::IMPORT_MODULE;
pub const IMPORT:&str=crate::xr_motion_arms::LEFT_REQUEST_FLAGS_IMPORT;
const PRE:u32=38481;
const NATIVE_REQUEST:u32=38506;
/// patch5 + four-body motion (12), arms (3), cache (1), view model (1).
const NINE_BODY_SHIFT:u32=17;
const BLEND_IN:f32=0.0666;
const BLEND_OUT:f32=0.1333;
#[derive(Clone,Copy,Debug)]
pub(crate) struct Config{pub pre:u32,pub native_request:u32,pub request_hook:u32,pub complete_hook:u32,pub tls:u32}
pub struct Artifact{pub wasm:Vec<u8>,pub manifest:serde_json::Value}

pub fn generate(original:&[u8],wanted:&[(&str,&str)])->Result<Artifact>{
    let base=crate::xr_viewmodel_patch::generate(original,wanted)?;
    let old=Info::parse(original)?;let before=Info::parse(&base.wasm)?;
    ensure!(before.imports.checked_sub(old.imports)==Some(NINE_BODY_SHIFT),"left wrist requires exact nine-body view-model base");
    ensure!(old.ty(NATIVE_REQUEST)?.params()==[ValType::I64,ValType::I32,ValType::I64,ValType::I32,ValType::F32,ValType::F32,ValType::F32,ValType::F32]
        &&old.ty(NATIVE_REQUEST)?.results().is_empty(),"native38506 ABI changed");
    let config=Config{pre:PRE+NINE_BODY_SHIFT,native_request:NATIVE_REQUEST+NINE_BODY_SHIFT,request_hook:before.function_import(MODULE,"request")?,
        complete_hook:before.function_import(MODULE,"request_complete")?,tls:crate::xr_motion_arms_patch::named_global(&base.wasm,"__tls_base")?};
    ensure!(before.ty(config.request_hook)?.params()==[ValType::I64,ValType::F32,ValType::I64,ValType::I64,ValType::I32,ValType::I32]
        &&before.ty(config.request_hook)?.results()==[ValType::I64],"arm request hook ABI changed");
    ensure!(before.ty(config.complete_hook)?.params()==[ValType::I64,ValType::I64]&&before.ty(config.complete_hook)?.results().is_empty(),"arm completion ABI changed");
    let manifest=serde_json::json!({"schema":"muguet.motion-left-wrist","version":1,"base":base.manifest,
        "changed_original_bodies":[76735,82184,76770,76778,38481,38548,28266,44415,44355],
        "rewrite":"instrumented38481 left native38506 flags constant0 -> muguet_motion_arms_v1.left_request_flags(I64 token,I64 TLS)->I32; right baked256 unchanged",
        "native_flag_semantics":"flags256 -> original38548 ArmGoal+90 bit0 -> original3551 copies target basis; flags0 keeps native wrist",
        "host_policy":"256 only for a committed calibrated grip-to-native-hand lease with matching controller model fingerprint (product, vendor, fixed left hand; not a unit serial) and rig; otherwise 0",
        "physical_acceptance":false});
    let wasm=instrument(&base.wasm,config,Some(&manifest))?;Ok(Artifact{wasm,manifest})
}

/// Locate the exact injected left request in the instrumented PreIkUpdate:
/// LocalGet(m) LocalGet(dt) LocalGet(s) GlobalGet(tls) LocalGet(first) I32Const(arm) Call(request)
/// LocalTee(token) I64Eqz I32Eqz If(empty) LocalGet(m) I32Const(arm) LocalGet(s) I32Const(flags)
/// F32Const(in) F32Const(out) F32Const(0) F32Const(0) Call(native38506) LocalGet(token) GlobalGet(tls)
/// Call(complete) End. Requires exactly one right (256) and one left (0) site.
pub(crate) fn left_flags_site(ops:&[O<'_>],c:&Config)->Result<(usize,u32)>{
    let mut left=vec![];let mut right=0;
    for i in 0..ops.len(){
        if !matches!(ops[i],O::Call{function_index}if function_index==c.native_request){continue;}
        let(Some(before),Some(after))=(i.checked_sub(19).map(|s|&ops[s..i]),ops.get(i+1..i+5))else{continue};
        let(O::LocalGet{local_index:m},O::LocalGet{..},O::LocalGet{local_index:s},O::GlobalGet{global_index:tls},O::LocalGet{..},O::I32Const{value:arm},
            O::Call{function_index:hook},O::LocalTee{local_index:token},O::I64Eqz,O::I32Eqz,O::If{blockty:BlockType::Empty},
            O::LocalGet{local_index:m2},O::I32Const{value:arm2},O::LocalGet{local_index:s2},O::I32Const{value:flags},
            O::F32Const{value:a},O::F32Const{value:b},O::F32Const{value:z1},O::F32Const{value:z2})=(&before[0],&before[1],&before[2],&before[3],&before[4],&before[5],
            &before[6],&before[7],&before[8],&before[9],&before[10],&before[11],&before[12],&before[13],&before[14],&before[15],&before[16],&before[17],&before[18])else{continue};
        let(O::LocalGet{local_index:token2},O::GlobalGet{global_index:tls2},O::Call{function_index:complete},O::End)=(&after[0],&after[1],&after[2],&after[3])else{continue};
        if *hook!=c.request_hook||*complete!=c.complete_hook||*tls!=c.tls||*tls2!=c.tls||m!=m2||s!=s2||arm!=arm2||token!=token2
            ||a.bits()!=BLEND_IN.to_bits()||b.bits()!=BLEND_OUT.to_bits()||z1.bits()!=0||z2.bits()!=0{continue;}
        match(*arm,*flags){(1,256)=>right+=1,(0,0)=>left.push((i-5,*token)),_=>anyhow::bail!("unexpected baked arm request flags")}
    }
    ensure!(right==1&&left.len()==1,"left-wrist request seam count: right {right}, left {}",left.len());Ok(left[0])
}

struct Info<'a>{imports:u32,entries:Vec<(String,String,TypeRef)>,types:Vec<FuncType>,funcs:Vec<u32>,bodies:Vec<FunctionBody<'a>>}
impl<'a>Info<'a>{fn parse(bytes:&'a[u8])->Result<Self>{let mut s=Self{imports:0,entries:vec![],types:vec![],funcs:vec![],bodies:vec![]};for p in Parser::new(0).parse_all(bytes){match p?{
    Payload::TypeSection(t)=>for t in t.into_iter_err_on_gc_types(){s.types.push(t?);},Payload::ImportSection(i)=>for i in i.into_imports(){let i=i?;s.entries.push((i.module.into(),i.name.into(),i.ty));if let TypeRef::Func(t)=i.ty{s.imports+=1;s.funcs.push(t);}},Payload::FunctionSection(f)=>for t in f{s.funcs.push(t?);},Payload::CodeSectionEntry(b)=>s.bodies.push(b),_=>{}}}Ok(s)}
    fn body(&self,i:u32)->Result<&FunctionBody<'a>>{self.bodies.get(i.checked_sub(self.imports).context("left wrist target imported")?as usize).context("left wrist body missing")}
    fn ty(&self,i:u32)->Result<&FuncType>{self.types.get(*self.funcs.get(i as usize).context("left wrist function type missing")?as usize).context("left wrist type missing")}
    fn function_import(&self,module:&str,name:&str)->Result<u32>{
        let found:Vec<_>=self.entries.iter().filter(|e|matches!(e.2,TypeRef::Func(_))).enumerate().filter(|(_,e)|e.0==module&&e.1==name).map(|(i,_)|i as u32).collect();
        ensure!(found.len()==1,"left wrist import {module}.{name} absent or ambiguous");Ok(found[0])}}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+if i>=self.imports{self.shift}else{0})}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>>{let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("left wrist locals {e:?}"))?;let mut reader=body.get_operators_reader()?;while !reader.eof(){f.instruction(&r.parse_instruction(&mut reader).map_err(|e|anyhow::anyhow!("left wrist canonical {e:?}"))?);}Ok(f.into_raw_body())}
pub(crate) fn instrument(bytes:&[u8],config:Config,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let before=Info::parse(bytes)?;ensure!(!before.entries.iter().any(|e|e.0==MODULE&&e.1==IMPORT),"left wrist already instrumented");
    let mut r=Hook{imports:before.imports,types:before.types.len()as u32,next:before.imports,config,changed:0};let mut module=wasm_encoder::Module::new();
    r.parse_core_module(&mut module,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("left wrist reencode {e:?}"))?;ensure!(r.changed==1,"left wrist target count");
    if let Some(manifest)=manifest{module.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MARKER),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let out=module.finish();wasmparser::Validator::new().validate_all(&out).context("left wrist artifact validation")?;
    let after=Info::parse(&out)?;ensure!(after.entries.len()==before.entries.len()+1&&after.entries[..before.entries.len()]==before.entries&&after.imports==before.imports+1,"left wrist existing imports changed");
    ensure!(after.types.len()==before.types.len()+1&&after.types[..before.types.len()]==before.types,"left wrist existing types changed");
    ensure!(after.bodies.len()==before.bodies.len()&&after.funcs[after.imports as usize..]==before.funcs[before.imports as usize..],"left wrist functions changed");
    for i in before.imports..before.funcs.len()as u32{if i!=config.pre{ensure!(canonical(before.body(i)?,before.imports,1)?==canonical(after.body(i+1)?,after.imports,0)?,"left wrist changed nontarget{i}");}}
    let mut remap=Remap{imports:before.imports,shift:1};let mut expected=wasm_encoder::Module::new();remap.parse_core_module(&mut expected,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("left wrist preserve {e:?}"))?;
    let sections=|bytes:&[u8]|->Result<Vec<(u8,Vec<u8>)>>{let mut out=vec![];for p in Parser::new(0).parse_all(bytes){let p=p?;if matches!(&p,Payload::CustomSection(c)if c.name()=="name"||c.name()==MARKER){continue;}if let Some((id,r))=p.as_section(){if ![1,2,10].contains(&id){out.push((id,bytes[r.start as usize..r.end as usize].to_vec()));}}}Ok(out)};
    ensure!(sections(&expected.finish())?==sections(&out)?,"left wrist changed native data/globals/tables/elements/exports");
    // The rewritten PreIkUpdate differs from its base in exactly one place.
    let(a,b)=(before.body(config.pre)?,after.body(config.pre+1)?);let(ra,rb)=(a.get_operators_reader()?,b.get_operators_reader()?);
    let(oa,ob):(Vec<_>,Vec<_>)=(ra.into_iter().collect::<Result<_,_>>()?,rb.into_iter().collect::<Result<_,_>>()?);
    let(site,_)=left_flags_site(&oa,&config)?;ensure!(ob.len()==oa.len()+2,"left wrist rewrite size");
    ensure!(matches!(ob[site+2],O::Call{function_index}if function_index==before.imports),"left wrist rewrite call");
    Ok(out)
}
struct Hook{imports:u32,types:u32,next:u32,config:Config,changed:u32}
impl Reencode for Hook{type Error=anyhow::Error;
    fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+u32::from(i>=self.imports))}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_type_section(self,out,s)?;out.ty().function([V::I64,V::I64],[V::I32]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_import_section(self,out,s)?;out.import(MODULE,IMPORT,wasm_encoder::EntityType::Function(self.types));Ok(())}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{
        let index=self.next;self.next+=1;if index!=self.config.pre{return utils::parse_function_body(self,out,body);}
        let ops:Vec<O<'_>>=body.get_operators_reader()?.into_iter().collect::<Result<_,_>>()?;
        let(site,token)=left_flags_site(&ops,&self.config).map_err(Error::UserError)?;
        let mut f=self.new_function_with_parsed_locals(&body)?;
        for(n,op)in ops.into_iter().enumerate(){
            if n==site{f.instruction(&I::LocalGet(token)).instruction(&I::GlobalGet(self.config.tls)).instruction(&I::Call(self.imports));continue;}
            f.instruction(&self.instruction(op)?);
        }
        out.function(&f);self.changed+=1;Ok(())
    }
}

#[cfg(test)]mod tests{
    use super::*;
    use std::sync::{Arc,Mutex,atomic::{AtomicI32,Ordering}};
    const MODULE_PRE:u32=7;
    fn config()->Config{Config{pre:MODULE_PRE,native_request:1,request_hook:4,complete_hook:5,tls:1}}
    /// The actual arms-instrumented fixture, then this layer; executed.
    fn run(bytes:&[u8],left_flags:i32)->Vec<String>{
        use wasmtime::{Config as EC,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder};
        let mut ec=EC::new();ec.wasm_memory64(true).wasm_threads(true).shared_memory(true);let engine=Engine::new(&ec).unwrap();
        let module=wasmtime::Module::new(&engine,bytes).unwrap();let memory=SharedMemory::new(&engine,MemoryTypeBuilder::new().min(310).max(Some(310)).memory64(true).shared(true).build().unwrap()).unwrap();
        for(a,v)in[(20_159_720,1024),(1032,77),(1200+352,77),(1200+264,3000),(3000,4096),(2000+480,77)]{assert!(crate::write_u64(&memory,a,v));}
        let events=Arc::new(Mutex::new(Vec::<String>::new()));let mut store=Store::new(&engine,());let mut linker=Linker::new(&engine);linker.define(&mut store,"env","memory",memory.clone()).unwrap();
        linker.func_wrap("env","predicate",|_:i64,_:i32,_:i32,_:i32,_:i32,_:i32|1i32).unwrap();
        let mem=memory.clone();linker.func_wrap("env","convert",move|_:i64,tag:i32,out:i64|{let value=match tag{57005=>10,28422=>11,18905=>20,60309=>21,_=>panic!("bone")};assert!(crate::write_u32(&mem,out as u64,value));1i32}).unwrap();
        let sink=events.clone();linker.func_wrap("env","request",move|_:i64,arm:i32,_:i64,flags:i32,_:f32,_:f32,_:f32,_:f32|sink.lock().unwrap().push(format!("native{arm}:{flags}"))).unwrap();
        linker.func_wrap("env","event",|_:i32|{}).unwrap();
        linker.func_wrap(MODULE,"request",|_:i64,_:f32,_:i64,_:i64,_:i32,arm:i32|42i64+arm as i64).unwrap();
        let sink=events.clone();linker.func_wrap(MODULE,"request_complete",move|token:i64,_:i64|sink.lock().unwrap().push(format!("commit{token}"))).unwrap();
        linker.func_wrap(MODULE,"target",|_:i64,_:i64,_:i64,_:i32,_:i64,_:i64,_:i64,_:i64,_:i32|{}).unwrap();
        let value=Arc::new(AtomicI32::new(left_flags));let sink=events.clone();
        linker.func_wrap(MODULE,IMPORT,move|token:i64,tls:i64|{sink.lock().unwrap().push(format!("flags{token}/{tls}"));value.load(Ordering::Relaxed)}).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();instance.get_typed_func::<(i64,f32),()>(&mut store,"pre").unwrap().call(&mut store,(1200,0.016)).unwrap();
        assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8192));
        let out=events.lock().unwrap().clone();out
    }
    #[test]fn left_native_request_flags_come_from_the_exact_pending_lease_and_right_stays_256(){
        let base=crate::xr_motion_arms_patch::tests::instrumented_fixture();let out=instrument(&base,config(),None).unwrap();
        assert_eq!(run(&out,256),["native1:256","commit43","flags42/123","native0:256","commit42"]);
        assert_eq!(run(&out,0),["native1:256","commit43","flags42/123","native0:0","commit42"]);
    }
    #[test]fn only_the_exact_instrumented_shape_is_rewritten(){
        let base=crate::xr_motion_arms_patch::tests::instrumented_fixture();let out=instrument(&base,config(),None).unwrap();
        assert!(instrument(&out,config(),None).is_err(),"double instrumentation");
        assert!(instrument(&base,Config{complete_hook:6,..config()},None).is_err(),"wrong completion hook");
        assert!(instrument(&base,Config{native_request:2,..config()},None).is_err(),"wrong native request");
        assert!(instrument(&base,Config{tls:0,..config()},None).is_err(),"wrong TLS");
        let a=Info::parse(&base).unwrap();let b=Info::parse(&out).unwrap();assert_eq!(b.imports,a.imports+1);
        assert_eq!(canonical(a.body(8).unwrap(),a.imports,1).unwrap(),canonical(b.body(9).unwrap(),b.imports,0).unwrap(),"Iterate unchanged");
    }
    #[test]#[ignore="exact separate left-wrist parser/reencoder over the nine-body chain; never instantiates game"]
    fn exact_original_left_wrist_layer_preserves_every_other_body_and_native_data(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();let artifact=generate(&original,crate::mp::COMMANDS).unwrap();
        if let Ok(path)=std::env::var("MUGUET_MOTION_LEFT_WRIST_ARTIFACT_OUT"){std::fs::write(&path,&artifact.wasm).unwrap();std::fs::write(format!("{path}.json"),serde_json::to_vec_pretty(&artifact.manifest).unwrap()).unwrap();}
        eprintln!("left-wrist motion WASM {} bytes SHA256 {}",artifact.wasm.len(),crate::xr_motion_patch::sha256(&artifact.wasm).unwrap());
    }
}

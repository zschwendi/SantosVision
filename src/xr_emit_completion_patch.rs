//! Offline/JIT-only post-Emit artifact factory; never called by runtime or CLI.
//!
//! Completion means only reaching Emit's normal-exit join after producer release/optional
//! notify and a completed existing midpoint import. It grants no foreground,
//! shader-consumption, packet-publication, stereo or callback-safety authority.
//! The returned in-memory artifact needs future explicit host/certificate support.
//! The pointer is a value, not a ring-memory lease; concurrent reuse is possible.
//! Unknown-import stubbing in generic hosts is NOT prevented by this namespace.
use anyhow::{bail, ensure, Context, Result};
use std::{borrow::Cow, collections::HashMap};
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasmparser::{FuncType, FunctionBody, KnownCustom, Name, Operator as O, Parser, Payload, TypeRef, ValType};

pub const SCHEMA: &str = "muguet.emit-completion";
pub const VERSION: u32 = 1;
pub const IMPORT_MODULE: &str = "muguet_emit_completion_v1";
pub const COMPLETE: &str = "complete";
const MANIFEST: &str = "muguet.emit-completion.v1";
const ORIGINAL_SHA: &str = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0";
const EMIT_NAME: &str = "wasm_gpu::Emit(unsigned int, unsigned int const*, unsigned int)";
const EMIT: u32 = 550;

pub struct Artifact { pub wasm: Vec<u8>, pub manifest: serde_json::Value }

/// Exact-original, explicit API only. Does not persist game bytes, generate AOT,
/// register imports, change cache keys, enable mailbox delivery or run the game.
pub fn generate(original: &[u8], wanted: &[(&str,&str)]) -> Result<Artifact> {
    ensure!(sha256(original)?==ORIGINAL_SHA,"Emit completion requires exact original SHA256");
    let before=Info::parse(original)?;
    for (index,name,length,hash,params,results) in [
        (549,"wasm_gpu::Init()",476,"c4e911e8707f123ffd4c48f3cf8fadc7fc2414a4e632c45ee97d35a1821e786a",vec![],vec![ValType::I32]),
        (550,EMIT_NAME,318,"b279df74315035bf5f0c26a61d66870a63b6da3b7af4f9ab26c43018bd5c87ea",vec![ValType::I32,ValType::I64,ValType::I32],vec![]),
        (551,"wasm_gpu::Reserve(unsigned int)",356,"8e284b119e0763de659f7b090c66bb5a71413d75f8583c6498fbfeb48081245d",vec![ValType::I32],vec![ValType::I64]),
    ] {
        let body=before.body(index)?;let r=body.range();
        ensure!(r.end-r.start==length&&sha256(&original[r.start as usize..r.end as usize])?==hash,"Emit source body certificate mismatch");
        ensure!(before.names.get(name).is_some_and(|v|v.as_slice()==[index]),"Emit source name/index mismatch");
        ensure!(before.ty(index)?.params()==params&&before.ty(index)?.results()==results,"Emit source signature mismatch");
    }
    let patched=crate::patch::patch(original,wanted)?;
    let after=Info::parse(&patched)?;
    ensure!(crate::patch::VERSION==5,"Emit completion requires patch5");
    let shift=after.import_count.checked_sub(before.import_count).context("patch5 removed imports")?;
    ensure!(shift==8&&after.imports.len()==before.imports.len()+8,"unexpected patch5 import shift");
    ensure!(after.imports[..before.imports.len()]==before.imports,"patch5 original imports changed");
    ensure!(after.types.get(..before.types.len())==Some(before.types.as_slice()),"patch5 original types changed");
    let expected=[("muguet_script",2,false),("muguet_explosion",1,false),("muguet_timewarp",1,true),
        ("muguet_xr_camera_post",2,false),("muguet_xr_scene_post",1,false),
        ("muguet_xr_viewport_command",1,false),("muguet_xr_current_view",2,false),("muguet_xr_gpu_command",1,false)];
    for (offset,(name,argc,returns)) in expected.into_iter().enumerate() {
        let (module,actual,ty)=&after.imports[before.imports.len()+offset];
        ensure!(module=="env"&&actual==name,"unexpected patch5 import name");
        let TypeRef::Func(ty)=ty else{bail!("patch5 hook is not a function")};
        let ty=after.types.get(*ty as usize).context("patch5 import type invalid")?;
        ensure!(ty.params()==vec![ValType::I64;argc]&&ty.results()==if returns{vec![ValType::I64]}else{vec![]},"patch5 hook signature changed");
    }
    let index=EMIT+shift;let midpoint=before.import_count+7;
    ensure!(after.names.get(EMIT_NAME).is_some_and(|v|v.as_slice()==[index])&&after.ty(index)?==before.ty(EMIT)?,"patch5 Emit name/type changed");
    let expected=canonical(before.body(EMIT)?,before.import_count,shift,Some(midpoint))?;
    ensure!(canonical(after.body(index)?,after.import_count,0,None)?==expected,"patch5 Emit differs from exact midpoint insertion/remap");
    let manifest=serde_json::json!({"schema":SCHEMA,"version":VERSION,"base_patch_version":5,
        "original_sha256":ORIGINAL_SHA,"patch5_sha256":sha256(&patched)?,"patch5_import_shift":shift,
        "original_emit":EMIT,"patch5_emit":index,"artifact_emit":index+1,
        "completion_imports":1,"completion_abi":"(i64 saved_actual_command_pointer) -> ()",
        "original_normal_join":"0x17c141, after outer End0x17c140 and producer release/optional wake",
        "meaning":"completed Emit normal exit only; midpoint completed",
        "init_bypass_or_trap":"no completion","runtime_activation":false,"current_signed_runtime_compatible":false,
        "authority":"no shader consumption, publication success, foreground family, coherent stereo or globally safe mailbox boundary"});
    let wasm=instrument(&patched,index,midpoint,EMIT_NAME,Some(&manifest))?;
    Ok(Artifact {wasm,manifest})
}

fn sha256(bytes:&[u8])->Result<String> {
    ensure!(bytes.len()<=u32::MAX as usize,"Emit hash input too large");
    #[cfg(target_vendor="apple")]
    { extern "C" {fn CC_SHA256(p:*const std::ffi::c_void,n:u32,out:*mut u8)->*mut u8;}
      let mut digest=[0u8;32];unsafe{CC_SHA256(bytes.as_ptr().cast(),bytes.len() as u32,digest.as_mut_ptr());}
      Ok(digest.iter().map(|b|format!("{b:02x}")).collect()) }
    #[cfg(not(target_vendor="apple"))]
    bail!("Emit artifact hashing requires the Apple host tool");
}
struct Info<'a>{types:Vec<FuncType>,function_types:Vec<u32>,imports:Vec<(String,String,TypeRef)>,import_count:u32,names:HashMap<String,Vec<u32>>,bodies:Vec<FunctionBody<'a>>,explicit_sections:bool}
impl<'a> Info<'a> {
    fn parse(bytes:&'a [u8])->Result<Self>{
        let mut s=Self{types:vec![],function_types:vec![],imports:vec![],import_count:0,names:HashMap::new(),bodies:vec![],explicit_sections:false};let mut has_types=false;let mut has_imports=false;
        for p in Parser::new(0).parse_all(bytes){match p? {
            Payload::TypeSection(t)=>{has_types=true;for t in t.into_iter_err_on_gc_types(){s.types.push(t?);}},
            Payload::ImportSection(i)=>{has_imports=true;for i in i.into_imports(){let i=i?;if let TypeRef::Func(t)=i.ty{s.import_count+=1;s.function_types.push(t);}s.imports.push((i.module.into(),i.name.into(),i.ty));}},
            Payload::FunctionSection(f)=>for t in f{s.function_types.push(t?);},
            Payload::CodeSectionEntry(b)=>s.bodies.push(b),
            Payload::CustomSection(c)=>{ensure!(c.name()!=MANIFEST,"Emit artifact already marked");if let KnownCustom::Name(n)=c.as_known(){for n in n{if let Name::Function(n)=n?{for n in n{let n=n?;s.names.entry(n.name.into()).or_default().push(n.index);}}}}},_=>{}
        }}s.explicit_sections=has_types&&has_imports;Ok(s)
    }
    fn ty(&self,index:u32)->Result<&FuncType>{self.types.get(*self.function_types.get(index as usize).context("Emit function type missing")? as usize).context("Emit type invalid")}
    fn body(&self,index:u32)->Result<&FunctionBody<'a>>{self.bodies.get(index.checked_sub(self.import_count).context("Emit target imported")? as usize).context("Emit body missing")}
}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>>{if index>=self.imports{index.checked_add(self.shift).ok_or_else(||Error::UserError(anyhow::anyhow!("Emit function index overflow")))}else{Ok(index)}}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32,midpoint:Option<u32>)->Result<Vec<u8>>{
    let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("Emit locals: {e:?}"))?;
    let mut ops=body.get_operators_reader()?;let mut injected=0;
    while !ops.eof(){let offset=ops.original_position()-body.range().start;let op=ops.read()?;
        let boundary=offset==0x9a&&matches!(op,O::LocalSet{local_index:2});
        f.instruction(&r.instruction(op).map_err(|e|anyhow::anyhow!("Emit instruction: {e:?}"))?);
        if boundary {if let Some(midpoint)=midpoint{f.instruction(&wasm_encoder::Instruction::LocalGet(3)).instruction(&wasm_encoder::Instruction::Call(midpoint));injected+=1;}}
    }
    ensure!(midpoint.is_none()||injected==1,"Emit midpoint certificate missing");Ok(f.into_raw_body())
}
fn instrument(bytes:&[u8],target:u32,midpoint:u32,name:&str,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let info=Info::parse(bytes)?;
    ensure!(info.explicit_sections,"Emit factory requires explicit type/import sections");
    ensure!(!info.imports.iter().any(|(m,_,_)|m==IMPORT_MODULE),"Emit completion import already present");
    ensure!(info.names.get(name).is_some_and(|v|v.as_slice()==[target]),"Emit target name/index mismatch");
    ensure!(info.ty(target)?.params()==[ValType::I32,ValType::I64,ValType::I32]&&info.ty(target)?.results().is_empty(),"Emit target signature mismatch");
    ensure!(midpoint<info.import_count&&info.ty(midpoint)?.params()==[ValType::I64]&&info.ty(midpoint)?.results().is_empty(),"Emit midpoint import signature mismatch");
    info.body(target)?;
    let mut r=CompletionReencoder{imports:info.import_count,types:info.types.len() as u32,next:info.import_count,target,midpoint,wrapped:0};
    let mut module=wasm_encoder::Module::new();r.parse_core_module(&mut module,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("Emit completion reencode: {e:?}"))?;
    ensure!(r.wrapped==1,"Emit target not wrapped exactly once");
    if let Some(manifest)=manifest{module.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MANIFEST),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let output=module.finish();wasmparser::Validator::new().validate_all(&output).context("Emit completion artifact validation")?;Ok(output)
}
struct CompletionReencoder{imports:u32,types:u32,next:u32,target:u32,midpoint:u32,wrapped:u32}
impl Reencode for CompletionReencoder{
    type Error=anyhow::Error;
    fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>>{if index>=self.imports{index.checked_add(1).ok_or_else(||Error::UserError(anyhow::anyhow!("Emit remap overflow")))}else{Ok(index)}}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_type_section(self,out,s)?;out.ty().function([wasm_encoder::ValType::I64],[]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_import_section(self,out,s)?;out.import(IMPORT_MODULE,COMPLETE,wasm_encoder::EntityType::Function(self.types));Ok(())}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{
        let index=self.next;self.next+=1;if index!=self.target{return utils::parse_function_body(self,out,body);}
        use wasm_encoder::{Function,Instruction as I,ValType as V,BlockType};
        let mut locals=Vec::new();let mut saved=3u32;
        for l in body.get_locals_reader()?{let (count,ty)=l?;saved=saved.checked_add(count).ok_or_else(||Error::UserError(anyhow::anyhow!("Emit local overflow")))?;locals.push((count,self.val_type(ty)?));}
        saved.checked_add(2).ok_or_else(||Error::UserError(anyhow::anyhow!("Emit local overflow")))?;
        locals.push((1,V::I64));locals.push((1,V::I32));let mut f=Function::new(locals);
        // Fresh invocation locals start at zero. No outer block is introduced:
        // original branch depths and debug label/local numbers remain unchanged.
        let mut ops=body.get_operators_reader()?;let mut depth=0u32;let mut midpoint_count=0;let mut ended=false;let mut previous_local3=false;
        while !ops.eof(){let op=ops.read()?;
            if matches!(op,O::Call{function_index} if function_index==self.midpoint){
                if !previous_local3{return Err(Error::UserError(anyhow::anyhow!("Emit midpoint argument is not local3")));}
                f.instruction(&self.instruction(op)?);
                // Set only after midpoint returns; its trap emits no completion.
                f.instruction(&I::LocalGet(3)).instruction(&I::LocalSet(saved)).instruction(&I::I32Const(1)).instruction(&I::LocalSet(saved+1));midpoint_count+=1;previous_local3=false;continue;
            }
            previous_local3=matches!(op,O::LocalGet{local_index:3});
            match op{
                O::End if depth==0=>{if !ops.eof(){return Err(Error::UserError(anyhow::anyhow!("Emit trailing operators")));}
                    f.instruction(&I::LocalGet(saved+1)).instruction(&I::If(BlockType::Empty)).instruction(&I::LocalGet(saved)).instruction(&I::Call(self.imports)).instruction(&I::End).instruction(&I::End);ended=true;},
                O::End=>{depth-=1;f.instruction(&I::End);},
                O::Block{..}|O::Loop{..}|O::If{..}=>{depth+=1;f.instruction(&self.instruction(op)?);},
                O::Br{relative_depth}|O::BrIf{relative_depth} if relative_depth>=depth=>return Err(Error::UserError(anyhow::anyhow!("Emit branch escapes certified normal join"))),
                O::Return|O::BrTable{..}|O::ReturnCall{..}|O::ReturnCallIndirect{..}|O::ReturnCallRef{..}|O::Try{..}|O::TryTable{..}|O::Catch{..}|O::CatchAll|O::Throw{..}|O::ThrowRef|O::Rethrow{..}|O::Delegate{..}=>return Err(Error::UserError(anyhow::anyhow!("unqualified Emit exit/exception flow"))),
                _=>{f.instruction(&self.instruction(op)?);}
            }
        }
        if !ended||depth!=0||midpoint_count!=1{return Err(Error::UserError(anyhow::anyhow!("Emit join/midpoint count mismatch")));}
        out.function(&f);self.wrapped+=1;Ok(())
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use wasm_encoder::{Instruction as I,ValType as V,Function,BlockType,ConstExpr,ExportKind,Elements,RefType,TableType};
    use std::sync::{Arc,Mutex};
    fn fixture(extra:Option<I<'static>>)->Vec<u8>{
        let mut m=wasm_encoder::Module::new();let mut types=wasm_encoder::TypeSection::new();
        types.ty().function([V::I32,V::I64,V::I32],[]);types.ty().function([V::I64],[]);types.ty().function([],[]);m.section(&types);
        let mut imports=wasm_encoder::ImportSection::new();imports.import("env","midpoint",wasm_encoder::EntityType::Function(1));imports.import("env","event",wasm_encoder::EntityType::Function(1));m.section(&imports);
        let mut funcs=wasm_encoder::FunctionSection::new();for t in [0,0,2]{funcs.function(t);}m.section(&funcs);
        let mut table=wasm_encoder::TableSection::new();table.table(TableType{element_type:RefType::FUNCREF,table64:false,minimum:1,maximum:Some(1),shared:false});m.section(&table);
        let mut exports=wasm_encoder::ExportSection::new();exports.export("target",ExportKind::Func,2);exports.export("dispatch",ExportKind::Func,3);exports.export("table",ExportKind::Table,0);m.section(&exports);
        m.section(&wasm_encoder::StartSection{function_index:4});
        let mut elements=wasm_encoder::ElementSection::new();elements.active(Some(0),&ConstExpr::i32_const(0),Elements::Functions(Cow::Borrowed(&[2])));m.section(&elements);
        let mut code=wasm_encoder::CodeSection::new();let mut f=Function::new([(1,V::I64),(2,V::I32)]);
        f.instruction(&I::Block(BlockType::Empty)).instruction(&I::LocalGet(0)).instruction(&I::I32Eqz).instruction(&I::BrIf(0));
        f.instruction(&I::LocalGet(1)).instruction(&I::LocalSet(3)).instruction(&I::LocalGet(3)).instruction(&I::Call(0));
        if let Some(extra)=extra{f.instruction(&extra);}
        f.instruction(&I::LocalGet(0)).instruction(&I::I32Const(2)).instruction(&I::I32Eq).instruction(&I::If(BlockType::Empty)).instruction(&I::Unreachable).instruction(&I::End);
        f.instruction(&I::LocalGet(0)).instruction(&I::I32Const(4)).instruction(&I::I32Eq).instruction(&I::If(BlockType::Empty));
        f.instruction(&I::I32Const(1)).instruction(&I::LocalGet(1)).instruction(&I::I64Const(1)).instruction(&I::I64Add).instruction(&I::I32Const(0)).instruction(&I::Call(2)).instruction(&I::End);
        // The midpoint's actual pointer must survive reuse of original local3.
        f.instruction(&I::I64Const(999)).instruction(&I::LocalSet(3));
        f.instruction(&I::I64Const(10)).instruction(&I::Call(1));
        f.instruction(&I::LocalGet(0)).instruction(&I::I32Const(3)).instruction(&I::I32Eq).instruction(&I::BrIf(0));
        f.instruction(&I::I64Const(11)).instruction(&I::Call(1)).instruction(&I::End).instruction(&I::End);code.function(&f);
        let mut dispatch=Function::new([]);dispatch.instruction(&I::RefFunc(2)).instruction(&I::Drop);
        dispatch.instruction(&I::LocalGet(0)).instruction(&I::LocalGet(1)).instruction(&I::LocalGet(2)).instruction(&I::I32Const(0)).instruction(&I::CallIndirect{type_index:0,table_index:0}).instruction(&I::End);code.function(&dispatch);
        let mut start=Function::new([]);start.instruction(&I::I64Const(-1)).instruction(&I::Call(1)).instruction(&I::End);code.function(&start);m.section(&code);
        let mut names=wasm_encoder::NameSection::new();let mut fnames=wasm_encoder::NameMap::new();fnames.append(2,EMIT_NAME);fnames.append(3,"dispatch");fnames.append(4,"start");names.functions(&fnames);
        let mut locals=wasm_encoder::IndirectNameMap::new();let mut l=wasm_encoder::NameMap::new();l.append(3,"command");locals.append(2,&l);names.locals(&locals);
        let mut labels=wasm_encoder::IndirectNameMap::new();let mut l=wasm_encoder::NameMap::new();l.append(0,"outer");labels.append(2,&l);names.labels(&labels);m.section(&names);m.finish()
    }
    type Events=Arc<Mutex<Vec<(String,i64)>>>;
    fn run(mode:i32,ptr:i64,via_table:bool)->(bool,Vec<(String,i64)>){
        let bytes=instrument(&fixture(None),2,0,EMIT_NAME,None).unwrap();
        let engine=wasmtime::Engine::default();let module=wasmtime::Module::new(&engine,bytes).unwrap();let mut linker=wasmtime::Linker::new(&engine);
        let events:Events=Arc::default();let e=events.clone();
        linker.func_wrap("env","midpoint",move|ptr:i64|->wasmtime::Result<()>{e.lock().unwrap().push(("midpoint".into(),ptr));if ptr==-7{Err(wasmtime::Error::msg("midpoint trap"))}else{Ok(())}}).unwrap();
        let e=events.clone();linker.func_wrap("env","event",move|value:i64|{e.lock().unwrap().push(("event".into(),value));}).unwrap();
        let e=events.clone();linker.func_wrap(IMPORT_MODULE,COMPLETE,move|ptr:i64|{e.lock().unwrap().push(("complete".into(),ptr));}).unwrap();
        let mut store=wasmtime::Store::new(&engine,());let instance=linker.instantiate(&mut store,&module).unwrap();
        let result=instance.get_typed_func::<(i32,i64,i32),()>(&mut store,if via_table{"dispatch"}else{"target"}).unwrap().call(&mut store,(mode,ptr,0));
        let collected=events.lock().unwrap().clone();(result.is_ok(),collected)
    }
    fn ev(kind:&str,value:i64)->(String,i64){(kind.into(),value)}
    #[test]fn completion_follows_unlock_notify_and_preserves_saved_pointer(){
        assert_eq!(run(1,41,false),(true,vec![ev("event",-1),ev("midpoint",41),ev("event",10),ev("event",11),ev("complete",41)]));
        assert_eq!(run(3,42,false),(true,vec![ev("event",-1),ev("midpoint",42),ev("event",10),ev("complete",42)]));
    }
    #[test]fn init_bypass_midpoint_trap_and_later_trap_emit_no_completion(){
        assert_eq!(run(0,41,false),(true,vec![ev("event",-1)]));
        assert_eq!(run(1,-7,false),(false,vec![ev("event",-1),ev("midpoint",-7)]));
        assert_eq!(run(2,41,false),(false,vec![ev("event",-1),ev("midpoint",41)]));
    }
    #[test]fn nested_invocations_retain_their_own_pointer_and_completion_order(){
        assert_eq!(run(4,41,false),(true,vec![ev("event",-1),ev("midpoint",41),ev("midpoint",42),ev("event",10),ev("event",11),ev("complete",42),ev("event",10),ev("event",11),ev("complete",41)]));
    }
    #[test]fn exports_start_calls_refs_elements_names_and_original_locals_remap(){
        assert_eq!(run(1,41,true),run(1,41,false));
        let bytes=instrument(&fixture(None),2,0,EMIT_NAME,None).unwrap();let info=Info::parse(&bytes).unwrap();assert_eq!(info.import_count,3);assert_eq!(info.names[EMIT_NAME],[3]);
        let mut maps=vec![];
        for p in Parser::new(0).parse_all(&bytes){if let Payload::CustomSection(c)=p.unwrap(){if let KnownCustom::Name(ns)=c.as_known(){for n in ns{match n.unwrap(){Name::Local(m)|Name::Label(m)=>for e in m{let e=e.unwrap();for name in e.names{let name=name.unwrap();maps.push((e.index,name.index,name.name.to_owned()));}},_=>{}}}}}}
        assert!(maps.contains(&(3,3,"command".into())));assert!(maps.contains(&(3,0,"outer".into())));
    }
    #[test]fn unqualified_control_flow_and_public_nonoriginal_inputs_rejected(){
        for op in [I::Return,I::Br(1)]{assert!(instrument(&fixture(Some(op)),2,0,EMIT_NAME,None).is_err());}
        assert!(instrument(&fixture(None),2,1,EMIT_NAME,None).is_err());
        assert!(generate(&fixture(None),&[]).is_err());
    }
    #[test]
    #[ignore="requires exact private original; parser/reencoder only, never game instantiation"]
    fn private_original_default_identity_and_artifact_relationship(){
        let original=std::fs::read("/Users/zach/gaming/game-assets/playgta5-full-game-v1/game.wasm").unwrap();
        let baseline=crate::patch::patch(&original,&[]).unwrap();
        assert_eq!(baseline.len(),63202373);assert_eq!(sha256(&baseline).unwrap(),"8cfe0a666883d5ce7db2d530a71fceafbf182b1482baebf8912275d40c6bc07e");
        let artifact=generate(&original,&[]).unwrap();assert_eq!(artifact.manifest["runtime_activation"],false);assert_eq!(artifact.manifest["artifact_emit"],559);
        let mut imports=0;let mut completion_calls=vec![];let mut next=0;let mut manifest_count=0;
        for p in Parser::new(0).parse_all(&artifact.wasm){match p.unwrap(){
            Payload::ImportSection(s)=>for i in s.into_imports(){if let TypeRef::Func(_)=i.unwrap().ty{imports+=1;}},
            Payload::FunctionSection(_)=>next=imports,
            Payload::CodeSectionEntry(b)=>{let mut ops=b.get_operators_reader().unwrap();while !ops.eof(){if let O::Call{function_index:93}=ops.read().unwrap(){completion_calls.push(next);}}next+=1;},
            Payload::CustomSection(c) if c.name()==MANIFEST=>{manifest_count+=1;assert_eq!(serde_json::from_slice::<serde_json::Value>(c.data()).unwrap(),artifact.manifest);},_=>{}
        }}
        assert_eq!(imports,94);assert_eq!(completion_calls,[559]);assert_eq!(manifest_count,1);
        assert_eq!(crate::patch::patch(&original,&[]).unwrap(),baseline);
        eprintln!("Emit artifact: {} bytes SHA256 {}; default patch5: {} bytes SHA256 {}",artifact.wasm.len(),sha256(&artifact.wasm).unwrap(),baseline.len(),sha256(&baseline).unwrap());
    }
}

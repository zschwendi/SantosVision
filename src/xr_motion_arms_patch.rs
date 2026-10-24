//! A second, separately requested factory layered on the preserved four-body
//! motion artifact. Only original38481 and38548 gain arm-input hooks.
use anyhow::{ensure,Context,Result};
use std::borrow::Cow;
use wasm_encoder::{reencode::{utils,Error,Reencode},Instruction as I,ValType as V,BlockType as B};
use wasmparser::{Parser,Payload,TypeRef,FuncType,FunctionBody,Operator as O,ValType};
const IMPORTS:u32=3;
const PRE:u32=38481;
const ITERATE:u32=38548;
const MODULE:&str=crate::xr_motion_arms::IMPORT_MODULE;
const MARKER:&str="muguet.motion-arms.v1";
const KNOWN:&[(u32,usize,&str)]=&[
    (PRE,11022,"fe268b5e1845b15af7236197c83ccf6aec90613fdbc278a26b0623fffe3f2abd"),
    (ITERATE,20402,"2599fc05a25d03b307d662bae4c6be181bbbe8e9c078b933118430d80b7ea963"),
    (38487,7622,"95cfec7fe1ebdbcfb54b8106fda24c6b4dd90b85b664e25c91ffc7ff62c2270a"),
    (38506,453,"1e6e2177a4958b13dcd2d0da58651cf35640185ae21c0c104eb3ba09f3e63f8f"),
];
pub struct Artifact{pub wasm:Vec<u8>,pub manifest:serde_json::Value}
pub fn generate(original:&[u8],wanted:&[(&str,&str)])->Result<Artifact>{
    // The first factory pins the entire original and certifies its four bodies.
    let base=crate::xr_motion_patch::generate(original,wanted)?;
    let old=Info::parse(original)?;let patched=Info::parse(&base.wasm)?;
    let shift=patched.imports.checked_sub(old.imports).context("arm base removed imports")?;
    ensure!(shift==12,"arm extension requires exact patch5 + four-body motion base");
    for &(index,size,hash) in KNOWN{let body=old.body(index)?;let range=body.range();
        ensure!(range.end-range.start==size as u64&&crate::xr_motion_patch::sha256(&original[range.start as usize..range.end as usize])?==hash,"native arm source mismatch {index}");
        ensure!(canonical(body,old.imports,shift)?==canonical(patched.body(index+shift)?,patched.imports,0)?,"four-body base changed native arm body {index}");
    }
    ensure!(old.ty(PRE)?.params()==[ValType::I64,ValType::F32]&&old.ty(PRE)?.results().is_empty(),"PreIkUpdate ABI changed");
    ensure!(old.ty(ITERATE)?.params()==[ValType::I64,ValType::F32,ValType::I64]&&old.ty(ITERATE)?.results().is_empty(),"Arm Iterate ABI changed");
    ensure!(old.ty(38506)?.params()==[ValType::I64,ValType::I32,ValType::I64,ValType::I32,ValType::F32,ValType::F32,ValType::F32,ValType::F32]&&old.ty(38506)?.results().is_empty(),"absolute arm request ABI changed");
    ensure!(old.ty(9209)?.params()==[ValType::I64,ValType::I32,ValType::I64]&&old.ty(9209)?.results()==[ValType::I32],"native bone conversion ABI changed");
    let pre=ordinal(old.body(PRE)?,3523,|o|matches!(o,O::LocalGet{local_index:0}))?;
    let target=ordinal(old.body(ITERATE)?,0x1604be7-0x1604985,|o|matches!(o,O::If{..}))?;
    let config=Config{pre:PRE+shift,iterate:ITERATE+shift,predicate:45913+shift,request:38506+shift,convert:9209+shift,
        sp:0,tls:named_global(&base.wasm,"__tls_base")?,pre_ordinal:pre,target_ordinal:target};
    let manifest=serde_json::json!({"schema":"muguet.motion-arms","version":1,"base":base.manifest,
        "changed_original_bodies":[76735,82184,76770,76778,PRE,ITERATE],"arm_request":"38481+3523 after stock requests, native38506 flags256",
        "target":"38548 after original38487 true at0x01604be7, private stack+96 before inverse/blend",
        "native_blend_preserved":true,"shared_goal_writes":false,"bone_or_weapon_writes":false,
        "request_scratch_bytes":32,"arm":"right full pose with live K/A; left grip translation preserving native wrist; exact packet padding cookies",
        "left_orientation_policy":"native wrist basis and native flags0; tracked grip translation only",
        "unsupported":"native fallback for cached muzzle branch, alternate attachment orientation, foreign model/owner, stale/lost packet or mode exit",
        "physical_acceptance":false});
    let wasm=instrument(&base.wasm,config,Some(&manifest))?;Ok(Artifact{wasm,manifest})
}
fn ordinal(body:&FunctionBody<'_>,at:u64,check:impl Fn(&O<'_>)->bool)->Result<usize>{let mut r=body.get_operators_reader()?;let mut n=0;
    while !r.eof(){let offset=r.original_position()-body.range().start;let op=r.read()?;if offset==at{ensure!(check(&op),"native arm seam opcode changed");return Ok(n);}n+=1;}anyhow::bail!("native arm seam absent")}
pub(crate) fn named_global(bytes:&[u8],name:&str)->Result<u32>{let mut found=vec![];
    for p in Parser::new(0).parse_all(bytes){if let Payload::CustomSection(c)=p?{if let wasmparser::KnownCustom::Name(n)=c.as_known(){for n in n{if let wasmparser::Name::Global(m)=n?{for n in m{let n=n?;if n.name==name{found.push(n.index);}}}}}}}
    ensure!(found.len()==1,"native arm global name ambiguous/absent");Ok(found[0])}
struct Info<'a>{imports:u32,entries:Vec<(String,String,TypeRef)>,types:Vec<FuncType>,funcs:Vec<u32>,bodies:Vec<FunctionBody<'a>>}
impl<'a>Info<'a>{fn parse(bytes:&'a[u8])->Result<Self>{let mut s=Self{imports:0,entries:vec![],types:vec![],funcs:vec![],bodies:vec![]};
    for p in Parser::new(0).parse_all(bytes){match p?{
        Payload::TypeSection(t)=>for t in t.into_iter_err_on_gc_types(){s.types.push(t?);},
        Payload::ImportSection(i)=>for i in i.into_imports(){let i=i?;s.entries.push((i.module.into(),i.name.into(),i.ty));if let TypeRef::Func(t)=i.ty{s.imports+=1;s.funcs.push(t);}},
        Payload::FunctionSection(f)=>for t in f{s.funcs.push(t?);},Payload::CodeSectionEntry(b)=>s.bodies.push(b),_=>{}}}Ok(s)}
    fn body(&self,index:u32)->Result<&FunctionBody<'a>>{self.bodies.get(index.checked_sub(self.imports).context("arm target imported")?as usize).context("arm body absent")}
    fn ty(&self,index:u32)->Result<&FuncType>{self.types.get(*self.funcs.get(index as usize).context("arm function absent")?as usize).context("arm type absent")}}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+if i>=self.imports{self.shift}else{0})}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>>{let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("arm locals {e:?}"))?;
    let mut reader=body.get_operators_reader()?;while !reader.eof(){f.instruction(&r.parse_instruction(&mut reader).map_err(|e|anyhow::anyhow!("arm canonical {e:?}"))?);}Ok(f.into_raw_body())}
#[derive(Clone,Copy)]struct Config{pre:u32,iterate:u32,predicate:u32,request:u32,convert:u32,sp:u32,tls:u32,pre_ordinal:usize,target_ordinal:usize}
fn instrument(bytes:&[u8],config:Config,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let info=Info::parse(bytes)?;ensure!(!info.entries.iter().any(|e|e.0==MODULE),"arm artifact already instrumented");
    let mut r=Arms{imports:info.imports,types:info.types.len()as u32,next:info.imports,config,changed:0};let mut m=wasm_encoder::Module::new();
    r.parse_core_module(&mut m,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("arm reencode {e:?}"))?;ensure!(r.changed==2,"arm targets not changed exactly once");
    if let Some(manifest)=manifest{m.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MARKER),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let out=m.finish();wasmparser::Validator::new().validate_all(&out).context("native arm artifact validation")?;
    preserve(bytes,&out,config)?;Ok(out)
}
fn preserve(before:&[u8],after:&[u8],config:Config)->Result<()>{let a=Info::parse(before)?;let b=Info::parse(after)?;
    ensure!(b.entries.len()==a.entries.len()+IMPORTS as usize&&b.entries[..a.entries.len()]==a.entries&&b.imports==a.imports+IMPORTS,"arm changed existing imports");
    ensure!(b.types.len()==a.types.len()+IMPORTS as usize&&b.types[..a.types.len()]==a.types,"arm changed existing types");
    ensure!(b.bodies.len()==a.bodies.len()&&b.funcs[a.imports as usize+IMPORTS as usize..]==a.funcs[a.imports as usize..],"arm changed native function/type relationships");
    for i in a.imports..a.funcs.len()as u32{if i!=config.pre&&i!=config.iterate{ensure!(canonical(a.body(i)?,a.imports,IMPORTS)?==canonical(b.body(i+IMPORTS)?,b.imports,0)?,"arm changed nontarget function{i}");}}
    let mut remap=Remap{imports:a.imports,shift:IMPORTS};let mut m=wasm_encoder::Module::new();remap.parse_core_module(&mut m,Parser::new(0),before).map_err(|e|anyhow::anyhow!("arm preserve remap {e:?}"))?;let expected=m.finish();
    let sections=|bytes:&[u8]|->Result<Vec<(u8,Vec<u8>)>>{let mut out=vec![];for p in Parser::new(0).parse_all(bytes){let p=p?;
        if matches!(&p,Payload::CustomSection(c)if c.name()=="name"||c.name()==MARKER){continue;}if let Some((id,r))=p.as_section(){if ![1,2,10].contains(&id){out.push((id,bytes[r.start as usize..r.end as usize].to_vec()));}}}Ok(out)};
    ensure!(sections(&expected)?==sections(after)?,"arm changed data/globals/tables/elements/exports or preserved sections");Ok(())}
struct Arms{imports:u32,types:u32,next:u32,config:Config,changed:u32}
impl Reencode for Arms{type Error=anyhow::Error;
    fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>>{index.checked_add(if index>=self.imports{IMPORTS}else{0}).ok_or_else(||Error::UserError(anyhow::anyhow!("arm index overflow")))}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_type_section(self,out,s)?;
        out.ty().function([V::I64,V::F32,V::I64,V::I64,V::I32,V::I32],[V::I64]);out.ty().function([V::I64,V::I64],[]);
        out.ty().function([V::I64,V::I64,V::I64,V::I32,V::I64,V::I64,V::I64,V::I64,V::I32],[]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_import_section(self,out,s)?;
        for(i,name)in["request","request_complete","target"].into_iter().enumerate(){out.import(MODULE,name,wasm_encoder::EntityType::Function(self.types+i as u32));}Ok(())}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{let index=self.next;self.next+=1;
        if index==self.config.pre{self.pre(out,body)?;self.changed+=1;Ok(())}else if index==self.config.iterate{self.iterate(out,body)?;self.changed+=1;Ok(())}else{utils::parse_function_body(self,out,body)}}
}
fn mem(offset:u64)->wasm_encoder::MemArg{wasm_encoder::MemArg{offset,align:3,memory_index:0}}
impl Arms{
    fn locals(&mut self,body:&FunctionBody<'_>,params:u32,extra:&[(u32,V)])->Result<(wasm_encoder::Function,u32),Error<anyhow::Error>>{let mut locals=vec![];let mut fresh=params;
        for l in body.get_locals_reader()?{let(n,t)=l?;fresh+=n;locals.push((n,self.val_type(t)?));}locals.extend_from_slice(extra);Ok((wasm_encoder::Function::new(locals),fresh))}
    fn local_ped_test(&mut self,f:&mut wasm_encoder::Function,ped:u32){
        f.instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(mem(0))).instruction(&I::I64Eqz).instruction(&I::If(B::Result(V::I32)))
            .instruction(&I::I32Const(0)).instruction(&I::Else).instruction(&I::I64Const(20_159_720)).instruction(&I::I64Load(mem(0))).instruction(&I::I64Load(mem(8)))
            .instruction(&I::LocalGet(ped)).instruction(&I::I64Eq).instruction(&I::LocalGet(ped)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::I32And).instruction(&I::End);
    }
    fn predicate(&mut self,f:&mut wasm_encoder::Function,ped:u32)->Result<(),Error<anyhow::Error>>{f.instruction(&I::LocalGet(ped));for v in [0,0,0,1,0]{f.instruction(&I::I32Const(v));}f.instruction(&I::Call(self.function_index(self.config.predicate)?));Ok(())}
    fn pre(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<anyhow::Error>>{
        let(mut f,fresh)=self.locals(&body,2,&[(5,V::I64),(1,V::F32),(1,V::I32)])?;
        let(manager,scratch,token,skel,ped,dt,first)=(fresh,fresh+1,fresh+2,fresh+3,fresh+4,fresh+5,fresh+6);
        f.instruction(&I::LocalGet(0)).instruction(&I::LocalSet(manager)).instruction(&I::LocalGet(1)).instruction(&I::LocalSet(dt));
        let mut reader=body.get_operators_reader()?;let mut n=0;let mut inserted=0;
        while !reader.eof(){let op=reader.read()?;if n==self.config.pre_ordinal{
            f.instruction(&I::LocalGet(manager)).instruction(&I::I64Load(mem(352))).instruction(&I::LocalSet(ped));self.local_ped_test(&mut f,ped);
            f.instruction(&I::LocalGet(dt)).instruction(&I::F32Const(0f32.into())).instruction(&I::F32Gt).instruction(&I::I32And).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(manager)).instruction(&I::I64Load(mem(264))).instruction(&I::LocalTee(skel)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            self.predicate(&mut f,ped)?;f.instruction(&I::LocalTee(first)).instruction(&I::If(B::Empty));
            f.instruction(&I::GlobalGet(self.config.sp)).instruction(&I::I64Const(32)).instruction(&I::I64Sub).instruction(&I::LocalTee(scratch)).instruction(&I::GlobalSet(self.config.sp));
            // Native tag conversion bootstraps the first request even when no
            // CArmIkSolver exists yet; the native API constructs that solver.
            for(arm,hand_tag,aux_tag,flags)in[(1,57005,28422,256),(0,18905,60309,0)]{
            for(tag,offset)in[(hand_tag,16),(aux_tag,20)]{
                f.instruction(&I::LocalGet(scratch)).instruction(&I::I32Const(-1)).instruction(&I::I32Store(wasm_encoder::MemArg{offset,align:2,memory_index:0}));
                f.instruction(&I::LocalGet(skel)).instruction(&I::I64Load(mem(0))).instruction(&I::I32Const(tag)).instruction(&I::LocalGet(scratch)).instruction(&I::I64Const(offset as i64)).instruction(&I::I64Add)
                    .instruction(&I::Call(self.function_index(self.config.convert)?)).instruction(&I::Drop);
            }
            f.instruction(&I::LocalGet(manager)).instruction(&I::LocalGet(dt)).instruction(&I::LocalGet(scratch)).instruction(&I::GlobalGet(self.config.tls)).instruction(&I::LocalGet(first)).instruction(&I::I32Const(arm))
                .instruction(&I::Call(self.imports)).instruction(&I::LocalTee(token)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            f.instruction(&I::LocalGet(manager)).instruction(&I::I32Const(arm)).instruction(&I::LocalGet(scratch)).instruction(&I::I32Const(flags));
            for v in [0.0666f32,0.1333,0.,0.]{f.instruction(&I::F32Const(v.into()));}f.instruction(&I::Call(self.function_index(self.config.request)?));
            f.instruction(&I::LocalGet(token)).instruction(&I::GlobalGet(self.config.tls)).instruction(&I::Call(self.imports+1)).instruction(&I::End);
            }
            f.instruction(&I::LocalGet(scratch)).instruction(&I::I64Const(32)).instruction(&I::I64Add).instruction(&I::GlobalSet(self.config.sp));
            f.instruction(&I::End).instruction(&I::End).instruction(&I::End);inserted+=1;
        }f.instruction(&self.instruction(op)?);n+=1;}
        if inserted!=1{return Err(Error::UserError(anyhow::anyhow!("arm pre seam count mismatch")));}out.function(&f);Ok(())
    }
    fn iterate(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<anyhow::Error>>{
        let(mut f,fresh)=self.locals(&body,3,&[(3,V::I64),(1,V::I32)])?;let(solver,skeleton,ped,first)=(fresh,fresh+1,fresh+2,fresh+3);
        f.instruction(&I::LocalGet(0)).instruction(&I::LocalSet(solver)).instruction(&I::LocalGet(2)).instruction(&I::LocalSet(skeleton));
        let mut reader=body.get_operators_reader()?;let mut n=0;let mut inserted=0;
        while !reader.eof(){let op=reader.read()?;f.instruction(&self.instruction(op)?);if n==self.config.target_ordinal{
            f.instruction(&I::LocalGet(solver)).instruction(&I::I64Load(mem(480))).instruction(&I::LocalSet(ped));self.local_ped_test(&mut f,ped);f.instruction(&I::If(B::Empty));
            self.predicate(&mut f,ped)?;f.instruction(&I::LocalSet(first));
            for local in [solver,skeleton,29,40,27,31]{f.instruction(&I::LocalGet(local));}
            f.instruction(&I::LocalGet(26)).instruction(&I::I64Const(96)).instruction(&I::I64Add).instruction(&I::GlobalGet(self.config.tls)).instruction(&I::LocalGet(first))
                .instruction(&I::Call(self.imports+2)).instruction(&I::End);inserted+=1;
        }n+=1;}
        if inserted!=1{return Err(Error::UserError(anyhow::anyhow!("arm target seam count mismatch")));}out.function(&f);Ok(())
    }
}

#[cfg(test)]pub(crate) mod tests{
    use super::*;
    fn fixture()->(Vec<u8>,Config){
        use wasm_encoder::*;
        let mut m=Module::new();let mut t=TypeSection::new();
        t.ty().function([V::I64,V::I32,V::I32,V::I32,V::I32,V::I32],[V::I32]);
        t.ty().function([V::I64,V::I32,V::I64,V::I32,V::F32,V::F32,V::F32,V::F32],[]);
        t.ty().function([V::I64,V::I32,V::I64],[V::I32]);t.ty().function([V::I32],[]);
        t.ty().function([V::I64,V::F32],[]);t.ty().function([V::I64,V::F32,V::I64],[]);m.section(&t);
        let mut imports=ImportSection::new();for(i,name)in["predicate","request","convert","event"].into_iter().enumerate(){imports.import("env",name,EntityType::Function(i as u32));}
        imports.import("env","memory",EntityType::Memory(MemoryType{minimum:310,maximum:Some(310),memory64:true,shared:true,page_size_log2:None}));m.section(&imports);
        let mut fs=FunctionSection::new();fs.function(4).function(5);m.section(&fs);
        let mut gs=GlobalSection::new();gs.global(GlobalType{val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(8192));
        gs.global(GlobalType{val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(123));m.section(&gs);
        let mut exports=ExportSection::new();exports.export("pre",ExportKind::Func,4).export("iterate",ExportKind::Func,5)
            .export("sp",ExportKind::Global,0).export("__muguet_motion_stack_pointer",ExportKind::Global,0);m.section(&exports);
        let mut code=CodeSection::new();let mut pre=Function::new([]);
        // Optimized parameter reuse must not corrupt the injected native args.
        let preops=[I::I32Const(1),I::Call(3),I::I64Const(999),I::LocalSet(0),I::F32Const(88f32.into()),I::LocalSet(1),I::I32Const(2),I::Call(3),I::End];
        for op in preops{pre.instruction(&op);}code.function(&pre);
        let mut iterate=Function::new([(37,V::I64),(1,V::I32)]);let mut ops=vec![
            I::GlobalGet(0),I::I64Const(288),I::I64Sub,I::LocalTee(26),I::GlobalSet(0),
            I::LocalGet(2),I::LocalSet(29),I::LocalGet(0),I::I64Const(128),I::I64Add,I::LocalSet(27),
            I::LocalGet(0),I::I64Const(352),I::I64Add,I::LocalSet(31),I::I32Const(1),I::LocalSet(40),
            I::I64Const(999),I::LocalSet(0),I::I64Const(888),I::LocalSet(2),I::I64Const(1600),I::I32Load(MemArg{offset:0,align:2,memory_index:0})];
        let target_ordinal=ops.len();ops.push(I::If(B::Empty));
        // Stand-in for the original native inverse/blend after the target seam.
        // Its correction3 must survive rather than being overwritten afterward.
        ops.extend([I::I64Const(1800),I::LocalGet(26),I::I64Load(mem(96)),I::I64Const(3),I::I64Add,I::I64Store(mem(0)),I::End,
            I::LocalGet(26),I::I64Const(288),I::I64Add,I::GlobalSet(0),I::End]);
        for op in ops{iterate.instruction(&op);}code.function(&iterate);m.section(&code);
        (m.finish(),Config{pre:4,iterate:5,predicate:0,request:1,convert:2,sp:0,tls:1,pre_ordinal:6,target_ordinal})
    }
    /// The real instrumented fixture for layered factory tests: imports
    /// predicate0 request1(native stand-in) convert2 event3, hooks4..6, pre7.
    pub(crate) fn instrumented_fixture()->Vec<u8>{let(bytes,config)=fixture();instrument(&bytes,config,None).unwrap()}
    #[test]fn executable_request_order_private_target_native_blend_and_false_fallback(){
        use std::sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}};
        use wasmtime::{Config as EngineConfig,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder};
        let(bytes,config)=fixture();let bytes=instrument(&bytes,config,None).unwrap();
        let mut ec=EngineConfig::new();ec.wasm_memory64(true).wasm_threads(true).shared_memory(true);let engine=Engine::new(&ec).unwrap();
        let module=wasmtime::Module::new(&engine,&bytes).unwrap();let mt=MemoryTypeBuilder::new().min(310).max(Some(310)).memory64(true).shared(true).build().unwrap();
        let memory=SharedMemory::new(&engine,mt).unwrap();let events=Arc::new(Mutex::new(Vec::<String>::new()));
        for(a,v)in[(20_159_720,1024),(1032,77),(1200+352,77),(1200+264,3000),(3000,4096),(2000+480,77)]{assert!(crate::write_u64(&memory,a,v));}
        assert!(crate::write_u32(&memory,1600,1));let mut store=Store::new(&engine,());let mut linker=Linker::new(&engine);linker.define(&mut store,"env","memory",memory.clone()).unwrap();
        let sink=events.clone();linker.func_wrap("env","predicate",move|ped:i64,a:i32,b:i32,c:i32,d:i32,e:i32|{assert_eq!((ped,a,b,c,d,e),(77,0,0,0,1,0));sink.lock().unwrap().push("predicate".into());1i32}).unwrap();
        let mem=memory.clone();let sink=events.clone();linker.func_wrap("env","convert",move|data:i64,tag:i32,out:i64|{assert_eq!(data,4096);let value=match tag{57005=>10,28422=>11,18905=>20,60309=>21,_=>panic!("unknown bone")};assert!(crate::write_u32(&mem,out as u64,value));sink.lock().unwrap().push(format!("bone{tag}"));1i32}).unwrap();
        let mem=memory.clone();let sink=events.clone();linker.func_wrap("env","request",move|manager:i64,arm:i32,position:i64,flags:i32,a:f32,b:f32,c:f32,d:f32|{
            assert_eq!((manager,position),(1200,8160));assert_eq!(flags,if arm==1{256}else{assert_eq!(arm,0);0});assert_eq!([a,b,c,d],[0.0666,0.1333,0.,0.]);assert_eq!(crate::read_u64(&mem,position as u64),Some(777));sink.lock().unwrap().push(format!("native-request{arm}"));}).unwrap();
        let sink=events.clone();linker.func_wrap("env","event",move|value:i32|sink.lock().unwrap().push(format!("stock{value}"))).unwrap();
        let deny=Arc::new(AtomicBool::new(false));let denied=deny.clone();let mem=memory.clone();let sink=events.clone();
        linker.func_wrap(MODULE,"request",move|manager:i64,dt:f32,position:i64,tls:i64,first:i32,arm:i32|{
            assert_eq!((manager,dt,position,tls,first),(1200,0.016,8160,123,1));sink.lock().unwrap().push("input".into());
            assert!(crate::write_u64(&mem,position as u64,777));if denied.load(Ordering::Relaxed){0i64}else{42i64+arm as i64}}).unwrap();
        let sink=events.clone();linker.func_wrap(MODULE,"request_complete",move|token:i64,tls:i64|{assert!([42,43].contains(&token));assert_eq!(tls,123);sink.lock().unwrap().push("commit".into());}).unwrap();
        let mem=memory.clone();let sink=events.clone();linker.func_wrap(MODULE,"target",move|solver:i64,passed:i64,resolved:i64,arm:i32,goal:i64,request:i64,target:i64,tls:i64,first:i32|{
            assert_eq!((solver,passed,resolved,arm,goal,request,target,tls,first),(2000,3000,3000,1,2128,2352,8000,123,1));
            sink.lock().unwrap().push("target".into());assert!(crate::write_u64(&mem,target as u64,777));}).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();let pre=instance.get_typed_func::<(i64,f32),()>(&mut store,"pre").unwrap();
        pre.call(&mut store,(1200,0.016)).unwrap();assert_eq!(*events.lock().unwrap(),["stock1","predicate","bone57005","bone28422","input","native-request1","commit","bone18905","bone60309","input","native-request0","commit","stock2"]);
        assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8192));
        events.lock().unwrap().clear();deny.store(true,Ordering::Relaxed);pre.call(&mut store,(1200,0.016)).unwrap();
        assert_eq!(*events.lock().unwrap(),["stock1","predicate","bone57005","bone28422","input","bone18905","bone60309","input","stock2"]);
        assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8192));
        events.lock().unwrap().clear();let iterate=instance.get_typed_func::<(i64,f32,i64),()>(&mut store,"iterate").unwrap();
        iterate.call(&mut store,(2000,0.016,3000)).unwrap();assert_eq!(crate::read_u64(&memory,1800),Some(780));assert_eq!(*events.lock().unwrap(),["predicate","target"]);
        assert_eq!(instance.get_global(&mut store,"sp").unwrap().get(&mut store).i64(),Some(8192));
        events.lock().unwrap().clear();assert!(crate::write_u32(&memory,1600,0));assert!(crate::write_u64(&memory,1800,99));
        iterate.call(&mut store,(2000,0.016,3000)).unwrap();assert!(events.lock().unwrap().is_empty());assert_eq!(crate::read_u64(&memory,1800),Some(99));
    }
    #[test]#[ignore="exact private source parser/reencoder only; never instantiates game"]
    fn exact_original_six_bodies_preserve_every_other_body_and_native_data(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();
        let base=crate::xr_motion_patch::generate(&original,crate::mp::COMMANDS).unwrap();let artifact=generate(&original,crate::mp::COMMANDS).unwrap();
        assert_eq!(crate::xr_motion_patch::generate(&original,crate::mp::COMMANDS).unwrap().wasm,base.wasm);
        assert_eq!(artifact.manifest["native_blend_preserved"],true);
        if let Ok(path)=std::env::var("MUGUET_MOTION_ARMS_ARTIFACT_OUT"){std::fs::write(&path,&artifact.wasm).unwrap();std::fs::write(format!("{path}.json"),serde_json::to_vec_pretty(&artifact.manifest).unwrap()).unwrap();}
        eprintln!("six-body motion WASM {} bytes SHA256 {}",artifact.wasm.len(),crate::xr_motion_patch::sha256(&artifact.wasm).unwrap());
    }
}

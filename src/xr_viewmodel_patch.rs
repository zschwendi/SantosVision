//! Separately requested nine-body artifact layered on the preserved seven-body
//! cache artifact. Original44415 and44355 each gain one host call at their
//! certified completed-model seam, only when the original first-person branch
//! saved a parent viewport. Every other body, native data, global, table,
//! element and export is preserved. The host may rewrite only the invocation's
//! own twelve completed model coordinates (src/xr_viewmodel.rs).
use anyhow::{ensure,Context,Result};
use std::borrow::Cow;
use wasm_encoder::{reencode::{utils,Error,Reencode},Instruction as I,ValType as V,BlockType as B};
use wasmparser::{Parser,Payload,TypeRef,FuncType,FunctionBody,Operator as O,ValType};
const MARKER:&str="muguet.motion-viewmodel.v1";
const MODULE:&str=crate::xr_motion_arms::IMPORT_MODULE;
pub const IMPORT:&str="viewmodel";
const STREAM:u32=44415;
const SKINNED:u32=44355;
const KNOWN:&[(u32,u64,&str)]=&[
    (STREAM,4625,"c7da6a46fb0229575e2655e43ff2baae9e6145a9d11730395eac36fea23dfb90"),
    (SKINNED,2102,"37f7123bc3cc5eb1079350fbba57e1ff396a2dca87540c3dde81d5e7d78915d0"),
];
/// 44355 caches the completed translation in f32 locals20/19/18 before its
/// LOD distance; a rewritten model must be reloaded from Mat34+48/+52/+56.
const SKINNED_RELOAD:[(u32,u64);3]=[(20,1120),(19,1124),(18,1128)];
#[derive(Clone,Copy,Debug)]
pub(crate) struct Site{pub function:u32,pub ordinal:usize,pub family:i32,pub draw:u32,pub stack:u32,pub parent:u32,pub qualified:u32,pub reload:bool}
pub struct Artifact{pub wasm:Vec<u8>,pub manifest:serde_json::Value}

/// Exact original certificate: body bytes, ABI, local types and seam opcodes.
/// Returns the two seam operator ordinals in the ORIGINAL bodies.
pub(crate) fn certify_original(original:&[u8])->Result<(usize,usize)>{
    let old=Info::parse(original)?;
    for &(index,size,hash) in KNOWN{let body=old.body(index)?;let range=body.range();
        ensure!(range.end-range.start==size&&crate::xr_motion_patch::sha256(&original[range.start as usize..range.end as usize])?==hash,"completed-model source mismatch {index}");}
    ensure!(old.ty(STREAM)?.params()==[ValType::I64,ValType::I64,ValType::I64,ValType::I32,ValType::I32]&&old.ty(STREAM)?.results().is_empty(),"StreamPed Draw ABI changed");
    ensure!(old.ty(SKINNED)?.params()==[ValType::I64]&&old.ty(SKINNED)?.results().is_empty(),"skinned ExecuteStatic ABI changed");
    let i64=ValType::I64;let i32=ValType::I32;let f32=ValType::F32;
    ensure!([(0,i64),(5,i64),(18,i64),(34,i32)].iter().all(|&(l,t)|old.local(STREAM,l).ok()==Some(t)),"StreamPed seam local types changed");
    ensure!([(0,i64),(1,i64),(6,i64),(8,i32),(18,f32),(19,f32),(20,f32)].iter().all(|&(l,t)|old.local(SKINNED,l).ok()==Some(t)),"skinned seam local types changed");
    // Structural landmarks inside the pinned bodies: clone at frame+128, the
    // parent saved by SetCurrent, the non-first-person zero, completed model.
    let stream=old.body(STREAM)?;let skinned=old.body(SKINNED)?;
    landmarks(stream,&[(0x1a9c5d1,|o:&O<'_>|matches!(o,O::Call{function_index:9608})),(0x1a9c655,|o|matches!(o,O::LocalSet{local_index:18})),
        (0x1a9c857,|o|matches!(o,O::LocalSet{local_index:18})),(0x1a9c855,|o|matches!(o,O::I64Const{value:0})),
        (0x1a9c5dd,|o|matches!(o,O::BrIf{relative_depth:0})),(0x1a9c5da,|o|matches!(o,O::LocalGet{local_index:34}))])?;
    landmarks(skinned,&[(0x1a63cad,|o:&O<'_>|matches!(o,O::Call{function_index:9608})),(0x1a63d2a,|o|matches!(o,O::LocalSet{local_index:6})),
        (0x1a63cbf,|o|matches!(o,O::LocalGet{local_index:8})),(0x1a63fa9,|o|matches!(o,O::LocalGet{local_index:18}))])?;
    let a=seam(stream,0x1a9c894,|prev,op|matches!(prev,O::F32Store{memarg}if memarg.offset==1108)&&matches!(op,O::Block{..}))?;
    let b=seam(skinned,0x1a63f6e,|prev,op|matches!(prev,O::End)&&matches!(op,O::LocalGet{local_index:9}))?;
    Ok((a,b))
}
pub fn generate(original:&[u8],wanted:&[(&str,&str)])->Result<Artifact>{
    let(stream,skinned)=certify_original(original)?;
    let base=crate::xr_motion_cache_patch::generate(original,wanted)?;
    let old=Info::parse(original)?;let before=Info::parse(&base.wasm)?;
    let shift=before.imports.checked_sub(old.imports).context("viewmodel base imports")?;
    ensure!(shift==16,"viewmodel requires exact seven-body cache base");
    for &(index,_,_) in KNOWN{ensure!(canonical(old.body(index)?,old.imports,shift)?==canonical(before.body(index+shift)?,before.imports,0)?,"seven-body base changed completed-model body {index}");}
    let sites=vec![Site{function:STREAM+shift,ordinal:stream,family:44415,draw:0,stack:5,parent:18,qualified:34,reload:false},
        Site{function:SKINNED+shift,ordinal:skinned,family:44355,draw:1,stack:0,parent:6,qualified:8,reload:true}];
    let manifest=serde_json::json!({"schema":"muguet.motion-viewmodel","version":1,"base":base.manifest,
        "changed_original_bodies":[76735,82184,76770,76778,38481,38548,28266,STREAM,SKINNED],
        "seams":{"44415":"before0x01a9c894 after completed stack+1072 model; parent local18, context local34",
            "44355":"before0x01a63f6e after join; parent local6, context local8; reload f32 locals20/19/18 from +1120/+1124/+1128"},
        "hook_abi":"muguet_motion_arms_v1.viewmodel(I32 family,I64 draw_data,I64 frame,I64 parent_viewport,I32 context_qualified,I64 TLS)->()",
        "guest_gate":"called only when the original first-person branch saved a nonzero parent viewport",
        "host_writes":"invocation-owned completed model coordinates only","native_draw_calls":"original","physical_acceptance":false});
    let wasm=instrument(&base.wasm,&sites,crate::xr_motion_arms_patch::named_global(&base.wasm,"__tls_base")?,Some(&manifest))?;
    Ok(Artifact{wasm,manifest})
}
fn landmarks(body:&FunctionBody<'_>,checks:&[(u64,fn(&O<'_>)->bool)])->Result<()>{
    let mut r=body.get_operators_reader()?;let mut seen=0;
    while !r.eof(){let at=r.original_position();let op=r.read()?;if let Some((_,check))=checks.iter().find(|(a,_)|*a==at){ensure!(check(&op),"completed-model landmark changed at {at:#x}");seen+=1;}}
    ensure!(seen==checks.len(),"completed-model landmark absent");Ok(())
}
fn seam(body:&FunctionBody<'_>,at:u64,check:impl Fn(&O<'_>,&O<'_>)->bool)->Result<usize>{
    let mut r=body.get_operators_reader()?;let mut n=0;let mut previous=None;
    while !r.eof(){let offset=r.original_position();let op=r.read()?;
        if offset==at{ensure!(previous.as_ref().is_some_and(|p|check(p,&op)),"completed-model seam opcode changed");return Ok(n);}previous=Some(op);n+=1;}
    anyhow::bail!("completed-model seam absent")
}
struct Info<'a>{imports:u32,entries:Vec<(String,String,TypeRef)>,types:Vec<FuncType>,funcs:Vec<u32>,bodies:Vec<FunctionBody<'a>>}
impl<'a>Info<'a>{fn parse(bytes:&'a[u8])->Result<Self>{let mut s=Self{imports:0,entries:vec![],types:vec![],funcs:vec![],bodies:vec![]};for p in Parser::new(0).parse_all(bytes){match p?{
    Payload::TypeSection(t)=>for t in t.into_iter_err_on_gc_types(){s.types.push(t?);},Payload::ImportSection(i)=>for i in i.into_imports(){let i=i?;s.entries.push((i.module.into(),i.name.into(),i.ty));if let TypeRef::Func(t)=i.ty{s.imports+=1;s.funcs.push(t);}},Payload::FunctionSection(f)=>for t in f{s.funcs.push(t?);},Payload::CodeSectionEntry(b)=>s.bodies.push(b),_=>{}}}Ok(s)}
    fn body(&self,i:u32)->Result<&FunctionBody<'a>>{self.bodies.get(i.checked_sub(self.imports).context("viewmodel target imported")?as usize).context("viewmodel body missing")}
    fn ty(&self,i:u32)->Result<&FuncType>{self.types.get(*self.funcs.get(i as usize).context("viewmodel function type missing")?as usize).context("viewmodel type missing")}
    fn local(&self,function:u32,index:u32)->Result<ValType>{let params=self.ty(function)?.params();if let Some(t)=params.get(index as usize){return Ok(*t);}
        let mut next=params.len()as u32;for l in self.body(function)?.get_locals_reader()?{let(n,t)=l?;if index<next+n{return Ok(t);}next+=n;}anyhow::bail!("viewmodel local absent")}}
struct Remap{imports:u32,shift:u32}
impl Reencode for Remap{type Error=anyhow::Error;fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+if i>=self.imports{self.shift}else{0})}}
fn canonical(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>>{let mut r=Remap{imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("viewmodel locals {e:?}"))?;let mut reader=body.get_operators_reader()?;while !reader.eof(){f.instruction(&r.parse_instruction(&mut reader).map_err(|e|anyhow::anyhow!("viewmodel canonical {e:?}"))?);}Ok(f.into_raw_body())}
pub(crate) fn instrument(bytes:&[u8],sites:&[Site],tls:u32,manifest:Option<&serde_json::Value>)->Result<Vec<u8>>{
    let before=Info::parse(bytes)?;ensure!(!before.entries.iter().any(|e|e.0==MODULE&&e.1==IMPORT),"viewmodel already instrumented");
    let mut r=Hook{imports:before.imports,types:before.types.len()as u32,next:before.imports,sites:sites.to_vec(),tls,changed:0};let mut module=wasm_encoder::Module::new();
    r.parse_core_module(&mut module,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("viewmodel reencode {e:?}"))?;ensure!(r.changed==sites.len(),"viewmodel target count");
    if let Some(manifest)=manifest{module.section(&wasm_encoder::CustomSection{name:Cow::Borrowed(MARKER),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let out=module.finish();wasmparser::Validator::new().validate_all(&out).context("viewmodel artifact validation")?;
    let after=Info::parse(&out)?;ensure!(after.entries.len()==before.entries.len()+1&&after.entries[..before.entries.len()]==before.entries&&after.imports==before.imports+1,"viewmodel existing imports changed");
    ensure!(after.types.len()==before.types.len()+1&&after.types[..before.types.len()]==before.types,"viewmodel existing types changed");
    ensure!(after.bodies.len()==before.bodies.len()&&after.funcs[after.imports as usize..]==before.funcs[before.imports as usize..],"viewmodel functions changed");
    for i in before.imports..before.funcs.len()as u32{if !sites.iter().any(|s|s.function==i){ensure!(canonical(before.body(i)?,before.imports,1)?==canonical(after.body(i+1)?,after.imports,0)?,"viewmodel changed nontarget{i}");}}
    let mut remap=Remap{imports:before.imports,shift:1};let mut expected=wasm_encoder::Module::new();remap.parse_core_module(&mut expected,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("viewmodel preserve {e:?}"))?;
    let sections=|bytes:&[u8]|->Result<Vec<(u8,Vec<u8>)>>{let mut out=vec![];for p in Parser::new(0).parse_all(bytes){let p=p?;if matches!(&p,Payload::CustomSection(c)if c.name()=="name"||c.name()==MARKER){continue;}if let Some((id,r))=p.as_section(){if ![1,2,10].contains(&id){out.push((id,bytes[r.start as usize..r.end as usize].to_vec()));}}}Ok(out)};
    ensure!(sections(&expected.finish())?==sections(&out)?,"viewmodel changed native data/globals/tables/elements/exports");Ok(out)
}
struct Hook{imports:u32,types:u32,next:u32,sites:Vec<Site>,tls:u32,changed:usize}
impl Reencode for Hook{type Error=anyhow::Error;
    fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{Ok(i+u32::from(i>=self.imports))}
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,s:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_type_section(self,out,s)?;out.ty().function([V::I32,V::I64,V::I64,V::I64,V::I32,V::I64],[]);Ok(())}
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,s:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>>{utils::parse_import_section(self,out,s)?;out.import(MODULE,IMPORT,wasm_encoder::EntityType::Function(self.types));Ok(())}
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>>{
        let index=self.next;self.next+=1;let Some(site)=self.sites.iter().copied().find(|s|s.function==index)else{return utils::parse_function_body(self,out,body);};
        let mut f=self.new_function_with_parsed_locals(&body)?;let mut reader=body.get_operators_reader()?;let mut ordinal=0;let mut inserted=0;
        while !reader.eof(){let op=reader.read()?;if ordinal==site.ordinal{
            f.instruction(&I::LocalGet(site.parent)).instruction(&I::I64Eqz).instruction(&I::I32Eqz).instruction(&I::If(B::Empty));
            f.instruction(&I::I32Const(site.family)).instruction(&I::LocalGet(site.draw)).instruction(&I::LocalGet(site.stack)).instruction(&I::LocalGet(site.parent))
                .instruction(&I::LocalGet(site.qualified)).instruction(&I::GlobalGet(self.tls)).instruction(&I::Call(self.imports));
            if site.reload{for(local,offset)in SKINNED_RELOAD{f.instruction(&I::LocalGet(site.stack)).instruction(&I::F32Load(wasm_encoder::MemArg{offset,align:2,memory_index:0})).instruction(&I::LocalSet(local));}}
            f.instruction(&I::End);inserted+=1;
        }f.instruction(&self.instruction(op)?);ordinal+=1;}
        if inserted!=1{return Err(Error::UserError(anyhow::anyhow!("viewmodel insertion count")));}out.function(&f);self.changed+=1;Ok(())
    }
}

#[cfg(test)]mod tests{
    use super::*;
    use std::sync::{Arc,Mutex};
    const FRAME:i64=100_000;
    /// Two synthetic families: params/locals mirror the original seam local
    /// indices and types. Each completes a model, reaches the seam, then copies
    /// what downstream code observes (model words and44355's cached locals).
    fn module()->(Vec<u8>,Vec<Site>){use wasm_encoder::*;let mut m=Module::new();let mut types=TypeSection::new();
        types.ty().function([V::I64,V::I64,V::I64,V::I32,V::I32],[]);types.ty().function([V::I64,V::I64],[]);m.section(&types);
        let mut imports=ImportSection::new();imports.import("env","memory",EntityType::Memory(MemoryType{minimum:4,maximum:Some(4),memory64:true,shared:true,page_size_log2:None}));m.section(&imports);
        let mut funcs=FunctionSection::new();funcs.function(0);funcs.function(1);funcs.function(1);m.section(&funcs);
        let mut globals=GlobalSection::new();globals.global(GlobalType{val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(4321));m.section(&globals);
        let mut exports=ExportSection::new();exports.export("stream",ExportKind::Func,0).export("skinned",ExportKind::Func,1).export("other",ExportKind::Func,2);m.section(&exports);
        let st=|o:u64|MemArg{offset:o,align:2,memory_index:0};let mut code=CodeSection::new();
        // StreamPed-like (draw=L0, frame=L5, parent=L18, context=L34).
        let mut s=Function::new([(21,V::I64),(16,V::I32)]);
        for op in [I::I64Const(FRAME),I::LocalSet(5),I::LocalGet(1),I::LocalSet(18),I::LocalGet(3),I::LocalSet(34),
            I::LocalGet(5),I::F32Const(7.5f32.into()),I::F32Store(st(1120)),I::LocalGet(5),I::F32Const(1.25f32.into()),I::F32Store(st(1108))]{s.instruction(&op);}
        let stream_ordinal=12;
        for op in [I::Block(BlockType::Empty),I::End,I::LocalGet(5),I::LocalGet(5),I::F32Load(st(1120)),I::F32Store(st(2000)),I::End]{s.instruction(&op);}
        code.function(&s);
        // Skinned-like (frame=L0, draw=L1, parent=L6, context=L8, cached f32 L18..20).
        let mut k=Function::new([(5,V::I64),(11,V::I32),(12,V::F32)]);
        for op in [I::LocalGet(0),I::LocalSet(6),I::I64Const(FRAME),I::LocalSet(0),I::I32Const(1),I::LocalSet(8),
            I::Block(BlockType::Empty),I::LocalGet(0),I::F32Const(3f32.into()),I::LocalTee(20),I::F32Store(st(1120)),I::LocalGet(0),I::F32Const(4f32.into()),I::LocalTee(19),I::F32Store(st(1124)),
            I::LocalGet(0),I::F32Const(5f32.into()),I::LocalTee(18),I::F32Store(st(1128)),I::End]{k.instruction(&op);}
        let skinned_ordinal=20;
        for op in [I::LocalGet(9),I::Drop,I::LocalGet(0),I::LocalGet(20),I::F32Store(st(2000)),I::LocalGet(0),I::LocalGet(19),I::F32Store(st(2004)),I::LocalGet(0),I::LocalGet(18),I::F32Store(st(2008)),I::End]{k.instruction(&op);}
        code.function(&k);let mut o=Function::new([]);for op in [I::LocalGet(0),I::LocalGet(1),I::I64Store(MemArg{offset:3000,align:3,memory_index:0}),I::End]{o.instruction(&op);}code.function(&o);
        m.section(&code);let mut data=DataSection::new();data.passive([1u8,2,3]);m.section(&data);
        (m.finish(),vec![Site{function:0,ordinal:stream_ordinal,family:44415,draw:0,stack:5,parent:18,qualified:34,reload:false},
            Site{function:1,ordinal:skinned_ordinal,family:44355,draw:1,stack:0,parent:6,qualified:8,reload:true}])}
    type Calls=Arc<Mutex<Vec<(i32,i64,i64,i64,i32,i64)>>>;
    fn run(bytes:&[u8],fail:bool)->(wasmtime::SharedMemory,wasmtime::Store<()>,wasmtime::Instance,Calls){
        use wasmtime::{Config,Engine,Linker,Store,SharedMemory,MemoryTypeBuilder};
        let mut c=Config::new();c.wasm_memory64(true).wasm_threads(true).shared_memory(true);let engine=Engine::new(&c).unwrap();let module=wasmtime::Module::new(&engine,bytes).unwrap();
        let memory=SharedMemory::new(&engine,MemoryTypeBuilder::new().min(4).max(Some(4)).memory64(true).shared(true).build().unwrap()).unwrap();
        let calls:Calls=Arc::default();let mut linker=Linker::new(&engine);let mut store=Store::new(&engine,());linker.define(&mut store,"env","memory",memory.clone()).unwrap();
        let sink=calls.clone();let mem=memory.clone();
        linker.func_wrap(MODULE,IMPORT,move|family:i32,draw:i64,frame:i64,parent:i64,qualified:i32,tls:i64|->wasmtime::Result<()>{
            sink.lock().unwrap().push((family,draw,frame,parent,qualified,tls));if fail{return Err(wasmtime::Error::msg("synthetic viewmodel trap"));}
            for(i,v)in[9.5f32,10.5,11.5].into_iter().enumerate(){assert!(crate::write_u32(&mem,frame as u64+1120+i as u64*4,v.to_bits()));}Ok(())}).ok();
        let instance=linker.instantiate(&mut store,&module).unwrap();(memory,store,instance,calls)}
    #[test]fn seams_call_host_only_for_saved_parent_and_downstream_reads_corrected_model(){
        let(bytes,sites)=module();let out=instrument(&bytes,&sites,0,None).unwrap();
        let(memory,mut store,instance,calls)=run(&out,false);
        let stream=instance.get_typed_func::<(i64,i64,i64,i32,i32),()>(&mut store,"stream").unwrap();
        let skinned=instance.get_typed_func::<(i64,i64),()>(&mut store,"skinned").unwrap();
        let f=|a:u64|f32::from_bits(crate::read_u32(&memory,a).unwrap());
        stream.call(&mut store,(7000,0,0,1,1)).unwrap();assert!(calls.lock().unwrap().is_empty());assert_eq!(f(FRAME as u64+2000),7.5);
        stream.call(&mut store,(7000,8000,0,1,1)).unwrap();assert_eq!(*calls.lock().unwrap(),vec![(44415,7000,FRAME,8000,1,4321)]);assert_eq!(f(FRAME as u64+2000),9.5);
        calls.lock().unwrap().clear();skinned.call(&mut store,(0,6000)).unwrap();assert!(calls.lock().unwrap().is_empty());assert_eq!([f(FRAME as u64+2000),f(FRAME as u64+2004),f(FRAME as u64+2008)],[3.,4.,5.]);
        skinned.call(&mut store,(8000,6000)).unwrap();assert_eq!(*calls.lock().unwrap(),vec![(44355,6000,FRAME,8000,1,4321)]);
        // Locals20/19/18 now carry the corrected translation into native LOD code.
        assert_eq!([f(FRAME as u64+2000),f(FRAME as u64+2004),f(FRAME as u64+2008)],[9.5,10.5,11.5]);
        let other=instance.get_typed_func::<(i64,i64),()>(&mut store,"other").unwrap();other.call(&mut store,(8,6)).unwrap();assert_eq!(crate::read_u64(&memory,3008),Some(6));
    }
    #[test]fn host_failure_propagates_before_any_downstream_native_use(){
        let(bytes,sites)=module();let out=instrument(&bytes,&sites,0,None).unwrap();let(memory,mut store,instance,_)=run(&out,true);
        let skinned=instance.get_typed_func::<(i64,i64),()>(&mut store,"skinned").unwrap();assert!(skinned.call(&mut store,(8000,6000)).is_err());assert_eq!(crate::read_u32(&memory,FRAME as u64+2000),Some(0));
    }
    #[test]fn uninstrumented_bodies_types_and_data_are_preserved_and_double_instrumentation_fails(){
        let(bytes,sites)=module();let out=instrument(&bytes,&sites,0,None).unwrap();assert!(instrument(&out,&sites,0,None).is_err());
        let wrong=[Site{ordinal:999,..sites[0]}];assert!(instrument(&bytes,&wrong,0,None).is_err());
        let a=Info::parse(&bytes).unwrap();let b=Info::parse(&out).unwrap();assert_eq!(b.imports,a.imports+1);assert_eq!(b.entries.last().map(|e|(e.0.as_str(),e.1.as_str())),Some((MODULE,IMPORT)));
        assert_eq!(canonical(a.body(2).unwrap(),a.imports,1).unwrap(),canonical(b.body(3).unwrap(),b.imports,0).unwrap());
    }
    #[test]#[ignore="reads exact private original; parser-only certificate, never instantiates game"]
    fn exact_original_completed_model_seams_are_certified(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();
        assert_eq!(crate::xr_motion_patch::sha256(&original).unwrap(),"11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0");
        assert_eq!(certify_original(&original).unwrap(),(500,686));
    }
    #[test]#[ignore="exact separate nine-body parser/reencoder only; never instantiates game"]
    fn exact_original_nine_bodies_preserve_every_other_body_and_native_data(){
        let original=std::fs::read(std::env::var("MUGUET_TRACE_ORIGINAL_WASM").unwrap()).unwrap();let artifact=generate(&original,crate::mp::COMMANDS).unwrap();
        if let Ok(path)=std::env::var("MUGUET_MOTION_VIEWMODEL_ARTIFACT_OUT"){std::fs::write(&path,&artifact.wasm).unwrap();std::fs::write(format!("{path}.json"),serde_json::to_vec_pretty(&artifact.manifest).unwrap()).unwrap();}
        eprintln!("nine-body motion WASM {} bytes SHA256 {}",artifact.wasm.len(),crate::xr_motion_patch::sha256(&artifact.wasm).unwrap());
    }
}

/// Opt-in original-slice support: re-encode an exact ORIGINAL operator range
/// with its original locals, replacing each direct call by an import of the
/// identical original type. Used only by the ignored original slice tests.
#[cfg(test)]pub(crate) mod slice{
    use super::*;
    pub(crate) struct Slice{pub wasm:Vec<u8>,pub calls:Vec<(u32,wasmparser::FuncType)>,pub seam:usize}
    /// prologue/epilogue are raw instructions around [first,last] original ordinals;
    /// seam is reported as an ordinal of the produced function.
    pub(crate) fn build(original:&[u8],function:u32,first:usize,last:usize,seam:usize,prologue:&[I<'static>],epilogue:&[I<'static>],pages:u64)->Result<Slice>{
        let info=Info::parse(original)?;let body=info.body(function)?;let ty=info.ty(function)?.clone();
        let mut calls:Vec<(u32,wasmparser::FuncType)>=vec![];let mut reader=body.get_operators_reader()?;let mut n=0;let mut ops=vec![];
        while !reader.eof(){let op=reader.read()?;if (first..=last).contains(&n){if let O::Call{function_index}=op{if !calls.iter().any(|c|c.0==function_index){calls.push((function_index,info.ty(function_index)?.clone()));}}ops.push(op);}n+=1;}
        let vt=|t:&wasmparser::ValType|match t{wasmparser::ValType::I32=>V::I32,wasmparser::ValType::I64=>V::I64,wasmparser::ValType::F32=>V::F32,wasmparser::ValType::F64=>V::F64,wasmparser::ValType::V128=>V::V128,_=>panic!("unsupported slice type")};
        use wasm_encoder::*;let mut m=Module::new();let mut types=TypeSection::new();
        types.ty().function(ty.params().iter().map(vt),ty.results().iter().map(vt));for(_,t)in &calls{types.ty().function(t.params().iter().map(vt),t.results().iter().map(vt));}m.section(&types);
        let mut imports=ImportSection::new();imports.import("env","memory",EntityType::Memory(MemoryType{minimum:pages,maximum:Some(pages),memory64:true,shared:true,page_size_log2:None}));
        for(i,(index,_))in calls.iter().enumerate(){imports.import("env",&format!("f{index}"),EntityType::Function(1+i as u32));}m.section(&imports);
        let mut funcs=FunctionSection::new();funcs.function(0);m.section(&funcs);
        let mut globals=GlobalSection::new();globals.global(GlobalType{val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(77));m.section(&globals);
        let mut exports=ExportSection::new();exports.export("slice",ExportKind::Func,calls.len()as u32);m.section(&exports);
        struct Map(Vec<u32>);impl Reencode for Map{type Error=anyhow::Error;fn function_index(&mut self,i:u32)->Result<u32,Error<Self::Error>>{self.0.iter().position(|c|*c==i).map(|p|p as u32).ok_or(Error::UserError(anyhow::anyhow!("slice call outside map")))}}
        let mut map=Map(calls.iter().map(|c|c.0).collect());let mut f=map.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("{e:?}"))?;
        for op in prologue{f.instruction(op);}for op in ops{f.instruction(&map.instruction(op).map_err(|e|anyhow::anyhow!("{e:?}"))?);}for op in epilogue{f.instruction(op);}
        let mut code=CodeSection::new();code.function(&f);m.section(&code);let wasm=m.finish();wasmparser::Validator::new().validate_all(&wasm).context("slice validation")?;
        Ok(Slice{wasm,seam:prologue.len()+seam-first,calls})
    }
}

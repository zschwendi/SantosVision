//! Explicit JIT-only ancestry-artifact generator. Not a runtime/CLI option.
//!
//! No caller in the app invokes this module. Default patch5 and its cache key are
//! unchanged. The returned artifact requires two new imports and a new runtime
//! certificate; the currently signed original-AOT runtime cannot run it. Markers
//! mean function ANCESTRY, never foreground branch/draw/MRT ownership. Guest traps
//! intentionally emit no exit: a future runtime must integrate the independently
//! tested invocation cleanup, certified identities and immutable packet provenance.
use anyhow::{bail, ensure, Context, Result};
use std::{borrow::Cow, collections::{BTreeMap, HashMap}};
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasmparser::{FuncType, FunctionBody, GlobalType, KnownCustom, Name, Operator as O, Parser, Payload, TypeRef, ValType};

pub const SCHEMA: &str = "muguet.foreground-ancestry";
pub const VERSION: u32 = 1;
pub const IMPORT_MODULE: &str = "muguet_ancestry_v1";
pub const ENTER: &str = "enter";
pub const EXIT: &str = "exit";
const MANIFEST_SECTION: &str = "muguet.foreground-ancestry.v1";

#[derive(Clone, Debug)]
pub struct FamilyMarker {
    pub family: i32,
    pub name: String,
    pub original_function: u32,
    pub patch5_function: u32,
    pub artifact_function: u32,
}
#[derive(Debug)]
pub struct Artifact {
    pub wasm: Vec<u8>,
    pub manifest: serde_json::Value,
}

/// Requires the exact private original; never accepts a prepatched module or a
/// same-named substitute. Returns bytes in memory only; performs no disk writes,
/// AOT generation, host dispatch changes or runtime activation.
pub fn generate(original: &[u8], wanted: &[(&str, &str)]) -> Result<Artifact> {
    ensure!(sha256(original)? == crate::xr_foreground_scope::ORIGINAL_SHA256,
        "ancestry generator requires exact original SHA256");
    let certificate = crate::xr_foreground_scope::certify(original)?;
    let patched = crate::patch::patch(original, wanted)?;
    let (targets, tls, shift) = verify_patch5_relationship(original, &patched, &certificate)?;
    let markers: Vec<_> = targets.iter().map(|(&index,target)| FamilyMarker {
        family: target.family, name:target.name.clone(), original_function:index-shift,
        patch5_function:index, artifact_function:index+2,
    }).collect();
    let manifest = serde_json::json!({
        "schema":SCHEMA,"version":VERSION,"base_patch_version":crate::patch::VERSION,
        "original_sha256":crate::xr_foreground_scope::ORIGINAL_SHA256,
        "patch5_sha256":sha256(&patched)?,"patch5_function_import_shift":shift,
        "marker_function_imports":2,"tls_global":tls,"classification":"function ancestry only",
        "enter_abi":"(i32 family, i64 original_arg0, i64 original_tls) -> i64 token",
        "exit_abi":"(i32 family, i64 original_arg0, i64 original_tls, i64 token) -> ()",
        "trap_exit":"none; certified runtime invocation cleanup is required",
        "current_signed_runtime_compatible":false,"runtime_activation":false,
        "families":markers.iter().map(|m|serde_json::json!({"family":m.family,"name":m.name,
            "original_function":m.original_function,"patch5_function":m.patch5_function,
            "artifact_function":m.artifact_function})).collect::<Vec<_>>()
    });
    let wasm = instrument(&patched,targets,tls,Some(&manifest))?;
    Ok(Artifact {wasm,manifest})
}

fn sha256(bytes: &[u8]) -> Result<String> {
    ensure!(bytes.len() <= u32::MAX as usize,"ancestry input exceeds hash API length");
    #[cfg(target_vendor="apple")]
    {
        extern "C" { fn CC_SHA256(data:*const std::ffi::c_void,length:u32,digest:*mut u8)->*mut u8; }
        let mut digest=[0u8;32];
        unsafe { CC_SHA256(bytes.as_ptr().cast(),bytes.len() as u32,digest.as_mut_ptr()); }
        Ok(digest.iter().map(|b|format!("{b:02x}")).collect())
    }
    #[cfg(not(target_vendor="apple"))]
    bail!("ancestry artifact hashing is supported only by this Apple host tool");
}

struct Info<'a> {
    types: Vec<FuncType>, function_types: Vec<u32>, imports: Vec<(String,String,TypeRef)>,
    function_imports:u32, names:HashMap<String,Vec<u32>>, tls_names:Vec<u32>, globals:Vec<GlobalType>,
    bodies:Vec<FunctionBody<'a>>, has_import_section:bool, has_type_section:bool,
}
impl<'a> Info<'a> {
    fn parse(bytes:&'a [u8]) -> Result<Self> {
        let mut s=Self {types:vec![],function_types:vec![],imports:vec![],function_imports:0,
            names:HashMap::new(),tls_names:vec![],globals:vec![],bodies:vec![],has_import_section:false,has_type_section:false};
        for payload in Parser::new(0).parse_all(bytes) {match payload? {
            Payload::TypeSection(types)=>{s.has_type_section=true;for ty in types.into_iter_err_on_gc_types(){s.types.push(ty?);}},
            Payload::ImportSection(imports)=>{s.has_import_section=true;for import in imports.into_imports(){let import=import?;
                match import.ty {TypeRef::Func(ty)=>{s.function_imports+=1;s.function_types.push(ty);},TypeRef::Global(ty)=>s.globals.push(ty),_=>{}}
                s.imports.push((import.module.into(),import.name.into(),import.ty));
            }},
            Payload::FunctionSection(functions)=>for ty in functions{s.function_types.push(ty?);},
            Payload::GlobalSection(globals)=>for global in globals{s.globals.push(global?.ty);},
            Payload::CodeSectionEntry(body)=>s.bodies.push(body),
            Payload::CustomSection(custom)=>{
                ensure!(custom.name()!=MANIFEST_SECTION,"ancestry artifact already marked");
                if let KnownCustom::Name(names)=custom.as_known(){for subsection in names{match subsection?{
                    Name::Function(names)=>for n in names{let n=n?;s.names.entry(n.name.into()).or_default().push(n.index);},
                    Name::Global(names)=>for n in names{let n=n?;if n.name=="__tls_base"{s.tls_names.push(n.index);}},_=>{}
                }}}
            },_=>{}
        }}
        Ok(s)
    }
    fn ty(&self,index:u32)->Result<&FuncType> {
        self.types.get(*self.function_types.get(index as usize).context("missing ancestry function type")? as usize).context("invalid ancestry type")
    }
    fn body(&self,index:u32)->Result<&FunctionBody<'a>> {
        self.bodies.get(index.checked_sub(self.function_imports).context("ancestry target is imported")? as usize).context("missing ancestry body")
    }
    fn tls(&self)->Result<u32> {
        ensure!(self.tls_names.len()==1,"ancestry TLS name missing or ambiguous");
        let index=self.tls_names[0];let ty=self.globals.get(index as usize).context("ancestry TLS index invalid")?;
        ensure!(ty.content_type==ValType::I64&&ty.mutable&&!ty.shared,"ancestry TLS must be mutable nonshared i64");Ok(index)
    }
}
#[derive(Clone)]
struct Target {family:i32,name:String,params:u32}

fn verify_patch5_relationship(original:&[u8],patched:&[u8],certificate:&crate::xr_foreground_scope::SourceCertificate)
    ->Result<(BTreeMap<u32,Target>,u32,u32)> {
    let before=Info::parse(original)?;let after=Info::parse(patched)?;
    ensure!(crate::patch::VERSION==5,"ancestry generator requires patch5");
    let shift=after.function_imports.checked_sub(before.function_imports).context("patch5 removed imports")?;
    ensure!(shift==8&&after.imports.len()==before.imports.len()+8,"unexpected patch5 import shift");
    ensure!(after.imports[..before.imports.len()]==before.imports,"patch5 changed original imports");
    ensure!(after.types.get(..before.types.len())==Some(before.types.as_slice()),"patch5 changed original types");
    let expected=[("muguet_script",2,false),("muguet_explosion",1,false),("muguet_timewarp",1,true),
        ("muguet_xr_camera_post",2,false),("muguet_xr_scene_post",1,false),
        ("muguet_xr_viewport_command",1,false),("muguet_xr_current_view",2,false),("muguet_xr_gpu_command",1,false)];
    for (offset,(name,argc,returns)) in expected.into_iter().enumerate() {
        let (module,actual,ty)=&after.imports[before.imports.len()+offset];
        ensure!(module=="env"&&actual==name,"unexpected patch5 hook import");
        let TypeRef::Func(ty)=ty else{bail!("patch5 hook is not a function")};let ty=after.types.get(*ty as usize).context("patch5 hook type out of bounds")?;
        ensure!(ty.params()==vec![ValType::I64;argc]&&ty.results()==if returns{vec![ValType::I64]}else{vec![]},"patch5 hook ABI mismatch");
    }
    let tls=before.tls()?;ensure!(tls==certificate.tls_global&&after.tls()?==tls&&before.globals==after.globals,"patch5 changed TLS relationship");
    let mut targets=BTreeMap::new();
    for (family,audit) in certificate.functions.iter().enumerate() {
        let old=audit.original_function;let index=old.checked_add(shift).context("patch5 index overflow")?;
        ensure!(before.names.get(audit.name).is_some_and(|v|v.as_slice()==[old])
            &&after.names.get(audit.name).is_some_and(|v|v.as_slice()==[index]),"patch5 ancestry name/index mismatch");
        let ty=before.ty(old)?;ensure!(after.ty(index)?==ty&&ty.params().first()==Some(&ValType::I64)&&ty.results().is_empty(),"patch5 ancestry ABI mismatch");
        let expected=canonical_body(before.body(old)?,before.function_imports,shift)?;
        let actual=canonical_body(after.body(index)?,after.function_imports,0)?;
        ensure!(expected==actual,"patch5 ancestry body differs from original import remap: {}",audit.name);
        targets.insert(index,Target {family:family as i32+1,name:audit.name.into(),params:ty.params().len() as u32});
    }
    ensure!(targets.len()==3,"ancestry requires exactly three certified families");
    Ok((targets,tls,shift))
}
struct Remap {imports:u32,shift:u32}
impl Reencode for Remap {
    type Error=anyhow::Error;
    fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>> {
        if index>=self.imports {index.checked_add(self.shift).ok_or_else(||Error::UserError(anyhow::anyhow!("function index overflow")))} else{Ok(index)}
    }
}
fn canonical_body(body:&FunctionBody<'_>,imports:u32,shift:u32)->Result<Vec<u8>> {
    let mut r=Remap {imports,shift};let mut f=r.new_function_with_parsed_locals(body).map_err(|e|anyhow::anyhow!("ancestry locals: {e:?}"))?;
    let mut ops=body.get_operators_reader()?;while !ops.eof(){f.instruction(&r.parse_instruction(&mut ops).map_err(|e|anyhow::anyhow!("ancestry op: {e:?}"))?);}
    Ok(f.into_raw_body())
}

fn instrument(bytes:&[u8],targets:BTreeMap<u32,Target>,tls:u32,manifest:Option<&serde_json::Value>)->Result<Vec<u8>> {
    let info=Info::parse(bytes)?;
    ensure!(info.has_type_section&&info.has_import_section,"ancestry input needs explicit type and import sections");
    ensure!(info.tls()?==tls,"ancestry TLS relationship mismatch");
    ensure!(!info.imports.iter().any(|(m,_,_)|m==IMPORT_MODULE),"ancestry imports already present");
    for (&index,target) in &targets {let ty=info.ty(index)?;
        ensure!(ty.params().len()==target.params as usize&&ty.params().first()==Some(&ValType::I64)&&ty.results().is_empty(),"ancestry target signature rejected");
        ensure!(info.names.get(&target.name).is_some_and(|v|v.as_slice()==[index]),"ancestry target name rejected");
        info.body(index)?;
    }
    let mut r=MarkerReencoder {imports:info.function_imports,types:info.types.len() as u32,tls,
        next:info.function_imports,targets,wrapped:0};
    let expected=r.targets.len();let mut module=wasm_encoder::Module::new();
    r.parse_core_module(&mut module,Parser::new(0),bytes).map_err(|e|anyhow::anyhow!("ancestry reencode: {e:?}"))?;
    ensure!(r.wrapped==expected,"not every ancestry target was wrapped");
    if let Some(manifest)=manifest {module.section(&wasm_encoder::CustomSection {name:Cow::Borrowed(MANIFEST_SECTION),data:Cow::Owned(manifest.to_string().into_bytes())});}
    let output=module.finish();wasmparser::Validator::new().validate_all(&output).context("ancestry output validation")?;Ok(output)
}
struct MarkerReencoder {imports:u32,types:u32,tls:u32,next:u32,targets:BTreeMap<u32,Target>,wrapped:usize}
impl Reencode for MarkerReencoder {
    type Error=anyhow::Error;
    fn function_index(&mut self,index:u32)->Result<u32,Error<Self::Error>> {
        if index>=self.imports{index.checked_add(2).ok_or_else(||Error::UserError(anyhow::anyhow!("marker function index overflow")))}else{Ok(index)}
    }
    fn parse_type_section(&mut self,out:&mut wasm_encoder::TypeSection,section:wasmparser::TypeSectionReader<'_>)->Result<(),Error<Self::Error>> {
        utils::parse_type_section(self,out,section)?;
        out.ty().function([wasm_encoder::ValType::I32,wasm_encoder::ValType::I64,wasm_encoder::ValType::I64],[wasm_encoder::ValType::I64]);
        out.ty().function([wasm_encoder::ValType::I32,wasm_encoder::ValType::I64,wasm_encoder::ValType::I64,wasm_encoder::ValType::I64],[]);Ok(())
    }
    fn parse_import_section(&mut self,out:&mut wasm_encoder::ImportSection,section:wasmparser::ImportSectionReader<'_>)->Result<(),Error<Self::Error>> {
        utils::parse_import_section(self,out,section)?;
        out.import(IMPORT_MODULE,ENTER,wasm_encoder::EntityType::Function(self.types));
        out.import(IMPORT_MODULE,EXIT,wasm_encoder::EntityType::Function(self.types+1));Ok(())
    }
    fn parse_custom_name_subsection(&mut self,out:&mut wasm_encoder::NameSection,section:Name<'_>)->Result<(),Error<Self::Error>> {
        if let Name::Label(map)=section {
            let mut labels=wasm_encoder::IndirectNameMap::new();
            for entry in map {let entry=entry?;let mut names=wasm_encoder::NameMap::new();
                let shift=u32::from(self.targets.contains_key(&entry.index));
                for name in entry.names {let name=name?;
                    let index=name.index.checked_add(shift).ok_or_else(||Error::UserError(anyhow::anyhow!("ancestry label name overflow")))?;
                    names.append(index,name.name);
                }
                labels.append(self.function_index(entry.index)?,&names);
            }
            out.labels(&labels);Ok(())
        } else {utils::parse_custom_name_subsection(self,out,section)}
    }
    fn parse_function_body(&mut self,out:&mut wasm_encoder::CodeSection,body:FunctionBody<'_>)->Result<(),Error<Self::Error>> {
        let index=self.next;self.next+=1;
        let Some(target)=self.targets.get(&index).cloned() else{return utils::parse_function_body(self,out,body)};
        use wasm_encoder::{BlockType,Function,Instruction as I,ValType::I64};
        let mut locals=Vec::new();let mut saved=target.params;
        for local in body.get_locals_reader()? {let (count,ty)=local?;
            saved=saved.checked_add(count).ok_or_else(||Error::UserError(anyhow::anyhow!("ancestry local overflow")))?;
            locals.push((count,self.val_type(ty)?));
        }
        saved.checked_add(3).ok_or_else(||Error::UserError(anyhow::anyhow!("ancestry local overflow")))?;
        locals.push((3,I64));let mut f=Function::new(locals);
        f.instruction(&I::LocalGet(0)).instruction(&I::LocalSet(saved));
        f.instruction(&I::GlobalGet(self.tls)).instruction(&I::LocalSet(saved+1));
        f.instruction(&I::I32Const(target.family)).instruction(&I::LocalGet(saved)).instruction(&I::LocalGet(saved+1));
        f.instruction(&I::Call(self.imports)).instruction(&I::LocalSet(saved+2));
        // The original implicit function label becomes this explicit void block;
        // existing branch/table depths stay unchanged. Return branches out once.
        f.instruction(&I::Block(BlockType::Empty));
        let mut depth=0u32;let mut ended=false;let mut ops=body.get_operators_reader()?;
        while !ops.eof() {let op=ops.read()?;match op {
            O::End if depth==0=>{if !ops.eof(){return Err(Error::UserError(anyhow::anyhow!("ancestry operators after function end")))}f.instruction(&I::End);ended=true;},
            O::End=>{depth-=1;f.instruction(&I::End);},
            O::Return=>{f.instruction(&I::Br(depth));},
            O::Block{..}|O::Loop{..}|O::If{..}=>{depth+=1;f.instruction(&self.instruction(op)?);},
            O::ReturnCall{..}|O::ReturnCallIndirect{..}|O::ReturnCallRef{..}|O::Try{..}|O::TryTable{..}|O::Catch{..}|O::CatchAll|O::Throw{..}|O::ThrowRef|O::Rethrow{..}|O::Delegate{..}=>return Err(Error::UserError(anyhow::anyhow!("unqualified ancestry tail-call/exception control flow"))),
            _=>{f.instruction(&self.instruction(op)?);}
        }}
        if !ended||depth!=0{return Err(Error::UserError(anyhow::anyhow!("ancestry function end missing")))}
        f.instruction(&I::I32Const(target.family)).instruction(&I::LocalGet(saved)).instruction(&I::LocalGet(saved+1)).instruction(&I::LocalGet(saved+2));
        f.instruction(&I::Call(self.imports+1)).instruction(&I::End);out.function(&f);self.wrapped+=1;Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::{BlockType,ConstExpr,Elements,ExportKind,Function,Instruction as I,RefType,TableType,ValType as V};
    // This private fixture path never bypasses the public original-file SHA gate.
    fn fixture(ops:&[I<'_>],params5:bool)->Vec<u8> {
        fixture_with_nested(ops,params5,false)
    }
    fn fixture_with_nested(ops:&[I<'_>],params5:bool,nested_traps:bool)->Vec<u8> {
        let mut m=wasm_encoder::Module::new();let mut types=wasm_encoder::TypeSection::new();
        types.ty().function(if params5{vec![V::I64,V::I64,V::I64,V::I32,V::I32]}else{vec![V::I64,V::I32]},[]);
        types.ty().function([V::I64],[]);types.ty().function([],[]);m.section(&types);
        let mut imports=wasm_encoder::ImportSection::new();imports.import("env","existing_hook",wasm_encoder::EntityType::Function(1));m.section(&imports);
        let mut functions=wasm_encoder::FunctionSection::new();for ty in [0,0,1,0,2]{functions.function(ty);}m.section(&functions);
        let mut tables=wasm_encoder::TableSection::new();tables.table(TableType{element_type:RefType::FUNCREF,table64:false,minimum:2,maximum:Some(2),shared:false});m.section(&tables);
        let mut globals=wasm_encoder::GlobalSection::new();globals.global(wasm_encoder::GlobalType {val_type:V::I64,mutable:true,shared:false},&ConstExpr::i64_const(4096));m.section(&globals);
        let mut exports=wasm_encoder::ExportSection::new();for (name,index) in [("target",1),("nested",2),("helper",3),("dispatch",4)]{exports.export(name,ExportKind::Func,index);}
        exports.export("tls",ExportKind::Global,0);exports.export("table",ExportKind::Table,0);m.section(&exports);
        m.section(&wasm_encoder::StartSection{function_index:5});
        let mut elements=wasm_encoder::ElementSection::new();elements.active(Some(0),&ConstExpr::i32_const(0),Elements::Functions(Cow::Borrowed(&[1,2])));m.section(&elements);
        let mut code=wasm_encoder::CodeSection::new();
        let mut target=Function::new([(2,V::F32),(1,V::I32),(2,V::I64)]);
        target.instruction(&I::LocalGet(0)).instruction(&I::Call(3));
        target.instruction(&I::I64Const(99)).instruction(&I::LocalSet(0));
        target.instruction(&I::I64Const(8192)).instruction(&I::GlobalSet(0));
        for op in ops {target.instruction(op);} target.instruction(&I::End);code.function(&target);
        let mut nested=Function::new([]);nested.instruction(&I::LocalGet(0)).instruction(&I::Call(0));if nested_traps{nested.instruction(&I::Unreachable);}nested.instruction(&I::End);code.function(&nested);
        let mut helper=Function::new([]);helper.instruction(&I::LocalGet(0)).instruction(&I::Call(0)).instruction(&I::RefFunc(1)).instruction(&I::Drop).instruction(&I::End);code.function(&helper);
        let mut dispatch=Function::new([]);for arg in 0..if params5{5}else{2}{dispatch.instruction(&I::LocalGet(arg));}
        dispatch.instruction(&I::I32Const(0)).instruction(&I::CallIndirect{type_index:0,table_index:0}).instruction(&I::End);code.function(&dispatch);
        let mut start=Function::new([]);start.instruction(&I::I64Const(808)).instruction(&I::Call(3)).instruction(&I::End);code.function(&start);m.section(&code);
        let mut names=wasm_encoder::NameSection::new();let mut functions=wasm_encoder::NameMap::new();
        for (index,name) in [(0,"existing_hook"),(1,"target_family"),(2,"nested_family"),(3,"helper"),(4,"dispatch"),(5,"start")]{functions.append(index,name);}names.functions(&functions);
        let mut globals=wasm_encoder::NameMap::new();globals.append(0,"__tls_base");names.globals(&globals);
        let mut locals=wasm_encoder::IndirectNameMap::new();let mut incoming=wasm_encoder::NameMap::new();incoming.append(0,"incoming_arg0");locals.append(1,&incoming);names.locals(&locals);
        let mut labels=wasm_encoder::IndirectNameMap::new();let mut targetlabels=wasm_encoder::NameMap::new();targetlabels.append(0,"original_first_block");labels.append(1,&targetlabels);names.labels(&labels);m.section(&names);m.finish()
    }
    fn targets(params5:bool)->BTreeMap<u32,Target>{BTreeMap::from([
        (1,Target{family:1,name:"target_family".into(),params:if params5{5}else{2}}),
        (2,Target{family:2,name:"nested_family".into(),params:if params5{5}else{2}})])}
    #[derive(Clone,Debug,PartialEq)]
    enum Event {Enter(i32,i64,i64,i64),Exit(i32,i64,i64,i64),Existing(i64)}
    #[derive(Default)]
    struct Events {items:Vec<Event>,next:i64,enter_mutates_tls:bool}
    fn load(bytes:&[u8],mutate:bool)->(wasmtime::Store<Events>,wasmtime::Instance) {
        let engine=wasmtime::Engine::default();let module=wasmtime::Module::new(&engine,bytes).unwrap();
        let mut store=wasmtime::Store::new(&engine,Events{enter_mutates_tls:mutate,..Default::default()});let mut linker=wasmtime::Linker::new(&engine);
        linker.func_wrap("env","existing_hook",|mut c:wasmtime::Caller<'_,Events>,arg:i64|{c.data_mut().items.push(Event::Existing(arg));}).unwrap();
        linker.func_wrap(IMPORT_MODULE,ENTER,|mut c:wasmtime::Caller<'_,Events>,family:i32,arg:i64,tls:i64|->i64{
            let token={let s=c.data_mut();s.next+=1;let token=s.next;s.items.push(Event::Enter(family,arg,tls,token));token};
            if c.data().enter_mutates_tls{c.get_export("tls").unwrap().into_global().unwrap().set(&mut c,wasmtime::Val::I64(1234)).unwrap();}token
        }).unwrap();
        linker.func_wrap(IMPORT_MODULE,EXIT,|mut c:wasmtime::Caller<'_,Events>,family:i32,arg:i64,tls:i64,token:i64|{c.data_mut().items.push(Event::Exit(family,arg,tls,token));}).unwrap();
        let instance=linker.instantiate(&mut store,&module).unwrap();assert_eq!(store.data().items,[Event::Existing(808)]);store.data_mut().items.clear();(store,instance)
    }
    #[test]
    fn normal_exit_forms_emit_one_exit_with_saved_argument_tls_and_token() {
        let forms=vec![vec![],vec![I::Return],vec![I::Br(0)],
            vec![I::Block(BlockType::Empty),I::Return,I::End],vec![I::I32Const(0),I::BrIf(0)],
            vec![I::Block(BlockType::Empty),I::I32Const(1),I::BrIf(1),I::Unreachable,I::End],
            vec![I::Block(BlockType::Empty),I::I32Const(0),I::BrTable(Cow::Owned(vec![1]),1),I::Unreachable,I::End],
            vec![I::Block(BlockType::Empty),I::I32Const(7),I::BrTable(Cow::Owned(vec![0]),1),I::End]];
        for ops in forms {
            let bytes=instrument(&fixture(&ops,false),targets(false),0,None).unwrap();let(mut s,i)=load(&bytes,true);
            i.get_typed_func::<(i64,i32),()>(&mut s,"target").unwrap().call(&mut s,(17,0)).unwrap();
            assert_eq!(s.data().items,[Event::Enter(1,17,4096,1),Event::Existing(17),Event::Exit(1,17,4096,1)]);
            assert_eq!(i.get_global(&mut s,"tls").unwrap().get(&mut s).i64(),Some(8192));
        }
    }
    #[test]
    fn nested_ancestry_is_balanced_and_indirect_dispatch_keeps_table_slot() {
        let bytes=instrument(&fixture(&[I::I64Const(27),I::I32Const(0),I::Call(2)],false),targets(false),0,None).unwrap();
        let(mut s,i)=load(&bytes,false);
        i.get_typed_func::<(i64,i32),()>(&mut s,"dispatch").unwrap().call(&mut s,(17,0)).unwrap();
        assert_eq!(s.data().items,[Event::Enter(1,17,4096,1),Event::Existing(17),Event::Enter(2,27,8192,2),Event::Existing(27),Event::Exit(2,27,8192,2),Event::Exit(1,17,4096,1)]);
    }
    #[test]
    fn five_argument_family_keeps_fresh_local_indices() {
        let bytes=instrument(&fixture(&[I::Return],true),targets(true),0,None).unwrap();let(mut s,i)=load(&bytes,false);
        i.get_typed_func::<(i64,i64,i64,i32,i32),()>(&mut s,"target").unwrap().call(&mut s,(91,92,93,94,95)).unwrap();
        assert_eq!(s.data().items,[Event::Enter(1,91,4096,1),Event::Existing(91),Event::Exit(1,91,4096,1)]);
    }
    #[test]
    fn traps_do_not_fabricate_normal_exits_in_either_nested_scope() {
        let bytes=instrument(&fixture(&[I::Unreachable],false),targets(false),0,None).unwrap();let(mut s,i)=load(&bytes,false);
        assert!(i.get_typed_func::<(i64,i32),()>(&mut s,"target").unwrap().call(&mut s,(17,0)).is_err());
        assert_eq!(s.data().items,[Event::Enter(1,17,4096,1),Event::Existing(17)]);
        let nested=fixture_with_nested(&[I::I64Const(27),I::I32Const(0),I::Call(2)],false,true);
        let bytes=instrument(&nested,targets(false),0,None).unwrap();let(mut s,i)=load(&bytes,false);
        assert!(i.get_typed_func::<(i64,i32),()>(&mut s,"target").unwrap().call(&mut s,(17,0)).is_err());
        assert_eq!(s.data().items,[Event::Enter(1,17,4096,1),Event::Existing(17),Event::Enter(2,27,8192,2),Event::Existing(27)]);
        // No host cleanup is connected to this artifact fixture; these recorded
        // events prove lack of fake leave, not successful runtime scope cleanup.
    }
    #[test]
    fn import_calls_exports_start_elements_ref_func_and_names_remap() {
        let input=fixture(&[I::Block(BlockType::Empty),I::End],false);
        let output=instrument(&input,targets(false),0,None).unwrap();let info=Info::parse(&output).unwrap();
        assert_eq!(info.function_imports,3);assert_eq!(info.names["target_family"],[3]);assert_eq!(info.tls().unwrap(),0);
        assert_eq!(info.ty(1).unwrap().params(),[ValType::I32,ValType::I64,ValType::I64]);assert_eq!(info.ty(1).unwrap().results(),[ValType::I64]);
        assert_eq!(info.ty(2).unwrap().params(),[ValType::I32,ValType::I64,ValType::I64,ValType::I64]);assert!(info.ty(2).unwrap().results().is_empty());
        let mut saw_start=false;let mut saw_elements=false;let mut saw_locals=false;let mut saw_labels=false;let mut saw_ref=false;
        for p in Parser::new(0).parse_all(&output){match p.unwrap(){
            Payload::StartSection{func,..}=>{assert_eq!(func,7);saw_start=true;},
            Payload::ElementSection(elements)=>for element in elements{if let wasmparser::ElementItems::Functions(fs)=element.unwrap().items{assert_eq!(fs.into_iter().map(Result::unwrap).collect::<Vec<_>>(),[3,4]);saw_elements=true;}},
            Payload::CustomSection(c)=>if let KnownCustom::Name(names)=c.as_known(){for n in names{match n.unwrap(){
                Name::Local(map)=>for n in map{let n=n.unwrap();assert_eq!(n.index,3);assert_eq!(n.names.into_iter().next().unwrap().unwrap().index,0);saw_locals=true;},
                Name::Label(map)=>for n in map{let n=n.unwrap();assert_eq!(n.index,3);assert_eq!(n.names.into_iter().next().unwrap().unwrap().index,1);saw_labels=true;},_=>{}}}},_=>{}}}
        let mut ops=info.body(5).unwrap().get_operators_reader().unwrap();while !ops.eof(){if let O::RefFunc{function_index}=ops.read().unwrap(){assert_eq!(function_index,3);saw_ref=true;}}
        assert!(saw_start&&saw_elements&&saw_locals&&saw_labels&&saw_ref);
        let(mut s,i)=load(&output,false);i.get_typed_func::<(i64,i32),()>(&mut s,"dispatch").unwrap().call(&mut s,(11,0)).unwrap();
        assert!(s.data().items.contains(&Event::Existing(11)));
    }
    #[test]
    fn public_factory_rejects_substitute_and_private_wrapper_rejects_unsupported_paths() {
        let input=fixture(&[],false);assert!(generate(&input,&[]).unwrap_err().to_string().contains("SHA256"));
        let mut wrong=targets(false);wrong.get_mut(&1).unwrap().params=5;assert!(instrument(&input,wrong,0,None).is_err());
        assert!(instrument(&input,targets(false),1,None).is_err());
        let mut wrong=targets(false);wrong.get_mut(&1).unwrap().name="wrong".into();assert!(instrument(&input,wrong,0,None).is_err());
        assert!(instrument(&fixture(&[I::ReturnCall(2)],false),targets(false),0,None).unwrap_err().to_string().contains("tail-call"));
        assert!(instrument(&fixture(&[I::Throw(0)],false),targets(false),0,None).unwrap_err().to_string().contains("exception"));
        let output=instrument(&input,targets(false),0,None).unwrap();assert!(instrument(&output,targets(false),0,None).unwrap_err().to_string().contains("already present"));
    }
    #[test]
    #[ignore="requires exact private original fixture via MUGUET_TRACE_ORIGINAL_WASM; reencodes in memory only"]
    fn exact_original_default_patch_identity_and_ancestry_artifact() {
        let original=std::fs::read(std::env::var_os("MUGUET_TRACE_ORIGINAL_WASM").expect("private original path")).unwrap();
        let baseline=crate::patch::patch(&original,&[]).unwrap();
        assert_eq!(sha256(&baseline).unwrap(),"8cfe0a666883d5ce7db2d530a71fceafbf182b1482baebf8912275d40c6bc07e");
        let artifact=generate(&original,&[]).unwrap();
        assert_eq!(artifact.manifest["patch5_sha256"],sha256(&baseline).unwrap());
        assert_eq!(artifact.manifest["patch5_function_import_shift"],8);
        assert_eq!(artifact.manifest["families"].as_array().unwrap().len(),3);
        // Parse without removing or changing artifact bytes: Info rejects its own
        // manifest to prevent instrumenting an already marked artifact twice.
        let mut imports=0u32;let mut names=HashMap::new();let mut marker_calls=HashMap::new();let mut next=0u32;let mut manifest_count=0;
        for p in Parser::new(0).parse_all(&artifact.wasm){match p.unwrap(){
            Payload::ImportSection(s)=>for i in s.into_imports(){if let TypeRef::Func(_)=i.unwrap().ty{imports+=1;}},
            Payload::FunctionSection(_)=>next=imports,
            Payload::CodeSectionEntry(body)=>{let mut calls=vec![];let mut ops=body.get_operators_reader().unwrap();while !ops.eof(){if let O::Call{function_index}=ops.read().unwrap(){if function_index==93||function_index==94{calls.push(function_index);}}}if !calls.is_empty(){marker_calls.insert(next,calls);}next+=1;},
            Payload::CustomSection(c)=>{if c.name()==MANIFEST_SECTION{manifest_count+=1;assert_eq!(serde_json::from_slice::<serde_json::Value>(c.data()).unwrap(),artifact.manifest);}if let KnownCustom::Name(s)=c.as_known(){for n in s{if let Name::Function(map)=n.unwrap(){for n in map{let n=n.unwrap();names.insert(n.name.to_owned(),n.index);}}}}},_=>{}}}
        assert_eq!(imports,95);assert_eq!(manifest_count,1);assert_eq!(marker_calls.len(),3);
        for family in artifact.manifest["families"].as_array().unwrap(){let index=family["artifact_function"].as_u64().unwrap() as u32;assert_eq!(names[family["name"].as_str().unwrap()],index);assert_eq!(marker_calls[&index],[93,94]);}
        assert_eq!(crate::patch::patch(&original,&[]).unwrap(),baseline);
        eprintln!("ancestry artifact: {} bytes SHA256 {}; default patch5: {} bytes SHA256 {}",artifact.wasm.len(),sha256(&artifact.wasm).unwrap(),baseline.len(),sha256(&baseline).unwrap());
        let mut modified=original.clone();modified[8]^=1;assert!(generate(&modified,&[]).is_err());
    }
}

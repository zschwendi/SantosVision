//! Source compatibility audit only: NO runtime foreground scopes are installed.
//! Normal returns can be wrapped, but traps and host-recovered longjmp require
//! guest-call-boundary cleanup first. This certificate grants no render authority.
//! FNV body guards detect version mismatches, not cryptographic authenticity.
use anyhow::{bail, ensure, Context, Result};
use std::collections::HashMap;
use wasmparser::{FuncType, KnownCustom, Name, Operator as O, Parser, Payload, TypeRef, ValType};

pub const ORIGINAL_SHA256: &str = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0";

struct KnownBody {
    function: u32,
    name: &'static str,
    params: &'static [ValType],
    bytes: usize,
    fingerprint: u64,
}
const KNOWN: &[KnownBody] = &[
    KnownBody { function:44395, name:"CEntityDrawDataFm::Draw()", params:&[ValType::I64], bytes:2223, fingerprint:0x4676c5c67237fc20 },
    KnownBody { function:44415, name:"CEntityDrawDataStreamPed::Draw(CAddCompositeSkeletonCommand&, CEntityDrawDataPedPropsBase*, unsigned char, unsigned int)", params:&[ValType::I64,ValType::I64,ValType::I64,ValType::I32,ValType::I32], bytes:4625, fingerprint:0x3d31059c6c2dd8f3 },
    KnownBody { function:44355, name:"CDrawSkinnedEntityDC::ExecuteStatic(rage::dlCmdBase&)", params:&[ValType::I64], bytes:2102, fingerprint:0x3741160b03deea55 },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionAudit {
    pub original_function: u32,
    pub name: &'static str,
    pub body_bytes: usize,
    pub body_fingerprint: u64,
    pub explicit_returns: u32,
    /// Counts each branch/table arm targeting the implicit function label.
    pub function_label_branch_arms: u32,
    pub max_control_depth: u32,
    pub calls: u32,
    pub unreachable_ops: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCertificate {
    pub functions: Vec<FunctionAudit>,
    /// Exact original name/type required; no fallback to global index 1.
    pub tls_global: u32,
    pub original_function_imports: u32,
    /// Always false. Control-flow compatibility is not runtime trap cleanup.
    pub runtime_scopes_installed: bool,
}

fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h,b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}
fn check_shape(known: &KnownBody, ty: &FuncType) -> Result<()> {
    ensure!(ty.params() == known.params && ty.results().is_empty(), "foreground ABI mismatch: {}", known.name);
    Ok(())
}

/// Audit exact original function indexes/names/types/body fingerprints and the
/// current module's TLS global. Does not mutate WASM or configure any host state.
/// ORIGINAL_SHA256 is the independently verified fixture identity, not a claim
/// that this routine cryptographically hashes the entire supplied module.
pub fn certify(wasm: &[u8]) -> Result<SourceCertificate> {
    let mut types=Vec::new(); let mut function_types=Vec::new();
    let mut imports=0u32; let mut next_body=0u32;
    let mut globals=Vec::new(); let mut tls_names=Vec::new();
    let mut names: HashMap<String,Vec<u32>>=HashMap::new();
    let mut bodies=HashMap::new();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(s) => for t in s.into_iter_err_on_gc_types() { types.push(t?); },
            Payload::ImportSection(s) => for i in s.into_imports() { match i?.ty {
                TypeRef::Func(t) => {imports+=1; function_types.push(t);},
                TypeRef::Global(t) => globals.push(t), _=>{}
            }},
            Payload::FunctionSection(s) => {next_body=imports; for t in s {function_types.push(t?);} },
            Payload::GlobalSection(s) => for g in s {globals.push(g?.ty);},
            Payload::CodeSectionEntry(body) => {
                if KNOWN.iter().any(|k| k.function==next_body) {bodies.insert(next_body,body);}
                next_body+=1;
            },
            Payload::CustomSection(c) => if let KnownCustom::Name(s)=c.as_known() { for n in s {match n? {
                Name::Function(map) => for n in map {let n=n?; names.entry(n.name.into()).or_default().push(n.index);},
                Name::Global(map) => for n in map {let n=n?; if n.name=="__tls_base" {tls_names.push(n.index);}},
                _=>{}
            }}},
            _=>{}
        }
    }
    ensure!(tls_names.len()==1,"foreground TLS global name missing or ambiguous");
    let tls_global=tls_names[0]; let tls=globals.get(tls_global as usize).context("foreground TLS global index out of bounds")?;
    ensure!(tls.content_type==ValType::I64 && tls.mutable && !tls.shared,"foreground TLS global must be mutable nonshared i64");
    let mut audits=Vec::new();
    for known in KNOWN {
        ensure!(names.get(known.name).is_some_and(|ids| ids.as_slice()==[known.function]),"foreground function name/index missing or ambiguous: {}",known.name);
        let type_index=*function_types.get(known.function as usize).context("foreground function missing")?;
        check_shape(known,types.get(type_index as usize).context("foreground type missing")?)?;
        let body=bodies.get(&known.function).context("foreground original body missing")?;
        let range=body.range(); let bytes=wasm.get(usize::try_from(range.start)?..usize::try_from(range.end)?).context("foreground body range invalid")?;
        ensure!(bytes.len()==known.bytes && fingerprint(bytes)==known.fingerprint,"foreground body mismatch: {}",known.name);
        audits.push(audit_control(body,known)?);
    }
    Ok(SourceCertificate {functions:audits,tls_global,original_function_imports:imports,runtime_scopes_installed:false})
}

fn audit_control(body: &wasmparser::FunctionBody<'_>, known: &KnownBody) -> Result<FunctionAudit> {
    let mut audit=FunctionAudit {original_function:known.function,name:known.name,body_bytes:known.bytes,body_fingerprint:known.fingerprint,
        explicit_returns:0,function_label_branch_arms:0,max_control_depth:0,calls:0,unreachable_ops:0};
    let mut depth=0u32; let mut ended=false; let mut ops=body.get_operators_reader()?;
    while !ops.eof() {
        match ops.read()? {
            O::Block{..}|O::Loop{..}|O::If{..} => {depth+=1; audit.max_control_depth=audit.max_control_depth.max(depth);},
            O::End if depth==0 => {ensure!(ops.eof(),"operators after foreground function end");ended=true;},
            O::End => depth-=1,
            O::Return => audit.explicit_returns+=1,
            O::Br{relative_depth}|O::BrIf{relative_depth} => {
                ensure!(relative_depth<=depth,"foreground branch outside function");
                audit.function_label_branch_arms+=u32::from(relative_depth==depth);
            },
            O::BrTable{targets} => {
                for target in targets.targets().chain(std::iter::once(Ok(targets.default()))) {
                    let target=target?; ensure!(target<=depth,"foreground table branch outside function");
                    audit.function_label_branch_arms+=u32::from(target==depth);
                }
            },
            O::Call{..}|O::CallIndirect{..}|O::CallRef{..} => audit.calls+=1,
            O::Unreachable => audit.unreachable_ops+=1,
            O::ReturnCall{..}|O::ReturnCallIndirect{..}|O::ReturnCallRef{..} => bail!("foreground tail call is not qualified"),
            O::Try{..}|O::TryTable{..}|O::Catch{..}|O::CatchAll|O::Throw{..}|O::ThrowRef|O::Rethrow{..}|O::Delegate{..} => bail!("foreground exception control flow is not qualified"),
            _=>{}
        }
    }
    ensure!(ended && depth==0,"foreground function end missing");
    Ok(audit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_guards_reject_pointer_width_and_results() {
        for known in KNOWN {
            assert!(check_shape(known,&FuncType::new(known.params.iter().copied(),[])).is_ok());
            let mut params=known.params.to_vec(); params[0]=ValType::I32;
            assert!(check_shape(known,&FuncType::new(params,[])).is_err());
            assert!(check_shape(known,&FuncType::new(known.params.iter().copied(),[ValType::I64])).is_err());
        }
    }
    fn fixture(ops: &[wasm_encoder::Instruction<'_>]) -> Vec<u8> {
        use wasm_encoder::{Module,TypeSection,FunctionSection,CodeSection,Function};
        let mut m=Module::new(); let mut t=TypeSection::new();t.ty().function([],[]);m.section(&t);
        let mut fs=FunctionSection::new();fs.function(0);m.section(&fs);
        let mut f=Function::new([]);for op in ops {f.instruction(op);}let mut code=CodeSection::new();code.function(&f);m.section(&code);m.finish()
    }
    fn audit_fixture(bytes: &[u8]) -> Result<FunctionAudit> {
        for p in Parser::new(0).parse_all(bytes) {if let Payload::CodeSectionEntry(body)=p? {return audit_control(&body,&KNOWN[0]);}}
        bail!("fixture body missing")
    }
    #[test]
    fn every_normal_exit_form_and_trap_site_is_accounted() {
        use wasm_encoder::{Instruction as I,BlockType};
        let module=fixture(&[I::Block(BlockType::Empty),I::I32Const(0),I::BrIf(1),
            I::Block(BlockType::Empty),I::I32Const(0),I::BrTable(vec![0,2].into(),2),I::End,I::End,
            I::Unreachable,I::Return,I::End]);
        let a=audit_fixture(&module).unwrap();assert_eq!(a.explicit_returns,1);
        assert_eq!(a.function_label_branch_arms,3);assert_eq!(a.max_control_depth,2);assert_eq!(a.unreachable_ops,1);
    }
    #[test]
    fn tail_calls_and_exception_paths_are_not_qualified_as_balanced() {
        use wasm_encoder::Instruction as I;
        assert!(audit_fixture(&fixture(&[I::ReturnCall(0),I::End])).unwrap_err().to_string().contains("tail call"));
        assert!(audit_fixture(&fixture(&[I::Throw(0),I::End])).unwrap_err().to_string().contains("exception"));
    }

}

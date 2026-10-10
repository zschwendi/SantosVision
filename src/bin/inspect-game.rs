//! Inventory real WASM names and signatures without executing the game.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use wasmparser::{KnownCustom, Name, Parser, Payload, TypeRef, Validator};

const REQUIRED_HOOKS: &[&str] = &[
    "GtaThread::Update(int)",
    "CExplosionManager::AddExplosion(CExplosionManager::CExplosionArgs&, CProjectile*, bool)",
    "CTimeWarper::SetTargetTimeWarp(TW_Dir)",
];

fn inventory(bytes: &[u8]) -> Result<Value> {
    Validator::new().validate_all(bytes).context("invalid WebAssembly module")?;
    let mut types = Vec::new();
    let mut function_types = Vec::new();
    let mut names = BTreeMap::new();
    let mut exports = BTreeMap::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::TypeSection(section) => {
                for ty in section.into_iter_err_on_gc_types() {
                    types.push(ty.context("game inventory supports function types, not GC types")?);
                }
            }
            Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    if let TypeRef::Func(ty) = import?.ty {
                        function_types.push(ty);
                    }
                }
            }
            Payload::FunctionSection(section) => {
                for ty in section { function_types.push(ty?); }
            }
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export?;
                    if export.kind == wasmparser::ExternalKind::Func {
                        exports.insert(export.name.to_owned(), export.index);
                    }
                }
            }
            Payload::CustomSection(section) => {
                if let KnownCustom::Name(section) = section.as_known() {
                    for name in section {
                        if let Name::Function(section) = name? {
                            for name in section {
                                let name = name?;
                                // Preserve overloads and duplicate labels using function indices.
                                names.insert(name.index, name.name.to_owned());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let describe = |index: u32, name: &str| -> Result<Value> {
        let type_index = *function_types.get(index as usize).context("named function index is out of range")?;
        let ty = types.get(type_index as usize).context("function type index is out of range")?;
        Ok(json!({
            "name": name,
            "function_index": index,
            "type_index": type_index,
            "params": ty.params().iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
            "results": ty.results().iter().map(|t| format!("{t:?}")).collect::<Vec<_>>()
        }))
    };
    let mut hooks = Vec::new();
    for required in REQUIRED_HOOKS {
        let matches = names.iter().filter(|(_, name)| name.as_str() == *required)
            .map(|(&index, name)| describe(index, name)).collect::<Result<Vec<_>>>()?;
        hooks.push(json!({"required_name": required, "matches": matches}));
    }
    let camera_candidates = names.iter().filter(|(_, name)| {
        let name = name.to_ascii_lowercase();
        name.contains("camera") || name.contains("gameplaycam") || name.contains("setcam")
            || name.contains("renderscriptcam") || name.contains("frustum")
            || name.contains("projectionmatrix") || name.contains("viewmatrix")
            || name.contains("grcviewport::")
    }).map(|(&index, name)| describe(index, name)).collect::<Result<Vec<_>>>()?;
    let exported_functions = exports.iter().map(|(name, &index)| describe(index, name))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "module_bytes": bytes.len(),
        "function_count": function_types.len(),
        "named_function_count": names.len(),
        "required_hook_names": hooks,
        "camera_candidates": camera_candidates,
        "exported_functions": exported_functions,
        "scope": "Read-only names and WASM signatures. Candidate names do not prove camera control, memory layouts, stereo rendering or compatibility. No code executed."
    }))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().context("usage: inspect-game <game.wasm>")?;
    anyhow::ensure!(args.next().is_none(), "usage: inspect-game <game.wasm>");
    let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;
    println!("{}", serde_json::to_string_pretty(&inventory(&bytes)?)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn real_function_type_is_resolved_and_missing_hooks_stay_missing() {
        // A minimal add(i32, i32) -> i32 module; exports are independent of names.
        let module = b"\0asm\x01\0\0\0\x01\x07\x01\x60\x02\x7f\x7f\x01\x7f\x03\x02\x01\0\x07\x07\x01\x03add\0\0\x0a\x09\x01\x07\0\x20\0\x20\x01\x6a\x0b";
        let result = super::inventory(module).unwrap();
        assert_eq!(result["exported_functions"][0]["params"], serde_json::json!(["I32", "I32"]));
        assert_eq!(result["exported_functions"][0]["results"], serde_json::json!(["I32"]));
        assert_eq!(result["named_function_count"], 0);
        for hook in result["required_hook_names"].as_array().unwrap() {
            assert!(hook["matches"].as_array().unwrap().is_empty());
        }
    }
}

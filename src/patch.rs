// muguet - patch.rs
// Constantin Clerc (c22dev) - 0.1

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasm_encoder::ValType::I64;
use wasmparser::{FuncType, KnownCustom, Name, Parser, Payload, TypeRef, ValType};

#[derive(Clone, Copy, PartialEq)]
enum Arg {
    Local0,
    Local1,
    Local1Extended,
    Local3,
    TlsBase,

    Local1Replaced,
}

// The boolean marks a post-hook. A post-hook saves the incoming parameters in
// fresh locals: the camera and scene implementations reuse their parameter locals.
const HOOKS: &[(&str, &str, &[Arg], bool)] = &[
    ("muguet_script", "GtaThread::Update(int)", &[Arg::Local0, Arg::TlsBase], false),
    ("muguet_explosion", "CExplosionManager::AddExplosion(CExplosionManager::CExplosionArgs&, CProjectile*, bool)", &[Arg::Local0], false),
    ("muguet_timewarp", "CTimeWarper::SetTargetTimeWarp(TW_Dir)", &[Arg::Local1Replaced], false),
    ("muguet_xr_camera_post", "CViewport::PropagateCameraFrame(camFrame const&)", &[Arg::Local0, Arg::Local1], true),
    ("muguet_xr_scene_post", "CRenderPhaseDrawScene::UpdateViewport()", &[Arg::Local0], true),
    ("muguet_xr_viewport_command", "rage::dlCmdSetCurrentViewport::ExecuteStatic(rage::dlCmdBase&)", &[Arg::Local0], false),
    ("muguet_xr_current_view", "rage::grcViewport::SetCurrent(rage::grcViewport const*, bool)", &[Arg::Local0, Arg::Local1Extended], false),
    ("muguet_xr_gpu_command", "wasm_gpu::Emit(unsigned int, unsigned int const*, unsigned int)", &[Arg::Local3], false),
];

pub const VERSION: u32 = 5;

// These ABI shapes were resolved from the supplied game module's actual type
// section. C++ names alone are insufficient: scrVector return values use hidden
// output pointers, and this port's memory64 pointers are i64.
const VERIFIED_CAMERA_SIGNATURES: &[(&str, &[ValType], &[ValType])] = &[
    ("camera_commands::CommandGetGameplayCamCoord()", &[ValType::I64], &[]),
    ("camera_commands::CommandGetGameplayCamRot(int)", &[ValType::I64, ValType::I32], &[]),
    ("camera_commands::CommandGetGameplayCamRelativeHeading()", &[], &[ValType::F32]),
    ("camera_commands::CommandGetGameplayCamRelativePitch()", &[], &[ValType::F32]),
    ("camera_commands::CommandSetGameplayCamRelativeHeading(float)", &[ValType::F32], &[]),
    ("camera_commands::CommandSetGameplayCamRelativePitch(float, float)", &[ValType::F32, ValType::F32], &[]),
    ("camera_commands::CommandSetFollowPedCamViewMode(int)", &[ValType::I32], &[]),
    ("camera_commands::CommandSetFollowVehicleCamViewMode(int)", &[ValType::I32], &[]),
    ("camera_commands::CommandGetFollowPedCamViewMode()", &[], &[ValType::I32]),
    ("camera_commands::CommandGetFollowVehicleCamViewMode()", &[], &[ValType::I32]),
    ("camera_commands::CommandIsGameplayCamRendering()", &[], &[ValType::I32]),
    ("camera_commands::CommandCreateCam(char const*, bool)", &[ValType::I64, ValType::I32], &[ValType::I32]),
    ("camera_commands::CommandDestroyCam(int, bool)", &[ValType::I32, ValType::I32], &[]),
    ("camera_commands::CommandSetCamActive(int, bool)", &[ValType::I32, ValType::I32], &[]),
    ("camera_commands::CommandSetCamCoord(int, rage::scrVector const&)", &[ValType::I32, ValType::I64], &[]),
    ("camera_commands::CommandSetCamRotation(int, rage::scrVector const&, int)", &[ValType::I32, ValType::I64, ValType::I32], &[]),
    ("camera_commands::CommandRenderScriptCams(bool, bool, int, bool, bool, int)", &[ValType::I32; 6], &[]),
    ("rage::grcViewport::SetCameraMtx(rage::Mat34V const&)", &[ValType::I64, ValType::I64], &[]),
    ("rage::grcViewport::SetProjection(rage::Mat44V const&)", &[ValType::I64, ValType::I64], &[]),
    ("rage::grcViewport::SetCurrent(rage::grcViewport const*, bool)", &[ValType::I64, ValType::I32], &[ValType::I64]),
    ("CViewport::PropagateCameraFrame(camFrame const&)", &[ValType::I64, ValType::I64], &[]),
    ("CRenderPhaseDrawScene::UpdateViewport()", &[ValType::I64], &[]),
    ("rage::dlCmdSetCurrentViewport::ExecuteStatic(rage::dlCmdBase&)", &[ValType::I64], &[]),
    ("wasm_gpu::Emit(unsigned int, unsigned int const*, unsigned int)", &[ValType::I32, ValType::I64, ValType::I32], &[]),
];

// Stable body fingerprints certify the layouts and publication boundary used by
// xr_view. This is a compatibility guard, not a cryptographic authenticity claim.
const WORLD_LAYOUT_BODIES: &[(&str, usize, u64)] = &[
    ("CViewport::PropagateCameraFrame(camFrame const&)", 894, 0xe87a207a7921921f),
    ("CRenderPhaseDrawScene::UpdateViewport()", 220, 0x37f09f67ced1f45d),
    ("rage::dlCmdSetCurrentViewport::ExecuteStatic(rage::dlCmdBase&)", 21, 0xf823d9d806050941),
    ("rage::grcViewport::SetCurrent(rage::grcViewport const*, bool)", 1030, 0x91e54e687bea74fe),
    ("wasm_gpu::Emit(unsigned int, unsigned int const*, unsigned int)", 318, 0xc168cf4febffbe2a),
    ("camManager::UpdateInternal()", 21216, 0xe2de3dda5d3f6822),
    ("camera_commands::CommandGetGameplayCamCoord()", 45, 0x03fd208166e7aa91),
    ("rage::grcViewport::RegenerateNearFarClip()", 2450, 0xaee335be32ccbd3e),
    ("rage::grcViewport::RegeneratePerspective()", 202, 0x5383d3243d308612),
    ("rage::grcViewport::Perspective(float, float, float, float)", 332, 0x7660d7d2c97d060c),
    ("rage::grcViewport::SetProjection(rage::Mat44V const&)", 72, 0x3455ca099bcc38d1),
    ("rage::grcViewport::SetCameraMtx(rage::Mat34V const&)", 579, 0x75195b23a51efd91),
    ("DeferredLighting::GetProjectionShaderParams(rage::Vec4V&, rage::Vec3V&, rage::Vec3V&, rage::Vec3V&)", 375, 0x2165d1e35f3f6511),
];

fn body_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100000001b3))
}

fn check_camera_signature(name: &str, function: u32, function_types: &[u32], types: &[FuncType]) -> Result<()> {
    let Some((_, params, results)) = VERIFIED_CAMERA_SIGNATURES.iter().find(|(known, _, _)| *known == name) else { return Ok(()); };
    let type_index = *function_types.get(function as usize).context("camera function index out of bounds")?;
    let ty = types.get(type_index as usize).context("camera function type index out of bounds")?;
    if ty.params() != *params || ty.results() != *results {
        bail!("camera ABI mismatch for {name}: expected {params:?} -> {results:?}, found {:?} -> {:?}", ty.params(), ty.results());
    }
    Ok(())
}

fn returns(args: &[Arg]) -> bool {
    args.contains(&Arg::Local1Replaced)
}

struct Patcher {
    imported_funcs: u32,
    types: u32,
    tls_global: u32,

    targets: HashMap<u32, usize>,
    exports: Vec<(String, u32)>,
    next_func: u32,
}

impl Patcher {
    fn shift(&self) -> u32 {
        HOOKS.len() as u32
    }
}

impl Reencode for Patcher {
    type Error = anyhow::Error;

    fn function_index(&mut self, f: u32) -> Result<u32, Error<Self::Error>> {
        Ok(if f >= self.imported_funcs { f + self.shift() } else { f })
    }

    fn parse_type_section(&mut self, types: &mut wasm_encoder::TypeSection, section: wasmparser::TypeSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_type_section(self, types, section)?;

        for (_, _, args, _) in HOOKS {
            types.ty().function(vec![I64; args.len()], if returns(args) { vec![I64] } else { vec![] });
        }
        Ok(())
    }

    fn parse_import_section(&mut self, imports: &mut wasm_encoder::ImportSection, section: wasmparser::ImportSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_import_section(self, imports, section)?;
        for (k, (name, _, _, _)) in HOOKS.iter().enumerate() {
            imports.import("env", name, wasm_encoder::EntityType::Function(self.types + k as u32));
        }
        Ok(())
    }

    fn parse_export_section(&mut self, exports: &mut wasm_encoder::ExportSection, section: wasmparser::ExportSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_export_section(self, exports, section)?;
        for (name, f) in std::mem::take(&mut self.exports) {
            let index = self.function_index(f)?;
            exports.export(&name, wasm_encoder::ExportKind::Func, index);
        }
        Ok(())
    }

    fn parse_function_body(&mut self, code: &mut wasm_encoder::CodeSection, func: wasmparser::FunctionBody<'_>) -> Result<(), Error<Self::Error>> {
        let index = self.next_func;
        self.next_func += 1;
        let Some(&hook) = self.targets.get(&index) else {
            return utils::parse_function_body(self, code, func);
        };
        if HOOKS[hook].0 == "muguet_xr_gpu_command" {
            return self.parse_gpu_publish_body(code, func, hook);
        }
        if HOOKS[hook].3 {
            return self.parse_post_hook_body(code, func, hook);
        }
        let mut f = self.new_function_with_parsed_locals(&func)?;
        use wasm_encoder::Instruction as I;
        for arg in HOOKS[hook].2 {
            match arg {
                Arg::Local0 => f.instruction(&I::LocalGet(0)),
                Arg::Local1 => f.instruction(&I::LocalGet(1)),
                Arg::Local1Extended => f.instruction(&I::LocalGet(1)).instruction(&I::I64ExtendI32U),
                Arg::Local3 => f.instruction(&I::LocalGet(3)),
                Arg::TlsBase => f.instruction(&I::GlobalGet(self.tls_global)),
                Arg::Local1Replaced => f.instruction(&I::LocalGet(1)).instruction(&I::I64ExtendI32U),
            };
        }
        f.instruction(&I::Call(self.imported_funcs + hook as u32));
        if returns(HOOKS[hook].2) {
            f.instruction(&I::I32WrapI64);
            f.instruction(&I::LocalSet(1));
        }
        let mut reader = func.get_operators_reader()?;
        while !reader.eof() {
            f.instruction(&self.parse_instruction(&mut reader)?);
        }
        code.function(&f);
        Ok(())
    }
}

impl Patcher {
    fn parse_gpu_publish_body(&mut self, code: &mut wasm_encoder::CodeSection, func: wasmparser::FunctionBody<'_>, hook: usize) -> Result<(), Error<anyhow::Error>> {
        use wasm_encoder::Instruction as I;
        let mut f = self.new_function_with_parsed_locals(&func)?;
        let mut reader = func.get_operators_reader()?;
        let mut injected = false;
        while !reader.eof() {
            let relative_offset = reader.original_position() - func.range().start;
            let op = reader.read()?;
            let boundary = relative_offset == 0x9a && matches!(op, wasmparser::Operator::LocalSet { local_index: 2 });
            // Tiny unit fixtures exercise signatures and post-hook control flow;
            // production modules must contain the certified exact midpoint.
            let fixture_boundary = cfg!(test) && func.range().end - func.range().start < 32 && matches!(op, wasmparser::Operator::End);
            if fixture_boundary {
                f.instruction(&I::LocalGet(3)); f.instruction(&I::Call(self.imported_funcs + hook as u32)); injected = true;
            }
            f.instruction(&self.instruction(op)?);
            if boundary {
                f.instruction(&I::LocalGet(3)); f.instruction(&I::Call(self.imported_funcs + hook as u32)); injected = true;
            }
        }
        if !injected { return Err(Error::UserError(anyhow::anyhow!("unqualified GPU command publication boundary"))); }
        code.function(&f);
        Ok(())
    }

    fn parse_post_hook_body(&mut self, code: &mut wasm_encoder::CodeSection, func: wasmparser::FunctionBody<'_>, hook: usize) -> Result<(), Error<anyhow::Error>> {
        use wasm_encoder::{BlockType, Function, Instruction as I};
        use wasmparser::Operator as O;
        let args = HOOKS[hook].2;
        let params = args.len() as u32;
        let mut locals = Vec::new();
        let mut saved_start = params;
        for local in func.get_locals_reader()? {
            let (count, ty) = local?;
            saved_start += count;
            locals.push((count, self.val_type(ty)?));
        }
        locals.push((params, I64));
        let mut f = Function::new(locals);
        for (slot, arg) in args.iter().enumerate() {
            let incoming = match arg { Arg::Local0 => 0, Arg::Local1 => 1, _ => unreachable!("post-hooks have i64 parameters only") };
            f.instruction(&I::LocalGet(incoming));
            f.instruction(&I::LocalSet(saved_start + slot as u32));
        }
        // Replace the implicit function label with an explicit void block. All
        // original br/br_if/br_table depths remain valid; returns branch to this
        // block so the host snapshot executes once after every normal exit.
        f.instruction(&I::Block(BlockType::Empty));
        let mut depth = 0u32;
        let mut reader = func.get_operators_reader()?;
        while !reader.eof() {
            let op = reader.read()?;
            match op {
                O::End if depth == 0 => {
                    f.instruction(&I::End);
                    if !reader.eof() { return Err(Error::UserError(anyhow::anyhow!("operators after post-hook function end"))); }
                }
                O::End => { depth -= 1; f.instruction(&I::End); }
                O::Return => { f.instruction(&I::Br(depth)); }
                O::Block { .. } | O::Loop { .. } | O::If { .. } | O::TryTable { .. } | O::Try { .. } => {
                    depth += 1;
                    f.instruction(&self.instruction(op)?);
                }
                O::ReturnCall { .. } | O::ReturnCallIndirect { .. } | O::ReturnCallRef { .. } => {
                    return Err(Error::UserError(anyhow::anyhow!("unqualified tail call in {}", HOOKS[hook].1)));
                }
                _ => { f.instruction(&self.instruction(op)?); }
            }
        }
        for slot in 0..params { f.instruction(&I::LocalGet(saved_start + slot)); }
        f.instruction(&I::Call(self.imported_funcs + hook as u32));
        f.instruction(&I::End);
        code.function(&f);
        Ok(())
    }
}

pub fn patch(wasm: &[u8], wanted: &[(&str, &str)]) -> Result<Vec<u8>> {
    let (mut imported_funcs, mut types, mut tls_global) = (0u32, 0u32, None);
    let mut by_name: HashMap<String, u32> = HashMap::new();
    let mut signatures = Vec::new();
    let mut function_types = Vec::new();
    let mut duplicate_names = std::collections::HashSet::new();
    let mut body_fingerprints = HashMap::new();
    let mut next_body = None;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(s) => {
                types = s.count();
                for ty in s.into_iter_err_on_gc_types() { signatures.push(ty?); }
            }
            Payload::ImportSection(s) => {
                for imp in s.into_imports() {
                    if let TypeRef::Func(ty) = imp?.ty {
                        imported_funcs += 1;
                        function_types.push(ty);
                    }
                }
            }
            Payload::FunctionSection(s) => {
                next_body = Some(imported_funcs);
                for ty in s { function_types.push(ty?); }
            }
            Payload::CodeSectionEntry(body) => {
                let index = next_body.context("body before function section")?;
                let range = body.range();
                body_fingerprints.insert(index, ((range.end - range.start) as usize, body_fingerprint(&wasm[range.start as usize..range.end as usize])));
                next_body = Some(index + 1);
            }
            Payload::CustomSection(c) => {
                if let KnownCustom::Name(names) = c.as_known() {
                    for sub in names {
                        match sub? {
                            Name::Function(map) => {
                                for n in map {
                                    let n = n?;
                                    if by_name.insert(n.name.to_string(), n.index).is_some() {
                                        duplicate_names.insert(n.name.to_owned());
                                    }
                                }
                            }
                            Name::Global(map) => {
                                for n in map {
                                    let n = n?;
                                    if n.name == "__tls_base" { tls_global = Some(n.index); }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if by_name.contains_key(HOOKS[0].0) {
        bail!("game.wasm is already patched");
    }
    if !(cfg!(test) && wasm.len() < 4096) {
        for &(name, bytes, hash) in WORLD_LAYOUT_BODIES {
            let index = by_name.get(name).with_context(|| format!("world layout function {name} not found"))?;
            anyhow::ensure!(body_fingerprints.get(index) == Some(&(bytes, hash)), "unqualified world memory layout: {name}");
        }
    }
    let mut targets = HashMap::new();
    for (k, (_, func, _, _)) in HOOKS.iter().enumerate() {
        anyhow::ensure!(!duplicate_names.contains(*func), "ambiguous hook name {func}");
        let f = *by_name.get(*func).with_context(|| format!("{func} not found (no name section?)"))?;
        check_camera_signature(func, f, &function_types, &signatures)?;
        targets.insert(f, k);
    }
    let mut exports = Vec::new();
    for (export, func) in wanted {
        match by_name.get(*func) {
            Some(&i) => {
                anyhow::ensure!(!duplicate_names.contains(*func), "ambiguous command name {func}");
                check_camera_signature(func, i, &function_types, &signatures)?;
                exports.push((format!("mp:{export}"), i));
            }
            None if VERIFIED_CAMERA_SIGNATURES.iter().any(|(name, _, _)| *name == *func) => {
                bail!("required camera command {func} not found (name section must be preserved)");
            }
            None => eprintln!("[patch] {func} not found; multiplayer will skip it"),
        }
    }
    let mut p = Patcher {
        imported_funcs,
        types,
        tls_global: tls_global.unwrap_or(1),
        targets,
        exports,
        next_func: imported_funcs,
    };
    let mut module = wasm_encoder::Module::new();
    p.parse_core_module(&mut module, Parser::new(0), wasm).map_err(|e| anyhow::anyhow!("patch game.wasm: {e:?}"))?;
    Ok(module.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_module(camera_params: &[wasm_encoder::ValType]) -> Vec<u8> {
        use wasm_encoder::{ConstExpr, ExportKind, Function, GlobalType, Instruction as I, ValType as V};
        let mut module = wasm_encoder::Module::new();
        let mut types = wasm_encoder::TypeSection::new();
        types.ty().function([V::I64, V::I32], [V::I32]);
        types.ty().function([V::I64, V::I64, V::I32], [V::I32]);
        types.ty().function([V::I64, V::I32], []);
        types.ty().function(camera_params.iter().copied(), []);
        types.ty().function([V::I64, V::I64], []);
        types.ty().function([V::I64], []);
        types.ty().function([V::I64, V::I32], [V::I64]);
        types.ty().function([V::I32, V::I64, V::I32], []);
        module.section(&types);
        module.section(&wasm_encoder::ImportSection::new());
        let mut functions = wasm_encoder::FunctionSection::new();
        for ty in [0, 1, 2, 4, 5, 5, 6, 7, 3] { functions.function(ty); }
        module.section(&functions);
        let mut globals = wasm_encoder::GlobalSection::new();
        for _ in 0..2 {
            globals.global(GlobalType { val_type: V::I64, mutable: true, shared: false }, &ConstExpr::i64_const(0));
        }
        module.section(&globals);
        let mut exports = wasm_encoder::ExportSection::new();
        exports.export("entry", ExportKind::Func, 0);
        exports.export("camera", ExportKind::Func, 3);
        exports.export("scene", ExportKind::Func, 4);
        module.section(&exports);
        let mut code = wasm_encoder::CodeSection::new();
        for (index, local) in [Some(1), Some(2), None, None, None, None, Some(0), None, None].into_iter().enumerate() {
            let mut function = Function::new(if index == 7 { vec![(1, V::I64)] } else { vec![] });
            if let Some(local) = local { function.instruction(&I::LocalGet(local)); }
            if index == 3 {
                function.instruction(&I::I64Const(111)).instruction(&I::LocalSet(0));
                function.instruction(&I::I64Const(222)).instruction(&I::LocalSet(1));
                function.instruction(&I::Return);
            }
            if index == 4 {
                function.instruction(&I::Block(wasm_encoder::BlockType::Empty));
                function.instruction(&I::I64Const(333)).instruction(&I::LocalSet(0));
                function.instruction(&I::Br(1));
                function.instruction(&I::End);
            }
            function.instruction(&I::End);
            code.function(&function);
        }
        module.section(&code);
        let mut names = wasm_encoder::NameSection::new();
        let mut funcs = wasm_encoder::NameMap::new();
        for (index, (_, name, _, _)) in HOOKS.iter().enumerate() { funcs.append(index as u32, name); }
        funcs.append(8, "camera_commands::CommandGetGameplayCamCoord()");
        names.functions(&funcs);
        let mut globals = wasm_encoder::NameMap::new();
        globals.append(1, "__tls_base");
        names.globals(&globals);
        module.section(&names);
        module.finish()
    }

    #[test]
    fn camera_exports_retain_memory64_hidden_output_and_shift_indices() {
        let module = camera_module(&[wasm_encoder::ValType::I64]);
        let patched = patch(&module, &[("GetGameplayCamCoord", "camera_commands::CommandGetGameplayCamCoord()")]).unwrap();
        wasmparser::Validator::new().validate_all(&patched).unwrap();
        let mut index = None;
        for payload in Parser::new(0).parse_all(&patched) {
            if let Payload::ExportSection(exports) = payload.unwrap() {
                for export in exports {
                    let export = export.unwrap();
                    if export.name == "mp:GetGameplayCamCoord" { index = Some(export.index); }
                }
            }
        }
        assert_eq!(index, Some(16));
    }

    #[test]
    fn camera_named_like_expected_but_with_wrong_pointer_type_is_rejected() {
        let module = camera_module(&[wasm_encoder::ValType::I32]);
        let error = patch(&module, &[("GetGameplayCamCoord", "camera_commands::CommandGetGameplayCamCoord()")]).unwrap_err();
        assert!(error.to_string().contains("camera ABI mismatch"));
        assert!(error.to_string().contains("I64"));
    }

    #[test]
    fn post_hooks_keep_incoming_arguments_and_run_after_return_or_function_branch() {
        let patched = patch(&camera_module(&[wasm_encoder::ValType::I64]), &[]).unwrap();
        wasmparser::Validator::new().validate_all(&patched).unwrap();
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &patched).unwrap();
        let mut store = wasmtime::Store::new(&engine, Vec::<Vec<i64>>::new());
        let mut linker = wasmtime::Linker::new(&engine);
        linker.func_wrap("env", "muguet_script", |_: i64, _: i64| {}).unwrap();
        linker.func_wrap("env", "muguet_explosion", |_: i64| {}).unwrap();
        linker.func_wrap("env", "muguet_timewarp", |value: i64| value).unwrap();
        linker.func_wrap("env", "muguet_xr_camera_post", |mut caller: wasmtime::Caller<'_, Vec<Vec<i64>>>, viewport: i64, frame: i64| {
            caller.data_mut().push(vec![viewport, frame]);
        }).unwrap();
        linker.func_wrap("env", "muguet_xr_scene_post", |mut caller: wasmtime::Caller<'_, Vec<Vec<i64>>>, phase: i64| {
            caller.data_mut().push(vec![phase]);
        }).unwrap();
        linker.func_wrap("env", "muguet_xr_viewport_command", |_: i64| {}).unwrap();
        linker.func_wrap("env", "muguet_xr_current_view", |_: i64, _: i64| {}).unwrap();
        linker.func_wrap("env", "muguet_xr_gpu_command", |_: i64| {}).unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        instance.get_typed_func::<(i64, i64), ()>(&mut store, "camera").unwrap().call(&mut store, (17, 29)).unwrap();
        instance.get_typed_func::<i64, ()>(&mut store, "scene").unwrap().call(&mut store, 41).unwrap();
        assert_eq!(store.data(), &vec![vec![17, 29], vec![41]]);
    }
}

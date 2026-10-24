// muguet - main.rs
// Constantin Clerc (c22dev) - 0.1

#[cfg(not(target_os = "visionos"))]
mod app;
mod audio;
mod bc;
mod buffer_upload;
mod draw_setup;
pub mod config;
mod fs;
mod float_sampling;
mod gpu;
mod heap_autopsy;
mod host;
mod guest_call_scope;
mod input;
pub mod import;
mod io;
mod mp;
pub mod native;
pub mod capture;
pub mod world_frame;
pub mod world_ui;
pub mod world_command_trace;
pub mod xr;
pub mod xr_aim;
pub mod xr_hud;
pub mod xr_motion_aim;
pub mod xr_motion_arms;
#[cfg(feature = "jit")]
pub mod xr_motion_arms_patch;
#[cfg(feature="jit")]
pub mod xr_motion_cache_patch;
#[cfg(feature="jit")]
pub mod xr_viewmodel_patch;
#[cfg(feature="jit")]
pub mod xr_left_wrist_patch;
#[cfg(feature = "jit")]
pub mod xr_motion_patch;
pub mod xr_foreground;
#[cfg(feature = "jit")]
pub mod xr_foreground_scope;
#[cfg(feature = "jit")]
pub mod xr_foreground_patch;
#[cfg(feature = "jit")]
pub mod xr_emit_completion_patch;
pub mod xr_view;
mod pad;
#[cfg(feature = "jit")]
mod patch;
mod proto;
mod server;
#[cfg(target_os = "ios")]
mod touch;
mod upload_dedup;
#[cfg(all(test, target_os = "macos"))]
mod metal_prepass_tests;

#[cfg(all(target_os = "visionos", feature = "jit"))]
compile_error!("visionOS must use --no-default-features and an embedded signed AOT module");

use anyhow::{Context, Result};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use wasmtime::*;

pub struct ImportSpec {
    pub module: String,
    pub name: String,
    pub ty: FuncType,
}

pub struct Runtime {
    pub engine: Engine,
    pub module: Module,
    pub mem: SharedMemory,
    pub imports: Vec<ImportSpec>,
    pub counts: Vec<AtomicU64>,
    pub start: Instant,
}

pub struct HostState {
    pub pthread: u64,
    guest_calls: guest_call_scope::StoreLifetime,
    motion_aim: xr_motion_aim::StoreState,
    motion_arms: xr_motion_arms::StoreState,
}
impl HostState {
    pub fn new(pthread: u64) -> Self {
        Self { pthread, guest_calls: guest_call_scope::StoreLifetime::new(), motion_aim: xr_motion_aim::StoreState::default(),motion_arms:xr_motion_arms::StoreState::default() }
    }
}

pub static RT: OnceLock<Arc<Runtime>> = OnceLock::new();

pub fn footprint_mb() -> u64 {
    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
        fn getpid() -> i32;
    }

    let mut info = [0u64; 16];
    if unsafe { proc_pid_rusage(getpid(), 0, info.as_mut_ptr()) } != 0 {
        return 0;
    }
    info[2 + 7] >> 20
}

pub fn set_interactive_qos() {
    set_qos(QOS_CLASS_USER_INTERACTIVE);
}
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
const QOS_CLASS_USER_INITIATED: u32 = 0x19;
fn set_qos(class: u32) {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos: u32, relpri: i32) -> i32;
    }
    unsafe { pthread_set_qos_class_self_np(class, 0) };
}

fn engine_thread_qos(name: &str) -> u32 {
    if name.contains("RenderThread") || name.contains("mixer") || name.contains("Audio") && name.contains("Mix") {
        QOS_CLASS_USER_INTERACTIVE
    } else {
        QOS_CLASS_USER_INITIATED
    }
}

fn raise_open_file_limit() {
    #[repr(C)]
    struct Rlimit { cur: u64, max: u64 }
    extern "C" {
        fn getrlimit(resource: i32, rlp: *mut Rlimit) -> i32;
        fn setrlimit(resource: i32, rlp: *const Rlimit) -> i32;
    }
    const RLIMIT_NOFILE: i32 = 8;
    let mut r = Rlimit { cur: 0, max: 0 };
    unsafe {
        if getrlimit(RLIMIT_NOFILE, &mut r) == 0 {
            let want = r.max.min(10240);
            if want > r.cur {
                r.cur = want;
                setrlimit(RLIMIT_NOFILE, &r);
            }
        }
    }
}

#[cfg(not(target_os = "visionos"))]
pub fn desktop_main() -> Result<()> {
    raise_open_file_limit();
    let argv: Vec<String> = std::env::args().collect();

    if argv.get(1).map(String::as_str) == Some("--import") {
        let zip = argv.get(2).context("--import <game.zip>")?;
        let dest = config::game_dir();
        return import::import(std::path::Path::new(zip), &dest, !code_embedded(), |done, total| println!("progress {done} {total}"));
    }

    #[cfg(feature = "jit")]
    if argv.get(1).map(String::as_str) == Some("--precompile") {
        let (wasm, out) = (argv.get(2).context("--precompile <wasm> <out>")?, argv.get(3).context("--precompile <wasm> <out>")?);
        let engine = Engine::new(&engine_config_for(false)).map_err(anyhow::Error::from)?;
        let m = compile_patched(&engine, std::path::Path::new(wasm))?;
        std::fs::write(out, m.serialize()?)?;
        eprintln!("[host] precompiled {wasm} -> {out}");
        return Ok(());
    }
    #[cfg(feature = "jit")]
    if matches!(argv.get(1).map(String::as_str),Some("--precompile-visionos-motion"|"--precompile-visionos-motion-arms"|"--precompile-visionos-motion-arms-cache"|"--precompile-visionos-motion-arms-cache-viewmodel"|"--precompile-visionos-motion-arms-cache-viewmodel-left-wrist")) {
        let wasm=argv.get(2).context("--precompile-visionos-motion <original.wasm> <separate.cwasm>")?;
        let out=argv.get(3).context("--precompile-visionos-motion <original.wasm> <separate.cwasm>")?;
        anyhow::ensure!(!std::path::Path::new(out).exists(),"motion diagnostic output must be a new file");
        let original=std::fs::read(wasm)?;
        let (artifact_wasm,mut manifest)=if argv[1]=="--precompile-visionos-motion-arms-cache-viewmodel-left-wrist"{
            let a=xr_left_wrist_patch::generate(&original,mp::COMMANDS)?;(a.wasm,a.manifest)
        }else if argv[1]=="--precompile-visionos-motion-arms-cache-viewmodel"{
            let a=xr_viewmodel_patch::generate(&original,mp::COMMANDS)?;(a.wasm,a.manifest)
        }else if argv[1]=="--precompile-visionos-motion-arms-cache"{
            let a=xr_motion_cache_patch::generate(&original,mp::COMMANDS)?;(a.wasm,a.manifest)
        }else if argv[1]=="--precompile-visionos-motion-arms"{
            let a=xr_motion_arms_patch::generate(&original,mp::COMMANDS)?;(a.wasm,a.manifest)
        }else{let a=xr_motion_patch::generate(&original,mp::COMMANDS)?;(a.wasm,a.manifest)};
        let mut config=engine_config_for(true);config.target("aarch64-apple-visionos").map_err(anyhow::Error::from)?;
        unsafe{config.cranelift_flag_enable("has_lse")};
        let engine=Engine::new(&config).map_err(anyhow::Error::from)?;
        let code=engine.precompile_module(&artifact_wasm).map_err(anyhow::Error::from)?;
        manifest["aot_sha256"]=xr_motion_patch::sha256(&code)?.into();
        manifest["patched_wasm_sha256"]=xr_motion_patch::sha256(&artifact_wasm)?.into();
        manifest["host_build_requirement"]="MUGUET_MOTION_AOT_SHA256 must equal aot_sha256".into();
        use std::io::Write;
        std::fs::OpenOptions::new().write(true).create_new(true).open(out)?.write_all(&code)?;
        std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{out}.motion.json"))?
            .write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        eprintln!("[host] separate motion diagnostic AOT written to {out}; normal game AOT unchanged");
        return Ok(());
    }
    #[cfg(feature = "jit")]
    if matches!(argv.get(1).map(String::as_str), Some("--precompile-ios" | "--precompile-visionos" | "--precompile-visionos-sim")) {
        let option = argv[1].as_str();
        let usage = format!("{option} <wasm> <out>");
        let (wasm, out) = (argv.get(2).context(usage.clone())?, argv.get(3).context(usage)?);
        let target = match option {
            "--precompile-visionos" => "aarch64-apple-visionos",
            "--precompile-visionos-sim" => "aarch64-apple-visionos-sim",
            _ => "aarch64-apple-ios",
        };
        let mut config = engine_config_for(true);
        config.target(target).map_err(anyhow::Error::from)?;
        unsafe { config.cranelift_flag_enable("has_lse") };
        let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
        let patched = patch::patch(&std::fs::read(wasm)?, mp::COMMANDS)?;
        std::fs::write(out, engine.precompile_module(&patched).map_err(anyhow::Error::from)?)?;
        eprintln!("[host] precompiled {wasm} for {target} -> {out}");
        return Ok(());
    }
    let startup = match config::setup() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[config] {e:#}");
            alert("Muguet", &format!("{e:#}"));
            std::process::exit(1);
        }
    };
    #[cfg(not(any(target_os = "ios", target_os = "visionos")))]
    let rt = Some(make_runtime(&startup)?);
    #[cfg(target_os = "ios")]
    let rt = None;

    let mut args: Vec<String> = [
        "./this.program", "-rootdir=/game/", "-nodisplaycalibration", "-noSocialClub",
        "-pc:nosocialclub", "-nonetwork", "-output",
        "-forceResolution", "-Script_all=warning", "-replay_all=warning",
        "-fastscriptcore",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(startup.engine_args.clone());

    args.extend(std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")));

    app::run(rt, startup, args)
}

pub fn make_runtime(startup: &config::Startup) -> Result<Arc<Runtime>> {
    let engine = Engine::new(&engine_config()).map_err(anyhow::Error::from).context("engine")?;
    let t0 = Instant::now();
    let module = load_module(&engine, &startup)?;
    eprintln!("[host] module ready in {:?}", t0.elapsed());

    let mut mem = None;
    let mut imports = Vec::new();
    for imp in module.imports() {
        match imp.ty() {
            ExternType::Memory(mt) => mem = Some(SharedMemory::new(&engine, heap_type(mt)?)?),
            ExternType::Func(ty) => imports.push(ImportSpec {
                module: imp.module().to_string(),
                name: imp.name().to_string(),
                ty,
            }),
            other => anyhow::bail!("unsupported import {}.{}: {other:?}", imp.module(), imp.name()),
        }
    }
    let counts = imports.iter().map(|_| AtomicU64::new(0)).collect();
    let rt = Arc::new(Runtime {
        engine,
        module,
        mem: mem.context("module must import env.memory")?,
        imports,
        counts,
        start: Instant::now(),
    });
    RT.set(rt.clone()).ok();
    Ok(rt)
}

pub fn run_engine(rt: &Arc<Runtime>, args: Vec<String>) -> Result<()> {
    set_interactive_qos();
    let (mut store, instance) = instantiate(rt, 0)?;
    call_export(&mut store, &instance, "_emscripten_tls_init", &[])?;
    call_export(&mut store, &instance, "__wasm_call_ctors", &[])?;
    let argv = build_argv(&mut store, &instance, &rt.mem, &args)?;
    eprintln!("[host] main: {}", args.join(" "));
    let main = instance.get_func(&mut store, "__main_argc_argv").context("no main")?;
    let mut out = [Val::I32(0)];
    match guest_call_scope::call(&mut store, guest_call_scope::Kind::Ordinary, |store| main.call(store, &[Val::I32(args.len() as i32), Val::I64(argv as i64)], &mut out)) {
        Ok(()) => eprintln!("[host] main returned {:?}", out[0]),
        Err(_) if host::take_unwind() => {
            eprintln!("[host] main unwound with a live runtime; waiting on engine threads");
            loop { std::thread::park(); }
        }
        Err(e) => {
            heap_autopsy::report_once(&rt.mem, "game main trapped");
            return Err(anyhow::Error::from(e).context("game main trapped"));
        }
    }
    Ok(())
}

#[cfg(all(not(feature = "jit"), not(any(target_os = "ios", target_os = "visionos")), not(game_code)))]
compile_error!("the macOS build needs the default `jit` feature, or MUGUET_GAME_CODE with the precompiled game");

#[cfg(all(not(feature = "jit"), not(any(target_os = "ios", target_os = "visionos")), game_code))]
pub(crate) fn load_module(engine: &Engine, _startup: &config::Startup) -> Result<Module> {
    static GAME_CODE: &[u8] = include_bytes!(env!("MUGUET_GAME_CODE"));
    unsafe { Module::deserialize(engine, GAME_CODE) }.map_err(anyhow::Error::from).context("embedded game code")
}

#[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), game_code))]
std::arch::global_asm!(
    ".section __GAME,__code,regular,pure_instructions",
    ".p2align 14",
    ".globl _muguet_game_code",
    "_muguet_game_code:",
    concat!(".incbin \"", env!("MUGUET_GAME_CODE"), "\""),
    ".globl _muguet_game_code_end",
    "_muguet_game_code_end:",
    ".text",
);

#[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), game_code))]
pub(crate) fn load_module(engine: &Engine, _startup: &config::Startup) -> Result<Module> {
    const _: &[u8] = include_bytes!(env!("MUGUET_GAME_CODE"));
    extern "C" {
        #[link_name = "\x01_muguet_game_code"]
        static START: u8;
        #[link_name = "\x01_muguet_game_code_end"]
        static END: u8;
    }
    let (start, end) = (&raw const START, &raw const END);
    let code = std::ptr::slice_from_raw_parts(start, end as usize - start as usize);
    crate::xr_motion_aim::qualify_embedded_aot(unsafe { &*code })?;
    unsafe { Module::deserialize_raw(engine, std::ptr::NonNull::new(code as *mut [u8]).unwrap()) }
        .map_err(anyhow::Error::from)
        .context("embedded game code")
}

#[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), not(game_code)))]
pub(crate) fn load_module(_engine: &Engine, _startup: &config::Startup) -> Result<Module> {
    anyhow::bail!("this native build has no embedded AOT game code; set MUGUET_GAME_CODE when building")
}

pub fn has_game_code() -> bool {
    cfg!(any(feature = "jit", game_code))
}

/// The game's code is part of this executable (no game.wasm needed).
pub fn code_embedded() -> bool {
    cfg!(all(not(feature = "jit"), game_code))
}

#[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), game_code))]
struct SignedCode;
#[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), game_code))]
impl CustomCodeMemory for SignedCode {
    fn required_alignment(&self) -> usize { 1 }
    fn publish_executable(&self, _ptr: *const u8, _len: usize) -> wasmtime::Result<()> { Ok(()) }
    fn unpublish_executable(&self, _ptr: *const u8, _len: usize) -> wasmtime::Result<()> { Ok(()) }
}

#[cfg(feature = "jit")]
pub(crate) fn load_module(engine: &Engine, startup: &config::Startup) -> Result<Module> {
    let wasm_path = startup.wasm_path.clone();
    let meta = std::fs::metadata(&wasm_path).with_context(|| format!("{}", wasm_path.display()))?;
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);

    let mp_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (patch::VERSION, mp::COMMANDS).hash(&mut h);
        h.finish() as u32
    };
    let cache = startup.cache_dir.join(format!("game-{}-{stamp}-{mp_key:08x}.cwasm", meta.len()));

    if let Some(m) = cache.exists().then(|| unsafe { Module::deserialize_file(engine, &cache) }.ok()).flatten() {
        return Ok(m);
    }
    eprintln!("[host] compiling {} to native code (one-time)…", wasm_path.display());
    let m = compile_patched(engine, &wasm_path)?;
    std::fs::write(&cache, m.serialize()?)?;
    Ok(m)
}

#[cfg(feature = "jit")]
fn compile_patched(engine: &Engine, wasm_path: &std::path::Path) -> Result<Module> {
    let wasm = std::fs::read(wasm_path).with_context(|| format!("{}", wasm_path.display()))?;
    let t = Instant::now();
    let patched = patch::patch(&wasm, mp::COMMANDS)?;
    eprintln!("[host] multiplayer hooks added in {:?}", t.elapsed());
    Module::new(engine, &patched).map_err(anyhow::Error::from).context("compile module")
}

const IOS_HEAP_MAX: u64 = ios_heap_gb() << 30;
const fn ios_heap_gb() -> u64 {
    match option_env!("MUGUET_IOS_HEAP_GB") {
        Some(v) => {
            let b = v.as_bytes();
            let mut n = 0u64;
            let mut k = 0;
            while k < b.len() {
                n = n * 10 + (b[k] - b'0') as u64;
                k += 1;
            }
            n
        }
        None => 5,
    }
}

fn heap_type(mt: MemoryType) -> Result<MemoryType> {
    if !ios_memory() {
        return Ok(mt);
    }
    let page = 1u64 << mt.page_size_log2();
    let cap = IOS_HEAP_MAX / page;
    let max = mt.maximum().map(|m| m.min(cap)).unwrap_or(cap).max(mt.minimum());
    MemoryTypeBuilder::new()
        .min(mt.minimum())
        .max(Some(max))
        .memory64(mt.is_64())
        .shared(mt.is_shared())
        .page_size_log2(mt.page_size_log2())
        .build()
        .map_err(anyhow::Error::from)
}

pub(crate) fn engine_config() -> Config {
    engine_config_for(ios_memory())
}

fn ios_memory() -> bool {
    cfg!(any(target_os = "ios", target_os = "visionos"))
}

fn engine_config_for(ios: bool) -> Config {
    let mut config = Config::new();
    config.wasm_memory64(true);
    config.wasm_threads(true);
    config.shared_memory(true);
    config.wasm_bulk_memory(true);
    config.wasm_multi_memory(true);
    #[cfg(feature = "jit")]
    config.cranelift_opt_level(OptLevel::Speed);

    if ios {
        config.memory_reservation(IOS_HEAP_MAX);
        config.memory_guard_size(64 << 10);
    } else {
        config.memory_reservation(17 << 30);
        config.memory_guard_size(2 << 30);
    }
    config.memory_may_move(false);
    #[cfg(all(not(feature = "jit"), any(target_os = "ios", target_os = "visionos"), game_code))]
    config.with_custom_code_memory(Some(Arc::new(SignedCode)));

    #[cfg(feature = "jit")]
    unsafe {
        config.cranelift_flag_set("enable_heap_access_spectre_mitigation", "false");
        config.cranelift_flag_set("enable_table_access_spectre_mitigation", "false");
    }
    config
}

#[cfg(all(test, feature = "jit"))]
mod platform_tests {
    #[test]
    fn visionos_aot_accepts_target_and_compiles_wasm() {
        // Check the actual cross compiler before asking for a multi-gigabyte game package.
        // This does not exercise the GTA engine or signed execution on a headset.
        let mut config = super::engine_config_for(true);
        config.target("aarch64-apple-visionos").expect("visionOS target");
        unsafe { config.cranelift_flag_enable("has_lse") };
        let engine = wasmtime::Engine::new(&config).expect("visionOS AOT compiler");
        // add(i32, i32) -> i32 forces actual machine-code generation.
        let module = b"\0asm\x01\0\0\0\x01\x07\x01\x60\x02\x7f\x7f\x01\x7f\x03\x02\x01\0\x07\x07\x01\x03add\0\0\x0a\x09\x01\x07\0\x20\0\x20\x01\x6a\x0b";
        let compiled = engine.precompile_module(module).expect("compile WASM function for visionOS");
        assert!(!compiled.is_empty());
    }
}

pub fn instantiate(rt: &Arc<Runtime>, pthread: u64) -> Result<(Store<HostState>, Instance)> {
    let mut store = Store::new(&rt.engine, HostState::new(pthread));
    let mut linker: Linker<HostState> = Linker::new(&rt.engine);
    linker.define(&store, "env", "memory", rt.mem.clone())?;
    for (idx, imp) in rt.imports.iter().enumerate() {
        linker.func_new(&imp.module, &imp.name, imp.ty.clone(), move |caller, params, results| {
            host::call(caller, idx, params, results).map_err(wasmtime::Error::from_anyhow)
        })?;
    }
    let instance = guest_call_scope::call(&mut store, guest_call_scope::Kind::Instantiate, |store| linker.instantiate(store, &rt.module))?;
    Ok((store, instance))
}

pub fn spawn_pthread(pthread_ptr: u64, start_routine: u64, arg: u64) -> i32 {
    let rt = RT.get().expect("runtime").clone();

    let name = if arg != 0 { read_cstr(&rt.mem, arg + 24) } else { String::new() };
    let name = if name.is_empty() || name.len() > 40 { format!("pthread-{pthread_ptr:x}") } else { name };
    let qos = engine_thread_qos(&name);
    let spawned = std::thread::Builder::new()
        .name(name)
        .stack_size(16 << 20)
        .spawn(move || {
            set_qos(qos);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_pthread(&rt, pthread_ptr, start_routine, arg)));
            let error = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("pthread 0x{pthread_ptr:x} failed: {error:#}")),
                Err(_) => Some(format!("pthread 0x{pthread_ptr:x} panicked")),
            };
            if let Some(error) = error {
                eprintln!("[host] {error}");
                #[cfg(target_os = "visionos")]
                crate::native::report_error(error);
            }
        });
    if spawned.is_ok() { 0 } else { 6 }
}

fn run_pthread(rt: &Arc<Runtime>, ptr: u64, start_routine: u64, arg: u64) -> Result<()> {
    let (mut store, inst) = instantiate(rt, ptr)?;

    let high = read_u64(&rt.mem, ptr + 80).unwrap_or(0);
    let size = read_u64(&rt.mem, ptr + 88).unwrap_or(0);
    call_export(&mut store, &inst, "emscripten_stack_set_limits", &[Val::I64(high as i64), Val::I64((high - size) as i64)])?;
    call_export(&mut store, &inst, "_emscripten_stack_restore", &[Val::I64(high as i64)])?;

    call_export(&mut store, &inst, "_emscripten_thread_init", &[Val::I64(ptr as i64), Val::I32(0), Val::I32(0), Val::I32(1), Val::I64(0), Val::I32(0)])?;
    call_export(&mut store, &inst, "_emscripten_tls_init", &[])?;
    host::mailbox_await(&rt.mem, ptr);

    let table = inst
        .get_table(&mut store, "__indirect_function_table")
        .context("no function table")?;
    let entry = table
        .get(&mut store, start_routine)
        .and_then(|r| r.as_func().flatten().cloned())
        .with_context(|| format!("null start routine {start_routine}"))?;
    let mut res = [Val::I64(0)];
    let result = match guest_call_scope::call(&mut store, guest_call_scope::Kind::Ordinary, |store| entry.call(store, &[Val::I64(arg as i64)], &mut res)) {
        Ok(()) => match res[0] { Val::I64(v) => v, Val::I32(v) => v as i64, _ => 0 },
        Err(_) if host::take_unwind() => return Ok(()),
        Err(e) => {
            let name = read_thread_name(&rt.mem, ptr);
            eprintln!("[host] thread {name} trapped: {e:?}");
            heap_autopsy::report_once(&rt.mem, &format!("engine thread {name} trapped"));
            #[cfg(target_os = "visionos")]
            return Err(anyhow::Error::from(e).context(format!("engine thread {name} trapped")));
            #[cfg(not(target_os = "visionos"))]
            return Ok(());
        }
    };
    match call_export(&mut store, &inst, "_emscripten_thread_exit", &[Val::I64(result)]) {
        Err(e) if !host::take_unwind() => Err(e.into()),
        _ => Ok(()),
    }
}

fn read_thread_name(_mem: &SharedMemory, ptr: u64) -> String {
    format!("0x{ptr:x}")
}

pub fn call_export(store: &mut Store<HostState>, inst: &Instance, name: &str, args: &[Val]) -> Result<Vec<Val>> {
    let f = inst.get_func(&mut *store, name).with_context(|| format!("no export {name}"))?;
    let ty = f.ty(&mut *store);
    let args = coerce_args(args, &ty);
    let mut results: Vec<Val> = ty.results().map(|t| default_val(&t)).collect();
    guest_call_scope::call(store, guest_call_scope::Kind::Ordinary, |store| f.call(store, &args, &mut results))?;
    Ok(results)
}

pub fn coerce_args(args: &[Val], ty: &FuncType) -> Vec<Val> {
    ty.params()
        .zip(args)
        .map(|(pt, v)| {
            let i = match v { Val::I32(x) => *x as i64, Val::I64(x) => *x, other => return other.clone() };
            match pt { ValType::I32 => Val::I32(i as i32), ValType::I64 => Val::I64(i), _ => v.clone() }
        })
        .collect()
}

fn build_argv(store: &mut Store<HostState>, inst: &Instance, mem: &SharedMemory, args: &[String]) -> Result<u64> {
    let alloc = inst.get_typed_func::<i64, i64>(&mut *store, "_emscripten_stack_alloc")?;
    let mut ptrs = Vec::new();
    for a in args {
        let bytes = [a.as_bytes(), &[0]].concat();
        let p = guest_call_scope::call(store, guest_call_scope::Kind::Ordinary, |store| alloc.call(store, bytes.len() as i64))? as u64;
        write_bytes(mem, p, &bytes);
        ptrs.push(p);
    }
    let argv = guest_call_scope::call(store, guest_call_scope::Kind::Ordinary, |store| alloc.call(store, ((ptrs.len() + 1) * 8) as i64))? as u64;
    for (i, p) in ptrs.iter().chain(std::iter::once(&0)).enumerate() {
        write_u64(mem, argv + i as u64 * 8, *p);
    }
    Ok(argv)
}


pub fn default_val(t: &ValType) -> Val {
    match t {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        ValType::V128 => Val::V128(0u128.into()),
        ValType::Ref(_) => Val::null_any_ref(),
    }
}

pub fn mem_base_len(mem: &SharedMemory) -> (*mut u8, usize) {
    let d = mem.data();
    (d.as_ptr() as *mut u8, d.len())
}
pub fn read_u64(mem: &SharedMemory, addr: u64) -> Option<u64> {
    let (base, len) = mem_base_len(mem);
    let a = addr as usize;
    if a + 8 > len { return None; }
    unsafe { Some(std::ptr::read_unaligned(base.add(a) as *const u64)) }
}
pub fn read_u32(mem: &SharedMemory, addr: u64) -> Option<u32> {
    let (base, len) = mem_base_len(mem);
    let a = addr as usize;
    if a + 4 > len { return None; }
    unsafe { Some(std::ptr::read_unaligned(base.add(a) as *const u32)) }
}
pub fn write_u64(mem: &SharedMemory, addr: u64, v: u64) -> bool {
    write_bytes(mem, addr, &v.to_le_bytes())
}
pub fn write_u32(mem: &SharedMemory, addr: u64, v: u32) -> bool {
    write_bytes(mem, addr, &v.to_le_bytes())
}
pub fn read_bytes(mem: &SharedMemory, addr: u64, n: u64) -> Vec<u8> {
    let (base, len) = mem_base_len(mem);
    let (a, n) = (addr as usize, n as usize);
    if a + n > len { return Vec::new(); }
    unsafe { std::slice::from_raw_parts(base.add(a), n).to_vec() }
}
pub fn write_bytes(mem: &SharedMemory, addr: u64, data: &[u8]) -> bool {
    let (base, len) = mem_base_len(mem);
    let a = addr as usize;
    if a + data.len() > len { return false; }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(a), data.len()); }
    true
}
pub fn read_cstr(mem: &SharedMemory, addr: u64) -> String {
    let (base, len) = mem_base_len(mem);
    let mut a = addr as usize;
    let mut out = Vec::new();
    while a < len && out.len() < 65536 {
        let b = unsafe { *base.add(a) };
        if b == 0 { break; }
        out.push(b);
        a += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn alert(title: &str, msg: &str) {
    let script = format!("display alert \"{}\" message \"{}\" as critical",
        title.replace('"', "'"), msg.replace('"', "'"));
    let _ = std::process::Command::new("osascript").args(["-e", &script]).status();
}

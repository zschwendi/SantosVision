//! C interface for a native CompositorServices shell. Buffers contain UTF-8 and a
//! trailing NUL. Success is 0; -1 is a runtime/input error; -2 is a short buffer.
use anyhow::{Context, Result};
use serde_json::json;
use std::ffi::{c_char, CStr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Status {
    state: &'static str,
    last_error: Option<String>,
    imports: usize,
    exports: usize,
    graphics: Option<serde_json::Value>,
}
static ACTIVE: AtomicBool = AtomicBool::new(true);
static APP_FROZEN_DEPTH_REQUESTED: AtomicBool = AtomicBool::new(false);
static INPUT_LIFECYCLE: Mutex<()> = Mutex::new(());

static STATUS: LazyLock<Mutex<Status>> = LazyLock::new(|| Mutex::new(Status { state: "idle", ..Default::default() }));

fn write_text(out: *mut c_char, capacity: usize, text: &str) -> i32 {
    if out.is_null() || capacity == 0 { return -2; }
    let bytes = text.as_bytes();
    // Never return a success response after silently truncating JSON or errors.
    if bytes.len() >= capacity {
        unsafe { *out = 0; }
        return -2;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast(), bytes.len());
        *out.add(bytes.len()) = 0;
    }
    0
}

pub(crate) fn report_error(error: String) {
    eprintln!("[native] {error}");
    if let Ok(mut status) = STATUS.lock() {
        status.state = "failed";
        status.last_error = Some(error);
    }
}

pub(crate) fn is_active() -> bool { ACTIVE.load(Ordering::Acquire) }

fn resolve_frozen_depth_request(environment: Option<&str>, app_requested: bool) -> bool {
    match environment {
        Some("1") => true,
        Some("0") => false,
        _ => app_requested,
    }
}

pub(crate) fn frozen_depth_transfer_requested() -> bool {
    let environment = std::env::var("MUGUET_TRANSFER_FROZEN_DEPTH").ok();
    resolve_frozen_depth_request(environment.as_deref(), APP_FROZEN_DEPTH_REQUESTED.load(Ordering::Acquire))
}

fn can_configure_depth_before_start(state: &str, runtime_initialized: bool) -> bool {
    !runtime_initialized && matches!(state, "idle" | "failed")
}

#[no_mangle]
pub extern "C" fn sv_world_set_frozen_depth_transfer(enabled: u32) -> i32 {
    if enabled > 1 { return -1; }
    let Ok(status) = STATUS.lock() else { return -1; };
    if !can_configure_depth_before_start(status.state, crate::RT.get().is_some()) {
        return -1;
    }
    APP_FROZEN_DEPTH_REQUESTED.store(enabled == 1, Ordering::Release);
    0
}

pub(crate) fn refresh_input_state() {
    let _guard = INPUT_LIFECYCLE.lock().unwrap_or_else(|poison| poison.into_inner());
    let active = is_active();
    crate::input::set_focus(active);
    crate::input::set_locked(active);
}

#[no_mangle]
pub extern "C" fn sv_runtime_set_active(active: u32) -> i32 {
    if active > 1 { return -1; }
    let _guard = INPUT_LIFECYCLE.lock().unwrap_or_else(|poison| poison.into_inner());
    let active = active == 1;
    ACTIVE.store(active, Ordering::Release);
    crate::gpu::SUSPENDED.store(!active, Ordering::Release);
    if !active { crate::pad::suspend(); }
    crate::input::set_focus(active);
    crate::input::set_locked(active);
    crate::xr::set_active(active);
    crate::xr_view::set_active(active);
    if !active { crate::world_frame::reset(); }
    0
}

pub(crate) fn record_exit(code: i32) {
    if let Ok(mut status) = STATUS.lock() {
        status.state = "exited";
        status.last_error = Some(format!("game requested exit({code})"));
    }
}

#[no_mangle]
pub extern "C" fn sv_runtime_abi_version() -> u32 { 1 }

#[no_mangle]
pub extern "C" fn sv_runtime_probe(out: *mut c_char, capacity: usize) -> i32 {
    let result = std::panic::catch_unwind(|| -> Result<_> {
        let engine = wasmtime::Engine::new(&crate::engine_config()).map_err(anyhow::Error::from).context("create native engine")?;
        let startup = crate::config::Startup {
            wasm_path: Default::default(), cache_dir: Default::default(), engine_args: Vec::new(),
            native_graphics: None,
        };
        let module = crate::load_module(&engine, &startup)?;
        let imports = module.imports().count();
        let exports = module.exports().count();
        let memories: Vec<_> = module.imports().filter_map(|import| match import.ty() {
            wasmtime::ExternType::Memory(memory) => Some(json!({
                "module": import.module(), "name": import.name(), "minimum_pages": memory.minimum(),
                "maximum_pages": memory.maximum(), "memory64": memory.is_64(), "shared": memory.is_shared()
            })), _ => None,
        }).collect();
        Ok(json!({"abi_version": 1, "loaded": true, "code_embedded": crate::code_embedded(),
            "import_count": imports, "export_count": exports, "memories": memories,
            "executed": false, "platform": std::env::consts::OS}))
    });
    match result {
        Ok(Ok(value)) => write_text(out, capacity, &value.to_string()),
        failed => {
            let error = match failed { Ok(Err(error)) => format!("{error:#}"), _ => "native module probe panicked".into() };
            let code = write_text(out, capacity, &json!({"abi_version": 1, "loaded": false, "executed": false, "error": error}).to_string());
            if code == 0 { -1 } else { code }
        }
    }
}

unsafe fn path_arg<'a>(value: *const c_char, label: &str) -> Result<&'a Path> {
    anyhow::ensure!(!value.is_null(), "{label} is null");
    let text = CStr::from_ptr(value).to_str().with_context(|| format!("{label} must be UTF-8"))?;
    anyhow::ensure!(!text.is_empty(), "{label} is empty");
    Ok(Path::new(text))
}

fn start(game_root: &Path, save_root: &Path) -> Result<()> {
    {
        let mut status = STATUS.lock().map_err(|_| anyhow::anyhow!("native status lock poisoned"))?;
        anyhow::ensure!(status.state == "idle" || (status.state == "failed" && crate::RT.get().is_none()), "runtime already initialized ({})", status.state);
        status.state = "initializing";
        status.last_error = None;
        status.graphics = None;
    }
    crate::raise_open_file_limit();
    let startup = crate::config::setup_native(game_root, save_root)?;
    let graphics = startup.native_graphics.as_ref().context("native startup has no graphics configuration")?;
    let (width, height) = (graphics.config.width, graphics.config.height);
    {
        let mut status = STATUS.lock().map_err(|_| anyhow::anyhow!("native status lock poisoned"))?;
        status.graphics = Some(graphics.status_json());
    }
    let rt = crate::make_runtime(&startup)?;
    crate::input::set_size(width as i32, height as i32);
    refresh_input_state();
    crate::gpu::SUSPENDED.store(!is_active(), Ordering::Release);
    {
        let mut status = STATUS.lock().map_err(|_| anyhow::anyhow!("native status lock poisoned"))?;
        status.imports = rt.imports.len();
        status.exports = rt.module.exports().count();
        status.state = "starting";
    }
    let mut args: Vec<String> = ["./this.program", "-rootdir=/game/", "-nodisplaycalibration", "-noSocialClub",
        "-pc:nosocialclub", "-nonetwork", "-output", "-forceResolution", "-Script_all=warning", "-replay_all=warning",
        "-fastscriptcore", "-wgpu"].iter().map(|arg| (*arg).into()).collect();
    args.push(format!("-width={width}"));
    args.push(format!("-height={height}"));
    args.extend(startup.engine_args);
    std::thread::Builder::new().name("engine-main".into()).stack_size(64 << 20).spawn(move || {
        if let Ok(mut status) = STATUS.lock() { status.state = "running"; }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::run_engine(&rt, args))) {
            Ok(Ok(())) => { if let Ok(mut status) = STATUS.lock() { if status.state == "running" { status.state = "returned"; } } }
            Ok(Err(error)) => report_error(format!("{error:#}")),
            Err(_) => report_error("native game engine panicked".into()),
        }
    }).context("spawn native game engine")?;
    std::thread::Builder::new().name("native-gamepad".into()).spawn(|| loop {
        {
            let _guard = INPUT_LIFECYCLE.lock().unwrap_or_else(|poison| poison.into_inner());
            if is_active() {
                // GameController returns autoreleased Objective-C temporaries on this
                // long-lived Rust thread; drain them at every controller sample.
                objc2::rc::autoreleasepool(|_| crate::pad::poll());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }).context("spawn native gamepad poller")?;
    Ok(())
}

#[no_mangle]
pub extern "C" fn sv_runtime_start(game_root: *const c_char, save_root: *const c_char, error: *mut c_char, capacity: usize) -> i32 {
    if error.is_null() || capacity == 0 { return -2; }
    let result = std::panic::catch_unwind(|| unsafe {
        start(path_arg(game_root, "game_root")?, path_arg(save_root, "save_root")?)
    });
    match result {
        Ok(Ok(())) => write_text(error, capacity, ""),
        failed => {
            let message = match failed { Ok(Err(error)) => format!("{error:#}"), _ => "native initialization panicked".into() };
            // A duplicate start is a caller error and must not corrupt a live runtime's state.
            if !message.starts_with("runtime already initialized") { report_error(message.clone()); }
            let code = write_text(error, capacity, &message);
            if code == 0 { -1 } else { code }
        }
    }
}

#[no_mangle]
pub extern "C" fn sv_runtime_status(out: *mut c_char, capacity: usize) -> i32 {
    let Ok(status) = STATUS.lock() else { return -1; };
    write_text(out, capacity, &json!({"abi_version": 1, "state": status.state, "active": is_active(),
        "import_count": status.imports, "export_count": status.exports, "last_error": status.last_error,
        "graphics": status.graphics,
        "heap_bytes": crate::RT.get().map(|runtime| runtime.mem.data_size()).unwrap_or(0),
        "gpu_started": crate::gpu::STARTED.load(Ordering::Relaxed),
        "frozen_depth_transfer_requested": frozen_depth_transfer_requested(),
        "presents": crate::gpu::PRESENTS.load(Ordering::Relaxed),
        "geometry_capture_diagnostics": crate::gpu::geometry_capture_diagnostics_json(),
        "immersive_game_renderer_connected": crate::world_frame::renderer_connected(),
        "immersive_stereo_connected": crate::world_frame::stereo_connected(), "xr": crate::xr::status_json(), "world_view": crate::xr_view::status_json(),
        "world_frames": crate::world_frame::status_json(), "capture": crate::capture::status_json()}).to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn depth_request_configuration_requires_uninitialized_runtime_and_prestart_state() {
        use super::can_configure_depth_before_start as allowed;
        assert!(allowed("idle", false));
        assert!(allowed("failed", false));
        for state in ["idle", "failed", "initializing", "starting", "running", "returned", "exited", "panicked"] {
            assert!(!allowed(state, true), "initialized runtime accepted state {state}");
        }
        for state in ["initializing", "starting", "running", "returned", "exited", "panicked", "unknown"] {
            assert!(!allowed(state, false), "prestart state {state} accepted");
        }
    }

    #[test]
    fn frozen_depth_environment_override_has_precedence_over_app_request() {
        use super::resolve_frozen_depth_request as resolve;
        assert!(!resolve(None, false));
        assert!(resolve(None, true));
        assert!(resolve(Some("1"), false));
        assert!(!resolve(Some("0"), true));
        assert!(resolve(Some("unexpected"), true));
    }

    #[test]
    fn undersized_json_buffer_is_reported_without_partial_json() {
        let mut out = [1i8; 3];
        assert_eq!(super::write_text(out.as_mut_ptr(), out.len(), "{\"ok\":true}"), -2);
        assert_eq!(out[0], 0);
    }
}

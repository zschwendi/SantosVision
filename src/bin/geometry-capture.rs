//! Opt-in Mac host driver for one real game draw capture. No XR pair is published.
//! Run only under the exclusive GPU slot with MUGUET_GEOMETRY_STEREO_CAPTURE=1.
use muguet::{native, xr, xr_view};
use std::ffi::CString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CAPTURE: &str = "target/verification/reprojection-implementation-v1/frozen-game-draw.json";
const PROJECTION_NEAR: f32 = 0.05;
const PROJECTION_FAR: f32 = 1000.0;

fn eye(x: f32) -> xr_view::SVXREyeView {
    let a = PROJECTION_NEAR / (PROJECTION_FAR - PROJECTION_NEAR);
    let b = PROJECTION_NEAR * PROJECTION_FAR / (PROJECTION_FAR - PROJECTION_NEAR);
    let mut head = identity();
    head[12] = x;
    xr_view::SVXREyeView {
        head_from_eye: head,
        projection_from_eye: [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., a, -1., 0., 0., b, 0.],
    }
}

fn identity() -> [f32; 16] {
    [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.]
}

fn cache_seed_name(path: &Path) -> Result<String, String> {
    if !path.is_absolute() { return Err("--cache-seed requires an absolute file path".into()); }
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("cache seed metadata: {e}"))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err("cache seed must be a regular file, not a symlink or directory".into());
    }
    if meta.len() == 0 || meta.len() > 512 * 1024 * 1024 {
        return Err("cache seed must contain 1..512 MiB".into());
    }
    let name = path.file_name().and_then(|name| name.to_str())
        .ok_or("cache seed filename is not UTF-8")?;
    let stem = name.strip_suffix(".cwasm").ok_or("cache seed filename must end in .cwasm")?;
    let parts: Vec<_> = stem.split('-').collect();
    if parts.len() != 4 || parts[0] != "game"
        || parts[1].is_empty() || !parts[1].bytes().all(|b| b.is_ascii_digit())
        || parts[2].is_empty() || !parts[2].bytes().all(|b| b.is_ascii_digit())
        || parts[3].len() != 8 || !parts[3].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("cache seed filename must be game-<length>-<stamp>-<8hex>.cwasm".into());
    }
    Ok(name.into())
}

fn copy_cache_seed(source: &Path, save_root: &Path, name: &str) -> Result<(), String> {
    let cache_dir = save_root.join("cache");
    std::fs::create_dir(&cache_dir).map_err(|e| format!("create fresh cache directory: {e}"))?;
    let mut input = std::fs::File::open(source).map_err(|e| format!("open cache seed: {e}"))?;
    let size = input.metadata().map_err(|e| format!("cache seed opened metadata: {e}"))?;
    if !size.is_file() || size.len() == 0 || size.len() > 512 * 1024 * 1024 {
        return Err("opened cache seed is not a bounded regular file".into());
    }
    let destination = cache_dir.join(name);
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&destination)
        .map_err(|e| format!("create exclusive cache seed target {}: {e}", destination.display()))?;
    let copied = std::io::copy(&mut input.take(512 * 1024 * 1024 + 1), &mut output)
        .map_err(|e| format!("copy cache seed: {e}"))?;
    if copied != size.len() { return Err("cache seed changed size during copy".into()); }
    output.sync_all().map_err(|e| format!("sync cache seed copy: {e}"))?;
    eprintln!("[geometry-capture] copied root-verified local Mac JIT cache seed {} ({} bytes); runtime will validate compatibility", name, copied);
    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf, Duration, Option<PathBuf>), String> {
    let mut args = std::env::args().skip(1);
    let (Some(game_root), Some(save_root)) = (args.next(), args.next()) else {
        return Err("usage: geometry-capture <absolute-game-root> <fresh-save-root> [deadline-seconds: 1..600] [--cache-seed <absolute-file>]".into());
    };
    let mut deadline = None;
    let mut cache_seed = None;
    while let Some(arg) = args.next() {
        if arg == "--cache-seed" {
            if cache_seed.is_some() { return Err("--cache-seed specified twice".into()); }
            cache_seed = Some(PathBuf::from(args.next().ok_or("--cache-seed needs a file")?));
        } else if deadline.is_none() {
            deadline = Some(arg.parse::<u64>().map_err(|_| "deadline must be an integer")?);
        } else {
            return Err("unexpected argument or duplicate deadline".into());
        }
    }
    let deadline = deadline.unwrap_or(120);
    if !(1..=600).contains(&deadline) { return Err("deadline must be 1..600 seconds".into()); }
    Ok((PathBuf::from(game_root), PathBuf::from(save_root), Duration::from_secs(deadline), cache_seed))
}

fn captured_status(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() { return Ok(None); }
    let bytes = std::fs::read(path).map_err(|e| format!("capture read: {e}"))?;
    // create_new makes the path visible before write_all completes. A short JSON
    // is still in flight; wait for complete valid JSON within the deadline.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Ok(None) };
    let status = value["status"].as_str().ok_or("capture JSON has no status")?;
    if status == "captured" || status == "rejected" { return Ok(Some(status.into())); }
    Err(format!("unexpected capture status {status}"))
}

fn native_status() -> Option<serde_json::Value> {
    let mut bytes = [0i8; 131072];
    (native::sv_runtime_status(bytes.as_mut_ptr(), bytes.len()) == 0)
        .then(|| unsafe { std::ffi::CStr::from_ptr(bytes.as_ptr()) })
        .and_then(|text| serde_json::from_slice(text.to_bytes()).ok())
}

fn write_final_native_status(save_root: &Path, outcome: &str) -> Result<(), String> {
    let status = native_status().ok_or("final native status unavailable")?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema":1,"outcome":outcome,"native_status":status,
        "input_kind":"synthetic static 64 mm two-view request; actual game draw required"
    })).map_err(|e| format!("serialize final native status: {e}"))?;
    let path = save_root.join("final-native-status.json");
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e| format!("create exclusive final native status {}: {e}", path.display()))?;
    file.write_all(&body).map_err(|e| format!("write final native status: {e}"))?;
    Ok(())
}

fn run() -> Result<(), String> {
    let (game_root, save_root, deadline, cache_seed) = parse_args()?;
    if std::env::var("MUGUET_GEOMETRY_STEREO_CAPTURE").as_deref() != Ok("1") {
        return Err("set MUGUET_GEOMETRY_STEREO_CAPTURE=1 explicitly".into());
    }
    if !Path::new("Cargo.toml").is_file() || !Path::new("src/gpu.rs").is_file() {
        return Err("run from the SantosVision repository root".into());
    }
    let capture = Path::new(CAPTURE);
    if capture.exists() { return Err(format!("capture already exists; preserve it: {}", capture.display())); }
    if !game_root.is_absolute() || !game_root.join("data/manifest.json").is_file() {
        return Err("game root must be absolute and contain data/manifest.json".into());
    }
    let has_build = std::fs::read_dir(game_root.join("b"))
        .map_err(|e| format!("game build directory: {e}"))?
        .filter_map(Result::ok)
        .any(|entry| entry.path().join("game.wasm").is_file()
            && entry.path().join("shaders/index.json").is_file());
    if !has_build { return Err("game root lacks b/<build>/game.wasm and shaders/index.json".into()); }
    let seed_name = cache_seed.as_deref().map(cache_seed_name).transpose()?;
    if save_root.exists() { return Err("save root must be fresh; existing saves are never reused".into()); }
    let parent = save_root.parent().ok_or("save root has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create run parent: {e}"))?;
    std::fs::create_dir(&save_root).map_err(|e| format!("create fresh save root: {e}"))?;
    if let (Some(source), Some(name)) = (cache_seed.as_deref(), seed_name.as_deref()) {
        // Only the caller can establish provenance against the exact game WASM,
        // toolchain and patch version. This copies one explicitly supplied file.
        copy_cache_seed(source, &save_root, name)?;
    }
    std::fs::write(save_root.join("native-graphics.json"),
        b"{\"width\":640,\"height\":360,\"fps_limit\":30,\"graphics_preset\":\"low\"}\n")
        .map_err(|e| format!("seed low graphics: {e}"))?;
    // This driver never starts a host audio stream or raises the temporary mute.
    std::env::set_var("MUGUET_MUTE", "1");
    std::env::set_var("MUGUET_MASTER_VOLUME", "0");

    let game_arg = CString::new(game_root.to_string_lossy().as_bytes()).map_err(|_| "game root contains NUL")?;
    let save_arg = CString::new(save_root.to_string_lossy().as_bytes()).map_err(|_| "save root contains NUL")?;
    let (startup_tx, startup_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    std::thread::spawn(move || {
        let mut error = [0i8; 8192];
        let code = native::sv_runtime_start(game_arg.as_ptr(), save_arg.as_ptr(), error.as_mut_ptr(), error.len());
        let result = if code == 0 { Ok(()) } else {
            let message = unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned();
            Err(format!("native startup {code}: {message}"))
        };
        let _ = startup_tx.send(result);
    });

    let enable = xr_view::sv_xr_world_camera_set_enabled(1);
    if enable != 0 { return Err(format!("enable qualified world camera: {enable}")); }
    // Fixed, explicitly synthetic tracked head. Request frame changes as the game
    // runs; matching presentation time stays fixed so either eye's consumed
    // request can be matched to the exact pose it used. No coherent pair claim.
    let pose = xr::SVXRHeadPose { version: 1, size: xr::POSE_SIZE, frame: 1,
        presentation_time_seconds: 1.0, stage_from_head: identity(), tracked: 1, reserved: 0 };
    let pose_code = unsafe { xr::sv_xr_submit_pose(&pose) };
    if pose_code != 0 { return Err(format!("synthetic pose rejected: {pose_code}")); }
    let started = Instant::now();
    let mut frame = 1u64;
    let mut next_request = Instant::now();
    let mut next_report = Instant::now() + Duration::from_secs(5);
    let mut startup_done = false;
    eprintln!("[geometry-capture] actual game startup, synthetic static 64 mm two-view input; deadline {}s", deadline.as_secs());
    while started.elapsed() < deadline {
        if !startup_done {
            match startup_rx.try_recv() {
                Ok(Ok(())) => { startup_done = true; eprintln!("[geometry-capture] native startup accepted"); }
                Ok(Err(error)) => return Err(error),
                Err(mpsc::TryRecvError::Disconnected) => return Err("native startup worker disconnected".into()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if Instant::now() >= next_request {
            let request = xr_view::SVXRViewRequest { version: 1, size: 288, frame,
                presentation_time_seconds: 1.0, view_count: 2, reserved: 0,
                views: [eye(-0.032), eye(0.032)] };
            let code = unsafe { xr_view::sv_xr_submit_views(&request) };
            if code != 0 { return Err(format!("synthetic stereo request {frame} rejected: {code}")); }
            frame += 1;
            next_request = Instant::now() + Duration::from_millis(350);
        }
        if let Some(status) = captured_status(capture)? {
            if status == "captured" {
                write_final_native_status(&save_root, "captured")?;
                eprintln!("[geometry-capture] real game draw captured at {}; input was synthetic, result is one draw only", capture.display());
                return Ok(());
            }
            let body = std::fs::read_to_string(capture).map_err(|e| format!("read rejected capture: {e}"))?;
            let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse rejected capture: {e}"))?;
            write_final_native_status(&save_root, "rejected")?;
            return Err(format!("capture rejected: {}", value["reason"].as_str().unwrap_or("reason unavailable")));
        }
        if Instant::now() >= next_report {
            let status = xr_view::status_json();
            let runtime = native_status().unwrap_or(serde_json::Value::Null);
            let gates = &runtime["geometry_capture_diagnostics"];
            eprintln!("[geometry-capture] startup={} native_state={} gpu_started={} presents={} elapsed={}s requests={} main_camera={} overrides={} last_error={}",
                startup_done, runtime["state"], runtime["gpu_started"], runtime["presents"],
                started.elapsed().as_secs(), frame - 1,
                status["main_camera_calls"], status["overrides_applied"], status["last_error"]);
            if gates["enabled"] == true {
                eprintln!("[geometry-capture] gates considered={} vs={} pair={} view={} target={} depth={} surface={} draw={} eligible={} world_depth_draws={} other_pairs={}",
                    gates["considered_unclamped_draws"], gates["vertex_match"], gates["pair_match"],
                    gates["view_match"], gates["target_match"], gates["depth_write_match"],
                    gates["surface_match"], gates["draw_match"], gates["eligible_attempts"],
                    gates["qualified_world_depth_draws"], gates["other_qualified_pair_draws"]);
            }
            next_report = Instant::now() + Duration::from_secs(5);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    write_final_native_status(&save_root, "deadline_expired")?;
    Err(format!("deadline expired after {}s; no complete real game draw capture", deadline.as_secs()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[geometry-capture] {error}");
        std::process::exit(1);
    }
}

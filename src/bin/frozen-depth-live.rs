//! Bounded Mac offscreen actual-game probe for the optional pre-UI depth handoff.
//! A two-view request is input only: the game still publishes alternating eyes.
use muguet::{capture, native, world_frame, xr, xr_view};
use std::ffi::CString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn identity() -> [f32; 16] {
    [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.]
}

fn eye(x: f32) -> xr_view::SVXREyeView {
    let near = 0.05;
    let far = 1000.;
    let mut head = identity();
    head[12] = x;
    xr_view::SVXREyeView { head_from_eye: head,
        projection_from_eye: [1., 0., 0., 0., 0., 16. / 9., 0., 0., 0., 0.,
            near / (far - near), -1., 0., 0., near * far / (far - near), 0.] }
}

fn native_status() -> Result<serde_json::Value, String> {
    let mut bytes = [0i8; 131072];
    if native::sv_runtime_status(bytes.as_mut_ptr(), bytes.len()) != 0 {
        return Err("native status unavailable".into());
    }
    let text = unsafe { std::ffi::CStr::from_ptr(bytes.as_ptr()) };
    serde_json::from_slice(text.to_bytes()).map_err(|error| format!("native status JSON: {error}"))
}

fn seed_name(source: &Path) -> Result<String, String> {
    if !source.is_absolute() { return Err("cache seed must be absolute".into()); }
    let metadata = std::fs::symlink_metadata(source).map_err(|error| format!("cache seed: {error}"))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 512 * 1024 * 1024 {
        return Err("cache seed must be a regular 1..512 MiB file".into());
    }
    let name = source.file_name().and_then(|name| name.to_str()).ok_or("invalid seed filename")?;
    let stem = name.strip_suffix(".cwasm").ok_or("cache seed must end in .cwasm")?;
    let parts: Vec<_> = stem.split('-').collect();
    if parts.len() != 4 || parts[0] != "game"
        || !parts[1..3].iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        || parts[3].len() != 8 || !parts[3].bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("cache seed filename must be game-<size>-<mtime>-<key>.cwasm".into());
    }
    Ok(name.into())
}

fn copy_seed(source: &Path, save: &Path) -> Result<(), String> {
    let name = seed_name(source)?;
    let cache = save.join("cache");
    std::fs::create_dir(&cache).map_err(|error| format!("fresh cache directory: {error}"))?;
    let mut input = std::fs::File::open(source).map_err(|error| format!("open seed: {error}"))?;
    let size = input.metadata().map_err(|error| format!("opened seed: {error}"))?.len();
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(cache.join(&name))
        .map_err(|error| format!("exclusive seed copy: {error}"))?;
    let copied = std::io::copy(&mut input.take(512 * 1024 * 1024 + 1), &mut output)
        .map_err(|error| format!("copy seed: {error}"))?;
    if copied != size { return Err("seed changed size during copy".into()); }
    output.sync_all().map_err(|error| format!("sync seed: {error}"))?;
    eprintln!("[frozen-depth-driver] copied caller-verified VERSION5 cache {name} ({copied} bytes)");
    Ok(())
}

struct Lease(world_frame::WorldFrame);
impl Drop for Lease {
    fn drop(&mut self) { let _ = world_frame::sv_world_release_frame(self.0.lease); }
}

fn acquire_matching(game_frame: u64) -> Result<Option<Lease>, String> {
    for eye in 0..2 {
        let mut frame = world_frame::WorldFrame { abi_version: 1,
            byte_size: std::mem::size_of::<world_frame::WorldFrame>() as u32,
            ..Default::default() };
        match world_frame::sv_world_acquire_frame(eye, &mut frame) {
            0 => {
                if frame.game_frame == game_frame && frame.color_texture != 0
                    && frame.depth_texture != 0 && frame.width == 640 && frame.height == 360 {
                    return Ok(Some(Lease(frame)));
                }
                let _ = world_frame::sv_world_release_frame(frame.lease);
            }
            1 => {}
            code => return Err(format!("world frame acquire eye {eye}: {code}")),
        }
    }
    Ok(None)
}

fn ppm_content(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("actual game PPM: {error}"))?;
    let header = b"P6\n640 360\n255\n";
    if !bytes.starts_with(header) || bytes.len() != header.len() + 640 * 360 * 3 {
        return Err("actual game PPM has wrong format or 640x360 extent".into());
    }
    let pixels = &bytes[header.len()..];
    let first = &pixels[..3];
    let varied = pixels.chunks_exact(3).any(|pixel| pixel != first);
    let nonzero = pixels.iter().any(|byte| *byte != 0);
    if !varied || !nonzero { return Err("actual game capture is blank or uniform".into()); }
    Ok(serde_json::json!({"path":path.display().to_string(),"bytes":bytes.len(),"width":640,"height":360,
        "nonzero":nonzero,"varied":varied}))
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let game = PathBuf::from(args.next().ok_or("usage: frozen-depth-live <absolute-game-root> <fresh-save-root> <deadline-seconds:1..600> [--cache-seed <absolute-VERSION5-file>]")?);
    let save = PathBuf::from(args.next().ok_or("missing fresh save root")?);
    let deadline: u64 = args.next().ok_or("missing deadline seconds")?.parse()
        .map_err(|_| "deadline must be an integer")?;
    if !(1..=600).contains(&deadline) { return Err("deadline must be 1..600 seconds".into()); }
    let seed = match args.next() {
        None => None,
        Some(flag) if flag == "--cache-seed" => Some(PathBuf::from(args.next().ok_or("missing seed path")?)),
        Some(_) => return Err("unexpected argument".into()),
    };
    if args.next().is_some() { return Err("unexpected trailing argument".into()); }
    if std::env::var("MUGUET_TRANSFER_FROZEN_DEPTH").as_deref() != Ok("1") {
        return Err("set MUGUET_TRANSFER_FROZEN_DEPTH=1 explicitly".into());
    }
    if !game.is_absolute() || !game.join("data/manifest.json").is_file() {
        return Err("game root must be absolute with data/manifest.json".into());
    }
    let builds: Vec<_> = std::fs::read_dir(game.join("b")).map_err(|error| format!("game build: {error}"))?
        .filter_map(Result::ok).map(|entry| entry.path())
        .filter(|path| path.join("game.wasm").is_file() && path.join("shaders/index.json").is_file())
        .collect();
    if builds.len() != 1 { return Err("game root needs exactly one WASM/shader build".into()); }
    if !save.is_absolute() || save.exists() { return Err("save root must be absolute and fresh".into()); }
    std::fs::create_dir_all(save.parent().ok_or("save has no parent")?)
        .map_err(|error| format!("save parent: {error}"))?;
    std::fs::create_dir(&save).map_err(|error| format!("fresh save root: {error}"))?;
    if let Some(seed) = seed.as_deref() { copy_seed(seed, &save)?; }
    std::fs::write(save.join("native-graphics.json"),
        b"{\"width\":640,\"height\":360,\"fps_limit\":30,\"graphics_preset\":\"low\"}\n")
        .map_err(|error| format!("seed Low 640x360 graphics: {error}"))?;
    std::env::set_var("MUGUET_MUTE", "1");
    std::env::set_var("MUGUET_MASTER_VOLUME", "0");
    std::env::set_var("MUGUET_XR_PAIRED_RENDER", "0");
    std::env::set_var("MUGUET_GEOMETRY_STEREO_CAPTURE", "0");
    std::env::set_var("MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT", "1");
    let game_arg = CString::new(game.to_string_lossy().as_bytes()).map_err(|_| "game path contains NUL")?;
    let save_arg = CString::new(save.to_string_lossy().as_bytes()).map_err(|_| "save path contains NUL")?;
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut error = [0i8; 8192];
        let code = native::sv_runtime_start(game_arg.as_ptr(), save_arg.as_ptr(), error.as_mut_ptr(), error.len());
        let result = if code == 0 { Ok(()) } else {
            Err(format!("native startup {code}: {}", unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }.to_string_lossy()))
        };
        let _ = startup_tx.send(result);
    });
    let enable = xr_view::sv_xr_world_camera_set_enabled(1);
    if enable != 0 { return Err(format!("world camera enable: {enable}")); }
    let capture_path = save.join("actual-game-source.ppm");
    let started = Instant::now();
    let mut request_frame = 1u64;
    let mut next_request = Instant::now();
    let mut startup_done = false;
    let mut capture_armed = false;
    let mut capture_frame = None;
    let mut held: Option<Lease> = None;
    let mut max_slots = 0u64;
    let mut max_leases = 0u64;
    eprintln!("[frozen-depth-driver] static synthetic 64 mm two-view requests; Low 640x360/30 muted; deadline {deadline}s");
    while started.elapsed() < Duration::from_secs(deadline) {
        if !startup_done {
            match startup_rx.try_recv() {
                Ok(Ok(())) => { startup_done = true; eprintln!("[frozen-depth-driver] native startup accepted"); }
                Ok(Err(error)) => return Err(error),
                Err(mpsc::TryRecvError::Disconnected) => return Err("startup worker disconnected".into()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if Instant::now() >= next_request {
            // A pose is consumed at one qualified camera update. Refresh its
            // frame while keeping the synthetic head transform stationary.
            let pose = xr::SVXRHeadPose { version: 1, size: xr::POSE_SIZE,
                frame: request_frame, presentation_time_seconds: 1.,
                stage_from_head: identity(), tracked: 1, reserved: 0 };
            if unsafe { xr::sv_xr_submit_pose(&pose) } != 0 {
                return Err(format!("synthetic pose {request_frame} rejected"));
            }
            let request = xr_view::SVXRViewRequest { version: 1, size: 288,
                frame: request_frame, presentation_time_seconds: 1.,
                view_count: 2, reserved: 0,
                views: [eye(-0.032), eye(0.032)] };
            let code = unsafe { xr_view::sv_xr_submit_views(&request) };
            if code != 0 { return Err(format!("synthetic request {request_frame}: {code}")); }
            request_frame += 1;
            next_request = Instant::now() + Duration::from_millis(350);
        }
        let status = native_status()?;
        let world = &status["world_frames"];
        let slots = world["slots"].as_u64().ok_or("world slot count missing")?;
        let leases = world["outstanding_leases"].as_u64().ok_or("world lease count missing")?;
        max_slots = max_slots.max(slots);
        max_leases = max_leases.max(leases);
        if slots > 6 || leases > 12 { return Err("world resource bound exceeded".into()); }
        if status["last_error"].as_str().is_some_and(|text| !text.is_empty()) {
            return Err(format!("native runtime error: {}", status["last_error"]));
        }
        if status["world_frames"]["error"].as_str().is_some_and(|text| !text.is_empty()) {
            return Err(format!("observed world frame error: {}", status["world_frames"]["error"]));
        }
        if startup_done && !capture_armed && world["frozen_depth_handoffs"].as_u64().unwrap_or(0) >= 2 {
            let path = CString::new(capture_path.to_string_lossy().as_bytes()).map_err(|_| "capture path contains NUL")?;
            if capture::sv_gpu_capture_next(path.as_ptr()) != 0 { return Err("arm actual game color capture failed".into()); }
            capture_armed = true;
            eprintln!("[frozen-depth-driver] capture armed after {} frozen handoffs", world["frozen_depth_handoffs"]);
        }
        let capture = &status["capture"];
        if capture_armed && capture["state"] == "failed" {
            return Err(format!("game capture failed: {}", capture["error"]));
        }
        if capture_armed && capture_frame.is_none() {
            capture_frame = capture["frame"].as_u64().filter(|frame| *frame > 0);
        }
        if let Some(target) = capture_frame {
            if held.is_none() { held = acquire_matching(target)?; }
            if capture["state"] == "written" && held.is_some() {
                let content = ppm_content(&capture_path)?;
                let lease = held.as_ref().unwrap().0;
                if lease.game_frame != target || lease.request_frame == 0 || lease.head_frame == 0
                    || lease.view_count != 2 || lease.view_index > 1 {
                    return Err("captured game frame and native lease identity differ".into());
                }
                let receipt = xr_view::consumed_world_frame_receipt(&lease)
                    .ok_or("no exact consumed request receipt for captured native lease")?;
                drop(held.take());
                let final_status = native_status()?;
                let final_world = &final_status["world_frames"];
                if final_world["frozen_depth_handoffs"].as_u64().unwrap_or(0) == 0
                    || final_world["published"].as_u64().unwrap_or(0) <= 2
                    || final_world["outstanding_leases"].as_u64() != Some(0)
                    || final_world["slots"].as_u64().unwrap_or(7) > 6
                    || final_world["error"].as_str().is_some() {
                    return Err("final world publication, lease, or error check failed".into());
                }
                let result = serde_json::json!({"schema":1,"status":"complete",
                    "scope":"one actual-game color capture joined to one GPU-complete leased source; synthetic two-view input, alternating-eye producer; no coherent stereo or live depth-pixel claim",
                    "input":{"width":640,"height":360,"fps_limit":30,"master_volume":0,
                        "head_pose_frame":lease.head_frame,"view_count_requested":2,"ipd_meters":0.064},
                    "capture":content,"capture_game_frame":target,
                    "consumed_request_receipt":receipt,
                    "leased_source":{"abi_version":lease.abi_version,"byte_size":lease.byte_size,
                        "lease":lease.lease,"depth_convention":lease.depth_convention,
                        "game_frame":lease.game_frame,"request_frame":lease.request_frame,
                        "head_frame":lease.head_frame,"view_index":lease.view_index,
                        "view_count":lease.view_count,"width":lease.width,"height":lease.height,
                        "presentation_time":lease.presentation_time,
                        "color_texture":lease.color_texture,"depth_texture":lease.depth_texture,
                        "projection_from_eye":lease.projection_from_eye,
                        "game_from_eye":lease.game_from_eye,"game_from_stage":lease.game_from_stage},
                    "max_slots":max_slots,"max_leases":max_leases,
                    "frozen_depth_handoffs":final_world["frozen_depth_handoffs"],
                    "copied_depth_snapshots":final_world["copied_depth_snapshots"],
                    "published":final_world["published"],"dropped":final_world["dropped"],
                    "final_native_status":final_status});
                let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
                    .open(save.join("frozen-depth-result.json"))
                    .map_err(|error| format!("create result: {error}"))?;
                file.write_all(serde_json::to_string_pretty(&result).unwrap().as_bytes())
                    .map_err(|error| format!("write result: {error}"))?;
                eprintln!("[frozen-depth-driver] {}", serde_json::to_string(&result).unwrap());
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("deadline expired: startup={startup_done} frozen={} capture_frame={capture_frame:?} lease_matched={}",
        native_status().ok().and_then(|status| status["world_frames"]["frozen_depth_handoffs"].as_u64()).unwrap_or(0), held.is_some()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[frozen-depth-driver] {error}");
        std::process::exit(1);
    }
}

//! Opt-in Mac capture of a natural game draw span with disposable local state.
//! No window, scene steering, coherent stereo or finished-world acceptance.
use anyhow::{ensure, Context, Result};
use muguet::{native, xr, xr_view};
use serde_json::{json, Value};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const GRAPHICS: [(&str, &str); 3] = [
    ("native-graphics.json", "580215049b6cd19e7f307097003dec08ea67342a35c83c17039675f2d8e6a0c4"),
    ("native-graphics-applied.json", "efce6e9588a619c1e81e22b3b2e90e72a70ca8297e73edbf9a34d56ada47274c"),
    ("settings.xml", "3acd159e5195c30b4347ce12919170d2441e76a9016cfa9131f37bdc862a80ea"),
];
const MAX_MANIFEST: u64 = 16 << 20;

struct Args {
    game: PathBuf, save: PathBuf, capture: PathBuf, graphics: PathBuf,
    aot: PathBuf, deadline: Duration, observe_after_capture: Duration,
}

fn identity() -> [f32; 16] {
    [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.]
}

fn eye(x: f32) -> xr_view::SVXREyeView {
    let near = 0.05; let far = 1000.;
    let mut head = identity(); head[12] = x;
    xr_view::SVXREyeView { head_from_eye: head,
        projection_from_eye: [1., 0., 0., 0., 0., 1., 0., 0., 0., 0.,
            near / (far - near), -1., 0., 0., near * far / (far - near), 0.] }
}

fn args() -> Result<Args> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    ensure!(a.len() == 7, "usage: native-span-capture GAME FRESH_SAVE NEW_CAPTURE GRAPHICS_SEED MAC_AOT DEADLINE_SECONDS OBSERVE_AFTER_CAPTURE_SECONDS");
    let seconds: u64 = a[5].parse().context("integer deadline")?;
    let observe: u64 = a[6].parse().context("integer post-capture observation")?;
    ensure!((1..=180).contains(&seconds), "deadline must be 1..180 seconds");
    ensure!(observe <= 30, "post-capture observation must be 0..30 seconds");
    Ok(Args { game: PathBuf::from(&a[0]), save: PathBuf::from(&a[1]), capture: PathBuf::from(&a[2]),
        graphics: PathBuf::from(&a[3]), aot: PathBuf::from(&a[4]), deadline: Duration::from_secs(seconds),
        observe_after_capture: Duration::from_secs(observe) })
}

struct ReadManifest { value: Value, sha256: String, byte_length: usize }
struct CompletedCapture { manifest: ReadManifest, first_status: Value, at: Instant, elapsed: f64 }

fn observation_stop(requested: Duration, elapsed: Duration, deadline: bool, failed: bool) -> Option<&'static str> {
    if failed { Some("runtime_failed") }
    else if requested.is_zero() { Some("not_requested") }
    else if elapsed >= requested { Some("completed") }
    else if deadline { Some("deadline_reached") }
    else { None }
}

fn capture_result(a: &Args, start: Instant, preparation: &Value, captured: &CompletedCapture,
                  observation_status: &str, final_status: Value) -> Value {
    let feedback = a.capture.parent().unwrap().join("first-color-feedback.json");
    json!({"schema":1,"outcome":"captured","capture":captured.manifest.value,
        "capture_identity":{"sha256":captured.manifest.sha256,"byte_length":captured.manifest.byte_length},
        "elapsed_seconds":start.elapsed().as_secs_f64(),"native_status":final_status,
        "preparation":preparation,"capture_byte_admission_required":true,
        "observation":{"scope":"normal runtime after completed initial capture; not whole-run GPU health acceptance",
            "requested_seconds":a.observe_after_capture.as_secs(),"elapsed_seconds":captured.at.elapsed().as_secs_f64(),
            "status":observation_status,"capture_elapsed_seconds":captured.elapsed,
            "initial_native_status":captured.first_status,"final_native_status":final_status,
            "first_color_feedback_exists":feedback.is_file(),"first_color_feedback_path":feedback,
            "whole_run_gpu_health_accepted":false},
        "finished_world_accepted":false,"coherent_stereo_accepted":false,"hardware_accepted":false})
}

fn exact_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute() && path.canonicalize()? == path, "absolute path without symlink or traversal required: {}", path.display());
    Ok(())
}

fn pinned(path: &Path, wanted: &str, max_bytes: u64) -> Result<Vec<u8>> {
    exact_path(path)?;
    let meta = path.metadata()?;
    ensure!(meta.is_file() && meta.len() > 0 && meta.len() <= max_bytes, "bounded regular input required: {}", path.display());
    let bytes = std::fs::read(path)?;
    ensure!(muguet::xr_motion_patch::sha256(&bytes)? == wanted, "input hash mismatch: {}", path.display());
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?; f.sync_all()?; Ok(())
}

fn status() -> Result<Value> {
    let mut bytes = vec![0i8; 2 << 20];
    ensure!(native::sv_runtime_status(bytes.as_mut_ptr(), bytes.len()) == 0, "native status unavailable/oversized");
    Ok(serde_json::from_slice(unsafe { CStr::from_ptr(bytes.as_ptr()) }.to_bytes())?)
}

fn parse_completion(bytes: &[u8]) -> Result<Option<ReadManifest>> {
    ensure!(bytes.len() as u64 <= MAX_MANIFEST, "capture result exceeds manifest bound");
    let Ok(value) = serde_json::from_slice(bytes) else { return Ok(None); };
    // Bind the bytes parsed here, not a reserialized Value: JSON float parsing
    // can change a matrix decimal by one ULP without any producer-file change.
    Ok(Some(ReadManifest { value, sha256:muguet::xr_motion_patch::sha256(bytes)?, byte_length:bytes.len() }))
}

fn read_completion(path: &Path) -> Result<Option<ReadManifest>> {
    if !path.exists() { return Ok(None); }
    exact_path(path)?;
    ensure!(path.metadata()?.len() <= MAX_MANIFEST, "capture result exceeds manifest bound");
    // Exclusive create becomes visible before write_all completes; retry only
    // incomplete JSON until the same bounded deadline, never accept it.
    parse_completion(&std::fs::read(path)?)
}

fn prepare(a: &Args) -> Result<Value> {
    exact_path(&a.game)?; exact_path(&a.graphics)?; exact_path(&a.aot)?;
    let parent = a.save.parent().context("save parent")?; exact_path(parent)?;
    ensure!(a.save.is_absolute() && a.capture.is_absolute() && a.capture.parent() == Some(parent), "save/capture must be absolute siblings");
    ensure!(a.save.file_name().is_some_and(|s| s == "fresh-save") && a.capture.file_name().is_some_and(|s| s == "capture"), "fixed disposable child names required");
    ensure!(!a.save.try_exists()? && !a.capture.try_exists()?, "existing save/capture must be preserved");
    ensure!(std::env::var_os("MUGUET_NATIVE_DRAW_CAPTURE").as_deref() == Some(a.capture.as_os_str()), "explicit capture environment must match new path");
    ensure!(std::env::var("MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT").as_deref() == Ok("1"), "consumed request retention must be enabled before native startup");
    ensure!(std::env::var("MUGUET_MUTE").as_deref() == Ok("1") && std::env::var("MUGUET_MASTER_VOLUME").as_deref() == Ok("0"), "explicit mute required");
    let aot = muguet::native_draw_capture_host::prepare(&a.game.join("b/8b0b5899ed/game.wasm"), &a.aot)?;
    let mut copies = Vec::new();
    for (name, expected) in GRAPHICS {
        copies.push((name, pinned(&a.graphics.join(name), expected, 1 << 20)?));
    }
    let graphics: Value = serde_json::from_slice(&copies[0].1)?;
    ensure!(graphics == json!({"width":2560,"height":1440,"fps_limit":60,"graphics_preset":"clarity"}), "exact original Clarity settings required");
    // Input verification precedes any save creation. Only these three settings
    // are copied; no player profiles, prior saves or old cache tree are read.
    std::fs::create_dir(&a.save)?;
    let settings = a.save.join("userdata/Documents/Rockstar Games/GTA V");
    std::fs::create_dir_all(&settings)?;
    for (name, bytes) in &copies {
        let destination = if *name == "settings.xml" { settings.join(name) } else { a.save.join(name) };
        write_new(&destination, bytes)?;
    }
    write_new(&a.save.join("native-gameplay.json"), b"{\"developer_mode\":true}\n")?;
    Ok(json!({"schema":1,"graphics":graphics,"aot":aot,
        "copied_files":GRAPHICS.iter().map(|(name,sha)|json!({"name":name,"sha256":sha})).collect::<Vec<_>>(),
        "scene":"fresh developer-mode natural game; no fixed scene or player-save import",
        "input":"synthetic static head; two eyes separated by 64mm",
        "finished_world_accepted":false,"coherent_stereo_accepted":false,"hardware_accepted":false}))
}

fn run(a: &Args, preparation: Value) -> Result<Value> {
    ensure!(native::sv_world_set_frozen_depth_transfer(1) == 0, "frozen depth prestart request");
    ensure!(native::sv_runtime_set_draw_setup_overhead(1) == 0, "draw setup prestart request");
    ensure!(xr_view::sv_xr_world_camera_set_enabled(1) == 0, "world camera enable");
    let game = CString::new(a.game.to_str().context("UTF8 game path")?)?;
    let save = CString::new(a.save.to_str().context("UTF8 save path")?)?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut error = vec![0i8; 65536];
        let code = native::sv_runtime_start(game.as_ptr(), save.as_ptr(), error.as_mut_ptr(), error.len());
        let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy().into_owned();
        let _ = tx.send((code, message));
    });
    let start = Instant::now(); let mut frame = 0u64; let mut startup_done = false;
    let mut next_request = start; let mut next_report = start;
    let mut captured: Option<CompletedCapture> = None;
    let mut next_observation_status = start;
    loop {
        if start.elapsed() >= a.deadline {
            if let Some(captured) = &captured {
                let final_status = status()?;
                let stop = observation_stop(a.observe_after_capture, captured.at.elapsed(), true, final_status["state"] == "failed").unwrap();
                return Ok(capture_result(a, start, &preparation, captured, stop, final_status));
            }
            anyhow::bail!("capture deadline expired after {} seconds", a.deadline.as_secs());
        }
        if !startup_done {
            match rx.try_recv() {
                Ok((code, message)) => { ensure!(code == 0, "native startup {code}: {message}"); startup_done = true; }
                Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("native startup thread disconnected"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if Instant::now() >= next_request {
            frame += 1; let presentation = start.elapsed().as_secs_f64() + 1.;
            let pose = xr::SVXRHeadPose { version:1, size:xr::POSE_SIZE, frame,
                presentation_time_seconds:presentation, stage_from_head:identity(), tracked:1, reserved:0 };
            let views = xr_view::SVXRViewRequest { version:1, size:288, frame,
                presentation_time_seconds:presentation, view_count:2, reserved:0,
                views:[eye(-0.032), eye(0.032)] };
            ensure!(unsafe { xr::sv_xr_submit_pose(&pose) } == 0, "static pose rejected");
            ensure!(unsafe { xr_view::sv_xr_submit_views(&views) } == 0, "two-eye request rejected");
            next_request = Instant::now() + Duration::from_millis(100);
        }
        if let Some(rejection) = read_completion(&a.capture.join("rejected.json"))? {
            anyhow::bail!("capture module rejected span: {}", rejection.value);
        }
        if captured.is_none() {
            if let Some(manifest) = read_completion(&a.capture.join("capture.json"))? {
                ensure!(startup_done, "capture completed before startup was acknowledged");
                ensure!(manifest.value["schema"] == 2 && manifest.value["kind"] == "native_draw_span"
                    && manifest.value["status"] == "captured" && manifest.value["closure"]["natural_submission_completed"] == true,
                    "capture manifest completion contract absent");
                captured = Some(CompletedCapture { manifest, first_status:status()?, at:Instant::now(), elapsed:start.elapsed().as_secs_f64() });
                eprintln!("[native-span-capture] initial capture complete; observing normal runtime up to {}s within original deadline", a.observe_after_capture.as_secs());
            }
        }
        if let Some(captured) = &captured {
            if Instant::now() >= next_observation_status || captured.at.elapsed() >= a.observe_after_capture {
                let final_status = status()?;
                if let Some(stop) = observation_stop(a.observe_after_capture, captured.at.elapsed(), false, final_status["state"] == "failed") {
                    return Ok(capture_result(a, start, &preparation, captured, stop, final_status));
                }
                next_observation_status = Instant::now() + Duration::from_millis(250);
            }
        }
        if Instant::now() >= next_report {
            let s = status()?;
            if s["state"] == "failed" {
                if let Some(captured) = &captured {
                    return Ok(capture_result(a, start, &preparation, captured, "runtime_failed", s));
                }
                anyhow::bail!("native runtime failed: {}", s["last_error"]);
            }
            eprintln!("[native-span-capture] elapsed={}s state={} presents={} (natural scene; no finished-world claim)", start.elapsed().as_secs(), s["state"], s["presents"]);
            next_report = Instant::now() + Duration::from_secs(5);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_identity_binds_original_bytes_not_reserialized_floats() {
        let bytes = br#"{"matrix":[-0.014437142759561539,-0.9598827958106995]}"#;
        let manifest = parse_completion(bytes).unwrap().unwrap();
        assert_eq!(manifest.byte_length, bytes.len());
        assert_eq!(manifest.sha256, muguet::xr_motion_patch::sha256(bytes).unwrap());
        let mut changed = bytes.to_vec(); changed.push(b' ');
        let same_value = parse_completion(&changed).unwrap().unwrap();
        assert_eq!(manifest.value, same_value.value);
        assert_ne!(manifest.sha256, same_value.sha256);
        assert!(parse_completion(b"{\"matrix\":").unwrap().is_none());
    }
    #[test]
    fn observation_never_extends_deadline_and_reports_runtime_failure_separately() {
        let seconds = Duration::from_secs;
        assert_eq!(observation_stop(seconds(0), seconds(0), false, false), Some("not_requested"));
        assert_eq!(observation_stop(seconds(20), seconds(19), false, false), None);
        assert_eq!(observation_stop(seconds(20), seconds(20), false, false), Some("completed"));
        assert_eq!(observation_stop(seconds(20), seconds(2), true, false), Some("deadline_reached"));
        assert_eq!(observation_stop(seconds(20), seconds(2), false, true), Some("runtime_failed"));
    }
}

fn main() {
    let result = args().and_then(|a| {
        // A failed admission must not write a result into an unowned/existing
        // parent. prepare creates the fresh save only after validating inputs.
        let preparation = prepare(&a)?;
        write_new(&a.save.parent().unwrap().join("driver-preparation.json"), &serde_json::to_vec_pretty(&preparation)?)?;
        let result = run(&a, preparation);
        let receipt = match &result { Ok(v) => v.clone(), Err(e) => json!({"schema":1,"outcome":"failed","error":format!("{e:#}"),"native_status":status().ok()}) };
        if let Some(parent) = a.save.parent().filter(|p| p.is_dir()) {
            write_new(&parent.join("driver-result.json"), &serde_json::to_vec_pretty(&receipt)?)?;
        }
        result.map(|_| ())
    });
    if let Err(error) = result { eprintln!("[native-span-capture] {error:#}"); std::process::exit(1); }
}

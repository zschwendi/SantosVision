//! First-person foreground depth: producer attribution, guest-call boundary
//! guard, immutable GPU-ring marker, status and bounded capture persistence.
//! Rules live in xr_foreground_depth_core.rs; GPU execution lives in gpu.rs.
//! Default OFF. Design and acceptance: docs/FP-FOREGROUND-DEPTH-20261024.md.
//!
//! A binding exists only between the completed-model seam that corrected an
//! original 44415/44355 first-person model and the next SetCurrent on the same
//! guest thread. The fixed-FOV family 44395 never binds. Emissions inside the
//! binding carry its generation and the exact displaced parent WorldView; the
//! GPU treats that as foreground raster in the parent's eye and projection.
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use wasmtime::SharedMemory;
use super::WorldView;

#[path = "xr_foreground_depth_core.rs"]
pub(crate) mod logic;

/// grcViewport layout used by xr_view's certified setters.
const CAMERA: u64 = 192;
const PROJECTION: u64 = 320;
const SCALARS: u64 = 872;
const SCALAR_STATE: u64 = 916;
const REVERSE_Z: u64 = 928;
/// Both originals build their clone at frame+128 and the model at frame+1072.
const CLONE: u64 = 128;
const MODEL: u64 = 1072;

fn resolve(runtime: Option<&str>, compiled: Option<&str>) -> bool {
    match runtime { Some("1") => true, Some(_) => false, None => compiled == Some("1") }
}
static MERGE_REQUESTED: LazyLock<bool> = LazyLock::new(|| resolve(
    std::env::var("MUGUET_XR_FP_FOREGROUND_DEPTH").ok().as_deref(), option_env!("MUGUET_XR_FP_FOREGROUND_DEPTH_DEFAULT")));
/// Absolute, or relative to the app's HOME container. Must already exist.
static CAPTURE_DIR: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    let value = std::env::var_os("MUGUET_XR_FP_DEPTH_CAPTURE_DIR").map(PathBuf::from)
        .or_else(|| option_env!("MUGUET_XR_FP_DEPTH_CAPTURE_DIR_DEFAULT").map(PathBuf::from))?;
    let path = if value.is_absolute() { value } else { PathBuf::from(std::env::var_os("HOME")?).join(value) };
    path.is_dir().then_some(path)
});

pub(crate) fn merge_requested() -> bool { *MERGE_REQUESTED }
pub(crate) fn capture_dir() -> Option<PathBuf> { CAPTURE_DIR.clone() }
/// Attribution alone never changes a published image; the merge flag does.
pub(crate) fn attribution_requested() -> bool {
    #[cfg(test)] if tests::FORCE_ATTRIBUTION.with(|force| force.get()) { return true; }
    *MERGE_REQUESTED || CAPTURE_DIR.is_some()
}
fn attribution_enabled() -> bool {
    attribution_requested() && crate::xr_motion_aim::viewmodel_enabled() && super::world_camera_enabled()
}

/// Immutable ring payload. The GPU never consults producer state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ForegroundMark { pub generation: u64, pub family: u32, pub view: WorldView }

thread_local! {
    static PRODUCER: RefCell<logic::Producer<WorldView>> = const { RefCell::new(logic::Producer::new()) };
}
/// Never recycled; zero means exhausted and no further binding.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static TAGGED_EMISSIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct ProducerStatus {
    bindings: u64, rejections: u64, balanced: u64, other_set_current: u64, boundary: u64, epoch: u64,
    superseded: u64, mismatches: u64,
    last_rejection: Option<(Instant, &'static str)>,
    last_end: Option<(Instant, u64, u32, &'static str)>,
    last_binding: Option<(Instant, u64, u32, u64, u32)>,
}
static PRODUCER_STATUS: LazyLock<Mutex<ProducerStatus>> = LazyLock::new(Mutex::default);

fn producer_status() -> std::sync::MutexGuard<'static, ProducerStatus> {
    PRODUCER_STATUS.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn record_end(binding: &logic::Binding<WorldView>, end: logic::End) {
    let mut status = producer_status();
    let name = match end {
        logic::End::Balanced => { status.balanced += 1; "balanced_parent_restoration" }
        logic::End::OtherSetCurrent => { status.other_set_current += 1; "other_set_current" }
        logic::End::Boundary => { status.boundary += 1; "guest_call_boundary_exit_without_restoration" }
        logic::End::Epoch => { status.epoch += 1; "view_epoch_changed" }
        logic::End::Superseded => { status.superseded += 1; "nested_seam_without_set_current" }
        logic::End::Mismatch(reason) => { status.mismatches += 1; reason }
    };
    status.last_end = Some((Instant::now(), binding.generation, binding.family, name));
}

fn with_producer<R>(f: impl FnOnce(&mut logic::Producer<WorldView>) -> R) -> Option<R> {
    PRODUCER.try_with(|producer| producer.try_borrow_mut().ok().map(|mut producer| f(&mut producer))).ok().flatten()
}

/// SetCurrent pre-hook entry, before any early return: no binding outlives it.
pub(crate) fn end_at_set_current(viewport: u64) {
    if !attribution_requested() { return; }
    if let Some((binding, end)) = with_producer(|producer| producer.end_at_set_current(viewport)).flatten() { record_end(&binding, end); }
}

/// SetCurrent pre-hook exit: the viewport now current and the view it carries.
pub(crate) fn note_current(epoch: u64, viewport: u64, view: Option<WorldView>) {
    if !attribution_requested() { return; }
    with_producer(|producer| producer.note_current(epoch, viewport, view));
}

pub(crate) fn binding_active() -> bool {
    attribution_requested() && PRODUCER.try_with(|producer| producer.try_borrow().is_ok_and(|producer| producer.active().is_some())).unwrap_or(false)
}

fn bits(m: &[f32; 16]) -> [u32; 16] { m.map(f32::to_bits) }

fn words<const N: usize>(mem: &SharedMemory, address: u64) -> Option<[u32; N]> {
    let end = address.checked_add(N as u64 * 4)?;
    if end > mem.data_size() as u64 { return None; }
    let bytes = crate::read_bytes(mem, address, (N * 4) as u64);
    let mut out = [0u32; N];
    for (word, chunk) in out.iter_mut().zip(bytes.chunks_exact(4)) { *word = u32::from_le_bytes(chunk.try_into().ok()?); }
    (bytes.len() == N * 4).then_some(out)
}

fn byte(mem: &SharedMemory, address: u64) -> Option<u8> {
    if address.checked_add(1)? > mem.data_size() as u64 { return None; }
    crate::read_bytes(mem, address, 1).first().copied()
}

fn seam_words(mem: &SharedMemory, parent: u64, clone: u64, frame: u64) -> Option<logic::SeamWords> {
    Some(logic::SeamWords {
        parent_camera: words(mem, parent.checked_add(CAMERA)?)?, parent_projection: words(mem, parent.checked_add(PROJECTION)?)?,
        parent_scalars: words(mem, parent.checked_add(SCALARS)?)?,
        parent_flags: [byte(mem, parent.checked_add(SCALAR_STATE)?)?, byte(mem, parent.checked_add(REVERSE_Z)?)?],
        clone_camera: words(mem, clone.checked_add(CAMERA)?)?, clone_projection: words(mem, clone.checked_add(PROJECTION)?)?,
        clone_scalars: words(mem, clone.checked_add(SCALARS)?)?,
        clone_flags: [byte(mem, clone.checked_add(SCALAR_STATE)?)?, byte(mem, clone.checked_add(REVERSE_Z)?)?],
        model: words(mem, frame.checked_add(MODEL)?)?,
    })
}

/// Called by the completed-model seam only after `qualify_and_correct` wrote
/// (or kept) the corrected model. `corrected` is the installed eye it used.
pub(crate) fn bind_after_correction(state: &crate::HostState, mem: &SharedMemory, family: u32, frame: u64, parent: u64,
    corrected: &WorldView) -> Result<u64, &'static str> {
    if !attribution_enabled() { return Err("foreground_attribution_not_requested"); }
    let result = bind_checked(mem, crate::guest_call_scope::producer_store_lifetime(state), state.pthread, family, frame, parent,
        corrected, super::view_epoch(), super::view_epoch_is_active);
    let mut status = producer_status();
    match result {
        Ok(binding) => {
            status.bindings += 1;
            status.last_binding = Some((Instant::now(), binding.generation, binding.family, binding.view.camera_sequence, binding.view.eye_index));
            Ok(binding.generation)
        }
        Err(reason) => { status.rejections += 1; status.last_rejection = Some((Instant::now(), reason)); Err(reason) }
    }
}

#[allow(clippy::too_many_arguments)]
fn bind_checked(mem: &SharedMemory, store: u64, pthread: u64, family: u32, frame: u64, parent: u64, corrected: &WorldView,
    epoch: u64, epoch_active: impl Fn(u64) -> bool) -> Result<logic::Binding<WorldView>, &'static str> {
    if family_rejected(family) { return Err("family_not_completed_model_seam"); }
    let clone = frame.checked_add(CLONE).ok_or("frame_range")?;
    let (displaced, view) = with_producer(|producer| producer.displaced(epoch, clone)).ok_or("producer_state_unavailable")??;
    if displaced != parent { return Err("parent_is_not_displaced_viewport"); }
    if !view.headset_applied || view.provenance_epoch != epoch || !epoch_active(epoch) {
        return Err("displaced_view_is_not_current_headset_eye");
    }
    if view.provenance_epoch != corrected.provenance_epoch || view.camera_sequence != corrected.camera_sequence
        || view.eye_index != corrected.eye_index
        || logic::coordinates(&bits(&view.game_from_eye)) != logic::coordinates(&bits(&corrected.game_from_eye)) {
        return Err("displaced_view_is_not_corrected_eye");
    }
    if store == 0 || pthread == 0 { return Err("unknown_store_or_pthread"); }
    let words = seam_words(mem, parent, clone, frame).ok_or("seam_words_unreadable")?;
    let certificate = logic::certify_clone(&words, &logic::RasterKey { camera: bits(&view.game_from_eye), projection: bits(&view.projection_from_eye) })?;
    let generation = NEXT_GENERATION.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| value.checked_add(1)).unwrap_or(0);
    let binding = logic::Binding { generation, family, epoch, store, pthread, clone, parent, frame, view, certificate };
    match with_producer(|producer| producer.bind(binding)).ok_or("producer_state_unavailable")? {
        Ok(()) => Ok(binding),
        Err((reason, revoked)) => { if let Some((old, end)) = revoked { record_end(&old, end); } Err(reason) }
    }
}

fn family_rejected(family: u32) -> bool { logic::family_bit(family) == 0 }

fn stack_pointer(caller: &mut wasmtime::Caller<'_, crate::HostState>) -> Option<u64> {
    caller.get_export("__muguet_motion_stack_pointer")?.into_global()?.get(&mut *caller).i64().map(|value| value as u64)
}

/// The invocation must still be live (guest stack at or below its frame) and
/// its clone camera, projection and corrected model unchanged, bit for bit.
fn still_certified(mem: &SharedMemory, binding: &logic::Binding<WorldView>, sp: Option<u64>) -> Result<(), &'static str> {
    if !attribution_enabled() { return Err("foreground_attribution_disabled"); }
    if !super::view_epoch_is_active(binding.view.provenance_epoch) { return Err("view_epoch_inactive"); }
    certified_memory(mem, binding, sp)
}

fn certified_memory(mem: &SharedMemory, binding: &logic::Binding<WorldView>, sp: Option<u64>) -> Result<(), &'static str> {
    match sp { Some(sp) if sp != 0 && sp <= binding.frame => {}, Some(_) => return Err("invocation_frame_unwound"), None => return Err("stack_pointer_unavailable") }
    let camera: [u32; 16] = words(mem, binding.clone + CAMERA).ok_or("clone_unreadable")?;
    if logic::coordinates(&camera) != binding.certificate.clone_camera { return Err("clone_camera_changed"); }
    let projection: [u32; 16] = words(mem, binding.clone + PROJECTION).ok_or("clone_unreadable")?;
    if projection != binding.certificate.projection { return Err("clone_projection_changed"); }
    let model: [u32; 16] = words(mem, binding.frame + MODEL).ok_or("model_unreadable")?;
    if logic::coordinates(&model) != binding.certificate.model { return Err("corrected_model_changed"); }
    Ok(())
}

/// wasm_gpu::Emit midpoint (producer mutex held). Host reads only: no guest
/// call, yield or mailbox here.
pub(crate) fn emission(caller: &mut wasmtime::Caller<'_, crate::HostState>, mem: &SharedMemory) -> Option<ForegroundMark> {
    if !binding_active() { return None; }
    let sp = stack_pointer(caller);
    let (store, pthread) = (crate::guest_call_scope::producer_store_lifetime(caller.data()), caller.data().pthread);
    let epoch = super::view_epoch();
    let (tag, ended) = with_producer(|producer| producer.emission(epoch, store, pthread, |binding| still_certified(mem, binding, sp)))?;
    if let Some((binding, end)) = ended { record_end(&binding, end); }
    let binding = tag?;
    TAGGED_EMISSIONS.fetch_add(1, Ordering::Relaxed);
    Some(ForegroundMark { generation: binding.generation, family: binding.family, view: binding.view })
}

/// Held for every host-to-guest call (guest_call_scope::call). Mailbox work and
/// its descendants never carry attribution; a binding opened inside a call that
/// returns, errors, LongJmps or panics without its original restoration is
/// revoked before the caller can recover (stack_restore/setThrew).
pub(crate) struct Boundary(Option<logic::BoundaryToken>);
impl Boundary {
    #[inline(always)]
    pub(crate) fn enter(mailbox: bool) -> Self {
        if !attribution_requested() { return Self(None); }
        let floor = NEXT_GENERATION.load(Ordering::SeqCst);
        Self(with_producer(|producer| producer.boundary_enter(mailbox, floor)))
    }
}
impl Drop for Boundary {
    fn drop(&mut self) {
        let Some(token) = self.0.take() else { return; };
        if let Some((binding, end)) = with_producer(|producer| producer.boundary_exit(token)).flatten() { record_end(&binding, end); }
    }
}

/// Bit-identical raster: identity, camera, stage, projection (with its depth
/// row) and optics. Foreground depth may only be combined with such a record.
pub(crate) fn same_raster(a: &WorldView, b: &WorldView) -> bool {
    a.headset_applied && b.headset_applied && a.provenance_epoch == b.provenance_epoch
        && a.camera_sequence == b.camera_sequence && a.request_frame == b.request_frame
        && a.head_pose_frame == b.head_pose_frame && a.eye_index == b.eye_index && a.view_count == b.view_count
        && a.depth_reverse_z == b.depth_reverse_z && a.optics == b.optics
        && a.presentation_time_seconds.to_bits() == b.presentation_time_seconds.to_bits()
        && logic::coordinates(&bits(&a.game_from_eye)) == logic::coordinates(&bits(&b.game_from_eye))
        && bits(&a.game_from_stage) == bits(&b.game_from_stage) && bits(&a.projection_from_eye) == bits(&b.projection_from_eye)
}

#[derive(Default)]
pub(crate) struct GpuStatus {
    pub foreground_draws: u64,
    pub foreground_depth_draws: u64,
    pub combined_live_writes: u64,
    pub overlay_writes: u64,
    pub overlays_opened: u64,
    /// Merged depth handed to world_frame publication (which may still drop it).
    pub merges_handed_to_publication: u64,
    pub generic: u64,
    pub withdrawn_world_depth: u64,
    pub last_generic: Option<(Instant, &'static str)>,
    pub last_withdrawal: Option<(Instant, &'static str)>,
    pub last_merge: Option<(Instant, u64, u64, u32, u64, u64)>,
}
static GPU_STATUS: LazyLock<Mutex<GpuStatus>> = LazyLock::new(Mutex::default);
pub(crate) fn gpu_status() -> std::sync::MutexGuard<'static, GpuStatus> {
    GPU_STATUS.lock().unwrap_or_else(|poison| poison.into_inner())
}
pub(crate) fn note_generic(reason: &'static str) {
    let mut status = gpu_status(); status.generic += 1; status.last_generic = Some((Instant::now(), reason));
}
pub(crate) fn note_withdrawal(reason: &'static str) {
    let mut status = gpu_status(); status.withdrawn_world_depth += 1; status.last_withdrawal = Some((Instant::now(), reason));
}
pub(crate) fn note_merge(present: u64, overlay: &logic::Overlay) {
    let mut status = gpu_status();
    status.merges_handed_to_publication += 1;
    status.last_merge = Some((Instant::now(), present, overlay.draws, overlay.families, overlay.first_generation, overlay.last_generation));
}

#[derive(Default)]
struct CaptureStatus { state: &'static str, path: Option<PathBuf>, error: Option<String>, images: usize, events: usize }
static CAPTURE_STATUS: LazyLock<Mutex<CaptureStatus>> = LazyLock::new(|| Mutex::new(CaptureStatus { state: "disabled", ..Default::default() }));
pub(crate) fn capture_state(state: &'static str) {
    if let Ok(mut status) = CAPTURE_STATUS.lock() { status.state = state; }
}

/// One registered raw image: rows in GPU copy order, padding removed on write.
pub(crate) struct CaptureImage {
    pub name: &'static str,
    pub buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    pub row_bytes: u32,
    pub padded_row_bytes: u32,
    pub format: String,
    pub source: serde_json::Value,
}

/// Map after the copies were submitted, then write on a background thread into
/// a NEW directory with create_new files. Never overwrites, never retries.
pub(crate) fn persist_capture(device: wgpu::Device, submission: Option<wgpu::SubmissionIndex>, images: Vec<CaptureImage>,
    mut manifest: serde_json::Value) {
    let Some(root) = capture_dir() else { capture_state("failed"); return; };
    let receivers: Vec<_> = images.iter().map(|image| {
        let (sender, receiver) = std::sync::mpsc::channel();
        image.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = sender.send(result); });
        receiver
    }).collect();
    if let Ok(mut status) = CAPTURE_STATUS.lock() { status.state = "persisting"; status.images = images.len(); }
    let spawned = std::thread::Builder::new().name("fp-depth-capture".into()).spawn(move || {
        let result = (|| -> anyhow::Result<PathBuf> {
            use anyhow::Context;
            use std::io::Write;
            let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let dir = root.join(format!("fp-depth-{millis}-{}", std::process::id()));
            std::fs::create_dir(&dir).with_context(|| format!("create new capture directory {}", dir.display()))?;
            device.poll(wgpu::PollType::Wait { submission_index: submission, timeout: Some(std::time::Duration::from_secs(10)) })
                .context("wait for foreground capture copies")?;
            let mut files = Vec::new();
            for (image, receiver) in images.iter().zip(receivers) {
                receiver.recv_timeout(std::time::Duration::from_secs(5)).context("wait for capture mapping")?.context("map capture")?;
                let mapped = image.buffer.slice(..).get_mapped_range().context("mapped capture range")?;
                let file_name = format!("{}.raw", image.name);
                let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&file_name))
                    .with_context(|| format!("create {file_name}"))?;
                for row in 0..image.height as usize {
                    let start = row * image.padded_row_bytes as usize;
                    file.write_all(mapped.get(start..start + image.row_bytes as usize).context("short capture row")?)?;
                }
                file.sync_all()?;
                drop(mapped);
                image.buffer.unmap();
                files.push(serde_json::json!({"name":image.name,"file":file_name,"width":image.width,"height":image.height,
                    "row_bytes":image.row_bytes,"bytes":image.row_bytes as u64*image.height as u64,"format":image.format,"source":image.source}));
            }
            manifest["images"] = serde_json::Value::Array(files);
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join("manifest.json")).context("create manifest")?;
            file.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
            file.sync_all()?;
            Ok(dir)
        })();
        if let Ok(mut status) = CAPTURE_STATUS.lock() {
            match result {
                Ok(dir) => { status.state = "written"; status.path = Some(dir); }
                Err(error) => { status.state = "failed"; status.error = Some(format!("{error:#}")); }
            }
        }
    });
    if spawned.is_err() { if let Ok(mut status) = CAPTURE_STATUS.lock() { status.state = "failed"; status.error = Some("spawn capture writer".into()); } }
}

pub(crate) fn note_capture_events(events: usize) {
    if let Ok(mut status) = CAPTURE_STATUS.lock() { status.events = events; }
}

fn age(at: Instant) -> u128 { at.elapsed().as_millis() }

pub(crate) fn status_json() -> serde_json::Value {
    let producer = producer_status();
    let gpu = gpu_status();
    let capture = CAPTURE_STATUS.lock().map(|c| serde_json::json!({"state":c.state,"path":c.path,"error":c.error,"images":c.images,"events":c.events}))
        .unwrap_or(serde_json::json!({"state":"status_lock_poisoned"}));
    serde_json::json!({
        "merge_requested": merge_requested(), "capture_requested": capture_dir().is_some(),
        "attribution_active": attribution_enabled(),
        "producer": {"bindings":producer.bindings,"rejections":producer.rejections,"tagged_emissions":TAGGED_EMISSIONS.load(Ordering::Relaxed),
            "ended":{"balanced":producer.balanced,"other_set_current":producer.other_set_current,"boundary":producer.boundary,
                "epoch":producer.epoch,"superseded":producer.superseded,"mismatch":producer.mismatches},
            "last_rejection":producer.last_rejection.map(|(at,reason)|serde_json::json!({"reason":reason,"age_ms":age(at)})),
            "last_end":producer.last_end.map(|(at,generation,family,reason)|serde_json::json!({"generation":generation,"family":family,"reason":reason,"age_ms":age(at)})),
            "last_binding":producer.last_binding.map(|(at,generation,family,camera,eye)|serde_json::json!({"generation":generation,"family":family,"camera_sequence":camera,"eye_index":eye,"age_ms":age(at)}))},
        "gpu": {"foreground_draws":gpu.foreground_draws,"foreground_depth_draws":gpu.foreground_depth_draws,
            "combined_live_writes":gpu.combined_live_writes,"overlay_writes":gpu.overlay_writes,"overlays_opened":gpu.overlays_opened,
            "merges_handed_to_publication":gpu.merges_handed_to_publication,"generic":gpu.generic,"withdrawn_world_depth":gpu.withdrawn_world_depth,
            "last_generic":gpu.last_generic.map(|(at,reason)|serde_json::json!({"reason":reason,"age_ms":age(at)})),
            "last_withdrawal":gpu.last_withdrawal.map(|(at,reason)|serde_json::json!({"reason":reason,"age_ms":age(at)})),
            "last_merge":gpu.last_merge.map(|(at,present,draws,families,first,last)|serde_json::json!({"present":present,"foreground_draws":draws,
                "families":families,"first_generation":first,"last_generation":last,"age_ms":age(at)}))},
        "capture": capture,
        "authority": "producer-certified attribution and actual GPU depth bookkeeping only; no physical visibility, barrel/reticle alignment, reprojection or comfort acceptance"
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::Cell;
    thread_local! { pub(super) static FORCE_ATTRIBUTION: Cell<bool> = const { Cell::new(false) }; }
    struct Force;
    impl Force { fn new() -> Self { FORCE_ATTRIBUTION.with(|force| force.set(true)); Self } }
    impl Drop for Force { fn drop(&mut self) { FORCE_ATTRIBUTION.with(|force| force.set(false)); } }

    const PARENT: u64 = 4096;
    const FRAME: u64 = 16384;
    fn eye() -> WorldView {
        let (s, c) = 0.3f32.sin_cos();
        WorldView { provenance_epoch: 5, camera_sequence: 41, request_frame: 9, head_pose_frame: 10, presentation_time_seconds: 2.5,
            eye_index: 1, view_count: 2, game_from_eye: [c, 0., -s, 0., 0., 1., 0., 0., s, 0., c, 0., 1210.5, 29.75, -1353.25, 1.],
            game_from_stage: [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.],
            projection_from_eye: [1.1, 0., 0., 0., 0., 1.3, 0., 0., 0.02, -0.01, 0.0001, -1., 0., 0., 0.05, 0.],
            depth_reverse_z: true, headset_applied: true, optics: super::super::ScopeOptics::default() }
    }
    fn put(mem: &SharedMemory, address: u64, words: &[u32]) {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert!(crate::write_bytes(mem, address, &bytes));
    }
    fn flip(mem: &SharedMemory, address: u64) { let word = crate::read_u32(mem, address).unwrap(); assert!(crate::write_u32(mem, address, word ^ 1)); }
    /// Parent viewport, its zero-translation clone at frame+128 (944 bytes,
    /// ending at the model at frame+1072) and the corrected model.
    fn seam_fixture(view: &WorldView) -> SharedMemory {
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true).shared_memory(true).wasm_memory64(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let mem = SharedMemory::new(&engine, wasmtime::MemoryTypeBuilder::new().min(2).max(Some(2)).memory64(true).shared(true).build().unwrap()).unwrap();
        let (camera, projection, scalars) = (bits(&view.game_from_eye), bits(&view.projection_from_eye), [3u32; 11]);
        let mut zeroed = camera;
        zeroed[12] = 0; zeroed[13] = 0; zeroed[14] = 0;
        for (viewport, camera) in [(PARENT, camera), (FRAME + CLONE, zeroed)] {
            put(&mem, viewport + CAMERA, &camera);
            put(&mem, viewport + PROJECTION, &projection);
            put(&mem, viewport + SCALARS, &scalars);
            assert!(crate::write_bytes(&mem, viewport + SCALAR_STATE, &[1]) && crate::write_bytes(&mem, viewport + REVERSE_Z, &[1]));
        }
        put(&mem, FRAME + MODEL, &(0..16).map(|i| (i as f32 * 0.125).to_bits()).collect::<Vec<_>>());
        mem
    }
    fn displaced(view: WorldView) {
        with_producer(|producer| { *producer = logic::Producer::new(); producer.note_current(5, PARENT, Some(view)); producer.note_current(5, FRAME + CLONE, None); });
    }
    fn bind(mem: &SharedMemory, parent: u64, family: u32) -> Result<logic::Binding<WorldView>, &'static str> {
        displaced(eye());
        bind_checked(mem, 77, 88, family, FRAME, parent, &eye(), 5, |epoch| epoch == 5)
    }
    fn bound() -> bool { with_producer(|producer| producer.active().is_some()).unwrap() }

    #[test]
    fn seam_binding_certifies_actual_displaced_parent_memory_and_rejects_every_mismatch() {
        let view = eye();
        let mem = seam_fixture(&view);
        let binding = bind(&mem, PARENT, logic::STREAM_PED).unwrap();
        assert!(binding.generation > 0 && binding.clone == FRAME + CLONE && binding.parent == PARENT && binding.view == view && bound());
        // Live invocation (guest stack at or below its frame) with unchanged clone/model.
        assert_eq!(certified_memory(&mem, &binding, Some(FRAME - 256)), Ok(()));
        assert_eq!(certified_memory(&mem, &binding, Some(FRAME)), Ok(()));
        assert_eq!(certified_memory(&mem, &binding, Some(FRAME + 16)), Err("invocation_frame_unwound"));
        assert_eq!(certified_memory(&mem, &binding, None), Err("stack_pointer_unavailable"));
        for (address, expected) in [(FRAME + CLONE + PROJECTION + 40, "clone_projection_changed"),
            (FRAME + CLONE + CAMERA + 4, "clone_camera_changed"), (FRAME + MODEL + 52, "corrected_model_changed")] {
            let changed = seam_fixture(&view);
            flip(&changed, address);
            assert_eq!(certified_memory(&changed, &binding, Some(FRAME - 8)), Err(expected));
        }
        let padding = seam_fixture(&view);
        assert!(crate::write_u32(&padding, FRAME + CLONE + CAMERA + 12, 0xdead_beef));
        assert_eq!(certified_memory(&padding, &binding, Some(FRAME)), Ok(()), "Mat34 padding is not raster state");
        // Seam-time rejections leave no binding.
        assert_eq!(bind(&mem, PARENT + 8, logic::STREAM_PED).unwrap_err(), "parent_is_not_displaced_viewport");
        assert_eq!(bind(&mem, PARENT, 44395).unwrap_err(), "family_not_completed_model_seam");
        for (address, expected) in [(FRAME + CLONE + PROJECTION + 56, "clone_projection_differs_from_parent"),
            (PARENT + PROJECTION + 40, "parent_projection_is_not_displaced_view"), (PARENT + CAMERA + 48, "parent_camera_is_not_displaced_view"),
            (FRAME + CLONE + CAMERA + 52, "clone_is_not_zeroed_parent_basis"), (FRAME + CLONE + SCALARS + 8, "clone_projection_state_differs_from_parent")] {
            let changed = seam_fixture(&view);
            flip(&changed, address);
            assert_eq!(bind(&changed, PARENT, logic::SKINNED).unwrap_err(), expected);
            assert!(!bound());
        }
        let reverse = seam_fixture(&view);
        assert!(crate::write_bytes(&reverse, FRAME + CLONE + REVERSE_Z, &[0]));
        assert_eq!(bind(&reverse, PARENT, logic::SKINNED).unwrap_err(), "clone_projection_state_differs_from_parent");
        displaced(view);
        assert_eq!(bind_checked(&mem, 77, 88, logic::SKINNED, FRAME, PARENT, &view, 5, |_| false).unwrap_err(), "displaced_view_is_not_current_headset_eye");
        assert_eq!(bind_checked(&mem, 77, 88, logic::SKINNED, FRAME, PARENT, &WorldView { camera_sequence: 42, ..view }, 5, |_| true).unwrap_err(),
            "displaced_view_is_not_corrected_eye");
        assert_eq!(bind_checked(&mem, 0, 88, logic::SKINNED, FRAME, PARENT, &view, 5, |_| true).unwrap_err(), "unknown_store_or_pthread");
        // The clone is not the current viewport (another SetCurrent intervened).
        with_producer(|producer| producer.note_current(5, PARENT + 512, None));
        assert_eq!(bind_checked(&mem, 77, 88, logic::SKINNED, FRAME, PARENT, &view, 5, |_| true).unwrap_err(), "clone_is_not_current_viewport");
        assert!(!bound());
    }

    fn install(mem: &SharedMemory) { bind(mem, PARENT, logic::STREAM_PED).unwrap(); assert!(bound()); }

    #[test]
    fn guest_call_boundaries_revoke_unrestored_bindings_before_the_caller_recovers() {
        use crate::guest_call_scope::{call, Kind};
        let _force = Force::new();
        let mem = seam_fixture(&eye());
        let mut store = wasmtime::Store::new(&wasmtime::Engine::default(), crate::HostState::new(88));
        // LongJmp out of the call that opened the binding: revoked when call()
        // returns, before any stack_restore/setThrew recovery could run.
        assert!(call(&mut store, Kind::Ordinary, |_| { install(&mem); Err::<(), _>(crate::host::LongJmp) }).is_err());
        assert!(!bound());
        // Normal return without the original SetCurrent(parent): revoked.
        call(&mut store, Kind::Ordinary, |_| { install(&mem); Ok::<_, ()>(()) }).unwrap();
        assert!(!bound());
        // Balanced restoration inside the call leaves nothing to revoke.
        call(&mut store, Kind::Ordinary, |_| { install(&mem); end_at_set_current(PARENT); assert!(!bound()); Ok::<_, ()>(()) }).unwrap();
        // An outer binding survives nested calls, is suspended for mailbox work,
        // and any other SetCurrent ends it.
        call(&mut store, Kind::Ordinary, |s| {
            install(&mem);
            call(s, Kind::Ordinary, |_| Ok::<_, ()>(())).unwrap();
            assert!(bound());
            assert!(call(s, Kind::Ordinary, |_| Err::<(), _>(crate::host::LongJmp)).is_err());
            assert!(bound());
            call(s, Kind::Mailbox, |s| {
                assert!(with_producer(|producer| producer.suspended()).unwrap());
                call(s, Kind::Ordinary, |_| { assert!(with_producer(|producer| producer.suspended()).unwrap()); Ok::<_, ()>(()) })
            }).unwrap();
            assert!(!with_producer(|producer| producer.suspended()).unwrap() && bound());
            end_at_set_current(PARENT + 64);
            assert!(!bound());
            Ok::<_, ()>(())
        }).unwrap();
        // Host panic unwinding through the call that opened the binding.
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            call(&mut store, Kind::Ordinary, |_| -> Result<(), ()> { install(&mem); panic!("synthetic host panic") })));
        assert!(panic.is_err());
        assert!(!bound());
    }

    #[cfg(feature = "jit")]
    #[test]
    fn actual_wasm_trap_after_the_seam_revokes_before_the_boundary_returns() {
        use wasm_encoder::{CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction as I, TypeSection};
        let _force = Force::new();
        let mem = seam_fixture(&eye());
        let mut module = wasm_encoder::Module::new();
        let mut types = TypeSection::new();
        types.ty().function([], []);
        module.section(&types);
        let mut imports = ImportSection::new();
        imports.import("host", "seam", EntityType::Function(0));
        module.section(&imports);
        let mut functions = FunctionSection::new();
        functions.function(0);
        module.section(&functions);
        let mut exports = ExportSection::new();
        exports.export("draw", ExportKind::Func, 1);
        module.section(&exports);
        let mut code = CodeSection::new();
        let mut draw = Function::new([]);
        for op in [I::Call(0), I::Unreachable, I::End] { draw.instruction(&op); }
        code.function(&draw);
        module.section(&code);
        let engine = wasmtime::Engine::default();
        let compiled = wasmtime::Module::new(&engine, module.finish()).unwrap();
        let mut store = wasmtime::Store::new(&engine, crate::HostState::new(88));
        let mut linker = wasmtime::Linker::new(&engine);
        let seam_memory = mem.clone();
        linker.func_wrap("host", "seam", move |_: wasmtime::Caller<'_, crate::HostState>| install(&seam_memory)).unwrap();
        let instance = linker.instantiate(&mut store, &compiled).unwrap();
        let draw = instance.get_typed_func::<(), ()>(&mut store, "draw").unwrap();
        let result = crate::guest_call_scope::call(&mut store, crate::guest_call_scope::Kind::Ordinary, |s| draw.call(s, ()));
        assert!(result.is_err());
        assert!(!bound(), "the trapped invocation's attribution never reaches later emissions");
    }

    #[test]
    fn merge_shader_parses_and_validates_with_the_naga_version_wgpu_uses() {
        use wgpu::naga;
        let module = naga::front::wgsl::parse_str(logic::MERGE_WGSL).expect("merge WGSL parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("merge WGSL validates");
        let stages: Vec<_> = module.entry_points.iter().map(|entry| (entry.name.as_str(), entry.stage)).collect();
        assert_eq!(stages, [("vs", naga::ShaderStage::Vertex), ("fs", naga::ShaderStage::Fragment)]);
        let bindings: Vec<_> = module.global_variables.iter().filter_map(|(_, global)| global.binding.as_ref().map(|b| (b.group, b.binding))).collect();
        assert_eq!(bindings, [(0, 0), (0, 1), (0, 2)]);
    }

    #[test]
    fn same_raster_requires_bit_identical_view_including_depth_row() {
        let view = eye();
        assert!(same_raster(&view, &view));
        let mut padding = view; padding.game_from_eye[3] = 9.;
        assert!(same_raster(&view, &padding), "Mat34 padding is not raster state");
        let changes: [fn(&mut WorldView); 7] = [|v| v.projection_from_eye[10] = f32::from_bits(v.projection_from_eye[10].to_bits() + 1),
            |v| v.projection_from_eye[14] = -v.projection_from_eye[14], |v| v.depth_reverse_z = false, |v| v.camera_sequence += 1,
            |v| v.game_from_eye[12] += 0.001, |v| v.headset_applied = false, |v| v.eye_index = 0];
        for change in changes {
            let mut other = view;
            change(&mut other);
            assert!(!same_raster(&view, &other) && !same_raster(&other, &view));
        }
    }
}

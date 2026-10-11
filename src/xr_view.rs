//! Qualified world-camera hooks and immutable GPU-ring provenance.
//!
//! Layouts below are certified against the supplied 11ca8d2c… game module by
//! patch5's function-body fingerprints. Neither shadows nor arbitrary viewport
//! setters receive a headset override. Views cross to the GPU worker at the
//! command publication boundary, while the guest's producer mutex is held.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use anyhow::{Context, Result};
use wasmtime::{Caller, SharedMemory};
use crate::{HostState, RT};

const ACTIVE_CVIEWPORT: u64 = 19_237_856;
const CAMERA_OFFSET: u64 = 192;
const PROJECTION_OFFSET: u64 = 320;
const HISTORY: usize = 96;
const MAX_MARKERS: usize = 4096;
const MAX_PRESENTS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SVXREyeView {
    pub head_from_eye: [f32; 16],
    pub projection_from_eye: [f32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SVXRViewRequest {
    pub version: u32,
    pub size: u32,
    pub frame: u64,
    pub presentation_time_seconds: f64,
    pub view_count: u32,
    pub reserved: u32,
    pub views: [SVXREyeView; 2],
}

const _: () = assert!(std::mem::size_of::<SVXREyeView>() == 128);
const _: () = assert!(std::mem::size_of::<SVXRViewRequest>() == 288);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldView {
    pub provenance_epoch: u64,
    pub camera_sequence: u64,
    pub request_frame: u64,
    pub head_pose_frame: u64,
    pub presentation_time_seconds: f64,
    pub eye_index: u32,
    pub view_count: u32,
    pub game_from_eye: [f32; 16],
    pub game_from_stage: [f32; 16],
    pub projection_from_eye: [f32; 16],
    pub depth_reverse_z: bool,
    pub headset_applied: bool,
}

/// A scene epoch may only be minted at a certified render-only guest boundary
/// after simulation has been frozen for both eye renders. There is deliberately
/// no constructor in the current natural-frame path: alternating camera updates
/// cannot claim a coherent pair, even if request/head counters happen to match.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CertifiedPairEpoch {
    scene_epoch: u64,
    provenance_epoch: u64,
}

impl CertifiedPairEpoch {
    pub(crate) fn scene_epoch(self) -> u64 { self.scene_epoch }
    pub(crate) fn provenance_epoch(self) -> u64 { self.provenance_epoch }
}

#[cfg(test)]
pub(crate) fn fixture_certified_pair_epoch(scene_epoch: u64, provenance_epoch: u64) -> CertifiedPairEpoch {
    CertifiedPairEpoch { scene_epoch, provenance_epoch }
}

#[derive(Clone, Copy)]
struct SavedViewport {
    pointer: u64, source: u64, view: WorldView,
    base_camera: [f32; 16], base_projection: [f32; 16],
}

#[derive(Clone, Copy)]
struct UnmatchedViewport {
    pointer: u64, camera: [f32; 16], projection: [f32; 16], closest: SavedViewport,
    camera_error: f32, base_camera_error: f32, projection_error: f32, base_projection_error: f32,
}

#[derive(Default)]
struct RenderThread {
    epoch: u64,
    current: Option<WorldView>,
}

// Only an opt-in proof run retains the request actually used by a successful
// camera update. Keeping the consumed WorldView avoids consulting a newer
// request when the GPU thread eventually reaches that draw.
#[derive(Clone, Copy)]
struct ConsumedRequest { view: WorldView, request: SVXRViewRequest }
const MAX_CONSUMED_REQUESTS: usize = 96;
static CAPTURE_CONSUMED_REQUESTS: LazyLock<bool> = LazyLock::new(||
    std::env::var("MUGUET_GEOMETRY_STEREO_CAPTURE").as_deref() == Ok("1")
    || std::env::var("MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT").as_deref() == Ok("1"));

fn push_consumed_request(state: &mut State, view: WorldView, request: SVXRViewRequest) {
    state.consumed_requests.push_back(ConsumedRequest { view, request });
    while state.consumed_requests.len() > MAX_CONSUMED_REQUESTS {
        state.consumed_requests.pop_front();
    }
}

fn remember_consumed_request(state: &mut State, view: WorldView, request: SVXRViewRequest) {
    if !*CAPTURE_CONSUMED_REQUESTS || !view.headset_applied || view.view_count != 2
        || request.view_count != 2 || view.request_frame != request.frame { return; }
    push_consumed_request(state, view, request);
}

fn find_consumed_request(state: &State, view: &WorldView) -> Option<SVXRViewRequest> {
    state.consumed_requests.iter().rev().find(|entry|
        entry.view.provenance_epoch == view.provenance_epoch
        && entry.view.request_frame == view.request_frame
        && entry.view.presentation_time_seconds.to_bits() == view.presentation_time_seconds.to_bits()
        && entry.view.head_pose_frame == view.head_pose_frame
        && entry.view.camera_sequence == view.camera_sequence
        && entry.view.eye_index == view.eye_index
        && entry.view == *view).map(|entry| entry.request)
}

fn same_matrix_bits(a: &[f32; 16], b: &[f32; 16]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

/// The opaque reverse-Z clone is derived from a successful forward-Z primary.
/// A raw request may follow it only when the exact recorded primary view and
/// the requested eye projection explain the entire derived command view.
fn remember_derived_consumed_request(state: &mut State, primary: WorldView, derived: WorldView) -> bool {
    if !primary.headset_applied || primary.depth_reverse_z || !derived.headset_applied
        || !derived.depth_reverse_z || primary.view_count != 2 || derived.view_count != 2
        || primary.eye_index >= 2 { return false; }
    let Some(request) = find_consumed_request(state, &primary) else { return false; };
    if validate_request(&request).is_err() || request.frame != primary.request_frame
        || request.presentation_time_seconds.to_bits() != primary.presentation_time_seconds.to_bits() {
        return false;
    }
    let requested_projection = request.views[primary.eye_index as usize].projection_from_eye;
    if reverse_z(&requested_projection) != Some(true)
        || !same_matrix_bits(&primary.projection_from_eye, &for_game_depth(requested_projection, false)) {
        return false;
    }
    let mut expected = primary;
    // The certified clone flips the already converted primary depth row.
    // An f32 double flip can differ by a few ULPs from the raw request, so
    // compare the exact operation the guest command actually received.
    expected.projection_from_eye = flip_projection_depth(primary.projection_from_eye);
    expected.depth_reverse_z = true;
    if derived != expected
        || derived.presentation_time_seconds.to_bits() != expected.presentation_time_seconds.to_bits()
        || !same_matrix_bits(&derived.projection_from_eye, &expected.projection_from_eye)
        || !same_matrix_bits(&derived.game_from_eye, &expected.game_from_eye)
        || !same_matrix_bits(&derived.game_from_stage, &expected.game_from_stage) {
        return false;
    }
    if let Some(existing) = find_consumed_request(state, &derived) {
        if request_bytes(existing) == request_bytes(request) { return true; }
        // A conflicting receipt for the same complete command view must not
        // survive as an apparently exact GPU lookup.
        state.consumed_requests.retain(|entry| entry.view != derived);
        return false;
    }
    push_consumed_request(state, derived, request);
    true
}

fn request_bytes(request: SVXRViewRequest) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(288);
    bytes.extend_from_slice(&request.version.to_le_bytes());
    bytes.extend_from_slice(&request.size.to_le_bytes());
    bytes.extend_from_slice(&request.frame.to_le_bytes());
    bytes.extend_from_slice(&request.presentation_time_seconds.to_le_bytes());
    bytes.extend_from_slice(&request.view_count.to_le_bytes());
    bytes.extend_from_slice(&request.reserved.to_le_bytes());
    for eye in request.views {
        for value in eye.head_from_eye.into_iter().chain(eye.projection_from_eye) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    debug_assert_eq!(bytes.len(), 288);
    bytes
}

pub(crate) fn consumed_request_receipt(view: &WorldView) -> Option<serde_json::Value> {
    if !*CAPTURE_CONSUMED_REQUESTS || !view.headset_applied || view.view_count != 2 { return None; }
    let request = find_consumed_request(&lock_world_state(), view)?;
    if request.frame != view.request_frame || request.view_count != 2 { return None; }
    let bytes = request_bytes(request);
    Some(serde_json::json!({"schema":1,"source":"sv_xr_submit_views",
        "request_bytes_hex":bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "provenance_epoch":view.provenance_epoch,"head_pose_frame":view.head_pose_frame}))
}

/// Diagnostic proof for an acquired, GPU-complete world lease. Match the
/// retained request actually consumed at the camera hook, not the newest
/// submitted request. This is enabled only for an explicit verification run.
pub fn consumed_world_frame_receipt(frame: &crate::world_frame::WorldFrame) -> Option<serde_json::Value> {
    if !*CAPTURE_CONSUMED_REQUESTS { return None; }
    let (view, request) = {
        let state = lock_world_state();
        match_world_frame_receipt(&state, frame)?
    };
    if !view_epoch_is_active(view.provenance_epoch) { return None; }
    let bytes = request_bytes(request);
    Some(serde_json::json!({"schema":1,"source":"consumed_world_frame_receipt",
        "request_bytes_hex":bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "provenance_epoch":view.provenance_epoch,"camera_sequence":view.camera_sequence,
        "headset_applied":view.headset_applied,"depth_reverse_z":view.depth_reverse_z,
        "frame_abi_version":frame.abi_version,"frame_byte_size":frame.byte_size,
        "frame_lease":frame.lease,"frame_depth_convention":frame.depth_convention,
        "game_frame":frame.game_frame,"request_frame":frame.request_frame,
        "head_frame":frame.head_frame,"presentation_time":frame.presentation_time,
        "view_index":frame.view_index,"view_count":frame.view_count,
        "projection_from_eye":frame.projection_from_eye,
        "game_from_eye":frame.game_from_eye,"game_from_stage":frame.game_from_stage}))
}

fn match_world_frame_receipt(state: &State, frame: &crate::world_frame::WorldFrame)
    -> Option<(WorldView, SVXRViewRequest)> {
    if frame.abi_version != 1
        || frame.byte_size as usize != std::mem::size_of::<crate::world_frame::WorldFrame>()
        || frame.lease == 0 || frame.color_texture == 0 || frame.depth_texture == 0
        || frame.depth_convention != 0 || frame.reserved != 0 { return None; }
    let matches: Vec<_> = state.consumed_requests.iter().filter(|entry| {
            let view = &entry.view;
            view.headset_applied && view.view_count == 2 && frame.view_count == 2
                && frame.view_index == view.eye_index
                && frame.request_frame == view.request_frame
                && frame.head_frame == view.head_pose_frame
                && frame.presentation_time.to_bits() == view.presentation_time_seconds.to_bits()
                && same_matrix_bits(&frame.projection_from_eye, &view.projection_from_eye)
                && same_matrix_bits(&frame.game_from_eye, &view.game_from_eye)
                && same_matrix_bits(&frame.game_from_stage, &view.game_from_stage)
                && entry.request.version == 1 && entry.request.size == 288
                && entry.request.frame == frame.request_frame
                && entry.request.presentation_time_seconds.to_bits() == frame.presentation_time.to_bits()
                && entry.request.view_count == frame.view_count
        }).collect();
    if matches.len() != 1 { return None; }
    Some((matches[0].view, matches[0].request))
}

thread_local! {
    static RENDER_THREAD: RefCell<RenderThread> = RefCell::new(RenderThread::default());
}

#[derive(Default)]
struct State {
    request: Option<(SVXRViewRequest, Instant)>,
    consumed_requests: VecDeque<ConsumedRequest>,
    gameplay_ready_at: Option<Instant>,
    history: VecDeque<SavedViewport>,
    markers: BTreeMap<u64, Option<WorldView>>,
    last_emitted: Option<WorldView>,
    frame_world: Option<WorldView>,
    frame_conflict: bool,
    presents: VecDeque<Option<WorldView>>,
    correlation_failed: bool,
    camera_calls: u64,
    main_camera_calls: u64,
    qualified_camera_calls: u64,
    overrides_applied: u64,
    overrides_failed: u64,
    scene_calls: u64,
    main_scene_calls: u64,
    viewport_commands: u64,
    current_calls: u64,
    matched_current_calls: u64,
    projection_clone_updates: u64,
    projection_clones_already_current: u64,
    projection_clone_failures: u64,
    original_primary_projection_scalars: Option<[f32; 11]>,
    unmatched_near_world: VecDeque<UnmatchedViewport>,
    emitted_commands: u64,
    emitted_presents: u64,
    consumed_presents: u64,
    world_presents: u64,
    conflicting_presents: u64,
    marker_changes: u64,
    sequence: u64,
    last_main: Option<SavedViewport>,
    last_current: Option<WorldView>,
    last_error: Option<String>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static ENABLED: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU64 = AtomicU64::new(1);
static MARKER_COUNT: AtomicU64 = AtomicU64::new(0);
static TRACE_ENABLED: AtomicBool = AtomicBool::new(cfg!(target_os = "visionos"));

fn lock_world_state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn world_camera_enabled() -> bool { ENABLED.load(Ordering::Acquire) && crate::xr::is_active() }

/// Pure atomic check, safe under the frame publisher's lock. The native C frame
/// ABI remains unchanged; this token only guards publication across sessions.
pub fn view_epoch_is_active(epoch: u64) -> bool {
    ENABLED.load(Ordering::Acquire) && epoch == EPOCH.load(Ordering::Acquire)
}

pub(crate) fn view_epoch() -> u64 { EPOCH.load(Ordering::Acquire) }

pub fn set_active(_active: bool) {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    let mut state = lock_world_state();
    invalidate_state(&mut state);
}

fn invalidate_state(state: &mut State) {
    state.request = None;
    state.consumed_requests.clear();
    state.gameplay_ready_at = None;
    state.history.clear();
    state.last_main = None;
    state.last_current = None;
    state.original_primary_projection_scalars = None;
    state.unmatched_near_world.clear();
    state.last_emitted = None;
    state.frame_world = None;
    state.frame_conflict = false;
    // Preserve queue positions for commands already published; convert their
    // payloads to nonworld rather than shifting the presentation correspondence.
    for view in state.markers.values_mut() { *view = None; }
    for view in state.presents.iter_mut() { *view = None; }
}

/// Enables the direct qualified world-camera path; poses alone never enable it.
#[no_mangle]
pub extern "C" fn sv_xr_world_camera_set_enabled(enabled: u32) -> i32 {
    if enabled > 1 { return -1; }
    // Keep the direct path closed until both provenance and completed-frame
    // generations have been invalidated. No STATE lock spans the frame reset.
    ENABLED.store(false, Ordering::Release);
    set_active(false);
    crate::world_frame::reset();
    if enabled != 0 { TRACE_ENABLED.store(true, Ordering::Release); }
    ENABLED.store(enabled != 0, Ordering::Release);
    0
}

fn identity() -> [f32; 16] {
    [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.]
}

pub(crate) fn rigid(m: &[f32; 16]) -> bool {
    if m.iter().any(|value| !value.is_finite()) || m[3].abs() > 1e-4 || m[7].abs() > 1e-4
        || m[11].abs() > 1e-4 || (m[15] - 1.).abs() > 1e-4 { return false; }
    for a in 0..3 { for b in 0..3 {
        let dot: f32 = (0..3).map(|r| m[a * 4 + r] * m[b * 4 + r]).sum();
        if (dot - if a == b { 1. } else { 0. }).abs() > 0.03 { return false; }
    }}
    let det = m[0] * (m[5] * m[10] - m[9] * m[6])
        - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2]);
    (0.97..=1.03).contains(&det)
}

fn validate_request(request: &SVXRViewRequest) -> Result<(), i32> {
    if request.version != 1 || request.size != 288 || request.reserved != 0
        || !(1..=2).contains(&request.view_count) { return Err(-2); }
    if !request.presentation_time_seconds.is_finite() || request.presentation_time_seconds < 0. { return Err(-3); }
    for view in request.views.iter().take(request.view_count as usize) {
        let p = &view.projection_from_eye;
        if !rigid(&view.head_from_eye) || view.head_from_eye[12..15].iter().any(|x| x.abs() > 0.25)
            || p.iter().any(|x| !x.is_finite()) || p[0] <= 0. || p[5] <= 0.
            || p[3].abs() > 1e-5 || p[7].abs() > 1e-5 || p[15].abs() > 1e-5
            || (p[11] + 1.).abs() > 1e-4 || p[10] < 0. || p[14] <= 0.
            || projection_scalars(p).is_none()
        { return Err(-3); }
    }
    Ok(())
}

/// Copies compositor eye geometry. The native game thread consumes this later.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_submit_views(request: *const SVXRViewRequest) -> i32 {
    if request.is_null() { return -1; }
    let request = unsafe { request.read_unaligned() };
    if let Err(error) = validate_request(&request) { return error; }
    let mut state = lock_world_state();
    if state.request.is_some_and(|(previous, _)| request.frame <= previous.frame) { return -4; }
    state.request = Some((request, Instant::now()));
    0
}

/// Called from the selected script's ready player/gameplay-camera check.
pub fn set_gameplay_camera(position: Option<[f32; 3]>) {
    if position.is_some() { TRACE_ENABLED.store(true, Ordering::Release); }
    lock_world_state().gameplay_ready_at = position.filter(|p| p.iter().all(|v| v.is_finite())).map(|_| Instant::now());
}

fn read_u64(mem: &SharedMemory, address: u64) -> Option<u64> {
    address.checked_add(8).filter(|end| *end <= mem.data_size() as u64)?;
    crate::read_u64(mem, address)
}

fn read_u32(mem: &SharedMemory, address: u64) -> Option<u32> {
    address.checked_add(4).filter(|end| *end <= mem.data_size() as u64)?;
    crate::read_u32(mem, address)
}

fn read_u8(mem: &SharedMemory, address: u64) -> Option<u8> {
    address.checked_add(1).filter(|end| *end <= mem.data_size() as u64)?;
    crate::read_bytes(mem,address,1).first().copied()
}

fn read_matrix(mem: &SharedMemory, address: u64) -> Option<[f32; 16]> {
    address.checked_add(64).filter(|end| *end <= mem.data_size() as u64)?;
    let bytes = crate::read_bytes(mem, address, 64);
    let mut values = [0.; 16];
    for (value, bytes) in values.iter_mut().zip(bytes.chunks_exact(4)) {
        *value = f32::from_le_bytes(bytes.try_into().ok()?);
    }
    values.iter().all(|value| value.is_finite()).then_some(values)
}

fn matrix_bytes(matrix: &[f32; 16]) -> [u8; 64] {
    let mut bytes = [0; 64];
    for (dst, value) in bytes.chunks_exact_mut(4).zip(matrix) { dst.copy_from_slice(&value.to_le_bytes()); }
    bytes
}

fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    std::array::from_fn(|index| {
        let (column, row) = (index / 4, index % 4);
        (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum()
    })
}

fn rigid_inverse(matrix: &[f32; 16]) -> [f32; 16] {
    let mut inverse = identity();
    for row in 0..3 { for column in 0..3 { inverse[column * 4 + row] = matrix[row * 4 + column]; } }
    for row in 0..3 { inverse[12 + row] = -(0..3).map(|k| inverse[k * 4 + row] * matrix[12 + k]).sum::<f32>(); }
    inverse
}

#[cfg(test)]
fn native_head(update: crate::xr::CameraUpdate) -> Option<[f32; 16]> {
    if update.relative_position_meters.iter().any(|n| !n.is_finite() || n.abs() > 5.) { return None; }
    let (sy, cy) = update.heading_degrees.to_radians().sin_cos();
    let (sp, cp) = update.pitch_degrees.to_radians().sin_cos();
    let (sr, cr) = update.roll_degrees.to_radians().sin_cos();
    let yaw = [cy,0.,-sy,0., 0.,1.,0.,0., sy,0.,cy,0., 0.,0.,0.,1.];
    let pitch = [1.,0.,0.,0., 0.,cp,sp,0., 0.,-sp,cp,0., 0.,0.,0.,1.];
    let roll = [cr,sr,0.,0., -sr,cr,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.];
    let mut head = multiply(&multiply(&yaw, &pitch), &roll);
    head[12..15].copy_from_slice(&update.relative_position_meters);
    rigid(&head).then_some(head)
}

fn reverse_z(projection: &[f32; 16]) -> Option<bool> {
    if projection[11] >= -0.9 || projection[15].abs() > 1e-4 { return None; }
    if projection[10] >= 0. && projection[14] > 0. { Some(true) }
    else if projection[10] < 0. && projection[14] < 0. { Some(false) }
    else { None }
}

fn for_game_depth(mut native: [f32; 16], game_reverse_z: bool) -> [f32; 16] {
    if !game_reverse_z { for col in 0..4 { native[col * 4 + 2] = native[col * 4 + 3] - native[col * 4 + 2]; } }
    native
}

fn flip_projection_depth(matrix: [f32; 16]) -> [f32; 16] { for_game_depth(matrix,false) }

/// Certified by Perspective/RegeneratePerspective and projection shader params:
/// near, far, crop X/Y, offset X/Y, FOV Y degrees, tan-half Y/X, aspect twice.
/// These shader scalars are not regenerated by the engine's custom SetProjection.
fn projection_scalars(p: &[f32; 16]) -> Option<[f32; 11]> {
    if p.iter().any(|v| !v.is_finite()) || p[0] <= 0. || p[5] <= 0.
        || (p[11] + 1.).abs() > 1e-5
        || [1,2,3,4,6,7,12,13,15].into_iter().any(|i| p[i].abs() > 1e-7) { return None; }
    let reverse = reverse_z(p)?;
    let z = p[10] as f64;
    let (near_denominator, far_denominator) = if reverse { (z + 1., z) } else { (z, z + 1.) };
    let near = (p[14] as f64 / near_denominator) as f32;
    let far = (p[14] as f64 / far_denominator) as f32;
    // Native requests use a finite 1km far plane. Allow its f32 reconstruction
    // error, while excluding infinite/oblique projections from these shaders.
    if !near.is_finite() || !far.is_finite() || near <= 0. || far <= near || far > 1002. { return None; }
    let tan_y = 1. / p[5];
    let tan_x = 1. / p[0];
    let aspect = p[5] / p[0];
    let fields = [near, far, 1., 1., p[8], p[9], 2. * tan_y.atan().to_degrees(), tan_y, tan_x, aspect, aspect];
    fields.iter().all(|v| v.is_finite()).then_some(fields)
}

fn read_projection_scalars(mem: &SharedMemory, viewport: u64) -> Option<[f32; 11]> {
    let address = viewport.checked_add(872)?;
    address.checked_add(44).filter(|end| *end <= mem.data_size() as u64)?;
    let bytes = crate::read_bytes(mem,address,44);
    let mut values = [0.;11];
    for (value,bytes) in values.iter_mut().zip(bytes.chunks_exact(4)) {
        *value = f32::from_le_bytes(bytes.try_into().ok()?);
    }
    values.iter().all(|v| v.is_finite()).then_some(values)
}

fn projection_state_is_current(mem: &SharedMemory, viewport: u64, projection: &[f32;16]) -> bool {
    let Some(expected) = projection_scalars(projection) else { return false; };
    read_projection_scalars(mem,viewport) == Some(expected)
        && read_u8(mem,viewport.saturating_add(916)) == Some(1)
        && read_u8(mem,viewport.saturating_add(928)) == reverse_z(projection).map(u8::from)
}

fn update_viewport(caller: &mut Caller<'_, HostState>, viewport: u64, camera: Option<&[f32; 16]>, projection: Option<&[f32; 16]>) -> Result<()> {
    let scalars = projection.map(|p| projection_scalars(p).context("unsupported finite eye projection")).transpose()?;
    let stack_get = caller.get_export("emscripten_stack_get_current").and_then(|e| e.into_func()).context("missing stack getter")?
        .typed::<(), i64>(&mut *caller)?;
    let allocate = caller.get_export("_emscripten_stack_alloc").and_then(|e| e.into_func()).context("missing stack allocator")?
        .typed::<i64, i64>(&mut *caller)?;
    let restore = caller.get_export("_emscripten_stack_restore").and_then(|e| e.into_func()).context("missing stack restore")?
        .typed::<i64, ()>(&mut *caller)?;
    let set_camera = caller.get_export("mp:XRSetViewportCameraMtx").and_then(|e| e.into_func()).context("missing camera setter")?
        .typed::<(i64, i64), ()>(&mut *caller)?;
    let set_projection = caller.get_export("mp:XRSetViewportProjection").and_then(|e| e.into_func()).context("missing projection setter")?
        .typed::<(i64, i64), ()>(&mut *caller)?;
    let stack = stack_get.call(&mut *caller, ())?;
    let scratch = allocate.call(&mut *caller, 128)? as u64;
    let result = (|| {
        let mem = &RT.get().context("missing runtime")?.mem;
        anyhow::ensure!(scratch != 0 && scratch.checked_add(128).is_some_and(|end| end <= mem.data_size() as u64), "camera scratch outside guest memory");
        if scalars.is_some() {
            anyhow::ensure!(viewport.checked_add(929).is_some_and(|end| end <= mem.data_size() as u64), "projection scalar fields outside guest memory");
        }
        if let Some(camera) = camera { anyhow::ensure!(crate::write_bytes(mem, scratch, &matrix_bytes(camera)), "camera scratch write failed"); }
        if let Some(projection) = projection {
            anyhow::ensure!(crate::write_bytes(mem, scratch + 64, &matrix_bytes(projection)), "projection scratch write failed");
        }
        // Setters regenerate the inverse, dependent matrices and frustum planes.
        if camera.is_some() { set_camera.call(&mut *caller, (viewport as i64, scratch as i64))?; }
        if let (Some(projection),Some(fields)) = (projection,scalars) {
            let bytes: Vec<u8> = fields.into_iter().flat_map(f32::to_le_bytes).collect();
            anyhow::ensure!(crate::write_bytes(mem,viewport + 872,&bytes), "projection scalar write failed");
            anyhow::ensure!(crate::write_bytes(mem,viewport + 928,&[reverse_z(projection).context("unsupported depth direction")? as u8]), "projection direction write failed");
            // Calling RegenerateNearFarClip afterward would replace the custom
            // matrix. SetProjection instead regenerates its inverses/frustums.
            set_projection.call(&mut *caller, (viewport as i64, (scratch + 64) as i64))?;
        }
        Ok(())
    })();
    let restored = restore.call(&mut *caller, stack);
    result.and(restored.map_err(Into::into))
}

/// Post-hook receives original parameter values saved by patch5, after camera
/// propagation has completed. The main source identity is a dynamic guest pointer.
pub fn camera_post(caller: &mut Caller<'_, HostState>, source: u64, _frame: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let active_source = read_u64(mem, ACTIVE_CVIEWPORT).unwrap_or(0);
    let mut state = lock_world_state();
    let provenance_epoch = EPOCH.load(Ordering::Acquire);
    state.camera_calls += 1;
    if source == 0 || source != active_source { return Ok(()); }
    state.main_camera_calls += 1;
    let Some(viewport) = source.checked_add(16) else { return Ok(()); };
    let Some(camera) = read_matrix(mem, viewport + CAMERA_OFFSET).filter(rigid) else { return Ok(()); };
    let Some(projection) = read_matrix(mem, viewport + PROJECTION_OFFSET) else { return Ok(()); };
    state.original_primary_projection_scalars = read_projection_scalars(mem,viewport);
    let depth_reverse_z = reverse_z(&projection).unwrap_or(false);
    // Active primary camera propagation is certified independently of the
    // gameplay director. Cinematics and third-person cameras can be far from
    // the player; preserve their native camera and add the reference-relative
    // head transform without requiring a gameplay-position match.
    let ready = crate::xr::is_active() && reverse_z(&projection).is_some();
    if ready { state.qualified_camera_calls += 1; }
    state.sequence += 1;
    let request = state.request.filter(|(_, time)| time.elapsed() < Duration::from_millis(500)).map(|(request, _)| request);
    // The certified camManager path propagates its active camera once per normal
    // update (its two callsites are mutually exclusive tiled/normal alternatives).
    // Alternate on those natural camera updates, independently of GPU lag.
    let eye_index = if request.is_some_and(|request| request.view_count == 2) { ((state.sequence - 1) & 1) as u32 } else { 0 };
    // Prepare one exact pose at the active camera propagation boundary. This
    // also works during cinematics/paused scripts and performs no guest calls.
    // Success is committed only after both typed viewport setters complete.
    let prepared = if ready && world_camera_enabled() && request.is_some() {
        crate::xr::prepare_camera_pose()
    } else { None };
    let mut view = WorldView { provenance_epoch, camera_sequence: state.sequence, request_frame: request.map(|r| r.frame).unwrap_or(0),
        head_pose_frame: prepared.map(|pose| pose.update.frame).unwrap_or(0), presentation_time_seconds: request.map(|r| r.presentation_time_seconds).unwrap_or(0.),
        eye_index, view_count: request.map(|r| r.view_count).unwrap_or(1), game_from_eye: camera, game_from_stage: identity(),
        projection_from_eye: projection, depth_reverse_z, headset_applied: false };
    let override_matrices = if ready && world_camera_enabled() && request.is_some() {
        prepared.filter(|pose| rigid(&pose.relative) && pose.relative[12..15].iter().all(|n| n.abs() <= 5.)).map(|pose| {
            let head_from_eye = request.map(|r| r.views[eye_index as usize].head_from_eye).unwrap_or_else(identity);
            let desired_camera = multiply(&multiply(&camera, &pose.relative), &head_from_eye);
            let desired_projection = request.and_then(|r| reverse_z(&projection).map(|direction| for_game_depth(r.views[eye_index as usize].projection_from_eye, direction)));
            (desired_camera, desired_projection)
        })
    } else { None };
    drop(state);
    if let Some((desired_camera, desired_projection)) = override_matrices.filter(|_| view_epoch_is_active(provenance_epoch)) {
        match update_viewport(caller, viewport, Some(&desired_camera), desired_projection.as_ref()) {
            Ok(()) => {
                if !view_epoch_is_active(provenance_epoch)
                    || !crate::xr::commit_camera_pose(prepared.expect("guarded prepared pose")) { return Ok(()); }
                view.game_from_eye = desired_camera;
                if let Some(projection) = desired_projection { view.projection_from_eye = projection; }
                let native_pose = prepared.expect("guarded prepared pose").native;
                let head_from_eye = request.map(|r| r.views[eye_index as usize].head_from_eye).unwrap_or_else(identity);
                let stage_from_eye = multiply(&native_pose.stage_from_head, &head_from_eye);
                view.game_from_stage = multiply(&desired_camera, &rigid_inverse(&stage_from_eye));
                view.presentation_time_seconds = native_pose.presentation_time_seconds;
                view.headset_applied = true;
                let mut state = lock_world_state();
                if provenance_epoch != EPOCH.load(Ordering::Acquire) { return Ok(()); }
                if let Some(consumed) = request { remember_consumed_request(&mut state, view, consumed); }
                state.overrides_applied += 1; state.last_error = None;
            }
            Err(error) => {
                let message = error.to_string();
                crate::xr::record_camera_result(Err(&message));
                let mut state = lock_world_state(); state.overrides_failed += 1; state.last_error = Some(message);
            }
        }
    }
    let saved = SavedViewport { pointer: viewport, source, view, base_camera: camera, base_projection: projection };
    let mut state = lock_world_state();
    if provenance_epoch != EPOCH.load(Ordering::Acquire) { return Ok(()); }
    state.last_main = Some(saved);
    state.history.push_back(saved);
    while state.history.len() > HISTORY { state.history.pop_front(); }
    Ok(())
}

pub fn scene_post(_caller: &mut Caller<'_, HostState>, phase: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let mut state = lock_world_state(); state.scene_calls += 1;
    let source = read_u64(mem, phase.saturating_add(1016)).unwrap_or(0);
    if source == 0 || source != read_u64(mem, ACTIVE_CVIEWPORT).unwrap_or(0)
        || read_u64(mem, phase.saturating_add(1064)).unwrap_or(0) != 0 { return Ok(()); }
    let Some(main) = state.last_main.filter(|main| main.source == source) else { return Ok(()); };
    let pointer = phase.saturating_add(32);
    let Some(camera) = read_matrix(mem, pointer + CAMERA_OFFSET) else { return Ok(()); };
    let Some(projection) = read_matrix(mem, pointer + PROJECTION_OFFSET) else { return Ok(()); };
    if !same_matrix(&camera, &main.view.game_from_eye) || !same_matrix(&projection, &main.view.projection_from_eye) { return Ok(()); }
    state.main_scene_calls += 1;
    state.history.push_back(SavedViewport { pointer, ..main });
    while state.history.len() > HISTORY { state.history.pop_front(); }
    Ok(())
}

fn same_matrix(a: &[f32; 16], b: &[f32; 16]) -> bool {
    a.iter().zip(b).all(|(a, b)| (*a - *b).abs() <= 1e-4)
}

fn matrix_error(a: &[f32; 16], b: &[f32; 16]) -> f32 {
    a.iter().zip(b).map(|(a,b)| (*a-*b).abs()).fold(0.,f32::max)
}

fn diagnose_unmatched(state: &mut State, mem: &SharedMemory, pointer: u64) {
    let Some(camera) = pointer.checked_add(CAMERA_OFFSET).and_then(|address|read_matrix(mem,address)) else { return; };
    let Some(projection) = pointer.checked_add(PROJECTION_OFFSET).and_then(|address|read_matrix(mem,address)) else { return; };
    let closest = state.history.iter().rev().filter(|saved| saved.view.headset_applied
        && saved.view.provenance_epoch == EPOCH.load(Ordering::Acquire)
        && (12..15).all(|i| (camera[i] - saved.base_camera[i]).abs() < 2.))
        .min_by(|a,b| {
            let score = |saved: &&SavedViewport| matrix_error(&camera,&saved.view.game_from_eye).min(matrix_error(&camera,&saved.base_camera));
            score(a).total_cmp(&score(b))
        }).copied();
    let Some(closest) = closest else { return; };
    let candidate = UnmatchedViewport { pointer, camera, projection,
        camera_error:matrix_error(&camera,&closest.view.game_from_eye),base_camera_error:matrix_error(&camera,&closest.base_camera),
        projection_error:matrix_error(&projection,&closest.view.projection_from_eye),base_projection_error:matrix_error(&projection,&closest.base_projection),closest };
    // Diagnostics alone: this does not broaden camera provenance or mutate any
    // unqualified viewport. Keep the report well within the native JSON buffer.
    if state.unmatched_near_world.back().is_some_and(|old| old.pointer == pointer
        && same_matrix(&old.camera,&camera) && same_matrix(&old.projection,&projection)) { return; }
    state.unmatched_near_world.push_back(candidate);
    while state.unmatched_near_world.len() > 8 { state.unmatched_near_world.pop_front(); }
}

fn match_viewport(state: &State, mem: &SharedMemory, pointer: u64) -> Option<WorldView> {
    if pointer == 0 { return None; }
    let camera = read_matrix(mem, pointer.checked_add(CAMERA_OFFSET)?)?;
    let projection = read_matrix(mem, pointer.checked_add(PROJECTION_OFFSET)?)?;
    // Baked command copies have different pointers. Match BOTH immutable matrices
    // against bounded source/scene history, never against a newer mutable camera.
    state.history.iter().rev().find(|saved| saved.view.provenance_epoch == EPOCH.load(Ordering::Acquire)
        && same_matrix(&camera, &saved.view.game_from_eye)
        && same_matrix(&projection, &saved.view.projection_from_eye)
        && (!saved.view.headset_applied || projection_state_is_current(mem,pointer,&projection))).map(|saved| saved.view)
}

fn reverse_clone_view(camera: &[f32;16], projection: &[f32;16], saved: &SavedViewport) -> Option<WorldView> {
    // Live v5d opaque clones retained the exact applied primary camera while
    // rebuilding the ORIGINAL source projection with z'=w-z. Accept only that
    // exact relationship. Near-plane variants, shadow/reflection cameras and
    // arbitrary perspective matrices do not inherit the primary eye.
    if !saved.view.headset_applied || *camera != saved.view.game_from_eye { return None; }
    let mut view = saved.view;
    if reverse_z(&view.projection_from_eye)? != true {
        view.projection_from_eye = flip_projection_depth(view.projection_from_eye);
    }
    let certified_original_clone = reverse_z(&saved.base_projection) == Some(false)
        && *projection == flip_projection_depth(saved.base_projection);
    // Once source scalar fields describe the requested eye, the engine can
    // regenerate this exact reverse matrix itself. This is equally narrow;
    // near-plane/FOV variants still do not match either certified projection.
    if !certified_original_clone && *projection != view.projection_from_eye { return None; }
    view.depth_reverse_z = true;
    Some(view)
}

fn match_reverse_clone(state: &State, mem: &SharedMemory, pointer: u64) -> Option<(SavedViewport, WorldView)> {
    let camera = read_matrix(mem,pointer.checked_add(CAMERA_OFFSET)?)?;
    let projection = read_matrix(mem,pointer.checked_add(PROJECTION_OFFSET)?)?;
    state.history.iter().rev().filter(|saved| view_epoch_is_active(saved.view.provenance_epoch))
        .find_map(|saved| reverse_clone_view(&camera,&projection,saved).map(|view|(*saved,view)))
}

pub fn viewport_command(_caller: &mut Caller<'_, HostState>, _command: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    lock_world_state().viewport_commands += 1;
    Ok(())
}

pub fn current_view(caller: &mut Caller<'_, HostState>, viewport: u64, _set_window: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let mut state = lock_world_state(); state.current_calls += 1;
    let mut view = if crate::xr::is_active() { match_viewport(&state, mem, viewport) } else { None };
    let clone = if view.is_none() && viewport != 0 && ENABLED.load(Ordering::Acquire) { match_reverse_clone(&state,mem,viewport) } else { None };
    if view.is_none() && clone.is_none() && viewport != 0 && ENABLED.load(Ordering::Acquire) { diagnose_unmatched(&mut state,mem,viewport); }
    drop(state);
    if let Some((saved, derived)) = clone {
        let already_current = read_matrix(mem,viewport + PROJECTION_OFFSET) == Some(derived.projection_from_eye)
            && projection_state_is_current(mem,viewport,&derived.projection_from_eye);
        let result = if already_current { Ok(()) } else { update_viewport(caller,viewport,None,Some(&derived.projection_from_eye)) };
        let mut state = lock_world_state();
        if view_epoch_is_active(derived.provenance_epoch) {
            match result {
                Ok(()) => {
                    // The typed setter regenerates inverse/dependent matrices
                    // and frustums BEFORE original SetCurrent binds the clone.
                    if already_current { state.projection_clones_already_current += 1; }
                    else { state.projection_clone_updates += 1; }
                    if *CAPTURE_CONSUMED_REQUESTS {
                        remember_derived_consumed_request(&mut state, saved.view, derived);
                    }
                    state.history.push_back(SavedViewport { pointer:viewport,view:derived,..saved });
                    while state.history.len() > HISTORY { state.history.pop_front(); }
                    view = Some(derived);
                }
                Err(error) => { state.projection_clone_failures += 1;state.last_error = Some(error.to_string()); }
            }
        }
    }
    let mut state = lock_world_state();
    view = view.filter(|view| view.provenance_epoch == EPOCH.load(Ordering::Acquire));
    if view.is_some() { state.matched_current_calls += 1; }
    state.last_current = view;
    drop(state);
    RENDER_THREAD.with(|thread| {
        let mut thread = thread.borrow_mut();
        let epoch = EPOCH.load(Ordering::Acquire);
        if thread.epoch != epoch { *thread = RenderThread { epoch, ..Default::default() }; }
        thread.current = view;
    });
    Ok(())
}

/// Called at the exact wasm_gpu::Emit midpoint while its producer mutex is held.
/// Command bytes are complete; the GPU consumer cannot yet see the published head.
pub fn gpu_command(_caller: &mut Caller<'_, HostState>, command: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let Some(opcode) = read_u32(mem, command) else { return Ok(()); };
    let mut state = lock_world_state();
    RENDER_THREAD.with(|thread| {
        let mut thread = thread.borrow_mut();
        let epoch = EPOCH.load(Ordering::Acquire);
        if thread.epoch != epoch { *thread = RenderThread { epoch, ..Default::default() }; }
        let current = thread.current.filter(|view| !view.headset_applied || view_epoch_is_active(view.provenance_epoch));
        record_emission(&mut state, command, opcode, current);
        MARKER_COUNT.store(state.markers.len() as u64, Ordering::Release);
    });
    Ok(())
}

fn record_emission(state: &mut State, command: u64, opcode: u32, current: Option<WorldView>) {
    state.emitted_commands += 1;
    if current != state.last_emitted {
        if state.markers.len() >= MAX_MARKERS || state.markers.contains_key(&command) {
            state.correlation_failed = true; state.last_error = Some("GPU ring view marker overrun; refusing uncorrelated images".into());
        } else {
            state.markers.insert(command, current); state.last_emitted = current; state.marker_changes += 1;
        }
    }
    // Accumulate draws in serialized ring order, rather than assuming the
    // producer that presents is the same guest pthread that emitted geometry.
    if opcode == 32 {
        if let Some(view) = current {
            if state.frame_world.is_some_and(|old| old.eye_index != view.eye_index || old.camera_sequence != view.camera_sequence) {
                state.frame_conflict = true;
            }
            state.frame_world = Some(view);
        }
    }
    if opcode == 7 {
        state.emitted_presents += 1;
        let view = if state.frame_conflict || state.correlation_failed { None } else { state.frame_world };
        if state.frame_conflict { state.conflicting_presents += 1; }
        if state.presents.len() >= MAX_PRESENTS {
            state.correlation_failed = true; state.last_error = Some("GPU presentation metadata overrun; refusing uncorrelated images".into());
        } else { state.presents.push_back(view); }
        state.frame_world = None; state.frame_conflict = false;
    }
}

/// GPU worker calls before executing each ring command. None means no context
/// change; Some(None) clears a previous world view for UI/shadow/loading commands.
pub fn take_command_view(command_address: u64) -> Option<Option<WorldView>> {
    if MARKER_COUNT.load(Ordering::Acquire) == 0 { return None; }
    let mut state = lock_world_state();
    let result = state.markers.remove(&command_address);
    MARKER_COUNT.store(state.markers.len() as u64, Ordering::Release);
    result
}

/// GPU worker calls once for EVERY opcode7, including nonworld presentations.
pub fn take_present_view() -> Option<WorldView> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return None; }
    let mut state = lock_world_state(); state.consumed_presents += 1;
    let view = state.presents.pop_front().flatten();
    if state.correlation_failed { return None; }
    if view.is_some() { state.world_presents += 1; }
    view
}

pub fn latest_world_view() -> Option<WorldView> { lock_world_state().last_main.map(|main| main.view) }

fn describe(view: WorldView) -> serde_json::Value {
    serde_json::json!({"provenance_epoch":view.provenance_epoch,"camera_sequence":view.camera_sequence, "request_frame":view.request_frame,
        "head_pose_frame":view.head_pose_frame, "eye_index":view.eye_index, "view_count":view.view_count,
        "presentation_time_seconds":view.presentation_time_seconds, "game_from_eye":view.game_from_eye,
        "game_from_stage":view.game_from_stage,
        "projection_from_eye":view.projection_from_eye, "depth_reverse_z":view.depth_reverse_z, "headset_applied":view.headset_applied})
}

pub fn status_json() -> serde_json::Value {
    let state = lock_world_state();
    let unmatched: Vec<_> = state.unmatched_near_world.iter().map(|candidate|serde_json::json!({
        "viewport":candidate.pointer,"closest_viewport":candidate.closest.pointer,"source":candidate.closest.source,
        "camera_sequence":candidate.closest.view.camera_sequence,"head_pose_frame":candidate.closest.view.head_pose_frame,
        "camera_error":candidate.camera_error,"base_camera_error":candidate.base_camera_error,
        "projection_error":candidate.projection_error,"base_projection_error":candidate.base_projection_error,
        "camera":candidate.camera,"projection":candidate.projection,
        "qualified_camera":candidate.closest.view.game_from_eye,"qualified_projection":candidate.closest.view.projection_from_eye,
        "base_camera":candidate.closest.base_camera,"base_projection":candidate.closest.base_projection
    })).collect();
    let mut status = serde_json::json!({"world_camera_enabled":ENABLED.load(Ordering::Acquire), "view_request_abi_version":1,
        "view_request_count":state.request.map(|(request,_)|request.view_count), "latest_request_frame":state.request.map(|(request,_)|request.frame),
        "camera_calls":state.camera_calls,"main_camera_calls":state.main_camera_calls,"qualified_camera_calls":state.qualified_camera_calls,
        "overrides_applied":state.overrides_applied,"overrides_failed":state.overrides_failed,"scene_calls":state.scene_calls,
        "main_scene_calls":state.main_scene_calls,"viewport_commands":state.viewport_commands,"current_view_calls":state.current_calls,
        "matched_current_view_calls":state.matched_current_calls,"gpu_commands_marked":state.emitted_commands,"view_marker_changes":state.marker_changes,
        "projection_clone_updates":state.projection_clone_updates,"projection_clone_failures":state.projection_clone_failures,
        "presents_enqueued":state.emitted_presents,"presents_consumed":state.consumed_presents,"world_presents":state.world_presents,
        "conflicting_presents":state.conflicting_presents,"pending_presents":state.presents.len(),"pending_markers":state.markers.len(),
        "correlation_failed":state.correlation_failed,"last_main_viewport":state.last_main.map(|main|main.pointer),
        "last_main_view":state.last_main.map(|main|describe(main.view)),
        "last_current_view":state.last_current.map(describe),"last_error":state.last_error,
        "unmatched_near_world":unmatched,
        "rendering_method":"one eye per natural game frame; two views use alternate-eye rendering, not simultaneous stereo"});
    status["projection_clones_already_current"] = serde_json::json!(state.projection_clones_already_current);
    status["original_primary_projection_scalars_872_to_912"] = serde_json::json!(state.original_primary_projection_scalars);
    status["eye_projection_scalars_872_to_912"] = serde_json::json!(state.last_main.and_then(|main|projection_scalars(&main.view.projection_from_eye)));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pose() -> crate::xr::CameraUpdate {
        crate::xr::CameraUpdate { frame: 1, heading_degrees: 90., pitch_degrees: 0., roll_degrees: 0., relative_position_meters: [0.2, 0.1, -0.3] }
    }
    fn projection() -> [f32;16] { [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,0.001,-1., 0.,0.,0.1,0.] }
    #[test]
    fn consumed_request_lookup_survives_newer_request_but_rejects_mismatch_reset_and_overflow() {
        let eye = SVXREyeView { head_from_eye: identity(), projection_from_eye: projection() };
        let make_request = |frame| SVXRViewRequest { version: 1, size: 288, frame,
            presentation_time_seconds: frame as f64, view_count: 2, reserved: 0, views: [eye; 2] };
        let mut state = State::default();
        let mut original = view(0, 1);
        original.request_frame = 10;
        original.camera_sequence = 100;
        original.presentation_time_seconds = 10.25;
        original.head_pose_frame = 99;
        push_consumed_request(&mut state, original, make_request(10));
        let mut latest = original;
        latest.request_frame = 11;
        latest.camera_sequence = 101;
        latest.presentation_time_seconds = 11.25;
        latest.head_pose_frame = 100;
        push_consumed_request(&mut state, latest, make_request(11));
        state.request = Some((make_request(11), Instant::now())); // latest advanced after the original camera consumed frame 10
        assert_eq!(find_consumed_request(&state, &original).map(|r| r.frame), Some(10));
        let bytes = request_bytes(find_consumed_request(&state, &original).unwrap());
        assert_eq!(bytes.len(), 288);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 10);
        let mut miss = original; miss.provenance_epoch += 1;
        assert!(find_consumed_request(&state, &miss).is_none());
        miss = original; miss.presentation_time_seconds += 0.001;
        assert!(find_consumed_request(&state, &miss).is_none());
        miss = original; miss.head_pose_frame += 1;
        assert!(find_consumed_request(&state, &miss).is_none());
        miss = original; miss.camera_sequence += 1;
        assert!(find_consumed_request(&state, &miss).is_none());
        for frame in 12..(12 + MAX_CONSUMED_REQUESTS as u64) {
            let mut view = original;
            view.request_frame = frame;
            view.camera_sequence = frame;
            view.presentation_time_seconds = frame as f64;
            push_consumed_request(&mut state, view, make_request(frame));
        }
        assert_eq!(state.consumed_requests.len(), MAX_CONSUMED_REQUESTS);
        assert!(find_consumed_request(&state, &original).is_none());
        assert!(find_consumed_request(&state, &latest).is_none());
        invalidate_state(&mut state);
        assert!(state.consumed_requests.is_empty());
    }

    #[test]
    fn acquired_world_frame_receipt_requires_exact_consumed_view_and_abi() {
        let eye = SVXREyeView { head_from_eye: identity(), projection_from_eye: projection() };
        let request = SVXRViewRequest { version: 1, size: 288, frame: 23,
            presentation_time_seconds: 23., view_count: 2, reserved: 0, views: [eye; 2] };
        let mut view = view(1, 42);
        view.request_frame = 23;
        view.head_pose_frame = 19;
        view.presentation_time_seconds = 23.;
        let mut state = State::default();
        push_consumed_request(&mut state, view, request);
        let frame = crate::world_frame::WorldFrame { abi_version: 1,
            byte_size: std::mem::size_of::<crate::world_frame::WorldFrame>() as u32,
            lease: 7, game_frame: 24, request_frame: 23, head_frame: 19,
            presentation_time: 23., color_texture: 100, depth_texture: 101,
            view_index: 1, view_count: 2, depth_convention: 0,
            projection_from_eye: view.projection_from_eye,
            game_from_eye: view.game_from_eye, game_from_stage: view.game_from_stage,
            ..Default::default() };
        assert_eq!(match_world_frame_receipt(&state, &frame).map(|(_, r)| r.frame), Some(23));
        let mut bad = frame; bad.abi_version = 2;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.byte_size -= 1;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.lease = 0;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.depth_convention = 1;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.request_frame += 1;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.head_frame += 1;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.view_index = 0;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        bad = frame; bad.presentation_time += 0.001;
        assert!(match_world_frame_receipt(&state, &bad).is_none());
        for matrix in [0, 1, 2] {
            bad = frame;
            let values = match matrix { 0 => &mut bad.projection_from_eye,
                1 => &mut bad.game_from_eye, _ => &mut bad.game_from_stage };
            values[0] += 0.001;
            assert!(match_world_frame_receipt(&state, &bad).is_none());
        }
        let mut wrong_time = State::default();
        let mut request_at_other_time = request;
        request_at_other_time.presentation_time_seconds = 24.;
        push_consumed_request(&mut wrong_time, view, request_at_other_time);
        assert!(match_world_frame_receipt(&wrong_time, &frame).is_none());
        push_consumed_request(&mut state, view, request);
        assert!(match_world_frame_receipt(&state, &frame).is_none());
        invalidate_state(&mut state);
        assert!(match_world_frame_receipt(&state, &frame).is_none());
    }

    #[test]
    fn reverse_clone_receipt_requires_exact_primary_request_and_derived_view() {
        let mut native = projection(); native[8] = 0.14; native[9] = -0.07;
        let eye = SVXREyeView { head_from_eye: identity(), projection_from_eye: native };
        let request = SVXRViewRequest { version: 1, size: 288, frame: 17,
            presentation_time_seconds: 1., view_count: 2, reserved: 0, views: [eye; 2] };
        assert_eq!(validate_request(&request), Ok(()));
        let mut primary = view(0, 7);
        primary.request_frame = 17;
        primary.head_pose_frame = 9;
        primary.projection_from_eye = for_game_depth(native, false);
        primary.depth_reverse_z = false;
        let mut derived = primary;
        derived.projection_from_eye = flip_projection_depth(primary.projection_from_eye);
        derived.depth_reverse_z = true;
        assert!(matrix_error(&derived.projection_from_eye, &native) < 1e-7);
        assert!(!same_matrix_bits(&derived.projection_from_eye, &native));
        let mut state = State::default();
        push_consumed_request(&mut state, primary, request);
        let mut later = request; later.frame = 18;
        state.request = Some((later, Instant::now()));
        assert!(remember_derived_consumed_request(&mut state, primary, derived));
        assert_eq!(find_consumed_request(&state, &derived).map(|r| r.frame), Some(17));
        assert!(remember_derived_consumed_request(&mut state, primary, derived));
        assert_eq!(state.consumed_requests.len(), 2); // repeated clone cannot flood the 96-entry history

        let mut wrong_projection = derived; wrong_projection.projection_from_eye[10] += 0.001;
        assert!(!remember_derived_consumed_request(&mut state, primary, wrong_projection));
        assert!(find_consumed_request(&state, &wrong_projection).is_none());
        let mut wrong_camera = derived; wrong_camera.game_from_eye[12] += 0.01;
        assert!(!remember_derived_consumed_request(&mut state, primary, wrong_camera));
        assert!(find_consumed_request(&state, &wrong_camera).is_none());
        let mut wrong_epoch = derived; wrong_epoch.provenance_epoch += 1;
        assert!(!remember_derived_consumed_request(&mut state, primary, wrong_epoch));
        assert!(find_consumed_request(&state, &wrong_epoch).is_none());

        let mut stale_state = State::default();
        let mut stale_request = request; stale_request.frame = 16;
        push_consumed_request(&mut stale_state, primary, stale_request);
        assert!(!remember_derived_consumed_request(&mut stale_state, primary, derived));
        assert!(find_consumed_request(&stale_state, &derived).is_none());
        invalidate_state(&mut state);
        assert!(!remember_derived_consumed_request(&mut state, primary, derived));
        assert!(find_consumed_request(&state, &derived).is_none());
    }

    #[test]
    fn full_camera_composition_keeps_head_translation_and_distinct_eye_offsets() {
        let head = native_head(pose()).unwrap();
        let mut left = identity(); left[12] = -0.032;
        let mut right = identity(); right[12] = 0.032;
        let a = multiply(&head, &left); let b = multiply(&head, &right);
        assert!((a[14] - b[14] - 0.064).abs() < 1e-5);
        assert_eq!(head[12..15], [0.2,0.1,-0.3]);
        let mut roll = pose(); roll.heading_degrees = 0.; roll.roll_degrees = 30.;
        let matrix = native_head(roll).unwrap(); assert!((matrix[1] - 0.5).abs() < 1e-5);
    }
    #[test]
    fn projection_conversion_changes_only_depth_row_and_preserves_asymmetry() {
        let mut native = projection(); native[8] = 0.14; native[9] = -0.07;
        let forward = for_game_depth(native, false);
        assert_eq!(reverse_z(&native), Some(true)); assert_eq!(reverse_z(&forward), Some(false));
        assert_eq!((forward[8],forward[9]), (0.14,-0.07));
        for col in 0..4 { assert!((forward[col*4+2] + native[col*4+2] - native[col*4+3]).abs() < 1e-6); }
    }
    #[test]
    fn shader_projection_scalars_reconstruct_asymmetric_rays_and_finite_depth() {
        let mut p = projection(); p[0]=1.2; p[5]=2.1; p[8]=0.14; p[9]=-0.07;
        for p in [p,flip_projection_depth(p)] {
            let s = projection_scalars(&p).unwrap();
            assert!(s[0]>0. && s[1]>s[0] && s[1]<=1002.);
            assert_eq!((s[2],s[3]),(1.,1.));
            for ndc in [-1.,0.,1.] {
                assert!(((ndc+s[4])*s[8]-(ndc+p[8])/p[0]).abs()<1e-6);
                assert!(((ndc+s[5])*s[7]-(ndc+p[9])/p[5]).abs()<1e-6);
            }
            let project_depth = |distance:f32| -p[10]+p[14]/distance;
            let (near_depth,far_depth) = if reverse_z(&p)==Some(true) {(1.,0.)}else{(0.,1.)};
            assert!((project_depth(s[0])-near_depth).abs()<1e-6);
            assert!((project_depth(s[1])-far_depth).abs()<1e-6);
            assert!(((s[6].to_radians()/2.).tan()-s[7]).abs()<1e-6);
            assert_eq!((s[9],s[10]),(p[5]/p[0],p[5]/p[0]));
        }
        let mut oblique=p; oblique[2]=0.01;assert!(projection_scalars(&oblique).is_none());
        let mut skew=p;skew[1]=0.01;assert!(projection_scalars(&skew).is_none());
        let mut infinite=p;infinite[10]=0.;assert!(projection_scalars(&infinite).is_none());
        // Legal .05m/1km request after f32 forward-Z cancellation reconstructs
        // far=1001.077m; retain that actual native rounding case.
        let mut rounded=p;rounded[10]=-1.0000499486923218;rounded[14]=-0.05000250041484833;
        let scalars=projection_scalars(&rounded).unwrap();
        assert!((scalars[0]-0.05).abs()<1e-6 && scalars[1]>1001. && scalars[1]<1002.);
    }
    #[test]
    fn opaque_reverse_clone_replaces_requested_fov_and_excludes_near_variants_or_other_cameras() {
        let mut native = projection(); native[8] = 0.14; native[9] = -0.07;
        let mut original = native; original[0] = 1.2062851; original[5] = 2.1445069;
        original[8] = 0.; original[9] = 0.; original[10] = 0.00000500679; original[14] = 0.05000025;
        let base_projection = flip_projection_depth(original);
        let mut source_view = view(0,3); source_view.projection_from_eye = flip_projection_depth(native); source_view.depth_reverse_z = false;
        let saved = SavedViewport { pointer:1000,source:984,view:source_view,base_camera:identity(),base_projection };
        let derived = reverse_clone_view(&identity(),&original,&saved).unwrap();
        assert!(matrix_error(&derived.projection_from_eye,&native) < 1e-7);
        assert_eq!((derived.projection_from_eye[0],derived.projection_from_eye[5],derived.projection_from_eye[8],derived.projection_from_eye[9]),
            (native[0],native[5],native[8],native[9]));
        assert!(derived.depth_reverse_z);
        assert_eq!(derived.game_from_eye,source_view.game_from_eye);
        assert_eq!(derived.game_from_stage,source_view.game_from_stage);
        let requested_reverse = flip_projection_depth(source_view.projection_from_eye);
        assert_eq!(reverse_clone_view(&identity(),&requested_reverse,&saved),Some(derived));
        let mut near_variant = original; near_variant[14] *= 0.5;
        assert!(reverse_clone_view(&identity(),&near_variant,&saved).is_none());
        let mut other_camera = identity();other_camera[12] = 0.01;
        assert!(reverse_clone_view(&other_camera,&original,&saved).is_none());
        let mut other_fov = original;other_fov[0] *= 1.01;
        assert!(reverse_clone_view(&identity(),&other_fov,&saved).is_none());
    }
    #[test]
    fn eye_request_rejects_reflections_wrong_depth_and_extra_views() {
        let eye = SVXREyeView { head_from_eye: identity(), projection_from_eye: projection() };
        let mut request = SVXRViewRequest {version:1,size:288,frame:1,presentation_time_seconds:1.,view_count:2,reserved:0,views:[eye;2]};
        assert_eq!(validate_request(&request),Ok(()));
        request.view_count=3; assert_eq!(validate_request(&request),Err(-2)); request.view_count=2;
        request.views[0].head_from_eye[0]=-1.; assert_eq!(validate_request(&request),Err(-3));
        request.views[0]=eye; request.views[0].projection_from_eye[14]=-0.1; assert_eq!(validate_request(&request),Err(-3));
    }
    fn view(eye_index: u32, camera_sequence: u64) -> WorldView {
        WorldView { provenance_epoch:1, camera_sequence, request_frame:1, head_pose_frame:1,presentation_time_seconds:1.,eye_index,view_count:2,
            game_from_eye:identity(),game_from_stage:identity(),projection_from_eye:projection(),depth_reverse_z:true,headset_applied:true }
    }
    #[test]
    fn ring_present_alignment_keeps_loading_slots_and_world_after_ui_switch() {
        let mut state = State::default();
        record_emission(&mut state,100,7,None);
        let left = view(0,3);
        record_emission(&mut state,120,32,Some(left));
        record_emission(&mut state,140,32,None); // HUD
        record_emission(&mut state,160,7,None); // present may be another producer
        let right = view(1,4);
        record_emission(&mut state,180,32,Some(right));
        record_emission(&mut state,200,7,None);
        assert_eq!(state.presents.pop_front(),Some(None));
        assert_eq!(state.presents.pop_front(),Some(Some(left)));
        assert_eq!(state.presents.pop_front(),Some(Some(right)));
        assert_eq!(state.markers.remove(&120),Some(Some(left)));
        assert_eq!(state.markers.remove(&140),Some(None));
        assert!(!state.correlation_failed);
    }
    #[test]
    fn mixed_camera_draws_and_ring_overrun_refuse_correlated_world_frame() {
        let mut state = State::default();
        record_emission(&mut state,100,32,Some(view(0,1)));
        record_emission(&mut state,120,32,Some(view(1,2)));
        record_emission(&mut state,140,7,None);
        assert_eq!(state.presents.pop_front(),Some(None));
        assert_eq!(state.conflicting_presents,1);
        state.presents = VecDeque::from(vec![None;MAX_PRESENTS]);
        record_emission(&mut state,160,7,None);
        assert!(state.correlation_failed);
        assert_eq!(state.presents.len(),MAX_PRESENTS);
    }
    #[test]
    fn invalidation_preserves_ring_positions_and_discards_ready_provenance() {
        let left = view(0,3);
        let saved = SavedViewport { pointer:1000,source:984,view:left,base_camera:identity(),base_projection:projection() };
        let eye = SVXREyeView { head_from_eye:identity(),projection_from_eye:projection() };
        let mut state = State { request:Some((SVXRViewRequest { version:1,size:288,frame:1,presentation_time_seconds:1.,
            view_count:2,reserved:0,views:[eye;2] },Instant::now())),gameplay_ready_at:Some(Instant::now()),
            history:VecDeque::from([saved]),last_main:Some(saved),last_current:Some(left),last_emitted:Some(left),
            frame_world:Some(left),frame_conflict:true,markers:BTreeMap::from([(120,Some(left)),(140,None)]),
            presents:VecDeque::from([None,Some(left)]),..State::default() };
        invalidate_state(&mut state);
        assert!(state.request.is_none() && state.gameplay_ready_at.is_none() && state.history.is_empty());
        assert!(state.last_main.is_none() && state.last_current.is_none() && state.last_emitted.is_none());
        assert!(state.frame_world.is_none() && !state.frame_conflict);
        assert_eq!(state.markers.len(),2);
        assert!(state.markers.values().all(Option::is_none));
        assert_eq!(state.presents,VecDeque::from([None,None]));
    }
    #[test]
    fn exact_native_stage_mapping_reproduces_eye_without_latest_pose() {
        let mut stage_head = native_head(pose()).unwrap(); stage_head[12] += 8.;
        let mut eye = identity(); eye[12] = -0.032;
        let mut game_eye = native_head(pose()).unwrap(); game_eye[12] += 120.;game_eye[14] -= 40.;
        let stage_eye = multiply(&stage_head,&eye);
        let game_stage = multiply(&game_eye,&rigid_inverse(&stage_eye));
        assert!(same_matrix(&multiply(&game_stage,&stage_eye),&game_eye));
    }
}

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

#[path = "world_packet_trace.rs"]
#[allow(dead_code)] // Inactive prerequisite; no live packet adapter is installed.
pub(crate) mod packet_trace;

#[path = "xr_projection_restore.rs"]
mod projection_restore;

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
    /// Immutable optical state used for this exact source image, not latest UI state.
    pub optics: ScopeOptics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScopeOptics {
    pub scope_sequence: u64,
    pub magnification: f32,
    pub reference_fov_degrees: f32,
    pub actual_fov_degrees: f32,
}
impl Default for ScopeOptics {
    fn default() -> Self { Self { scope_sequence:0, magnification:1., reference_fov_degrees:0., actual_fov_degrees:0. } }
}
impl ScopeOptics {
    pub(crate) fn valid(self)->bool {
        if self.scope_sequence==0 {return self==Self::default();}
        if self.reference_fov_degrees!=45. || !self.actual_fov_degrees.is_finite()
            || !(5. ..=45.).contains(&self.actual_fov_degrees) {return false;}
        let expected=((f64::from(self.reference_fov_degrees).to_radians()*0.5).tan()
            /(f64::from(self.actual_fov_degrees).to_radians()*0.5).tan()) as f32;
        self.magnification.to_bits()==expected.to_bits()
    }
}

/// Scale clip X/Y about the projection's own forward-ray principal point.
/// Multiplication changes neither clip Z nor W, including their exact bits.
fn optical_projection(mut projection:[f32;16], magnification:f32)->Option<[f32;16]> {
    if projection.iter().any(|v|!v.is_finite()) || !magnification.is_finite()
        || !(1. ..=10.).contains(&magnification) || projection[11] == 0. { return None; }
    if magnification==1. {return Some(projection);}
    let center = [projection[8]/projection[11], projection[9]/projection[11]];
    if center.iter().any(|v|!v.is_finite()) { return None; }
    // Use the algebraically equivalent form around c. For a canonical pinhole
    // its principal-point column remains bit-identical at every magnification.
    for col in 0..4 { for row in 0..2 {
        let index=col*4+row;
        projection[index] = if col==2 { projection[index] } else {
            magnification*projection[index]+(1.-magnification)*center[row]*projection[col*4+3]
        };
    }}
    projection.iter().all(|v|v.is_finite()).then_some(projection)
}

/// Ephemeral live pointers are never copied into WorldView or frame leases.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct OpticalAuthority {
    camera:u64, metadata:u64, helper:u64, helper_metadata:u64,
    scope_sequence:u64, reference_bits:u32, actual_bits:u32,
}
impl OpticalAuthority {
    fn optics(self)->Option<ScopeOptics> {
        let reference=f32::from_bits(self.reference_bits);
        let actual=f32::from_bits(self.actual_bits);
        let m=((f64::from(reference).to_radians()*0.5).tan()
            /(f64::from(actual).to_radians()*0.5).tan()) as f32;
        (m.is_finite() && (1. ..=10.).contains(&m)).then_some(ScopeOptics {
            scope_sequence:self.scope_sequence,magnification:m,
            reference_fov_degrees:reference,actual_fov_degrees:actual })
    }
}

fn optical_authority_from(camera:u64,scope_sequence:u64,
    mut read:impl FnMut(u64,usize)->Option<u64>)->Option<OpticalAuthority> {
    if camera==0 || scope_sequence==0{return None;}
    let metadata=read(camera.checked_add(568)?,8)?;
    if metadata==0 || read(camera.checked_add(16)?,8)?!=metadata{return None;}
    let (expected_helper,min_fov)=match read(metadata.checked_add(8)?,4)? as u32 {
        0xec22d348=>(0x26ea93d8,5.),0x8c40fb35=>(0xe2159f28,7.5),
        0x64e68b46=>(0x49f800ae,10.),_=>return None,
    };
    if read(metadata.checked_add(28)?,4)? as u32!=expected_helper{return None;}
    let helper=read(camera.checked_add(576)?,8)?;
    if helper==0{return None;}
    let helper_metadata=read(helper.checked_add(32)?,8)?;
    if helper_metadata==0 || read(helper_metadata.checked_add(8)?,4)? as u32!=expected_helper{return None;}
    let reference_bits=read(metadata.checked_add(32)?,4)? as u32;
    let actual_bits=read(camera.checked_add(160)?,4)? as u32;
    if reference_bits!=45f32.to_bits(){return None;}
    let actual=f32::from_bits(actual_bits);
    if !actual.is_finite() || !(min_fov..=45.).contains(&actual){return None;}
    let result=OpticalAuthority{camera,metadata,helper,helper_metadata,scope_sequence,reference_bits,actual_bits};
    result.optics()?;Some(result)
}

fn optical_authority(mem:&SharedMemory,epoch:u64)->Option<OpticalAuthority> {
    let (camera,scope_sequence)=crate::xr_aim::qualified_scope_camera_for_optics(epoch)?;
    let authority=optical_authority_from(camera,scope_sequence,|address,width| {
        match width {4=>read_u32(mem,address).map(u64::from),8=>read_u64(mem,address),_=>None}
    })?;
    (crate::xr_aim::qualified_scope_camera_for_optics(epoch)==Some((camera,scope_sequence))).then_some(authority)
}

fn qualified_projection_optics(authority:OpticalAuthority,before:projection_restore::Snapshot)->Option<ScopeOptics> {
    // Exact original RegeneratePerspective arithmetic and exact propagated
    // native FOV scalar. No zoom-control/global camera FOV substitutes qualify.
    if before.fields[6].to_bits()!=authority.actual_bits
        || !projection_restore::guest_projection(before.fields,before.reverse)
            .is_some_and(|p|same_matrix_bits(&p,&before.projection)) {return None;}
    authority.optics()
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
    emission_token: u64,
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
        || !optical_projection(for_game_depth(requested_projection,false),primary.optics.magnification)
            .is_some_and(|expected|same_matrix_bits(&primary.projection_from_eye,&expected)) {
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
    native_far_plane: Option<(f32, Instant)>,
    last_optics_qualification:Option<&'static str>,
    // A pending semantic crop reconciliation survives UI disable/recenter.
    // Only a CURRENT certified primary callback may consume it; no saved
    // pointer is dereferenced by set_active or the native camera enable API.
    primary_crop_owner: Option<projection_restore::Owner>,
    primary_crop_restores: u64,
    primary_crop_dropped: u64,
    primary_crop_unqualified: u64,
    primary_crop_failures: u64,
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
// Tokens describe a binding, not the view's matrix/value. A second binding of
// the same view deliberately takes a different token. Zero means no shortcut.
static NEXT_EMISSION_TOKEN: AtomicU64 = AtomicU64::new(1);
static LAST_EMITTED_TOKEN: AtomicU64 = AtomicU64::new(0);
static EMISSION_FAST_COMMANDS: AtomicU64 = AtomicU64::new(0);
static EMISSION_TOKEN_EXHAUSTED: AtomicBool = AtomicBool::new(false);
// Resolve the diagnostic override once. The pre-start app request remains an
// atomic read: a status probe may run before the launcher applies its choice.
static EMISSION_FAST_OVERRIDE: LazyLock<Option<bool>> = LazyLock::new(||
    crate::draw_setup::xr_emission_override(std::env::var(crate::draw_setup::XR_EMISSION_ENV).ok().as_deref()));

fn emission_fast_enabled() -> bool {
    crate::draw_setup::xr_emission_requested(*EMISSION_FAST_OVERRIDE)
}

fn next_token_value(value: u64) -> Option<u64> { value.checked_add(1) }

fn next_emission_token() -> u64 {
    if EMISSION_TOKEN_EXHAUSTED.load(Ordering::Acquire) { return 0; }
    match NEXT_EMISSION_TOKEN.fetch_update(Ordering::AcqRel, Ordering::Acquire, next_token_value) {
        Ok(token) => token,
        Err(_) => { EMISSION_TOKEN_EXHAUSTED.store(true, Ordering::Release); 0 }
    }
}

fn can_skip_emission(opcode: u32, token: u64, previous_token: u64) -> bool {
    token != 0 && token == previous_token && opcode != 32 && opcode != 7
}

fn token_after_slow_emission(state: &State, current: Option<WorldView>, token: u64) -> u64 {
    if !state.correlation_failed && state.last_emitted == current { token } else { 0 }
}

fn emission_binding_for_epoch(current: Option<WorldView>, token: u64, thread_epoch: u64, epoch: u64) -> (Option<WorldView>, u64) {
    if thread_epoch != epoch { (None, 0) } else { (current, if current.is_some() { token } else { 0 }) }
}

fn lock_world_state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn fresh_native_far_plane(observation: Option<(f32, Instant)>, now: Instant) -> f32 {
    observation.filter(|(far, at)| far.is_finite() && (1. ..=20_000.).contains(far)
        && now.checked_duration_since(*at).is_some_and(|age| age <= Duration::from_secs(2)))
        .map(|(far, _)| far).unwrap_or(0.)
}

/// Read-only current original lens authority; no guest pointer escapes camera_post.
/// Zero means unknown/stale, allowing the launcher to retain its bounded fallback.
#[no_mangle]
pub extern "C" fn sv_xr_native_far_plane_metres() -> f32 {
    fresh_native_far_plane(lock_world_state().native_far_plane, Instant::now())
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
    LAST_EMITTED_TOKEN.store(0, Ordering::Release);
    crate::xr_motion_aim::invalidate_camera();
}

fn invalidate_state(state: &mut State) {
    state.request = None;
    state.consumed_requests.clear();
    state.gameplay_ready_at = None;
    state.history.clear();
    state.last_main = None;
    state.last_current = None;
    state.original_primary_projection_scalars = None;
    state.native_far_plane = None;
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

fn canonical_request(mut request: SVXRViewRequest) -> Result<SVXRViewRequest, i32> {
    if request.version != 1 || request.size != 288 || request.reserved != 0
        || !(1..=2).contains(&request.view_count) { return Err(-2); }
    if !request.presentation_time_seconds.is_finite() || request.presentation_time_seconds < 0. { return Err(-3); }
    for view in request.views.iter_mut().take(request.view_count as usize) {
        if !rigid(&view.head_from_eye) || view.head_from_eye[12..15].iter().any(|x| x.abs() > 0.25)
        { return Err(-3); }
        view.projection_from_eye = canonical_eye_projection(&view.projection_from_eye)
            .ok_or(-3)?.0;
    }
    Ok(request)
}

fn validate_request(request: &SVXRViewRequest) -> Result<(), i32> {
    canonical_request(*request).map(|_| ())
}

/// Copies compositor eye geometry. The native game thread consumes this later.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_submit_views(request: *const SVXRViewRequest) -> i32 {
    if request.is_null() { return -1; }
    let request = match canonical_request(unsafe { request.read_unaligned() }) {
        Ok(request) => request,
        Err(error) => return error,
    };
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
    // Qualified native lenses use up to20km. The guest's forward-Z f32 rebuild
    // rounds p22 near-1 before flipping depth, so reserve a bounded2.56% range
    // reconstruction envelope. This never widens the whole-clip ray budget.
    if !near.is_finite() || !far.is_finite() || near <= 0. || far <= near || far > 20_512. { return None; }
    let tan_y = 1. / p[5];
    let tan_x = 1. / p[0];
    let aspect = p[5] / p[0];
    let fields = [near, far, 1., 1., p[8], p[9], 2. * tan_y.atan().to_degrees(), tan_y, tan_x, aspect, aspect];
    fields.iter().all(|v| v.is_finite()).then_some(fields)
}

// The game's eleven scalars encode an asymmetric finite pinhole, whereas the
// physical compositor includes calibration residuals. Admit a conversion only
// after bounding its error over the ENTIRE physical clip box. 1/16384 NDC is
// at most 0.125 pixel on a 4096-wide target; reverse depth has the same absolute
// clip-space budget. The compositor keeps its original physical projection.
const PROJECTION_CLIP_ERROR_BUDGET: f64 = 1.0 / 16384.0;

fn canonical_eye_projection(original: &[f32; 16]) -> Option<([f32; 16], [f64; 3])> {
    if original.iter().any(|v| !v.is_finite()) || original[0] <= 0. || original[5] <= 0.
        || (original[11] + 1.).abs() > 1e-4 || original[10] <= 0. || original[14] <= 0.
    { return None; }
    let scale = 1.0f32 / -original[11];
    let mut canonical = original.map(|v| v * scale);
    for index in [1, 2, 3, 4, 6, 7, 12, 13, 15] { canonical[index] = 0.; }
    canonical[11] = -1.;
    projection_scalars(&canonical)?;

    // Gauss-Jordan in f64 avoids f32 inversion noise consuming the admission
    // budget. This inverse is used only for the certificate, never rendering.
    let mut inverse = [[0.0f64; 8]; 4];
    for row in 0..4 { for column in 0..4 {
        inverse[row][column] = original[column * 4 + row] as f64;
        inverse[row][column + 4] = f64::from(row == column);
    }}
    for column in 0..4 {
        let pivot = (column..4).max_by(|&a, &b|
            inverse[a][column].abs().total_cmp(&inverse[b][column].abs()))?;
        inverse.swap(column, pivot);
        let divisor = inverse[column][column];
        if !divisor.is_finite() || divisor == 0. { return None; }
        for value in &mut inverse[column] { *value /= divisor; }
        for row in 0..4 {
            if row == column { continue; }
            let factor = inverse[row][column];
            for entry in 0..8 { inverse[row][entry] -= factor * inverse[column][entry]; }
        }
    }
    let mut mapping: [f64; 16] = std::array::from_fn(|index| {
        let (column, row) = (index / 4, index % 4);
        (0..4).map(|k| canonical[k * 4 + row] as f64 * inverse[k][column + 4]).sum()
    });
    if mapping.iter().any(|v| !v.is_finite()) || mapping[15] <= 0. { return None; }
    let constant_w = mapping[15];
    for value in &mut mapping { *value /= constant_w; }
    let denominator_error: f64 = [3, 7, 11].into_iter().map(|i| mapping[i].abs()).sum();
    if !denominator_error.is_finite() || denominator_error >= 1. { return None; }
    let bounds: [f64; 3] = std::array::from_fn(|row| {
        let numerator_error: f64 = (0..4).map(|column|
            (mapping[column * 4 + row] - f64::from(column == row)).abs()).sum();
        (numerator_error + denominator_error) / (1. - denominator_error)
    });
    bounds.iter().all(|v| v.is_finite() && *v <= PROJECTION_CLIP_ERROR_BUDGET)
        .then_some((canonical, bounds))
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

fn primary_projection_snapshot(mem: &SharedMemory, source: u64, store: u64) -> Option<projection_restore::Snapshot> {
    let viewport = source.checked_add(16)?;
    let address = viewport.checked_add(816)?;
    address.checked_add(56).filter(|end| *end <= mem.data_size() as u64)?;
    let words: Vec<u32> = crate::read_bytes(mem, address, 56).chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())).collect();
    let reverse = match read_u8(mem, viewport.checked_add(928)?)? { 0 => false, 1 => true, _ => return None };
    Some(projection_restore::Snapshot { source, store,
        geometry: projection_restore::WindowGeometry::from_words(words.try_into().ok()?)?,
        fields: read_projection_scalars(mem, viewport)?,
        projection: read_matrix(mem, viewport.checked_add(PROJECTION_OFFSET)?)?, reverse })
}

/// A guest-thread operation at the live primary callback, never from Swift's
/// enable/disable thread. Re-read all authority after the stack helper calls.
/// Only crop words change; current game lens and depth direction are retained.
fn restore_primary_projection(caller: &mut Caller<'_, HostState>, mem: &SharedMemory,
    restore: projection_restore::Restore) -> Result<bool> {
    let stack_get = caller.get_export("emscripten_stack_get_current").and_then(|e| e.into_func())
        .context("missing crop restore stack getter")?.typed::<(), i64>(&mut *caller)?;
    let allocate = caller.get_export("_emscripten_stack_alloc").and_then(|e| e.into_func())
        .context("missing crop restore stack allocator")?.typed::<i64, i64>(&mut *caller)?;
    let stack_restore = caller.get_export("_emscripten_stack_restore").and_then(|e| e.into_func())
        .context("missing crop restore stack restore")?.typed::<i64, ()>(&mut *caller)?;
    let set_projection = caller.get_export("mp:XRSetViewportProjection").and_then(|e| e.into_func())
        .context("missing crop restore projection setter")?.typed::<(i64, i64), ()>(&mut *caller)?;
    let stack = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary,
        |caller| stack_get.call(caller, ()))?;
    let result = (|| {
        let scratch = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary,
            |caller| allocate.call(caller, 64))? as u64;
        anyhow::ensure!(scratch != 0 && scratch.checked_add(64).is_some_and(|end| end <= mem.data_size() as u64),
            "crop restore scratch outside guest memory");
        if read_u64(mem, ACTIVE_CVIEWPORT) != Some(restore.before.source)
            || !primary_projection_snapshot(mem, restore.before.source,
                crate::guest_call_scope::producer_store_lifetime(caller.data()))
                .is_some_and(|current| restore.before.matches(current)) { return Ok(false); }
        let viewport = restore.before.source + 16;
        let bytes: Vec<u8> = restore.fields[2..6].iter().flat_map(|v| v.to_le_bytes()).collect();
        anyhow::ensure!(crate::write_bytes(mem, scratch, &matrix_bytes(&restore.projection)), "crop restore scratch write failed");
        anyhow::ensure!(crate::write_bytes(mem, viewport + 880, &bytes), "crop restore fields write failed");
        crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary,
            |caller| set_projection.call(caller, (viewport as i64, scratch as i64)))?;
        Ok(true)
    })();
    let restored = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary,
        |caller| stack_restore.call(caller, stack));
    match (result, restored) { (Ok(value), Ok(())) => Ok(value), (Err(error), _) => Err(error), (_, Err(error)) => Err(error.into()) }
}

fn update_viewport(caller: &mut Caller<'_, HostState>, mem:&SharedMemory, viewport: u64, camera: Option<&[f32; 16]>, projection: Option<&[f32; 16]>,
    primary: Option<(projection_restore::Snapshot, u64)>, optics:Option<(OpticalAuthority,u64)>) -> Result<()> {
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
    let stack = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary, |caller| stack_get.call(caller, ()))?;
    let result = (|| {
        let scratch = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary, |caller| allocate.call(caller, 128))? as u64;
        anyhow::ensure!(scratch != 0 && scratch.checked_add(128).is_some_and(|end| end <= mem.data_size() as u64), "camera scratch outside guest memory");
        if scalars.is_some() {
            anyhow::ensure!(viewport.checked_add(929).is_some_and(|end| end <= mem.data_size() as u64), "projection scalar fields outside guest memory");
        }
        let primary_is_current = |caller: &Caller<'_, HostState>| primary.is_none_or(|(before, epoch)|
            view_epoch_is_active(epoch) && read_u64(mem, ACTIVE_CVIEWPORT) == Some(before.source)
                && primary_projection_snapshot(mem, before.source, crate::guest_call_scope::producer_store_lifetime(caller.data()))
                    .is_some_and(|current| before.matches(current)));
        anyhow::ensure!(primary_is_current(caller), "primary projection ownership changed before setter");
        let optics_is_current=||optics.is_none_or(|(before,epoch)|view_epoch_is_active(epoch)
            && optical_authority(mem,epoch)==Some(before));
        anyhow::ensure!(optics_is_current(),"native optical identity changed before setter");
        if let Some(camera) = camera { anyhow::ensure!(crate::write_bytes(mem, scratch, &matrix_bytes(camera)), "camera scratch write failed"); }
        if let Some(projection) = projection {
            anyhow::ensure!(crate::write_bytes(mem, scratch + 64, &matrix_bytes(projection)), "projection scratch write failed");
        }
        // Setters regenerate the inverse, dependent matrices and frustum planes.
        if camera.is_some() { crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary, |caller| set_camera.call(caller, (viewport as i64, scratch as i64)))?; }
        if let (Some(projection),Some(fields)) = (projection,scalars) {
            anyhow::ensure!(primary_is_current(caller), "primary projection ownership changed during camera setter");
            anyhow::ensure!(optics_is_current(),"native optical identity changed during camera setter");
            let bytes: Vec<u8> = fields.into_iter().flat_map(f32::to_le_bytes).collect();
            anyhow::ensure!(crate::write_bytes(mem,viewport + 872,&bytes), "projection scalar write failed");
            anyhow::ensure!(crate::write_bytes(mem,viewport + 928,&[reverse_z(projection).context("unsupported depth direction")? as u8]), "projection direction write failed");
            // Calling RegenerateNearFarClip afterward would replace the custom
            // matrix. SetProjection instead regenerates its inverses/frustums.
            crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary, |caller| set_projection.call(caller, (viewport as i64, (scratch + 64) as i64)))?;
            anyhow::ensure!(optics_is_current(),"native optical identity changed during projection setter");
        }
        Ok(())
    })();
    let restored = crate::guest_call_scope::call(caller, crate::guest_call_scope::Kind::Ordinary, |caller| restore.call(caller, stack));
    result.and(restored.map_err(Into::into))
}

/// Post-hook receives original parameter values saved by patch5, after camera
/// propagation has completed. The main source identity is a dynamic guest pointer.
pub fn camera_post(caller: &mut Caller<'_, HostState>, source: u64, _frame: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let active_source = read_u64(mem, ACTIVE_CVIEWPORT).unwrap_or(0);
    let mut state = lock_world_state();
    state.camera_calls += 1;
    if source == 0 || source != active_source { return Ok(()); }
    state.main_camera_calls += 1;
    let owner = state.primary_crop_owner.take();
    drop(state);
    let store = crate::guest_call_scope::producer_store_lifetime(caller.data());
    if let Some(owner) = owner {
        let restore = primary_projection_snapshot(mem, source, store).and_then(|before| owner.restore(before));
        match restore.map(|restore| restore_primary_projection(caller, mem, restore)).transpose() {
            Ok(Some(true)) => lock_world_state().primary_crop_restores += 1,
            Ok(_) => lock_world_state().primary_crop_dropped += 1,
            Err(error) => {
                let mut state = lock_world_state();
                state.primary_crop_failures += 1;
                state.last_error = Some(format!("primary projection crop restoration failed: {error}"));
                return Ok(());
            }
        }
    }
    if read_u64(mem, ACTIVE_CVIEWPORT) != Some(source) { return Ok(()); }
    let native_projection = primary_projection_snapshot(mem, source, store);
    let native_far = native_projection.and_then(|snapshot| snapshot.native_far_plane());
    let before_projection = if world_camera_enabled() { native_projection } else { None };
    let optical_authority=if world_camera_enabled(){optical_authority(mem,EPOCH.load(Ordering::Acquire))}else{None};
    let source_optics=optical_authority.and_then(|authority|before_projection.and_then(|before|
        qualified_projection_optics(authority,before))).unwrap_or_default();
    let mut state = lock_world_state();
    state.native_far_plane = native_far.map(|far| (far, Instant::now()));
    state.last_optics_qualification=Some(if source_optics.scope_sequence!=0 {"qualified same-camera optical FOV"}
        else if optical_authority.is_some() {"native camera FOV and primary projection are not exact"}
        else {"no fresh qualified local scoped camera/metadata"});
    let provenance_epoch = EPOCH.load(Ordering::Acquire);
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
        projection_from_eye: projection, depth_reverse_z, headset_applied: false, optics:crate::xr_view::ScopeOptics::default() };
    let override_matrices = if ready && world_camera_enabled() && request.is_some() {
        prepared.filter(|pose| rigid(&pose.relative) && pose.relative[12..15].iter().all(|n| n.abs() <= 5.)).map(|pose| {
            let head_from_eye = request.map(|r| r.views[eye_index as usize].head_from_eye).unwrap_or_else(identity);
            let desired_camera = multiply(&multiply(&camera, &pose.relative), &head_from_eye);
            let desired_projection = request.and_then(|r| reverse_z(&projection).and_then(|direction|
                optical_projection(for_game_depth(r.views[eye_index as usize].projection_from_eye,direction),source_optics.magnification)));
            (desired_camera, desired_projection)
        })
    } else { None };
    drop(state);
    if let Some((desired_camera, desired_projection)) = override_matrices.filter(|_| view_epoch_is_active(provenance_epoch)) {
        let crop_owner = before_projection.and_then(|before| desired_projection.and_then(|projection|
            projection_scalars(&projection).and_then(|fields| projection_restore::Owner::acquire(before, fields))));
        let Some(crop_owner) = crop_owner else {
            let mut state = lock_world_state();
            state.primary_crop_unqualified += 1;
            state.last_error = Some("primary camera crop is not qualified for safe XR ownership".to_owned());
            return Ok(());
        };
        // Retain prospective ownership before guest execution. If a setter
        // traps after scalar writes, next callback may still reconcile only
        // those exact words. A failure before any writes simply fails the
        // last-written comparison and drops the record without touching memory.
        lock_world_state().primary_crop_owner = Some(crop_owner);
        match update_viewport(caller, mem, viewport, Some(&desired_camera), desired_projection.as_ref(),
            before_projection.map(|before| (before, provenance_epoch)),
            optical_authority.filter(|_|source_optics.scope_sequence!=0).map(|authority|(authority,provenance_epoch))) {
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
                view.optics=source_optics;
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
    crate::xr_motion_aim::capture_camera(view,camera);
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

/// Reproduce the guest's certified RegeneratePerspective/SetInvertZ operation
/// from the exact scalar fields written by update_viewport. This is a bitwise
/// certificate, not a tolerance on depth or eye rays. Compositor matrices can
/// contain tiny rotation residuals and clip W one ULP from -1; the guest rebuild
/// always clears those terms and writes -1 before computing its reverse-Z row.
/// Original WASM 9617/9610: 1/tanY, then /aspect; far/(near-far), then -1-z.
fn rebuilt_reverse_eye_projection(primary: &[f32; 16]) -> Option<[f32; 16]> {
    let fields = projection_scalars(primary)?;
    let [near, far, crop_x, crop_y, offset_x, offset_y, _, tan_y, _, aspect, _] = fields;
    let scale_y = 1.0f32 / tan_y;
    let forward_z = far / (near - far);
    let forward_translation = near * forward_z;
    let rebuilt = [scale_y / aspect * crop_x, 0., 0., 0.,
        0., scale_y * crop_y, 0., 0.,
        offset_x, offset_y, -1.0f32 - forward_z, -1.,
        0., 0., -forward_translation, 0.];
    projection_scalars(&rebuilt)?;
    Some(rebuilt)
}

fn reverse_clone_view(camera: &[f32;16], projection: &[f32;16], saved: &SavedViewport) -> Option<WorldView> {
    // Live v5d opaque clones retained the exact applied primary camera while
    // rebuilding the ORIGINAL source projection with z'=w-z. Accept only that
    // exact relationship. Near-plane variants, shadow/reflection cameras and
    // arbitrary perspective matrices do not inherit the primary eye.
    if !saved.view.headset_applied || !same_matrix_bits(camera, &saved.view.game_from_eye) { return None; }
    let mut view = saved.view;
    if reverse_z(&view.projection_from_eye)? != true {
        view.projection_from_eye = flip_projection_depth(view.projection_from_eye);
    }
    let certified_original_clone = reverse_z(&saved.base_projection) == Some(false)
        && *projection == flip_projection_depth(saved.base_projection);
    // Once source scalar fields describe the requested eye, the engine can
    // regenerate this exact reverse matrix itself. This is equally narrow;
    // near-plane/FOV variants still do not match either certified projection.
    if !certified_original_clone && *projection != view.projection_from_eye
        && !rebuilt_reverse_eye_projection(&saved.view.projection_from_eye)
            .is_some_and(|rebuilt| same_matrix_bits(projection, &rebuilt)) { return None; }
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
        let result = if already_current { Ok(()) } else { update_viewport(caller,mem,viewport,None,Some(&derived.projection_from_eye),None,None) };
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
        if emission_fast_enabled() {
            thread.emission_token = if view.is_some() { next_emission_token() } else { 0 };
        }
    });
    Ok(())
}

/// Called at the exact wasm_gpu::Emit midpoint while its producer mutex is held.
/// Command bytes are complete; the GPU consumer cannot yet see the published head.
pub fn gpu_command(_caller: &mut Caller<'_, HostState>, command: u64) -> Result<()> {
    if !TRACE_ENABLED.load(Ordering::Acquire) { return Ok(()); }
    let mem = &RT.get().context("missing runtime")?.mem;
    let Some(opcode) = read_u32(mem, command) else { return Ok(()); };
    if !emission_fast_enabled() {
        // Preserve the flag-off path's original lock/TLS/record publication
        // ordering exactly, including its epoch and current-view filtering.
        let mut state = lock_world_state();
        RENDER_THREAD.with(|thread| {
            let mut thread = thread.borrow_mut();
            let epoch = EPOCH.load(Ordering::Acquire);
            if thread.epoch != epoch { *thread = RenderThread { epoch, ..Default::default() }; }
            let current = thread.current.filter(|view| !view.headset_applied || view_epoch_is_active(view.provenance_epoch));
            record_emission(&mut state, command, opcode, current);
            MARKER_COUNT.store(state.markers.len() as u64, Ordering::Release);
        });
        return Ok(());
    }
    // wasm_gpu::Emit serializes producers while this callback runs. The token
    // comparison therefore observes the immediately preceding emitted command.
    // Draws/presents and every binding transition still use the legacy mutex.
    let (current, token, thread_epoch) = RENDER_THREAD.with(|thread| {
        let mut thread = thread.borrow_mut();
        let epoch = EPOCH.load(Ordering::Acquire);
        if thread.epoch != epoch { *thread = RenderThread { epoch, ..Default::default() }; }
        let current = thread.current.filter(|view| !view.headset_applied || view_epoch_is_active(view.provenance_epoch));
        let token = if current.is_some() { thread.emission_token } else { 0 };
        (current, token, thread.epoch)
    });
    if !EMISSION_TOKEN_EXHAUSTED.load(Ordering::Acquire)
        && thread_epoch == EPOCH.load(Ordering::Acquire)
        && can_skip_emission(opcode, token, LAST_EMITTED_TOKEN.load(Ordering::Acquire))
        && current.is_some_and(|view| !view.headset_applied || view_epoch_is_active(view.provenance_epoch)) {
        // If reset wins immediately after these checks, skipping still cannot
        // republish its old binding: this branch changes diagnostics only.
        // Draws, presents and the next binding always take the locked path.
        EMISSION_FAST_COMMANDS.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let mut state = lock_world_state();
    // Reset may have completed while this callback waited for STATE. Reject
    // the entire TLS snapshot, including non-headset views, under that lock.
    // If reset starts after this epoch read, its subsequent locked invalidation
    // clears our metadata, matching the legacy path's ordering.
    let (current, token) = emission_binding_for_epoch(current, token, thread_epoch, EPOCH.load(Ordering::Acquire));
    let current = current.filter(|view| !view.headset_applied || view_epoch_is_active(view.provenance_epoch));
    let token = if current.is_some() { token } else { 0 };
    record_emission(&mut state, command, opcode, current);
    LAST_EMITTED_TOKEN.store(token_after_slow_emission(&state, current, token), Ordering::Release);
    MARKER_COUNT.store(state.markers.len() as u64, Ordering::Release);
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
        "projection_from_eye":view.projection_from_eye, "depth_reverse_z":view.depth_reverse_z, "headset_applied":view.headset_applied,
        "optics":{"scope_sequence":view.optics.scope_sequence,"magnification":view.optics.magnification,
            "reference_fov_degrees":view.optics.reference_fov_degrees,"actual_fov_degrees":view.optics.actual_fov_degrees}})
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
        "matched_current_view_calls":state.matched_current_calls,
        "gpu_commands_marked":state.emitted_commands.saturating_add(EMISSION_FAST_COMMANDS.load(Ordering::Relaxed)),
        "emission_mutex_commands":state.emitted_commands,
        "emission_fast_commands":EMISSION_FAST_COMMANDS.load(Ordering::Relaxed),
        "emission_fastpath_enabled":emission_fast_enabled(),
        "emission_token_exhausted":EMISSION_TOKEN_EXHAUSTED.load(Ordering::Acquire),
        "view_marker_changes":state.marker_changes,
        "projection_clone_updates":state.projection_clone_updates,"projection_clone_failures":state.projection_clone_failures,
        "presents_enqueued":state.emitted_presents,"presents_consumed":state.consumed_presents,"world_presents":state.world_presents,
        "conflicting_presents":state.conflicting_presents,"pending_presents":state.presents.len(),"pending_markers":state.markers.len(),
        "correlation_failed":state.correlation_failed,"last_main_viewport":state.last_main.map(|main|main.pointer),
        "last_main_view":state.last_main.map(|main|describe(main.view)),
        "last_current_view":state.last_current.map(describe),"last_error":state.last_error,
        "unmatched_near_world":unmatched,
        "rendering_method":"one eye per natural game frame; two views use alternate-eye rendering, not simultaneous stereo"});
    status["projection_clones_already_current"] = serde_json::json!(state.projection_clones_already_current);
    status["primary_crop_restoration"] = serde_json::json!({"pending":state.primary_crop_owner.is_some(),
        "restored":state.primary_crop_restores,"dropped":state.primary_crop_dropped,
        "unqualified":state.primary_crop_unqualified,"failures":state.primary_crop_failures});
    status["original_primary_projection_scalars_872_to_912"] = serde_json::json!(state.original_primary_projection_scalars);
    status["native_original_far_plane_metres"] = serde_json::json!(fresh_native_far_plane(state.native_far_plane, Instant::now()));
    status["last_optics_qualification"]=serde_json::json!(state.last_optics_qualification);
    status["eye_projection_scalars_872_to_912"] = serde_json::json!(state.last_main.and_then(|main|projection_scalars(&main.view.projection_from_eye)));
    status["motion_aim"] = crate::xr_motion_aim::status_json();
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn optics_fixture()->BTreeMap<(u64,usize),u64> {
        BTreeMap::from([((0x1000+568,8),0x2000),((0x1000+16,8),0x2000),
            ((0x2000+8,4),0xec22d348),((0x2000+28,4),0x26ea93d8),
            ((0x1000+576,8),0x3000),((0x3000+32,8),0x4000),
            ((0x4000+8,4),0x26ea93d8),((0x2000+32,4),45f32.to_bits() as u64),
            ((0x1000+160,4),10f32.to_bits() as u64)])
    }
    #[test]
    fn scope_optics_require_exact_same_camera_metadata_helper_and_original_fov() {
        let memory=optics_fixture();
        let read=|memory:&BTreeMap<(u64,usize),u64>|optical_authority_from(0x1000,9,|a,w|memory.get(&(a,w)).copied());
        let authority=read(&memory).unwrap();let optics=authority.optics().unwrap();
        assert_eq!(optics.scope_sequence,9);assert!((optics.magnification-4.7344828).abs()<0.00001);
        for key in memory.keys() {let mut changed=memory.clone();changed.insert(*key,0);assert!(read(&changed).is_none(),"{key:?}");}
        for actual in [f32::NAN,0.,4.99,45.01,f32::INFINITY] {
            let mut changed=memory.clone();changed.insert((0x1000+160,4),actual.to_bits() as u64);assert!(read(&changed).is_none());
        }
        let mut game=crop_fixture_snapshot();game.fields[6]=10.;game.fields[7]=(5f32.to_radians()).tan();
        game.projection=projection_restore::guest_projection(game.fields,false).unwrap();
        assert_eq!(qualified_projection_optics(authority,game),Some(optics));
        let mut stale=game;stale.fields[6]=f32::from_bits(10f32.to_bits()+1);
        assert!(qualified_projection_optics(authority,stale).is_none());
        stale=game;stale.projection[0]=f32::from_bits(stale.projection[0].to_bits()+1);
        assert!(qualified_projection_optics(authority,stale).is_none());
    }
    #[test]
    fn scope_optical_clip_scaling_preserves_principal_point_depth_and_identity_exactly() {
        let mut source=projection();source[0]=0.73;source[5]=0.9;source[8]=-0.27;source[9]=-0.087;
        assert!(same_matrix_bits(&optical_projection(source,1.).unwrap(),&source));
        for m in [1.01,2.,4.7344828,9.48705] {
            let optical=optical_projection(source,m).unwrap();
            assert_eq!(optical[0],source[0]*m);assert_eq!(optical[5],source[5]*m);
            for index in [2,3,6,7,8,9,10,11,14,15] {assert_eq!(optical[index].to_bits(),source[index].to_bits());}
            assert!(projection_scalars(&optical).is_some());
        }
        for m in [0.,0.99,10.01,f32::NAN,f32::INFINITY] {assert!(optical_projection(source,m).is_none());}
    }
    #[cfg(feature="jit")]
    #[test]
    fn optical_projection_uses_actual_host_setters_and_restores_current_native_zoom_crop() {
        use wasmtime::{Engine,Linker,MemoryType,Module,Store};
        use wasm_encoder::{CodeSection,EntityType,ExportKind,ExportSection,Function,FunctionSection,
            ImportSection,Instruction as I,TypeSection,ValType};
        let mut module=wasm_encoder::Module::new();let mut types=TypeSection::new();
        types.ty().function([],[]);types.ty().function([],[ValType::I64]);
        types.ty().function([ValType::I64],[ValType::I64]);types.ty().function([ValType::I64],[]);
        types.ty().function([ValType::I64,ValType::I64],[]);module.section(&types);
        let mut imports=ImportSection::new();for (name,ty) in [("run",0),("get",1),("alloc",2),("restore",3),("camera",4),("projection",4)] {
            imports.import("fixture",name,EntityType::Function(ty));
        }module.section(&imports);
        let mut functions=FunctionSection::new();functions.function(0);module.section(&functions);
        let mut exports=ExportSection::new();for(name,index)in [("outer",6),("emscripten_stack_get_current",1),("_emscripten_stack_alloc",2),
            ("_emscripten_stack_restore",3),("mp:XRSetViewportCameraMtx",4),("mp:XRSetViewportProjection",5)] {
            exports.export(name,ExportKind::Func,index);
        }module.section(&exports);
        let mut code=CodeSection::new();let mut outer=Function::new([]);outer.instruction(&I::Call(0)).instruction(&I::End);
        code.function(&outer);module.section(&code);
        let mut config=wasmtime::Config::new();config.shared_memory(true);let engine=Engine::new(&config).unwrap();
        let module=Module::new(&engine,module.finish()).unwrap();
        let mem=SharedMemory::new(&engine,MemoryType::shared(300,300)).unwrap();
        let source=8192u64;let viewport=source+16;let mut game=crop_fixture_snapshot();
        game.fields[6]=10.;game.fields[7]=5f32.to_radians().tan();
        game.projection=projection_restore::guest_projection(game.fields,false).unwrap();
        let mut geometry=[0.,0.,1.,1.,0.,1.,0.,0.,1.,1.,0.,1.,0.,0.].map(f32::to_bits);
        geometry[12]=1920;geometry[13]=1080;
        for(address,bytes)in [(viewport+816,geometry.into_iter().flat_map(u32::to_le_bytes).collect::<Vec<_>>()),
            (viewport+872,game.fields.into_iter().flat_map(f32::to_le_bytes).collect()),
            (viewport+PROJECTION_OFFSET,matrix_bytes(&game.projection).to_vec()),
            (ACTIVE_CVIEWPORT,source.to_le_bytes().to_vec())] {assert!(crate::write_bytes(&mem,address,&bytes));}
        assert!(crate::write_bytes(&mem,viewport+928,&[0]));
        sv_xr_world_camera_set_enabled(1);let epoch=view_epoch();
        let desired=optical_projection(for_game_depth(projection(),false),4.7344828).unwrap();
        let mut linker=Linker::new(&engine);let run_mem=mem.clone();
        linker.func_wrap("fixture","run",move |mut caller:Caller<'_,HostState>|->wasmtime::Result<()> {
            let store=crate::guest_call_scope::producer_store_lifetime(caller.data());
            let before=primary_projection_snapshot(&run_mem,source,store).unwrap();
            let owner=projection_restore::Owner::acquire(before,projection_scalars(&desired).unwrap()).unwrap();
            update_viewport(&mut caller,&run_mem,viewport,Some(&identity()),Some(&desired),Some((before,epoch)),None)
                .map_err(wasmtime::Error::from_anyhow)?;
            assert!(same_matrix_bits(&read_matrix(&run_mem,viewport+PROJECTION_OFFSET).unwrap(),&desired));
            assert_eq!(read_projection_scalars(&run_mem,viewport).unwrap(),projection_scalars(&desired).unwrap());
            // Next native camera update recomputes its own 30-degree lens but
            // retains XR-owned crop. Production restoration must keep that lens.
            let mut fresh=game.fields;fresh[6]=30.;fresh[7]=15f32.to_radians().tan();
            fresh[2..6].copy_from_slice(&projection_scalars(&desired).unwrap()[2..6]);
            let p=projection_restore::guest_projection(fresh,false).unwrap();
            assert!(crate::write_bytes(&run_mem,viewport+872,&fresh.into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>()));
            assert!(crate::write_bytes(&run_mem,viewport+PROJECTION_OFFSET,&matrix_bytes(&p)));
            let current=primary_projection_snapshot(&run_mem,source,store).unwrap();
            assert!(restore_primary_projection(&mut caller,&run_mem,owner.restore(current).unwrap()).unwrap());
            let restored=read_projection_scalars(&run_mem,viewport).unwrap();
            assert_eq!(restored[6],30.);assert_eq!(restored[7],fresh[7]);
            assert_eq!(restored[2..6],game.fields[2..6]);Ok(())
        }).unwrap();
        linker.func_wrap("fixture","get",||4096i64).unwrap();
        linker.func_wrap("fixture","alloc",|size:i64|{assert!(matches!(size,64|128));4096i64}).unwrap();
        linker.func_wrap("fixture","restore",|stack:i64|assert_eq!(stack,4096)).unwrap();
        for(name,offset)in [("camera",CAMERA_OFFSET),("projection",PROJECTION_OFFSET)] {
            let target=mem.clone();linker.func_wrap("fixture",name,move|pointer:i64,scratch:i64|{
                assert_eq!(pointer,viewport as i64);let bytes=crate::read_bytes(&target,scratch as u64,64);
                assert!(crate::write_bytes(&target,viewport+offset,&bytes));
            }).unwrap();
        }
        let mut store=Store::new(&engine,HostState::new(0));let instance=linker.instantiate(&mut store,&module).unwrap();
        instance.get_typed_func::<(),()>(&mut store,"outer").unwrap().call(&mut store,()).unwrap();
        sv_xr_world_camera_set_enabled(0);
    }
    fn crop_fixture_geometry() -> projection_restore::WindowGeometry {
        let mut words = [0.,0.,1.,1.,0.,1.,0.,0.,1.,1.,0.,1.,0.,0.].map(f32::to_bits);
        words[12] = 1920; words[13] = 1080;
        projection_restore::WindowGeometry::from_words(words).unwrap()
    }
    fn crop_fixture_snapshot() -> projection_restore::Snapshot {
        let fields = [0.05,10_000.,1.,1.,0.,0.,50.,0.46630767,0.8289914,1.7777778,1.7777778];
        projection_restore::Snapshot { store:1,source:8192,geometry:crop_fixture_geometry(), fields,
            projection:projection_restore::guest_projection(fields,false).unwrap(),reverse:false }
    }
    #[test]
    fn crop_cleanup_authority_survives_disable_and_recenter_metadata_invalidation() {
        let game = crop_fixture_snapshot();
        let eye = [0.1,999.9341,1.,1.,-0.267949,-0.087487,95.2,1.09,1.36,1.24,1.24];
        let mut state = State::default();
        state.primary_crop_owner = projection_restore::Owner::acquire(game,eye);
        // This is the exact invalidator called by set_active/enable/disable.
        invalidate_state(&mut state);
        assert!(state.primary_crop_owner.is_some());
        let mut live = game; live.fields[2..6].copy_from_slice(&eye[2..6]);
        live.projection = projection_restore::guest_projection(live.fields,false).unwrap();
        let plan = state.primary_crop_owner.take().unwrap().restore(live).unwrap();
        assert_eq!(plan.fields[2..6],game.fields[2..6]);
        assert!(state.primary_crop_owner.is_none());
        // Reenable binds only a new live, legitimate baseline, not old metadata.
        state.primary_crop_owner = projection_restore::Owner::acquire(
            projection_restore::Snapshot { fields:plan.fields,projection:plan.projection,..live },eye);
        invalidate_state(&mut state);
        assert!(state.primary_crop_owner.take().unwrap().restore(live).is_some());
    }

    #[cfg(feature = "jit")]
    #[test]
    fn crop_restore_calls_actual_guest_setter_and_revalidates_after_guest_stack_helpers() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering as AtomicOrdering}};
        use wasmtime::{Engine,Linker,MemoryType,Module,Store};
        use wasm_encoder::{CodeSection,EntityType,ExportKind,ExportSection,Function,FunctionSection,
            ImportSection,Instruction as I,TypeSection,ValType};
        // A real Wasmtime Caller reenters the exact production restoration
        // driver. The tiny typed setter stands in for the certified original
        // setter; no game module, GPU, runtime singleton or device is started.
        let mut module = wasm_encoder::Module::new(); let mut types = TypeSection::new();
        types.ty().function([],[ValType::I32]);
        types.ty().function([],[ValType::I64]);
        types.ty().function([ValType::I64],[ValType::I64]);
        types.ty().function([ValType::I64],[]);
        types.ty().function([ValType::I64,ValType::I64],[]); module.section(&types);
        let mut imports = ImportSection::new();
        for (name,ty) in [("run",0),("get",1),("alloc",2),("restore",3),("projection",4)] {
            imports.import("fixture",name,EntityType::Function(ty));
        } module.section(&imports);
        let mut functions = FunctionSection::new(); functions.function(0); module.section(&functions);
        let mut exports = ExportSection::new();
        for (name,index) in [("outer",5),("emscripten_stack_get_current",1),("_emscripten_stack_alloc",2),
                ("_emscripten_stack_restore",3),("mp:XRSetViewportProjection",4)] {
            exports.export(name,ExportKind::Func,index);
        } module.section(&exports);
        let mut code = CodeSection::new(); let mut outer = Function::new([]);
        outer.instruction(&I::Call(0)).instruction(&I::End); code.function(&outer); module.section(&code);
        let mut config = wasmtime::Config::new(); config.shared_memory(true);
        let engine = Engine::new(&config).unwrap(); let module = Module::new(&engine,module.finish()).unwrap();
        for changed_during_allocation in 0..5 {
            let mem = SharedMemory::new(&engine,MemoryType::shared(300,300)).unwrap();
            let source = 8192u64; let viewport = source + 16;
            let mut geometry = [0.,0.,1.,1.,0.,1.,0.,0.,1.,1.,0.,1.,0.,0.].map(f32::to_bits);
            geometry[12] = 1920; geometry[13] = 1080;
            let bytes:Vec<u8> = geometry.into_iter().flat_map(u32::to_le_bytes).collect();
            assert!(crate::write_bytes(&mem,viewport+816,&bytes));
            assert!(crate::write_bytes(&mem,viewport+928,&[0]));
            assert!(crate::write_bytes(&mem,ACTIVE_CVIEWPORT,&source.to_le_bytes()));
            let mut game = crop_fixture_snapshot();
            let eye = [0.1,999.9341,1.,1.,-0.267949,-0.087487,95.2,1.09,1.36,1.24,1.24];
            game.fields[0] = 0.15; game.fields[1] = 20_000.; // Fresh game zoom/range.
            game.projection = projection_restore::guest_projection(game.fields,false).unwrap();
            let mut live = game; live.fields[2..6].copy_from_slice(&eye[2..6]);
            live.projection = projection_restore::guest_projection(live.fields,false).unwrap();
            let bytes:Vec<u8> = live.fields.into_iter().flat_map(f32::to_le_bytes).collect();
            assert!(crate::write_bytes(&mem,viewport+872,&bytes));
            assert!(crate::write_bytes(&mem,viewport+PROJECTION_OFFSET,&matrix_bytes(&live.projection)));
            let calls = Arc::new(AtomicUsize::new(0)); let restored_stack = Arc::new(AtomicUsize::new(0));
            let stack_pointer = Arc::new(AtomicUsize::new(4096));
            let mut linker = Linker::new(&engine); let run_mem = mem.clone();
            linker.func_wrap("fixture","run",move |mut caller:Caller<'_,HostState>| -> wasmtime::Result<i32> {
                let store = crate::guest_call_scope::producer_store_lifetime(caller.data());
                let original = projection_restore::Snapshot { store,..game };
                let owner = projection_restore::Owner::acquire(original,eye).unwrap();
                let before = primary_projection_snapshot(&run_mem,source,store).unwrap();
                let plan = owner.restore(before).unwrap();
                restore_primary_projection(&mut caller,&run_mem,plan).map(i32::from)
                    .map_err(wasmtime::Error::from_anyhow)
            }).unwrap();
            let get_stack = stack_pointer.clone();
            linker.func_wrap("fixture","get",move ||get_stack.load(AtomicOrdering::Relaxed) as i64).unwrap();
            let alloc_mem = mem.clone();
            let allocate_stack = stack_pointer.clone();
            linker.func_wrap("fixture","alloc",move |size:i64| -> wasmtime::Result<i64> {
                assert_eq!(size,64);
                allocate_stack.store(8192,AtomicOrdering::Relaxed);
                match changed_during_allocation {
                    1=>assert!(crate::write_bytes(&alloc_mem,viewport+888,&0.125f32.to_le_bytes())),
                    2=>assert!(crate::write_bytes(&alloc_mem,ACTIVE_CVIEWPORT,&(source+4096).to_le_bytes())),
                    3=>return Err(wasmtime::Error::msg("fixture allocator trap after changing stack")),
                    _=>{},
                } Ok(4096i64)
            }).unwrap();
            let stack_sink = restored_stack.clone();
            let restore_stack = stack_pointer.clone();
            linker.func_wrap("fixture","restore",move |stack:i64| {
                assert_eq!(stack,4096); restore_stack.store(stack as usize,AtomicOrdering::Relaxed);
                stack_sink.fetch_add(1,AtomicOrdering::Relaxed);
            }).unwrap();
            let setter_mem = mem.clone(); let sink = calls.clone();
            linker.func_wrap("fixture","projection",move |pointer:i64,scratch:i64| -> wasmtime::Result<()> {
                assert_eq!(pointer,viewport as i64);
                // The production driver changes ONLY crop before dispatch.
                assert_eq!(read_projection_scalars(&setter_mem,viewport).unwrap().map(f32::to_bits),game.fields.map(f32::to_bits));
                let bytes = crate::read_bytes(&setter_mem,scratch as u64,64);
                assert!(crate::write_bytes(&setter_mem,viewport+PROJECTION_OFFSET,&bytes));
                sink.fetch_add(1,AtomicOrdering::Relaxed);
                if changed_during_allocation==4 {return Err(wasmtime::Error::msg("fixture setter trap"));}
                Ok(())
            }).unwrap();
            let mut store = Store::new(&engine,HostState::new(0));
            let instance = linker.instantiate(&mut store,&module).unwrap();
            let outer = instance.get_typed_func::<(),i32>(&mut store,"outer").unwrap();
            let result = outer.call(&mut store,());
            if changed_during_allocation>=3 {assert!(result.is_err());}
            else {assert_eq!(result.unwrap(),i32::from(changed_during_allocation==0));}
            assert_eq!(calls.load(AtomicOrdering::Relaxed),usize::from(matches!(changed_during_allocation,0|4)));
            assert_eq!(restored_stack.load(AtomicOrdering::Relaxed),1);
            assert_eq!(stack_pointer.load(AtomicOrdering::Relaxed),4096);
            if matches!(changed_during_allocation,0|4) {
                assert_eq!(read_projection_scalars(&mem,viewport).unwrap().map(f32::to_bits),game.fields.map(f32::to_bits));
                assert!(projection_restore::same_matrix(&read_matrix(&mem,viewport+PROJECTION_OFFSET).unwrap(),
                    &projection_restore::guest_projection(game.fields,false).unwrap()));
            } else {
                assert!(projection_restore::same_matrix(&read_matrix(&mem,viewport+PROJECTION_OFFSET).unwrap(),&live.projection));
            }
        }
    }
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
    fn native_far_authority_is_fresh_bounded_and_invalidated_with_lifecycle() {
        let now = Instant::now();
        assert_eq!(fresh_native_far_plane(Some((10_000., now)), now), 10_000.);
        assert_eq!(fresh_native_far_plane(Some((20_000., now)), now + Duration::from_secs(2)), 20_000.);
        assert_eq!(fresh_native_far_plane(Some((10_000., now)), now + Duration::from_millis(2001)), 0.);
        assert_eq!(fresh_native_far_plane(Some((10_000., now + Duration::from_secs(1))), now), 0.);
        for far in [f32::NAN, f32::INFINITY, 0., 20_001.] {
            assert_eq!(fresh_native_far_plane(Some((far, now)), now), 0.);
        }
        let mut state = State::default(); state.native_far_plane = Some((10_000., now));
        invalidate_state(&mut state); assert!(state.native_far_plane.is_none());
    }

    #[test]
    fn extended_finite_depth_preserves_actual_scalar_rebuild_and_clone_admission() {
        for near in [0.05f32, 0.1, 0.15] { for far in [10_000f32, 20_000.] {
            let mut p = projection();
            p[10] = near / (far - near); p[14] = near * far / (far - near);
            p[8] = -0.26810136; p[9] = -0.08746173;
            for reverse in [false, true] {
                let primary = for_game_depth(p, reverse);
                let fields = projection_scalars(&primary).expect("bounded original source range");
                assert!(fields[1] > far * 0.975 && fields[1] < far * 1.025);
                let rebuilt = rebuilt_reverse_eye_projection(&primary).expect("actual guest f32 scalar rebuild");
                assert!(projection_scalars(&rebuilt).is_some());
                let mut source_view = view(0, 3); source_view.projection_from_eye = primary;
                source_view.depth_reverse_z = reverse;
                let saved = SavedViewport { pointer:1000, source:984, view:source_view,
                    base_camera:identity(), base_projection:primary };
                let derived = reverse_clone_view(&identity(), &rebuilt, &saved).expect("exact extended clone admission");
                assert!(derived.depth_reverse_z);
                let expected = if reverse { primary } else { flip_projection_depth(primary) };
                assert!(same_matrix_bits(&derived.projection_from_eye, &expected));
            }
        } }
        let mut p = projection(); p[10] = 0.1 / (30_000. - 0.1); p[14] = 3_000. / (30_000. - 0.1);
        assert!(projection_scalars(&p).is_none());
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
    fn physical_reverse_clone_accepts_only_reported_f32_rebuild_rounding() {
        // Literal applied camera, forward-Z eye, original FOV, and reverse-Z
        // command from headset-current-device-v1/native-runtime-report-first.
        let camera: [f32; 16] = [-0.5062965154647827, 0.8614988923072815, 0.038516126573085785, 0.,
            -0.005707232281565666, -0.04801009222865105, 0.9988305568695068, 0.,
            0.8623406887054443, 0.5054847002029419, 0.029224129393696785, 0.,
            -56.04039001464844, -1108.0574951171875, 27.25481414794922, 1.];
        let primary: [f32; 16] = [0.7320510149002075, 2.3878741206573295e-9, 1.4761494249171392e-9, 1.4761494249171392e-9,
            3.6488073673623944e-10, 0.9125128984451294, 5.3326298932177e-9, 5.3326298932177e-9,
            -0.2679489850997925, -0.08748721331357956, -1.000100016593933, -1.,
            0., 0., -0.10001001507043839, 0.];
        let original: [f32; 16] = [1.2062851190567017, 0., 0., 0., 0., 2.1445069313049316, 0., 0.,
            0.2679489552974701, -0.08748719841241837, -1.0000100135803223, -1.,
            0., 0., -0.10000099986791611, 0.];
        let rebuilt: [f32; 16] = [0.7320510745048523, 0., 0., 0., 0., 0.9125128984451294, 0., 0.,
            -0.2679489850997925, -0.08748721331357956, 0.00010001659393310547, -1.,
            0., 0., 0.10001001507043839, 0.];
        let mut source_view = view(0, 823);
        source_view.game_from_eye = camera;
        source_view.projection_from_eye = primary;
        source_view.depth_reverse_z = false;
        let saved = SavedViewport { pointer: 357054832, source: 357054816,
            view: source_view, base_camera: camera, base_projection: original };
        let expected = flip_projection_depth(primary);
        assert_eq!(rebuilt[0].to_bits().abs_diff(expected[0].to_bits()), 1);
        assert_eq!(rebuilt_reverse_eye_projection(&primary), Some(rebuilt));
        let derived = reverse_clone_view(&camera, &rebuilt, &saved).expect("physical reverse clone");
        assert_eq!(derived.projection_from_eye, expected);
        assert!(derived.depth_reverse_z && derived.headset_applied);

        let mut bad_camera = camera; bad_camera[12] += 0.01;
        assert!(reverse_clone_view(&bad_camera, &rebuilt, &saved).is_none());
        let mut bad = rebuilt; bad[0] = f32::from_bits(expected[0].to_bits() + 2);
        assert!(reverse_clone_view(&camera, &bad, &saved).is_none());
        bad = rebuilt; bad[5] = f32::from_bits(expected[5].to_bits() + 2);
        assert!(reverse_clone_view(&camera, &bad, &saved).is_none());
        for index in [8, 9, 10, 14] {
            bad = rebuilt; bad[index] = f32::from_bits(bad[index].to_bits() + 1);
            assert!(reverse_clone_view(&camera, &bad, &saved).is_none(), "field {index}");
        }
        bad = rebuilt; bad[1] = 1e-9; // A new tiny skew is not a zeroed source residual.
        assert!(projection_scalars(&bad).is_some());
        assert!(reverse_clone_view(&camera, &bad, &saved).is_none());
        bad = rebuilt; bad[2] = 0.01; // Oblique or HUD projection.
        assert!(reverse_clone_view(&camera, &bad, &saved).is_none());
        let mut hud = saved; hud.view.headset_applied = false;
        assert!(reverse_clone_view(&camera, &rebuilt, &hud).is_none());
    }
    #[test]
    fn physical_moving_head_reverse_clones_require_exact_guest_scalar_rebuild() {
        // Two independent literal opaque views from physical runtime-report-live2
        // (2026-10-13T03:05:20Z). Their applied clip W is -0.9999999403953552;
        // the guest rebuild writes -1 and computes Z from scalar near/far.
        let fixtures: [([f32; 16], [f32; 16], [f32; 16], [f32; 16]); 2] = [
            ([0.5658861398696899, 0.8211513757705688, 0.07404985278844833, 0., 0.27570265531539917, -0.2731078565120697, 0.9216288328170776, 0., 0.7770204544067383, -0.5011212825775146, -0.38094139099121094, 0., 5311.61865234375, -5211.8193359375, 84.8995590209961, 1.],
             [0.7320510149002075, 2.083286876342072e-09, -8.604077805784982e-08, -8.604077805784982e-08, -2.8352769021466884e-08, 0.9125126600265503, -3.4188193609452355e-08, -3.4188193609452355e-08, -0.26794904470443726, -0.0874871239066124, -1.0000998973846436, -0.9999999403953552, 0., 0., -0.100009985268116, 0.],
             [0.7320509552955627, 0., 0., 0., 0., 0.9125126004219055, 0., 0., -0.26794904470443726, -0.0874871239066124, 9.989738464355469e-05, -1., 0., 0., 0.100009985268116, 0.],
             [1.2062851190567017, 0., 0., 0., 0., 2.1445069313049316, 0., 0., 0.2679489850997925, -0.08748714625835419, -1.0003000497817993, -1., 0., 0., -0.1500450074672699, 0.]),
            ([0.563595175743103, 0.8229438662528992, 0.07158104330301285, 0., 0.2811311185359955, -0.2725694179534912, 0.9201472401618958, 0., 0.7767402529716492, -0.4984671473503113, -0.3849739134311676, 0., 5311.66064453125, -5211.7666015625, 84.90534973144531, 1.],
             [0.732050895690918, -2.492706308032666e-08, 8.724691014094788e-08, 8.724691014094788e-08, -3.246348079244399e-09, 0.9125127196311951, -6.430275423241483e-09, -6.430275423241483e-09, 0.2679488956928253, -0.08748716115951538, -1.0000998973846436, -0.9999999403953552, 0., 0., -0.10001000016927719, 0.],
             [0.732050895690918, 0., 0., 0., 0., 0.9125127196311951, 0., 0., 0.2679488956928253, -0.08748716115951538, 9.989738464355469e-05, -1., 0., 0., 0.10001000016927719, 0.],
             [1.2062851190567017, 0., 0., 0., 0., 2.1445069313049316, 0., 0., -0.2679489552974701, -0.08748725056648254, -1.0003000497817993, -1., 0., 0., -0.1500450074672699, 0.]),
        ];
        for (index, (camera, primary, rebuilt, original)) in fixtures.into_iter().enumerate() {
            assert_ne!(primary[11].to_bits(), (-1.0f32).to_bits());
            assert_eq!(rebuilt[11].to_bits(), (-1.0f32).to_bits());
            assert!(same_matrix_bits(&rebuilt_reverse_eye_projection(&primary).unwrap(), &rebuilt));
            let exact_flip = flip_projection_depth(primary);
            assert_ne!(exact_flip[10].to_bits(), rebuilt[10].to_bits());
            let mut source_view = view(index as u32, 14335 + index as u64);
            source_view.game_from_eye = camera;
            source_view.projection_from_eye = primary;
            source_view.depth_reverse_z = false;
            let saved = SavedViewport { pointer: 363208336, source: 357056096,
                view: source_view, base_camera: identity(), base_projection: original };
            let derived = reverse_clone_view(&camera, &rebuilt, &saved).expect("physical scalar rebuild");
            // The typed setter restores the certified eye matrix; publication
            // keeps that actual matrix, camera, eye and head/request provenance.
            assert!(same_matrix_bits(&derived.projection_from_eye, &exact_flip));
            assert_eq!(derived, WorldView { projection_from_eye: exact_flip,
                depth_reverse_z: true, ..source_view });
            for field in [0, 5, 8, 9, 10, 11, 14] {
                for delta in [-1i64, 1] {
                    let mut altered = rebuilt;
                    altered[field] = f32::from_bits((altered[field].to_bits() as i64 + delta) as u32);
                    assert!(reverse_clone_view(&camera, &altered, &saved).is_none(),
                        "fixture {index}, changed projection field {field} by {delta} ULP");
                }
            }
            for field in [0, 12, 13, 14] {
                let mut altered = camera;
                altered[field] = f32::from_bits(altered[field].to_bits() + 1);
                assert!(reverse_clone_view(&altered, &rebuilt, &saved).is_none(),
                    "fixture {index}, changed camera field {field}");
            }
            let mut unrelated = saved; unrelated.view.headset_applied = false;
            assert!(reverse_clone_view(&camera, &rebuilt, &unrelated).is_none());
            let mut oblique = rebuilt; oblique[2] = 0.00001;
            assert!(reverse_clone_view(&camera, &oblique, &saved).is_none());
        }
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
    #[test]
    fn physical_projection_request_calibration_is_bounded_and_canonical_at_storage() {
        // Exact 288-byte request rejected with -3 by physical build10. These
        // are compositor calibration terms, not an invented malformed matrix.
        let hex = "01000000200100002102000000000000d8bcaa3358a9c3400200000000000000f2ff7f3fdf39953a0c073e3a0000000045fb94babdff7f3f1a3128bb0000008049c93ebac323283bc4ff7f3f00000080f19106bd5e9ed0bc6b343bbd0000803fad89383fe4d28ab40000000089b261b6df4a3bb76b07663f000000002c89f4b7ff168abe0edab2bd8e8ace38bcff7fbf0000000000000000e7afc93d00000000ecff7f3fe36fc1bac8a52b3a0000000024b4c13a9eff7f3ff6d64bbb00000080396e2abacde64b3bacff7f3f000000804cc3053dfa6fd0bc49cd3bbd0000803f0063383f63b8b534000000003918693668cc37b790d7653f0000000098de0038a8eb893ed77eb3bd435fce380e0080bf0000000000000000a185c93d00000000";
        let bytes: Vec<u8> = hex.as_bytes().chunks_exact(2).map(|pair|
            u8::from_str_radix(std::str::from_utf8(pair).unwrap(),16).unwrap()).collect();
        assert_eq!(bytes.len(),std::mem::size_of::<SVXRViewRequest>());
        let request=unsafe { bytes.as_ptr().cast::<SVXRViewRequest>().read_unaligned() };
        assert!(request.views.iter().all(|eye|eye.projection_from_eye[7].abs()>1e-5));
        assert_eq!(validate_request(&request),Ok(()));
        let accepted=canonical_request(request).unwrap();
        // Bit patterns exported by the independent Swift conversion of this
        // exact physical request; both sides must retain identical eye geometry.
        let swift_bits: [[u32;16];2]=[
            [0x3f3889de,0,0,0,0,0x3f6607a8,0,0,0xbe8a1724,0xbdb2da3e,0x38ce8ac5,0xbf800000,0,0,0x3dc9b01d,0],
            [0x3f3862ec,0,0,0,0,0x3f65d777,0,0,0x3e89eb99,0xbdb37ec3,0x38ce5f2c,0xbf800000,0,0,0x3dc9858b,0],
        ];
        assert_eq!((accepted.frame,accepted.presentation_time_seconds),(request.frame,request.presentation_time_seconds));
        for index in 0..2 {
            assert_eq!(accepted.views[index].head_from_eye.map(f32::to_bits),request.views[index].head_from_eye.map(f32::to_bits));
            let original=request.views[index].projection_from_eye;
            let (canonical,bounds)=canonical_eye_projection(&original).unwrap();
            assert!(bounds[0]>5e-5 && bounds.iter().all(|v|*v<=PROJECTION_CLIP_ERROR_BUDGET));
            assert_eq!(canonical[11],-1.);
            for field in [1,2,3,4,6,7,12,13,15] { assert_eq!(canonical[field],0.); }
            assert_eq!(accepted.views[index].projection_from_eye.map(f32::to_bits),canonical.map(f32::to_bits));
            assert_eq!(canonical.map(f32::to_bits),swift_bits[index]);
            assert_eq!(canonical_eye_projection(&canonical).unwrap().0.map(f32::to_bits),canonical.map(f32::to_bits));
            assert!(projection_scalars(&canonical).is_some());
            // Focal lengths, asymmetric offsets and finite depth are preserved
            // under the same homogeneous normalization as the Swift bridge.
            let scale=1.0f32 / -original[11];
            for field in [0,5,8,9,10,14] { assert_eq!(canonical[field].to_bits(),(original[field]*scale).to_bits()); }
        }
        assert!(accepted.views[0].projection_from_eye[8]<0. && accepted.views[1].projection_from_eye[8]>0.);
    }
    #[test]
    fn physical_projection_request_rejects_material_skew_obliqueness_and_depth_changes() {
        let original=projection();
        for field in [1,2,3,4,6,7,12,13,15] {
            let mut malformed=original;malformed[field]=0.01;
            assert!(canonical_eye_projection(&malformed).is_none(),"material projection field {field}");
        }
        for (field,value) in [(0,f32::NAN),(5,0.),(10,0.),(11,0.),(14,-0.1)] {
            let mut malformed=original;malformed[field]=value;
            assert!(canonical_eye_projection(&malformed).is_none(),"invalid projection field {field}");
        }
    }
    fn view(eye_index: u32, camera_sequence: u64) -> WorldView {
        WorldView { provenance_epoch:1, camera_sequence, request_frame:1, head_pose_frame:1,presentation_time_seconds:1.,eye_index,view_count:2,
            game_from_eye:identity(),game_from_stage:identity(),projection_from_eye:projection(),depth_reverse_z:true,headset_applied:true, optics:crate::xr_view::ScopeOptics::default() }
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
    fn emission_fallback_rejects_snapshot_taken_before_concurrent_reset() {
        use std::sync::Barrier;
        // No global runtime state or environment mutation: barriers force the
        // real ordering of a producer snapshot, completed reset, then fallback.
        for headset_applied in [false, true] {
            let mut old = view(0, 3); old.headset_applied = headset_applied;
            let mut new = view(0, 4); new.provenance_epoch = old.provenance_epoch + 1;
            let state = Mutex::new(State::default());
            record_emission(&mut state.lock().unwrap(), 100, 32, Some(old));
            let epoch = AtomicU64::new(old.provenance_epoch);
            let snapshot_ready = Barrier::new(2);
            let reset_complete = Barrier::new(2);
            std::thread::scope(|scope| {
                let producer = scope.spawn(|| {
                    let thread_epoch = epoch.load(Ordering::Acquire);
                    let current = Some(old);
                    snapshot_ready.wait();
                    reset_complete.wait();
                    let mut state = state.lock().unwrap();
                    let (current, token) = emission_binding_for_epoch(current, 1, thread_epoch, epoch.load(Ordering::Acquire));
                    assert!(current.is_none()); assert_eq!(token, 0);
                    record_emission(&mut state, 200, 32, current);
                    assert_eq!(token_after_slow_emission(&state, current, token), 0);
                    assert!(state.frame_world.is_none());
                    record_emission(&mut state, 300, 32, Some(new));
                    record_emission(&mut state, 320, 7, None);
                });
                snapshot_ready.wait();
                epoch.fetch_add(1, Ordering::AcqRel);
                invalidate_state(&mut state.lock().unwrap());
                reset_complete.wait();
                producer.join().unwrap();
            });
            // The legacy locked TLS check resets the old binding to None.
            let mut legacy = State::default();
            record_emission(&mut legacy, 100, 32, Some(old));
            invalidate_state(&mut legacy);
            record_emission(&mut legacy, 200, 32, None);
            record_emission(&mut legacy, 300, 32, Some(new));
            record_emission(&mut legacy, 320, 7, None);
            let state = state.into_inner().unwrap();
            assert_eq!(state.markers, legacy.markers);
            assert_eq!(state.presents, legacy.presents);
            assert_eq!(state.presents, VecDeque::from([Some(new)]));
            assert_eq!(state.conflicting_presents, 0);
            assert!(!state.frame_conflict && !state.correlation_failed);
            assert_eq!(emission_binding_for_epoch(Some(new), 2, new.provenance_epoch, new.provenance_epoch), (Some(new), 2));
            assert_eq!(emission_binding_for_epoch(None, 2, new.provenance_epoch, new.provenance_epoch), (None, 0));
        }
    }
    #[test]
    fn emission_fast_skip_crossing_reset_cannot_restore_old_binding() {
        let old = view(0, 3);
        let mut new = view(0, 4); new.provenance_epoch = old.provenance_epoch + 1;
        let mut state = State::default();
        record_emission(&mut state, 100, 32, Some(old));
        // Producer has already passed the shortcut checks when reset wins.
        assert!(can_skip_emission(23, 1, 1));
        invalidate_state(&mut state);
        // The shortcut only increments its diagnostic counter; no emission is
        // recorded. The following draw cannot shortcut and uses the new epoch.
        assert!(!can_skip_emission(32, 2, 0));
        let (current, token) = emission_binding_for_epoch(Some(new), 2, new.provenance_epoch, new.provenance_epoch);
        record_emission(&mut state, 200, 32, current);
        assert_eq!(token_after_slow_emission(&state, current, token), 2);
        record_emission(&mut state, 220, 7, None);
        assert_eq!(state.markers.get(&100), Some(&None));
        assert_eq!(state.presents, VecDeque::from([Some(new)]));
        assert_eq!(state.conflicting_presents, 0);
    }
    #[test]
    fn emission_tokens_match_legacy_ring_state_across_producers_ui_and_epoch_reset() {
        // Tokens 1 and 2 are different SetCurrent bindings, even though their
        // view values are equal. Token 3 belongs to another producer. The
        // ordered sequence models the certified guest Emit producer mutex.
        let left = view(0, 3);
        let right = view(1, 4);
        let mut after_reset = left;
        after_reset.provenance_epoch = 2;
        let events: &[(u64, u32, Option<WorldView>, u64)] = &[
            (100, 26, Some(left), 1), (120, 23, Some(left), 1),
            (140, 32, Some(left), 1), (160, 23, Some(left), 1),
            (180, 26, Some(left), 2), (200, 7, Some(left), 2),
            (220, 26, Some(right), 3), (240, 32, Some(right), 3),
            (260, 23, Some(right), 3), (280, 7, None, 0),
            (300, 23, None, 0), (320, 26, Some(after_reset), 4),
            (340, 23, Some(after_reset), 4), (360, 32, Some(after_reset), 4),
            (380, 7, None, 0),
        ];
        let mut legacy = State::default();
        let mut optimized = State::default();
        let mut previous_token = 0;
        let mut fast_count = 0u64;
        for (index, &(address, opcode, current, token)) in events.iter().enumerate() {
            if index == 11 {
                invalidate_state(&mut legacy);
                invalidate_state(&mut optimized);
                previous_token = 0;
            }
            record_emission(&mut legacy, address, opcode, current);
            if can_skip_emission(opcode, token, previous_token) {
                fast_count += 1;
            } else {
                record_emission(&mut optimized, address, opcode, current);
                previous_token = token_after_slow_emission(&optimized, current, token);
            }
            assert_eq!(optimized.markers, legacy.markers, "marker step {index}");
            assert_eq!(optimized.presents, legacy.presents, "present step {index}");
            assert_eq!(optimized.last_emitted, legacy.last_emitted, "view step {index}");
            assert_eq!(optimized.frame_world, legacy.frame_world, "draw step {index}");
            assert_eq!(optimized.frame_conflict, legacy.frame_conflict, "conflict step {index}");
            assert_eq!(optimized.correlation_failed, legacy.correlation_failed, "error step {index}");
            assert_eq!(optimized.marker_changes, legacy.marker_changes, "marker count step {index}");
            assert_eq!(optimized.emitted_presents, legacy.emitted_presents, "present count step {index}");
            assert_eq!(optimized.emitted_commands + fast_count, legacy.emitted_commands, "command count step {index}");
        }
        assert!(fast_count > 0);
        assert_eq!(legacy.presents, VecDeque::from([None, None, Some(after_reset)]));
    }
    #[test]
    fn emission_fast_path_stops_after_marker_overrun_and_never_uses_zero_token() {
        let left = view(0, 3);
        let mut state = State::default();
        state.markers = (0..MAX_MARKERS as u64).map(|address| (address, None)).collect();
        record_emission(&mut state, MAX_MARKERS as u64 + 1, 26, Some(left));
        assert!(state.correlation_failed);
        assert_eq!(token_after_slow_emission(&state, Some(left), 4), 0);
        assert!(!can_skip_emission(26, 0, 0));
        assert!(!can_skip_emission(32, 4, 4));
        assert!(!can_skip_emission(7, 4, 4));
        assert!(!can_skip_emission(26, 5, 4));
        assert!(can_skip_emission(26, 4, 4));
    }
    #[test]
    fn emission_token_allocator_never_reuses_a_token_or_wraps_to_zero() {
        let next = AtomicU64::new(1);
        let mut issued = std::collections::BTreeSet::new();
        for _ in 0..4096 {
            let token = next.fetch_update(Ordering::AcqRel, Ordering::Acquire, next_token_value)
                .expect("token space remains");
            assert_ne!(token, 0);
            assert!(issued.insert(token), "binding token reused");
        }
        assert_eq!(issued.len(), 4096);
        let last_allocatable = u64::MAX - 1;
        next.store(last_allocatable, Ordering::Release);
        assert_eq!(next.fetch_update(Ordering::AcqRel, Ordering::Acquire, next_token_value), Ok(last_allocatable));
        assert_eq!(next.load(Ordering::Acquire), u64::MAX);
        assert_eq!(next.fetch_update(Ordering::AcqRel, Ordering::Acquire, next_token_value), Err(u64::MAX));
        assert_eq!(next.load(Ordering::Acquire), u64::MAX);
        assert_eq!(next_token_value(last_allocatable), Some(u64::MAX));
        assert_eq!(next_token_value(u64::MAX), None);
        // The production allocator latches the failed update and returns zero
        // only as a disabled shortcut sentinel; zero is never issued as a token.
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

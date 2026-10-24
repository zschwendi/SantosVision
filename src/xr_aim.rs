//! Project the gameplay input ray independently in each eye. Ordinary controls
//! retain the game's camera ray; tracked arms use qualified actual weapon
//! geometry. A genuine cached barrel producer is independent of IK convergence.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::hash::{Hash,Hasher};
use wasmtime::{AsContextMut, Caller, Ref, Table};
use crate::HostState;

pub const AIM_VERSION: u32 = 1;
pub const AIM_SIZE: u32 = 104;
pub const AIM_VALID: u32 = 1;
pub const AIM_VISIBLE: u32 = 2;
pub const AIM_NATIVE_RETICLE_HIDDEN: u32 = 4;
pub const AIM_IN_VEHICLE: u32 = 8;
pub const AIM_FIRST_PERSON: u32 = 16;
pub const AIM_FREE_AIMING: u32 = 32;
/// Read-only direction sampled from the current native attached weapon.
/// This flag does not certify an impact or remove native spread/correction.
pub const AIM_TRACKED_INPUT_DIRECTION: u32 = 64;
/// Genuine native cached gun_muzzle direction, independent of desired IK pose.
pub const AIM_NATIVE_CACHED_INPUT_DIRECTION:u32=128;
const MAX_AGE: Duration = Duration::from_millis(100);
static RETICLE_READY: AtomicBool = AtomicBool::new(false);
static RETICLE_READY_EPOCH: AtomicU64 = AtomicU64::new(0);
// Certified original game.wasm element segment: table0 offset1, element48294
// is function51781, hud_commands::CommandHideHudComponentThisFrame(int).
// patch5 adds function imports but leaves element/table slot ordering intact.
const RETICLE_HIDE_TABLE_SLOT: u64 = 48_295;
const RETICLE_COMPONENT: i32 = 14;
// Original49076,49042,49078. Existing patch5 preserves these table slots.
const FIRST_PERSON_AIM_TABLE_SLOT: u64 = 45_253;
const GAMEPLAY_FOV_TABLE_SLOT: u64 = 45_127;
const AIM_ZOOM_TABLE_SLOT: u64 = 45_259;
const FIRST_PERSON_AIM_VTABLE: u64 = 4_663_048;

#[derive(Clone, Copy, Debug, Default)]
struct ScopeObservation {
    active: Option<bool>,
    equipped: Option<bool>,
    original_first_person_aim_active: Option<bool>,
    gameplay_fov_degrees: Option<f32>,
    zoom_control: Option<f32>,
    identity:Option<[u64;2]>, scope_sequence:u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ScopeCandidate {
    factory: u64, ped: u64, director: u64, camera: u64, camera_vtable: u64,
    camera_owner: u64, manager: u64, physical: u64, weapon: u64,
    info: u64, info_flags: u64, component: u64, component_info: u64, component_scope: u64,
}
impl ScopeCandidate {
    fn equipped_scope(self) -> bool { self.info_flags & 8 != 0 || self.component_scope != 0 }
    fn identity(self)->[u64;2] {
        // A comparison commitment only; no guest pointer is retained or later
        // dereferenced. Current semantics are requalified before every use.
        [0x6173687461727363u64,0x6f70746963732d31].map(|salt| {
            let mut hash=std::collections::hash_map::DefaultHasher::new();
            salt.hash(&mut hash);self.hash(&mut hash);hash.finish()
        })
    }
}

// Exact original41682 +82071's physical-weapon branch. Unknown inventory-only
// fallback stays ordinary. Camera26714 uses virtual class+16 and owner+48;
// static vtable4663048 resolves those to28491 and28280 (owner at camera+544).
// Pointers exist only in this bounded local snapshot, never in retained state.
fn scope_candidate(mut read: impl FnMut(u64, usize) -> Option<u64>) -> Option<ScopeCandidate> {
    fn nonzero(value: u64) -> Option<u64> { (value != 0).then_some(value) }
    let factory = nonzero(read(20_159_720, 8)?)?;
    let ped = nonzero(read(factory.checked_add(8)?, 8)?)?;
    let director = nonzero(read(19_235_320, 8)?)?;
    let camera = nonzero(read(director.checked_add(960)?, 8)?)?;
    let camera_vtable = nonzero(read(camera, 8)?)?;
    let camera_owner = if camera_vtable == FIRST_PERSON_AIM_VTABLE {
        let owner = read(camera.checked_add(544)?, 8)?;
        if owner != ped { return None; }
        owner
    } else { 0 };
    let manager = nonzero(read(ped.checked_add(4344)?, 8)?)?;
    let physical = nonzero(read(manager.checked_add(120)?, 8)?)?;
    let weapon = nonzero(read(physical.checked_add(864)?, 8)?)?;
    // Original82026 initializes this back-reference; reject an incoherent swap.
    if read(weapon.checked_add(88)?, 8)? != physical { return None; }
    let info = read(weapon.checked_add(64)?, 8)?;
    let info_flags = if info != 0 { read(info.checked_add(2248)?, 1)? } else { 0 };
    let (component, component_info, component_scope) = if info_flags & 8 != 0 {
        (0, 0, 0) // The original predicate short-circuits here too.
    } else {
        let component = read(weapon.checked_add(304)?, 8)?;
        if component == 0 { (0, 0, 0) } else {
            let component_info = nonzero(read(component.checked_add(8)?, 8)?)?;
            (component, component_info, read(component_info.checked_add(84)?, 4)?)
        }
    };
    Some(ScopeCandidate { factory, ped, director, camera, camera_vtable, camera_owner,
        manager, physical, weapon, info, info_flags, component, component_info, component_scope })
}

fn read_atomic_guest_word(address:u64,width:usize)->Option<u64> {
    if !matches!(width,1|4|8){return None;}
    let runtime = crate::RT.get()?;
    let (base, length) = crate::mem_base_len(&runtime.mem);
    // RT owns the shared allocation for this entire read. No mutable byte slice
    // is borrowed while other guest threads may write the memory.
    let offset = usize::try_from(address).ok()?;
    if offset % width != 0 || offset.checked_add(width)? > length { return None; }
    let pointer = unsafe { base.add(offset) };
    Some(match width {
        1 => unsafe { &*pointer.cast::<AtomicU8>() }.load(Ordering::Relaxed) as u64,
        4 => unsafe { &*pointer.cast::<AtomicU32>() }.load(Ordering::Relaxed) as u64,
        8 => unsafe { &*pointer.cast::<AtomicU64>() }.load(Ordering::Relaxed),
        _ => return None,
    })
}
fn read_scope_candidate() -> Option<ScopeCandidate> {
    scope_candidate(read_atomic_guest_word)
}

fn read_scope_identity(candidate:ScopeCandidate)->Option<[u64;2]> {
    scope_identity_from(candidate,read_atomic_guest_word)
}
fn scope_identity_from(candidate:ScopeCandidate,mut read:impl FnMut(u64,usize)->Option<u64>)->Option<[u64;2]> {
    let camera=candidate.camera;
    let metadata=read(camera.checked_add(568)?,8)?;
    let base=read(camera.checked_add(16)?,8)?;
    let helper=read(camera.checked_add(576)?,8)?;
    if metadata==0 || base!=metadata || helper==0{return None;}
    let helper_metadata=read(helper.checked_add(32)?,8)?;
    if helper_metadata==0{return None;}
    let words=[metadata,base,helper,helper_metadata,
        read(metadata.checked_add(8)?,4)?,
        read(metadata.checked_add(28)?,4)?,
        read(metadata.checked_add(32)?,4)?,
        read(helper_metadata.checked_add(8)?,4)?];
    Some(candidate.identity().map(|seed| {
        let mut hash=std::collections::hash_map::DefaultHasher::new();seed.hash(&mut hash);words.hash(&mut hash);hash.finish()
    }))
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SVXRAimSnapshot {
    pub version: u32,
    pub size: u32,
    pub sequence: u64,
    pub view_epoch: u64,
    pub capture_time_seconds: f64,
    pub age_seconds: f64,
    pub origin: [f64; 3],
    pub direction: [f64; 3],
    pub flags: u32,
    pub ped_view_mode: i32,
    pub vehicle_view_mode: i32,
    pub reserved: u32,
}
const _: () = assert!(std::mem::size_of::<SVXRAimSnapshot>() == AIM_SIZE as usize);

/// One actual native ray and its independently qualified current reference.
/// The producer camera sequence remains separate: this is not an assertion
/// that an older displayed render lease contains this newly sampled barrel.
#[repr(C)]
#[derive(Clone,Copy,Debug,Default)]
pub struct SVXRAimReferenceSnapshotV2 {
    pub version:u32,pub size:u32,pub aim:SVXRAimSnapshot,
    pub reference_camera_sequence:u64,pub reference_request_frame:u64,pub reference_head_frame:u64,
    pub producer_camera_sequence:u64,pub scope_sequence:u64,pub game_from_stage:[f64;16],
}
pub const AIM_REFERENCE_SIZE:u32=280;
const _:()=assert!(std::mem::size_of::<SVXRAimReferenceSnapshotV2>()==AIM_REFERENCE_SIZE as usize);

#[derive(Clone, Copy)]
pub(crate) struct GameplayAim {
    pub origin: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub ped_view_mode: i32,
    pub vehicle_view_mode: i32,
    pub in_vehicle: bool,
    pub armed: bool,
    pub free_aiming: bool,
    pub aiming_control: bool,
    pub shooting: bool,
}

#[derive(Default)]
struct AimState {
    sequence: u64,
    samples: u64,
    hidden: u64,
    failures: u64,
    latest: Option<(SVXRAimSnapshot, Instant)>,
    last_error: Option<String>,
    scope: Option<(ScopeObservation, u64, Instant)>,
    gameplay_ui: Option<(bool, u64, Instant)>,
    scope_sequence:u64,
}
static STATE: Mutex<AimState> = Mutex::new(AimState {
    sequence: 0, samples: 0, hidden: 0, failures: 0, latest: None, last_error: None,
    scope: None, gameplay_ui: None,scope_sequence:0,
});

fn record_scope(state:&mut AimState,mut observation:ScopeObservation,epoch:u64,now:Instant) {
    if observation.active==Some(true) && observation.identity.is_some() {
        let same=fresh_scope(state,now,epoch,true).filter(|previous|
            previous.active==Some(true) && previous.identity==observation.identity && previous.scope_sequence!=0);
        observation.scope_sequence=if let Some(previous)=same {previous.scope_sequence} else {
            if let Some(next)=state.scope_sequence.checked_add(1) {state.scope_sequence=next;next} else {0}
        };
        if observation.scope_sequence==0 {observation=ScopeObservation::default();}
    }
    state.scope=Some((observation,epoch,now));
}

/// Stack-only camera authority for the actual qualified primary callback.
/// Fresh script mode alone is insufficient: the live local camera/weapon must
/// still match the exact identity observed across the certified original calls.
pub(crate) fn qualified_scope_camera_for_optics(epoch:u64)->Option<(u64,u64)> {
    let current=|| {
        let state=STATE.lock().unwrap_or_else(|p|p.into_inner());
        fresh_scope(&state,Instant::now(),epoch,reticle_ready_for_epoch(epoch))
            .filter(|scope|scope.active==Some(true)&&scope.scope_sequence!=0)
    };
    let observation=current()?;
    let candidate=read_scope_candidate()?;
    if candidate.camera_vtable!=FIRST_PERSON_AIM_VTABLE || !candidate.equipped_scope()
        || observation.identity!=read_scope_identity(candidate) {return None;}
    let after=current()?;
    (after.identity==observation.identity && after.scope_sequence==observation.scope_sequence)
        .then_some((candidate.camera,observation.scope_sequence))
}

pub(crate) fn record_gameplay_ui_eligibility(alive: Option<bool>, epoch: u64) {
    STATE.lock().unwrap_or_else(|p| p.into_inner()).gameplay_ui = alive.map(|alive| (alive, epoch, Instant::now()));
}

fn fresh_scope(state: &AimState, now: Instant, epoch: u64, active: bool) -> Option<ScopeObservation> {
    let (scope, observed_epoch, at) = state.scope?;
    (active && epoch == observed_epoch && now.saturating_duration_since(at) <= MAX_AGE).then_some(scope)
}

/// Script-sample freshness only, not provenance for a particular rendered lease.
/// 1=verified local scoped optic,0=fresh ordinary observation,-1=unknown/stale.
#[no_mangle]
pub extern "C" fn sv_xr_native_scope_active() -> i32 {
    let epoch = crate::xr_view::view_epoch();
    let active = reticle_ready_for_epoch(epoch);
    let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    fresh_scope(&state, Instant::now(), epoch, active).and_then(|scope| scope.active)
        .map(i32::from).unwrap_or(-1)
}

fn make_snapshot(sample: GameplayAim, epoch: u64, time: f64) -> Option<SVXRAimSnapshot> {
    if !time.is_finite() || time < 0.
        || sample.origin.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        || sample.rotation_degrees.iter().any(|v| !v.is_finite() || v.abs() > 36_000.) {
        return None;
    }
    // GTA's existing multiplayer aim code uses rotation order2: heading about
    // world Z, pitch about X, forward +Y. Roll does not change the aim ray.
    let pitch = (sample.rotation_degrees[0] as f64).to_radians();
    let yaw = (sample.rotation_degrees[2] as f64).to_radians();
    let direction = [-yaw.sin() * pitch.cos(), yaw.cos() * pitch.cos(), pitch.sin()];
    if direction.iter().any(|v| !v.is_finite()) { return None; }
    let first_person = if sample.in_vehicle { sample.vehicle_view_mode == 4 } else { sample.ped_view_mode == 4 };
    let visible = sample.armed && (first_person || sample.free_aiming || sample.aiming_control || sample.shooting);
    Some(SVXRAimSnapshot {
        version: AIM_VERSION, size: AIM_SIZE, view_epoch: epoch, capture_time_seconds: time,
        origin: sample.origin.map(f64::from), direction,
        flags: AIM_VALID | if visible { AIM_VISIBLE } else { 0 }
            | if sample.in_vehicle { AIM_IN_VEHICLE } else { 0 }
            | if first_person { AIM_FIRST_PERSON } else { 0 }
            | if sample.free_aiming { AIM_FREE_AIMING } else { 0 },
        ped_view_mode: sample.ped_view_mode, vehicle_view_mode: sample.vehicle_view_mode,
        ..Default::default()
    })
}

pub(crate) fn valid_gameplay_aim(sample: GameplayAim) -> bool {
    make_snapshot(sample, 0, 0.).is_some()
}

/// The owner enables this only after its replacement reticle pipeline exists.
/// Return0=accepted,-2=invalid Boolean,-3=no active immersive camera owner.
#[no_mangle]
pub extern "C" fn sv_xr_set_reticle_ready(enabled: u32) -> i32 {
    if enabled > 1 { return -2; }
    if enabled == 0 { RETICLE_READY.store(false, Ordering::Release); clear("replacement reticle frontend disabled"); return 0; }
    if !crate::xr_view::world_camera_enabled() { return -3; }
    let epoch = crate::xr_view::view_epoch();
    RETICLE_READY_EPOCH.store(epoch, Ordering::Relaxed);
    RETICLE_READY.store(true, Ordering::Release);
    if !reticle_ready_for_epoch(epoch) { RETICLE_READY.store(false, Ordering::Release); return -3; }
    0
}

pub(crate) fn reticle_ready_for_epoch(epoch: u64) -> bool {
    RETICLE_READY.load(Ordering::Acquire) && RETICLE_READY_EPOCH.load(Ordering::Relaxed) == epoch
        && crate::xr_view::world_camera_enabled() && crate::xr_view::view_epoch_is_active(epoch)
}

pub(crate) fn clear(reason: &str) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if state.latest.is_none() && state.last_error.as_deref() == Some(reason) { return; }
    state.latest = None;
    state.last_error = Some(reason.to_owned());
}

pub(crate) fn record_failure(reason: &str) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    state.latest = None; state.failures += 1; state.last_error = Some(reason.to_owned());
}

pub(crate) fn publish(sample: GameplayAim, epoch: u64, native_reticle_hidden: bool) -> bool {
    if !native_reticle_hidden || !reticle_ready_for_epoch(epoch) || !crate::xr_view::world_camera_enabled()
        || !crate::xr_view::view_epoch_is_active(epoch) { clear("aim publication inactive/unverified"); return false; }
    let time = crate::RT.get().map(|rt| rt.start.elapsed().as_secs_f64()).unwrap_or(0.);
    let Some(mut snapshot) = make_snapshot(sample, epoch, time) else { record_failure("invalid original gameplay camera ray"); return false; };
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    state.sequence = state.sequence.saturating_add(1); state.samples += 1; state.hidden += 1;
    snapshot.sequence = state.sequence; snapshot.flags |= AIM_NATIVE_RETICLE_HIDDEN;
    state.latest = Some((snapshot, Instant::now())); state.last_error = None;
    true
}

fn fresh_snapshot(state: &AimState, now: Instant, epoch: u64, active: bool) -> Option<SVXRAimSnapshot> {
    let (mut snapshot, captured) = state.latest?;
    let age = now.checked_duration_since(captured)?;
    if !active || snapshot.view_epoch != epoch || age > MAX_AGE { return None; }
    snapshot.age_seconds = age.as_secs_f64();
    Some(snapshot)
}

fn select_snapshot(baseline: Option<SVXRAimSnapshot>, tracked_scope: bool,
    ray: Option<crate::xr_motion_arms::CurrentInputRay>, now_seconds: f64) -> Option<SVXRAimSnapshot> {
    let mut snapshot = baseline?;
    if !tracked_scope { return Some(snapshot); }
    // The old gameplay camera is not a tracked weapon ray. While a tracked
    // target is waiting for native blend/attachment agreement, show no marker.
    let ray = ray?;
    let direction = ray.direction.map(f64::from);
    let norm: f64 = direction.iter().map(|v| v * v).sum();
    if ray.camera_sequence == 0 || ray.source_age > MAX_AGE
        || ray.origin.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.)
        || direction.iter().any(|v| !v.is_finite()) || (norm - 1.).abs() > 0.002
        || !now_seconds.is_finite() || now_seconds < 0. { return None; }
    snapshot.sequence = ray.camera_sequence;
    snapshot.origin = ray.origin.map(f64::from);
    snapshot.direction = direction;
    snapshot.age_seconds = ray.source_age.as_secs_f64();
    snapshot.capture_time_seconds = (now_seconds - snapshot.age_seconds).max(0.);
    snapshot.flags |= AIM_TRACKED_INPUT_DIRECTION;
    if ray.native_cached{snapshot.flags|=AIM_NATIVE_CACHED_INPUT_DIRECTION;}
    Some(snapshot)
}

fn tracked_scope(baseline: Option<SVXRAimSnapshot>) -> bool {
    baseline.is_some_and(|s| s.flags & AIM_FIRST_PERSON != 0 && s.flags & AIM_IN_VEHICLE == 0)
        && crate::xr_motion_aim::arms_enabled() && crate::pad::spatial_controls_selected()
        && crate::pad::verified_guest_gameplay_context()
        && !crate::xr_motion_arms::reticle_native_fallback_active()
}

fn current_snapshot(baseline: Option<SVXRAimSnapshot>, epoch: u64) -> Option<SVXRAimSnapshot> {
    let tracked_scope = tracked_scope(baseline);
    let ray = tracked_scope.then(|| crate::xr_motion_arms::current_actual_input_ray(epoch)).flatten();
    let time = crate::RT.get().map(|rt| rt.start.elapsed().as_secs_f64()).unwrap_or(0.);
    select_snapshot(baseline, tracked_scope, ray, time)
}

/// Copies one fresh controller/gameplay ray. No guest calls occur here.
/// Return0=valid,1=no current ray,-1=null output,-2=ABI size mismatch.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_get_aim_snapshot(output: *mut SVXRAimSnapshot, byte_size: u32) -> i32 {
    if output.is_null() { return -1; }
    if byte_size != AIM_SIZE { return -2; }
    let epoch = crate::xr_view::view_epoch();
    let active = reticle_ready_for_epoch(epoch);
    let baseline = fresh_snapshot(&STATE.lock().unwrap_or_else(|p| p.into_inner()), Instant::now(), epoch, active);
    let snapshot = current_snapshot(baseline, epoch);
    if let Some(snapshot) = snapshot.filter(|_| reticle_ready_for_epoch(epoch)) { unsafe { output.write_unaligned(snapshot) }; 0 }
    else { unsafe { output.write_unaligned(SVXRAimSnapshot::default()) }; 1 }
}

fn reference_snapshot(baseline:Option<SVXRAimSnapshot>,ray:crate::xr_motion_arms::CurrentInputRay,time:f64)->Option<SVXRAimReferenceSnapshotV2>{
    let reference=ray.reference?;
    let aim=select_snapshot(baseline,true,Some(ray),time)?;
    if !reference.headset_applied||reference.provenance_epoch!=aim.view_epoch
        ||reference.camera_sequence<ray.camera_sequence||reference.request_frame==0||reference.head_pose_frame==0
        ||!crate::xr_motion_aim::rigid(&reference.game_from_stage){return None;}
    Some(SVXRAimReferenceSnapshotV2{version:2,size:AIM_REFERENCE_SIZE,aim,
        reference_camera_sequence:reference.camera_sequence,reference_request_frame:reference.request_frame,
        reference_head_frame:reference.head_pose_frame,producer_camera_sequence:ray.camera_sequence,
        scope_sequence:reference.optics.scope_sequence,game_from_stage:reference.game_from_stage.map(f64::from)})
}

/// Additive ABI; the104-byte getter is unchanged. Both eyes must consume this
/// ONE immutable tuple. No guest calls, old source-frame guessing or new pose.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_get_aim_reference_snapshot_v2(output:*mut SVXRAimReferenceSnapshotV2,byte_size:u32)->i32{
    if output.is_null(){return -1;}if byte_size!=AIM_REFERENCE_SIZE{return -2;}
    let epoch=crate::xr_view::view_epoch();
    let baseline=fresh_snapshot(&STATE.lock().unwrap_or_else(|p|p.into_inner()),Instant::now(),epoch,reticle_ready_for_epoch(epoch));
    let result=(||{
        if !tracked_scope(baseline){return None;}
        let ray=crate::xr_motion_arms::current_actual_input_ray(epoch)?;
        let reference=ray.reference?;
        let time=crate::RT.get()?.start.elapsed().as_secs_f64();
        let snapshot=reference_snapshot(baseline,ray,time)?;
        (reticle_ready_for_epoch(epoch)&&crate::xr_motion_aim::native_ray_primary()==Some(reference)).then_some(snapshot)
    })();
    if let Some(snapshot)=result{unsafe{output.write_unaligned(snapshot)};0}
    else{unsafe{output.write_unaligned(SVXRAimReferenceSnapshotV2::default())};1}
}

pub fn status_json() -> serde_json::Value {
    let state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let epoch = crate::xr_view::view_epoch();
    let baseline = fresh_snapshot(&state, Instant::now(), epoch, reticle_ready_for_epoch(epoch));
    let (samples, hidden, failures, last_error) = (state.samples, state.hidden, state.failures, state.last_error.clone());
    let scope_sequences_allocated = state.scope_sequence;
    let scope = fresh_scope(&state, Instant::now(), epoch, reticle_ready_for_epoch(epoch));
    let scope_age = state.scope.map(|(_, _, at)| at.elapsed().as_secs_f64());
    let scope_epoch = state.scope.map(|(_, epoch, _)| epoch);
    let gameplay_ui = state.gameplay_ui.filter(|(_, observed_epoch, at)|
        *observed_epoch == epoch && at.elapsed() <= MAX_AGE && crate::xr_view::view_epoch_is_active(epoch));
    let gameplay_ui_age = state.gameplay_ui.map(|(_, _, at)| at.elapsed().as_secs_f64());
    drop(state);
    let pending_tracked = tracked_scope(baseline);
    let snapshot = current_snapshot(baseline, epoch);
    let tracked = snapshot.is_some_and(|s| s.flags & AIM_TRACKED_INPUT_DIRECTION != 0);
    let cached=snapshot.is_some_and(|s|s.flags&AIM_NATIVE_CACHED_INPUT_DIRECTION!=0);
    serde_json::json!({"abi_version": AIM_VERSION,"byte_size": AIM_SIZE,"samples":samples,
        "native_reticle_hides":hidden,"failures":failures,"fresh":snapshot.is_some(),
        "replacement_frontend_ready":reticle_ready_for_epoch(epoch),"maximum_age_ms":MAX_AGE.as_millis(),
        "sequence":snapshot.map(|s|s.sequence),"view_epoch":snapshot.map(|s|s.view_epoch),
        "age_seconds":snapshot.map(|s|s.age_seconds),"origin":snapshot.map(|s|s.origin),
        "direction":snapshot.map(|s|s.direction),"flags":snapshot.map(|s|s.flags),
        "native_scoped_optic_active":scope.and_then(|s|s.active),
        "native_scoped_optic_equipped":scope.and_then(|s|s.equipped),
        "original_first_person_aim_active":scope.and_then(|s|s.original_first_person_aim_active),
        "scope_observation_age_seconds":scope_age,
        "scope_observation_view_epoch":scope_epoch,
        "scope_gameplay_fov_degrees":scope.and_then(|s|s.gameplay_fov_degrees),
        "scope_zoom_control":scope.and_then(|s|s.zoom_control),
        "scope_sequence":scope.map(|s|s.scope_sequence),
        // Cumulative: qualified scoped-optic sequences ever minted (entry,
        // identity, epoch or stale recovery). Zero means never qualified.
        "scope_sequences_allocated":scope_sequences_allocated,
        "scope_zoom_control_applied":false,
        "gameplay_ui_eligible":gameplay_ui.map(|(alive,_,_)|alive),
        "gameplay_ui_age_seconds":gameplay_ui_age,
        "gameplay_ui_view_epoch":gameplay_ui.map(|(_,epoch,_)|epoch),
        "last_error":last_error,"aim_source":if scope.is_some_and(|s|s.active==Some(true)) {"original native scoped optic; generic marker suppressed"} else if cached {"actual native cached gun_muzzle; current producer, bone and primary view agreement"} else if tracked {"current native attached weapon direction; exact tracked packet and native pose agreement"} else if pending_tracked {"native weapon direction pending current producer or pose agreement"} else {"original controller gameplay camera; head movement is free look"},
        "bullet_scope":if tracked {"native weapon input direction; not an impact; native spread and correction remain"} else {"camera forward ray; weapon spread, recoil, muzzle parallax and assisted target are not synthesized"}})
}

fn invoke_first_person_aim<T: AsContextMut<Data = HostState>>(store: &mut T, table: Table, certified: bool) -> anyhow::Result<bool> {
    anyhow::ensure!(certified, "scope table call requires exact embedded AOT certificate");
    let Some(Ref::Func(Some(function))) = table.get(&mut *store, FIRST_PERSON_AIM_TABLE_SLOT) else {
        anyhow::bail!("certified original first-person aim getter unavailable");
    };
    let function = function.typed::<(), i32>(&mut *store)?;
    let result = crate::guest_call_scope::call(store, crate::guest_call_scope::Kind::Ordinary, |store| function.call(store, ()))?;
    anyhow::ensure!(matches!(result, 0 | 1), "original first-person aim getter returned a non-Boolean");
    Ok(result != 0)
}

fn invoke_scope_float<T: AsContextMut<Data = HostState>>(store: &mut T, table: Table, certified: bool, slot: u64) -> anyhow::Result<f32> {
    anyhow::ensure!(certified && matches!(slot, GAMEPLAY_FOV_TABLE_SLOT | AIM_ZOOM_TABLE_SLOT), "uncertified scope diagnostic getter");
    let Some(Ref::Func(Some(function))) = table.get(&mut *store, slot) else {
        anyhow::bail!("certified original scope diagnostic getter unavailable");
    };
    let function = function.typed::<(), f32>(&mut *store)?;
    let value = crate::guest_call_scope::call(store, crate::guest_call_scope::Kind::Ordinary, |store| function.call(store, ()))?;
    anyhow::ensure!(value.is_finite() && value > 0., "invalid scope diagnostic value");
    Ok(value)
}

/// Exact supported original only; run in the selected script TLS/scratch scope.
/// Unknown or incoherent observations leave the existing ordinary policy intact.
pub(crate) fn observe_native_scoped_optic(caller: &mut Caller<'_, HostState>, epoch: u64, certified: bool) -> bool {
    let observation = (|| -> anyhow::Result<ScopeObservation> {
        anyhow::ensure!(certified && reticle_ready_for_epoch(epoch), "scope observation inactive/unverified");
        let before = read_scope_candidate().ok_or_else(|| anyhow::anyhow!("local scope/physical weapon not qualified"))?;
        let equipped = before.equipped_scope();
        if !equipped || before.camera_vtable != FIRST_PERSON_AIM_VTABLE {
            anyhow::ensure!(read_scope_candidate() == Some(before), "scope identity changed during observation");
            return Ok(ScopeObservation { active: Some(false), equipped: Some(equipped), ..Default::default() });
        }
        let table = caller.get_export("__indirect_function_table").and_then(|e|e.into_table())
            .ok_or_else(||anyhow::anyhow!("original command table unavailable"))?;
        let original_active = invoke_first_person_aim(caller, table, certified)?;
        let identity=read_scope_identity(before);
        // These are diagnostics, not magnification. Original26805 divides one
        // metadata FOV by zoom_control then clamps it; no optical ratio is assumed.
        let gameplay_fov_degrees = invoke_scope_float(caller, table, certified, GAMEPLAY_FOV_TABLE_SLOT).ok();
        let zoom_control = invoke_scope_float(caller, table, certified, AIM_ZOOM_TABLE_SLOT).ok();
        anyhow::ensure!(read_scope_candidate() == Some(before) && read_scope_identity(before)==identity && reticle_ready_for_epoch(epoch),
            "scope identity or immersive epoch changed across original getters");
        Ok(ScopeObservation { active: Some(original_active), equipped: Some(equipped),
            original_first_person_aim_active: Some(original_active), gameplay_fov_degrees, zoom_control,
            identity,scope_sequence:0 })
    })().unwrap_or_default();
    let active = observation.active == Some(true);
    record_scope(&mut STATE.lock().unwrap_or_else(|p|p.into_inner()),observation,epoch,Instant::now());
    active
}

fn invoke_reticle_hider<T: AsContextMut<Data = HostState>>(store: &mut T, table: Table, certified: bool) -> anyhow::Result<()> {
    anyhow::ensure!(certified, "reticle table call requires exact embedded AOT certificate");
    let Some(Ref::Func(Some(function))) = table.get(&mut *store, RETICLE_HIDE_TABLE_SLOT) else {
        anyhow::bail!("certified original reticle command table slot unavailable");
    };
    let function = function.typed::<i32, ()>(&mut *store)?;
    crate::guest_call_scope::call(store, crate::guest_call_scope::Kind::Ordinary, |store| function.call(store, RETICLE_COMPONENT))?;
    Ok(())
}

/// Only call inside the existing selected guest script TLS/scratch scope.
pub(crate) fn hide_original_reticle(caller: &mut Caller<'_, HostState>, certified: bool) -> anyhow::Result<()> {
    anyhow::ensure!(certified, "reticle hide requires exact embedded AOT certificate");
    anyhow::ensure!(reticle_ready_for_epoch(crate::xr_view::view_epoch()), "replacement reticle frontend is not ready");
    let table = caller.get_export("__indirect_function_table").and_then(|e| e.into_table())
        .ok_or_else(|| anyhow::anyhow!("original command table is not exported"))?;
    invoke_reticle_hider(caller, table, certified)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> GameplayAim { GameplayAim { origin:[2.,3.,4.],rotation_degrees:[0.,0.,0.],
        ped_view_mode:4,vehicle_view_mode:1,in_vehicle:false,armed:true,free_aiming:false,aiming_control:false,shooting:false } }
    #[test]
    fn controller_aim_direction_preserves_game_axes_and_first_person_mode() {
        let s = make_snapshot(sample(),7,1.).unwrap(); assert_eq!(s.origin,[2.,3.,4.]);
        assert_eq!(s.direction,[0.,1.,0.]); assert_ne!(s.flags&AIM_VISIBLE,0);
        let mut input=sample();input.rotation_degrees=[30.,17.,90.];let s=make_snapshot(input,7,1.).unwrap();
        assert!((s.direction[0]+3_f64.sqrt()/2.).abs()<1e-12);assert!(s.direction[1].abs()<1e-12);assert!((s.direction[2]-0.5).abs()<1e-12);
        input.ped_view_mode=1;assert_eq!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
        input.free_aiming=true;assert_ne!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
        input.armed=false;assert_eq!(make_snapshot(input,7,1.).unwrap().flags&AIM_VISIBLE,0);
    }
    #[test]
    fn invalid_rays_and_expired_or_foreign_scene_snapshots_fail_closed() {
        let mut input=sample();input.origin[0]=f32::NAN;assert!(make_snapshot(input,7,1.).is_none());
        input=sample();input.rotation_degrees[2]=f32::INFINITY;assert!(make_snapshot(input,7,1.).is_none());
        let now=Instant::now();let snapshot=make_snapshot(sample(),7,1.).unwrap();
        let state=AimState { latest:Some((snapshot,now)),..Default::default() };
        assert!(fresh_snapshot(&state,now+Duration::from_millis(99),7,true).is_some());
        assert!(fresh_snapshot(&state,now+Duration::from_millis(101),7,true).is_none());
        assert!(fresh_snapshot(&state,now,8,true).is_none());assert!(fresh_snapshot(&state,now,7,false).is_none());
    }
    #[test]
    fn tracked_reticle_uses_current_native_ray_and_waits_for_pose_agreement() {
        let baseline = make_snapshot(sample(), 7, 10.).unwrap();
        let ray = || crate::xr_motion_arms::CurrentInputRay { origin:[8.,9.,10.], direction:[1.,0.,0.],
            source_age:Duration::from_millis(25), camera_sequence:123,native_cached:false,reference:None };
        let tracked = select_snapshot(Some(baseline), true, Some(ray()), 10.).unwrap();
        assert_eq!(tracked.origin, [8.,9.,10.]); assert_eq!(tracked.direction, [1.,0.,0.]);
        assert_eq!(tracked.sequence, 123); assert_eq!(tracked.view_epoch, 7);
        assert_eq!(tracked.flags & !AIM_TRACKED_INPUT_DIRECTION, baseline.flags);
        assert_eq!(tracked.age_seconds, 0.025); assert_eq!(tracked.capture_time_seconds, 9.975);
        let mut cached=ray();cached.native_cached=true;let native=select_snapshot(Some(baseline),true,Some(cached),10.).unwrap();assert_ne!(native.flags&AIM_NATIVE_CACHED_INPUT_DIRECTION,0);
        assert!(select_snapshot(Some(baseline), true, None, 10.).is_none());
        let mut stale = ray(); stale.source_age = Duration::from_millis(101);
        assert!(select_snapshot(Some(baseline), true, Some(stale), 10.).is_none());
        let mut invalid = ray(); invalid.direction = [0.,0.,0.];
        assert!(select_snapshot(Some(baseline), true, Some(invalid), 10.).is_none());
        let ordinary = select_snapshot(Some(baseline), false, Some(ray()), 10.).unwrap();
        assert_eq!(ordinary.origin, baseline.origin); assert_eq!(ordinary.direction, baseline.direction);
        assert_eq!(ordinary.flags, baseline.flags);
    }

    #[test]
    fn reference_tuple_preserves_one_actual_ray_with_its_qualified_base_and_separate_producer(){
        let baseline=make_snapshot(sample(),7,10.).unwrap();
        let mut reference=crate::xr_view::WorldView{provenance_epoch:7,camera_sequence:124,request_frame:80,head_pose_frame:90,
            presentation_time_seconds:1.,eye_index:0,view_count:2,game_from_eye:crate::xr_motion_aim::identity(),
            game_from_stage:crate::xr_motion_aim::identity(),projection_from_eye:crate::xr_motion_aim::identity(),depth_reverse_z:false,headset_applied:true,optics:crate::xr_view::ScopeOptics::default()};
        reference.game_from_stage[12]=19.;reference.optics.scope_sequence=8;
        let mut ray=crate::xr_motion_arms::CurrentInputRay{origin:[8.,9.,10.],direction:[1.,0.,0.],source_age:Duration::from_millis(25),camera_sequence:123,native_cached:true,reference:Some(reference)};
        let tuple=reference_snapshot(Some(baseline),ray,10.).unwrap();
        assert_eq!((tuple.version,tuple.size,tuple.reference_camera_sequence,tuple.producer_camera_sequence),(2,280,124,123));
        assert_eq!((tuple.reference_request_frame,tuple.reference_head_frame,tuple.scope_sequence),(80,90,8));
        assert_eq!(tuple.game_from_stage[12],19.);assert_eq!(tuple.aim.origin,[8.,9.,10.]);assert_eq!(tuple.aim.sequence,123);
        reference.provenance_epoch=8;ray.reference=Some(reference);assert!(reference_snapshot(Some(baseline),ray,10.).is_none());
        reference.provenance_epoch=7;reference.camera_sequence=122;ray.reference=Some(reference);assert!(reference_snapshot(Some(baseline),ray,10.).is_none());
        reference.camera_sequence=124;reference.game_from_stage[0]=2.;ray.reference=Some(reference);assert!(reference_snapshot(Some(baseline),ray,10.).is_none());
        ray.reference=None;assert!(reference_snapshot(Some(baseline),ray,10.).is_none());
        assert_eq!(unsafe{sv_xr_get_aim_reference_snapshot_v2(std::ptr::null_mut(),AIM_REFERENCE_SIZE)},-1);
        let mut output=SVXRAimReferenceSnapshotV2::default();assert_eq!(unsafe{sv_xr_get_aim_reference_snapshot_v2(&mut output,AIM_REFERENCE_SIZE-1)},-2);
    }
    #[test]
    fn replacement_frontend_cannot_be_enabled_without_active_camera_owner() {
        assert_eq!(sv_xr_set_reticle_ready(2),-2);
        assert_eq!(sv_xr_set_reticle_ready(0),0);
        assert!(!reticle_ready_for_epoch(crate::xr_view::view_epoch()));
        assert_eq!(sv_xr_set_reticle_ready(1),-3);
        let mut output=SVXRAimSnapshot::default();
        assert_eq!(unsafe {sv_xr_get_aim_snapshot(&mut output,AIM_SIZE)},1);
        assert_eq!(unsafe {sv_xr_get_aim_snapshot(&mut output,AIM_SIZE-1)},-2);
    }
    #[test]
    fn original_table_dispatch_requires_certificate_and_exact_argument_type() {
        use std::sync::{Arc,atomic::{AtomicI32,Ordering}};
        use wasmtime::{Engine,Func,RefType,Store,TableType};
        let engine=Engine::default();let mut store=Store::new(&engine,HostState::new(0));
        let received=Arc::new(AtomicI32::new(-1));let sink=received.clone();
        let function=Func::wrap(&mut store,move |component:i32| {sink.store(component,Ordering::Relaxed);});
        let table=Table::new(&mut store,TableType::new(RefType::FUNCREF,48296,Some(48296)),Ref::Func(None)).unwrap();
        table.set(&mut store,RETICLE_HIDE_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert!(invoke_reticle_hider(&mut store,table,false).is_err());assert_eq!(received.load(Ordering::Relaxed),-1);
        invoke_reticle_hider(&mut store,table,true).unwrap();assert_eq!(received.load(Ordering::Relaxed),14);
        let wrong=Func::wrap(&mut store,|_:f32| {});table.set(&mut store,RETICLE_HIDE_TABLE_SLOT,Ref::Func(Some(wrong))).unwrap();
        assert!(invoke_reticle_hider(&mut store,table,true).is_err());
    }

    #[test]
    fn scope_requires_local_camera_and_original_equipped_scope_routes() {
        use std::collections::BTreeMap;
        let mut memory = BTreeMap::from([
            ((20_159_720,8),0x1000),((0x1008,8),0x2000),
            ((19_235_320,8),0x4000),((0x4000+960,8),0x5000),
            ((0x5000,8),FIRST_PERSON_AIM_VTABLE),((0x5000+544,8),0x2000),
            ((0x2000+4344,8),0x7000),((0x7000+120,8),0x8000),
            ((0x8000+864,8),0x9000),((0x9000+88,8),0x8000),
            ((0x9000+64,8),0xa000),((0xa000+2248,1),8),
        ]);
        let read = |memory:&BTreeMap<(u64,usize),u64>| scope_candidate(|a,w|memory.get(&(a,w)).copied());
        let original = read(&memory).unwrap(); assert!(original.equipped_scope());
        for(key,value)in [((0x5000+568,8),0xd000),((0x5000+16,8),0xd000),
            ((0x5000+576,8),0xe000),((0xe000+32,8),0xf000),
            ((0xd000+8,4),0xec22d348),((0xd000+28,4),0x26ea93d8),
            ((0xd000+32,4),45f32.to_bits() as u64),((0xf000+8,4),0x26ea93d8)] {memory.insert(key,value);}
        let identity=|m:&BTreeMap<(u64,usize),u64>|scope_identity_from(original,|a,w|m.get(&(a,w)).copied());
        let stable=identity(&memory).unwrap();
        memory.insert((0x5000+160,4),5f32.to_bits() as u64);assert_eq!(identity(&memory),Some(stable));
        memory.insert((0x5000+160,4),30f32.to_bits() as u64);assert_eq!(identity(&memory),Some(stable));
        memory.insert((0xd000+8,4),0x8c40fb35);assert_ne!(identity(&memory),Some(stable));
        memory.insert((0xd000+8,4),0xec22d348);assert_eq!(identity(&memory),Some(stable));
        // Built-in flag short-circuits an absent runtime component, like41682.
        memory.insert((0xa000+2248,1),4); assert!(read(&memory).is_none());
        memory.insert((0x9000+304,8),0); assert!(!read(&memory).unwrap().equipped_scope());
        memory.insert((0x9000+304,8),0xb000);memory.insert((0xb000+8,8),0xc000);
        memory.insert((0xc000+84,4),2);assert!(read(&memory).unwrap().equipped_scope());
        memory.insert((0xc000+84,4),0);assert!(!read(&memory).unwrap().equipped_scope());
        memory.insert((0xa000+2248,1),8);
        memory.insert((0x5000+544,8),0x3000);assert!(read(&memory).is_none());
        memory.insert((0x5000+544,8),0x2000);
        memory.insert((0x9000+88,8),0x8010);assert!(read(&memory).is_none());
        memory.insert((0x9000+88,8),0x8000);
        memory.insert((0x7000+120,8),0);assert!(read(&memory).is_none());
        memory.insert((0x7000+120,8),0x8000);
        memory.insert((0x5000,8),FIRST_PERSON_AIM_VTABLE+8);
        let different_camera=read(&memory).unwrap();assert_ne!(different_camera.camera_vtable,FIRST_PERSON_AIM_VTABLE);
        assert_ne!(Some(different_camera),Some(original));
    }

    #[test]
    fn scoped_optic_status_requires_current_epoch_and_fresh_observation() {
        let now=Instant::now();
        let state=AimState { scope:Some((ScopeObservation {active:Some(true),..Default::default()},7,now)),..Default::default() };
        assert_eq!(fresh_scope(&state,now+Duration::from_millis(99),7,true).unwrap().active,Some(true));
        assert!(fresh_scope(&state,now+Duration::from_millis(101),7,true).is_none());
        assert!(fresh_scope(&state,now,8,true).is_none());assert!(fresh_scope(&state,now,7,false).is_none());
        let unknown=AimState {scope:Some((ScopeObservation::default(),7,now)),..Default::default()};
        assert_eq!(fresh_scope(&unknown,now,7,true).unwrap().active,None);
    }
    #[test]
    fn scoped_optic_sequence_tracks_identity_entry_epoch_and_freshness_not_zoom() {
        let now=Instant::now();let mut state=AimState::default();
        let sample=ScopeObservation{active:Some(true),identity:Some([10,20]),gameplay_fov_degrees:Some(45.),..Default::default()};
        record_scope(&mut state,sample,7,now);let initial=state.scope.unwrap().0.scope_sequence;assert_ne!(initial,0);
        let zoomed=ScopeObservation{gameplay_fov_degrees:Some(10.),zoom_control:Some(7.),..sample};
        record_scope(&mut state,zoomed,7,now+Duration::from_millis(30));assert_eq!(state.scope.unwrap().0.scope_sequence,initial);
        record_scope(&mut state,ScopeObservation{identity:Some([10,21]),..zoomed},7,now+Duration::from_millis(60));
        let changed=state.scope.unwrap().0.scope_sequence;assert!(changed>initial);
        record_scope(&mut state,ScopeObservation::default(),7,now+Duration::from_millis(70));
        record_scope(&mut state,sample,7,now+Duration::from_millis(80));let reentry=state.scope.unwrap().0.scope_sequence;assert!(reentry>changed);
        record_scope(&mut state,sample,8,now+Duration::from_millis(90));let epoch=state.scope.unwrap().0.scope_sequence;assert!(epoch>reentry);
        record_scope(&mut state,sample,8,now+Duration::from_millis(191));assert!(state.scope.unwrap().0.scope_sequence>epoch);
        state.scope_sequence=u64::MAX;state.scope=None;record_scope(&mut state,sample,8,now);
        assert_eq!(state.scope.unwrap().0.scope_sequence,0);assert_eq!(state.scope_sequence,u64::MAX);
        record_scope(&mut state,sample,8,now);assert_eq!(state.scope.unwrap().0.scope_sequence,0);
    }

    #[test]
    fn original_scope_getters_require_certificate_slot_signature_and_boolean() {
        use wasmtime::{Engine,Func,RefType,Store,TableType};
        let engine=Engine::default();let mut store=Store::new(&engine,HostState::new(0));
        let table=Table::new(&mut store,TableType::new(RefType::FUNCREF,48296,Some(48296)),Ref::Func(None)).unwrap();
        let function=Func::wrap(&mut store,||1i32);table.set(&mut store,FIRST_PERSON_AIM_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert!(invoke_first_person_aim(&mut store,table,false).is_err());
        assert!(invoke_first_person_aim(&mut store,table,true).unwrap());
        let function=Func::wrap(&mut store,||0i32);table.set(&mut store,FIRST_PERSON_AIM_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert!(!invoke_first_person_aim(&mut store,table,true).unwrap());
        let function=Func::wrap(&mut store,||2i32);table.set(&mut store,FIRST_PERSON_AIM_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert!(invoke_first_person_aim(&mut store,table,true).is_err());
        let function=Func::wrap(&mut store,||30f32);table.set(&mut store,GAMEPLAY_FOV_TABLE_SLOT,Ref::Func(Some(function))).unwrap();
        assert_eq!(invoke_scope_float(&mut store,table,true,GAMEPLAY_FOV_TABLE_SLOT).unwrap(),30.);
        assert!(invoke_scope_float(&mut store,table,false,GAMEPLAY_FOV_TABLE_SLOT).is_err());
        assert!(invoke_scope_float(&mut store,table,true,FIRST_PERSON_AIM_TABLE_SLOT).is_err());
        let wrong=Func::wrap(&mut store,|_:i32|1i32);table.set(&mut store,FIRST_PERSON_AIM_TABLE_SLOT,Ref::Func(Some(wrong))).unwrap();
        assert!(invoke_first_person_aim(&mut store,table,true).is_err());
    }

    #[cfg(feature = "jit")]
    #[test]
    #[ignore = "requires the user's exact original game.wasm fixture via MUGUET_TRACE_ORIGINAL_WASM"]
    fn exact_original_and_patch5_keep_certified_reticle_table_command() {
        use wasmparser::{ElementItems,ElementKind,KnownCustom,Name,Operator,Parser,Payload,TypeRef,ValType};
        // An explicit exact-input verification; never use another downloaded or
        // guessed game image as the table-slot certificate.
        let path=std::env::var_os("MUGUET_TRACE_ORIGINAL_WASM").expect("set exact original game.wasm verification input");
        let original=std::fs::read(path).unwrap();
        extern "C" { fn CC_SHA256(data:*const std::ffi::c_void,length:u32,digest:*mut u8)->*mut u8; }
        let mut hash=[0u8;32];unsafe {CC_SHA256(original.as_ptr().cast(),original.len() as u32,hash.as_mut_ptr());}
        assert_eq!(hash.iter().map(|b|format!("{b:02x}")).collect::<String>(),
            "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0");
        fn certify(bytes:&[u8]) {
            let expected=[
                (RETICLE_HIDE_TABLE_SLOT,"hud_commands::CommandHideHudComponentThisFrame(int)",vec![ValType::I32],vec![]),
                (FIRST_PERSON_AIM_TABLE_SLOT,"camera_commands::CommandIsGameFirstPersonAimCamActive()",vec![],vec![ValType::I32]),
                (GAMEPLAY_FOV_TABLE_SLOT,"camera_commands::CommandGetGameplayCamFov()",vec![],vec![ValType::F32]),
                (AIM_ZOOM_TABLE_SLOT,"camera_commands::CommandGetGameFirstPersonAimCamZoomFactor()",vec![],vec![ValType::F32]),
                (48_135,"hud_commands::CommandDisplaySniperScopeThisFrame()",vec![],vec![]),
            ];
            let mut names=std::collections::BTreeMap::new();let mut types=Vec::new();let mut function_types=Vec::new();let mut entries=std::collections::BTreeMap::new();
            for payload in Parser::new(0).parse_all(bytes) {
                match payload.unwrap() {
                    Payload::TypeSection(s)=>for ty in s.into_iter_err_on_gc_types(){types.push(ty.unwrap());},
                    Payload::ImportSection(s)=>for import in s.into_imports(){if let TypeRef::Func(ty)=import.unwrap().ty{function_types.push(ty);}},
                    Payload::FunctionSection(s)=>for ty in s{function_types.push(ty.unwrap());},
                    Payload::ElementSection(s)=>for element in s {
                        let element=element.unwrap();
                        if let ElementKind::Active{table_index,offset_expr}=element.kind {
                            if table_index.unwrap_or(0)!=0{continue;}
                            let mut ops=offset_expr.get_operators_reader();
                            let base=match ops.read().unwrap(){Operator::I32Const{value}=>u64::try_from(value).unwrap(),Operator::I64Const{value}=>u64::try_from(value).unwrap(),other=>panic!("unsupported offset {other:?}")};
                            if let ElementItems::Functions(functions)=element.items {
                                for (offset,index) in functions.into_iter().enumerate(){let slot=base+offset as u64;if expected.iter().any(|e|e.0==slot){assert!(entries.insert(slot,index.unwrap()).is_none());}}
                            }
                        }
                    },
                    Payload::CustomSection(s)=>if let KnownCustom::Name(s)=s.as_known(){for sub in s{if let Name::Function(s)=sub.unwrap(){for name in s{let name=name.unwrap();names.insert(name.index,name.name.to_owned());}}}},
                    _=>{}
                }
            }
            for (slot,name,params,results) in expected {
                let function=entries[&slot];assert_eq!(names[&function],name);
                let ty=&types[function_types[function as usize] as usize];assert_eq!(ty.params(),params);assert_eq!(ty.results(),results);
            }
        }
        certify(&original);
        let patched=crate::patch::patch(&original,crate::mp::COMMANDS).unwrap();certify(&patched);
    }
}

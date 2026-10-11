//! Copies native tracking poses without entering the guest on the compositor thread.
//!
//! Guest camera commands are consumed by `mp::script_tick`, inside its active script
//! TLS and guest scratch allocation. The world-view bridge consumes the same
//! successful pose snapshot for its full rigid camera transform.

use std::sync::Mutex;

pub const POSE_VERSION: u32 = 1;
pub const POSE_SIZE: u32 = 96;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SVXRHeadPose {
    pub version: u32,
    pub size: u32,
    pub frame: u64,
    pub presentation_time_seconds: f64,
    pub stage_from_head: [f32; 16],
    pub tracked: u32,
    pub reserved: u32,
}

const _: () = assert!(std::mem::size_of::<SVXRHeadPose>() == POSE_SIZE as usize);

#[derive(Clone, Copy, Debug)]
pub struct CameraUpdate {
    pub frame: u64,
    pub heading_degrees: f32,
    pub pitch_degrees: f32,
    pub roll_degrees: f32,
    pub relative_position_meters: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct CameraReadback {
    pub ped_view_mode: i32,
    pub vehicle_view_mode: i32,
    pub heading_degrees: f32,
    pub pitch_degrees: f32,
}

#[derive(Default)]
struct PoseState {
    suspended: bool,
    epoch: u64,
    consumed_epoch: u64,
    latest: Option<SVXRHeadPose>,
    reference: Option<[f32; 16]>,
    consumed_frame: Option<u64>,
    consumed_pose: Option<SVXRHeadPose>,
    consumed_relative_transform: Option<[f32; 16]>,
    submitted: u64,
    applied: u64,
    failed: u64,
    recentered: u64,
    last_update: Option<CameraUpdate>,
    last_applied_update: Option<CameraUpdate>,
    last_applied_pose: Option<SVXRHeadPose>,
    last_applied_relative_transform: Option<[f32; 16]>,
    immediate_readback: Option<CameraReadback>,
    before_next_update_readback: Option<CameraReadback>,
    last_error: Option<String>,
}

static POSE: Mutex<PoseState> = Mutex::new(PoseState {
    suspended: false,
    epoch: 0,
    consumed_epoch: 0,
    latest: None,
    reference: None,
    consumed_frame: None,
    consumed_pose: None,
    consumed_relative_transform: None,
    submitted: 0,
    applied: 0,
    failed: 0,
    recentered: 0,
    last_update: None,
    last_applied_update: None,
    last_applied_pose: None,
    last_applied_relative_transform: None,
    immediate_readback: None,
    before_next_update_readback: None,
    last_error: None,
});

fn validate_pose(pose: &SVXRHeadPose) -> Result<(), i32> {
    if pose.version != POSE_VERSION || pose.size != POSE_SIZE || pose.reserved != 0
        || pose.tracked > 1
    {
        return Err(-2);
    }
    let m = &pose.stage_from_head;
    if !pose.presentation_time_seconds.is_finite() || pose.presentation_time_seconds < 0.0
        || m.iter().any(|n| !n.is_finite())
        || m[3].abs() > 1e-4 || m[7].abs() > 1e-4 || m[11].abs() > 1e-4
        || (m[15] - 1.0).abs() > 1e-4
    {
        return Err(-3);
    }
    // Tracking providers produce rigid right-handed transforms. Reject projection,
    // scale, and reflected matrices before converting them to guest camera angles.
    for a in 0..3 {
        for b in 0..3 {
            let dot = (0..3).map(|r| m[4 * a + r] * m[4 * b + r]).sum::<f32>();
            if (dot - if a == b { 1.0 } else { 0.0 }).abs() > 0.02 {
                return Err(-3);
            }
        }
    }
    let det = m[0] * (m[5] * m[10] - m[9] * m[6])
        - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2]);
    if !(0.98..=1.02).contains(&det) { return Err(-3); }
    Ok(())
}

fn submit(state: &mut PoseState, pose: SVXRHeadPose) -> Result<(), i32> {
    validate_pose(&pose)?;
    if state.suspended && pose.tracked != 0 { return Err(-5); }
    if state.latest.is_some_and(|old| pose.frame <= old.frame) { return Err(-4); }
    if pose.tracked == 0 && state.latest.is_some_and(|old| old.tracked == 1) {
        state.epoch = state.epoch.wrapping_add(1);
        state.consumed_pose = None;
        state.consumed_relative_transform = None;
        state.last_applied_update = None;
        state.last_applied_pose = None;
        state.last_applied_relative_transform = None;
    }
    state.latest = Some(pose);
    state.submitted += 1;
    Ok(())
}

/// `pose` must point to a readable, correctly sized SVXRHeadPose for this call.
/// The structure is copied; no pointer is retained and no guest code is invoked.
#[no_mangle]
pub unsafe extern "C" fn sv_xr_submit_pose(pose: *const SVXRHeadPose) -> i32 {
    if pose.is_null() { return -1; }
    let pose = unsafe { pose.read_unaligned() };
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    let tracking_lost = pose.tracked == 0 && state.latest.is_some_and(|old| old.tracked == 1);
    let result = submit(&mut state, pose);
    drop(state);
    if result.is_ok() && tracking_lost { invalidate_world_tracking(); }
    result.map_or_else(|error| error, |_| 0)
}

fn invalidate_world_tracking() {
    // camera_post reads POSE while holding its world STATE lock. Always release
    // POSE first so recenter/loss cannot invert that lock order.
    crate::xr_view::set_active(false);
    crate::world_frame::reset();
}

/// The next tracked pose becomes the new reference; an old pose is never applied
/// again merely because a user requested recentering.
#[no_mangle]
pub extern "C" fn sv_xr_recenter() {
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    recenter(&mut state);
    drop(state);
    invalidate_world_tracking();
}

fn recenter(state: &mut PoseState) {
    state.reference = None;
    state.consumed_frame = state.latest.map(|pose| pose.frame);
    state.epoch = state.epoch.wrapping_add(1);
    state.consumed_pose = None;
    state.consumed_relative_transform = None;
    state.last_applied_update = None;
    state.last_applied_pose = None;
    state.last_applied_relative_transform = None;
    state.recentered += 1;
}

fn set_state_active(state: &mut PoseState, active: bool) {
    state.suspended = !active;
    if !active {
        state.epoch = state.epoch.wrapping_add(1);
        // Clear the source epoch as well as tracking. A resumed renderer may
        // restart its frame counter, and its first fresh pose must recenter.
        state.latest = None;
        state.reference = None;
        state.consumed_frame = None;
        state.consumed_pose = None;
        state.consumed_relative_transform = None;
        state.last_update = None;
        state.last_applied_update = None;
        state.last_applied_pose = None;
        state.last_applied_relative_transform = None;
    }
}

pub(crate) fn set_active(active: bool) {
    set_state_active(&mut POSE.lock().unwrap_or_else(|poison| poison.into_inner()), active);
}

fn relative_pose(reference: &[f32; 16], pose: &SVXRHeadPose) -> CameraUpdate {
    let m = &pose.stage_from_head;
    // The head looks along -Z. R_ref^T converts the current forward/up vectors
    // into the recentered head basis; GTA's heading is positive looking left,
    // and pitch is positive looking up. Angles supplied to the commands are degrees.
    let forward: [f32; 3] = std::array::from_fn(|a| {
        -(0..3).map(|r| reference[4 * a + r] * m[8 + r]).sum::<f32>()
    });
    let right: [f32; 3] = std::array::from_fn(|a| {
        (0..3).map(|r| reference[4 * a + r] * m[r]).sum::<f32>()
    });
    let up: [f32; 3] = std::array::from_fn(|a| {
        (0..3).map(|r| reference[4 * a + r] * m[4 + r]).sum::<f32>()
    });
    let delta: [f32; 3] = std::array::from_fn(|a| m[12 + a] - reference[12 + a]);
    CameraUpdate {
        frame: pose.frame,
        heading_degrees: (-forward[0]).atan2(-forward[2]).to_degrees(),
        pitch_degrees: forward[1].clamp(-1.0, 1.0).asin().to_degrees(),
        roll_degrees: right[1].atan2(up[1]).to_degrees(),
        relative_position_meters: std::array::from_fn(|a| {
            (0..3).map(|r| reference[4 * a + r] * delta[r]).sum()
        }),
    }
}

fn relative_transform(reference: &[f32; 16], pose: &SVXRHeadPose) -> [f32; 16] {
    // Both transforms are rigid column-major matrices. The relative matrix is
    // inverse(reference) * stage_from_head, with no Euler reconstruction.
    std::array::from_fn(|index| {
        let (column, row) = (index / 4, index % 4);
        if row == 3 { return if column == 3 { 1.0 } else { 0.0 }; }
        if column == 3 {
            (0..3).map(|axis| reference[4 * row + axis]
                * (pose.stage_from_head[12 + axis] - reference[12 + axis])).sum()
        } else {
            (0..3).map(|axis| reference[4 * row + axis] * pose.stage_from_head[4 * column + axis]).sum()
        }
    })
}

fn consume(state: &mut PoseState) -> Option<CameraUpdate> {
    if state.suspended { return None; }
    let pose = state.latest?;
    if pose.tracked == 0 || state.consumed_frame == Some(pose.frame) { return None; }
    let reference = *state.reference.get_or_insert(pose.stage_from_head);
    let update = relative_pose(&reference, &pose);
    state.consumed_frame = Some(pose.frame);
    state.consumed_epoch = state.epoch;
    state.consumed_pose = Some(pose);
    state.consumed_relative_transform = Some(relative_transform(&reference, &pose));
    state.last_update = Some(update);
    Some(update)
}

/// Consume only from a selected guest script tick; primary-camera qualification
/// and application happen separately in the camera propagation hook.
pub fn take_camera_update() -> Option<CameraUpdate> {
    consume(&mut POSE.lock().unwrap_or_else(|poison| poison.into_inner()))
}

pub fn has_pending_camera_update() -> bool {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    !state.suspended && state.latest.is_some_and(|pose| pose.tracked == 1 && state.consumed_frame != Some(pose.frame))
}

/// A camera propagation hook prepares one atomic pose, then commits it only
/// after the guest setters succeed. Recenter/tracking loss invalidates the token.
#[derive(Clone, Copy)]
pub(crate) struct PreparedCameraPose {
    pub update: CameraUpdate,
    pub native: SVXRHeadPose,
    pub relative: [f32; 16],
    epoch: u64,
}

pub(crate) fn prepare_camera_pose() -> Option<PreparedCameraPose> {
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    prepare(&mut state)
}

fn prepare(state: &mut PoseState) -> Option<PreparedCameraPose> {
    if state.suspended { return None; }
    let native = state.latest.filter(|pose| pose.tracked == 1)?;
    // Recenter marks the previous latest frame consumed and clears reference.
    // Wait for the next tracking submission before establishing the new origin.
    if state.reference.is_none() && state.consumed_frame == Some(native.frame) { return None; }
    let reference = *state.reference.get_or_insert(native.stage_from_head);
    Some(PreparedCameraPose { update: relative_pose(&reference, &native), native,
        relative: relative_transform(&reference, &native), epoch: state.epoch })
}

pub(crate) fn commit_camera_pose(prepared: PreparedCameraPose) -> bool {
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    commit(&mut state, prepared)
}

fn commit(state: &mut PoseState, prepared: PreparedCameraPose) -> bool {
    if state.suspended || state.epoch != prepared.epoch
        || !state.latest.is_some_and(|pose| pose.tracked == 1)
        || state.last_applied_pose.is_some_and(|pose| pose.frame > prepared.native.frame) { return false; }
    state.consumed_frame = Some(prepared.native.frame);
    state.consumed_epoch = prepared.epoch;
    state.consumed_pose = Some(prepared.native);
    state.consumed_relative_transform = Some(prepared.relative);
    state.last_update = Some(prepared.update);
    record_result(state, Ok(()));
    true
}

pub fn record_camera_result(result: Result<(), &str>) {
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    record_result(&mut state, result);
}

fn record_result(state: &mut PoseState, result: Result<(), &str>) {
    match result {
        Ok(()) => {
            state.applied += 1;
            if !state.suspended && state.consumed_epoch == state.epoch
                && state.latest.is_some_and(|pose| pose.tracked == 1) {
                state.last_applied_update = state.last_update;
                state.last_applied_pose = state.consumed_pose;
                state.last_applied_relative_transform = state.consumed_relative_transform;
            }
            state.last_error = None;
        }
        Err(error) => { state.failed += 1; state.last_error = Some(error.to_owned()); }
    }
}

/// Read-only host snapshot: never enters the guest or advances the pose queue.
pub fn last_applied_camera_update() -> Option<CameraUpdate> {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.suspended { None } else { state.last_applied_update }
}

/// The exact consumed native pose associated with the last successful guest
/// camera update. Later compositor submissions cannot change this snapshot.
pub fn last_applied_native_pose() -> Option<SVXRHeadPose> {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.suspended { None } else { state.last_applied_pose }
}

pub fn last_applied_relative_transform() -> Option<[f32; 16]> {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.suspended { None } else { state.last_applied_relative_transform }
}

pub fn last_applied_snapshot() -> Option<(CameraUpdate, SVXRHeadPose, [f32; 16])> {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.suspended { return None; }
    let update = state.last_applied_update?;
    let native = state.last_applied_pose?;
    let relative = state.last_applied_relative_transform?;
    (update.frame == native.frame).then_some((update, native, relative))
}

pub fn is_active() -> bool {
    !POSE.lock().unwrap_or_else(|poison| poison.into_inner()).suspended
}

pub fn record_camera_readback(readback: CameraReadback, before_update: bool) {
    let mut state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    if before_update { state.before_next_update_readback = Some(readback); }
    else { state.immediate_readback = Some(readback); }
}

pub fn status_json() -> serde_json::Value {
    let state = POSE.lock().unwrap_or_else(|poison| poison.into_inner());
    let describe_readback = |readback: CameraReadback| serde_json::json!({
        "ped_view_mode": readback.ped_view_mode,
        "vehicle_view_mode": readback.vehicle_view_mode,
        "heading_degrees": readback.heading_degrees,
        "pitch_degrees": readback.pitch_degrees
    });
    serde_json::json!({
        "pose_abi_version": POSE_VERSION,
        "submitted": state.submitted,
        "suspended": state.suspended,
        "tracked": state.latest.is_some_and(|pose| pose.tracked == 1),
        "latest_frame": state.latest.map(|pose| pose.frame),
        "consumed_frame": state.consumed_frame,
        "camera_updates_applied": state.applied,
        "camera_updates_failed": state.failed,
        "recenter_count": state.recentered,
        "heading_degrees": state.last_update.map(|pose| pose.heading_degrees),
        "pitch_degrees": state.last_update.map(|pose| pose.pitch_degrees),
        "roll_degrees": state.last_update.map(|pose| pose.roll_degrees),
        "relative_position_meters": state.last_update.map(|pose| pose.relative_position_meters),
        "camera_getters_after_apply": state.immediate_readback.map(describe_readback),
        "camera_getters_before_next_apply": state.before_next_update_readback.map(describe_readback),
        "last_error": state.last_error,
        "camera_scope": "consumed head snapshots preserve the game camera mode and look; actual qualified camera offsets and alternate-eye use are reported by world_view and world_frames"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(frame: u64) -> SVXRHeadPose {
        SVXRHeadPose { version: POSE_VERSION, size: POSE_SIZE, frame,
            presentation_time_seconds: frame as f64 / 90.0, tracked: 1, reserved: 0,
            stage_from_head: [1.,0.,0.,0., 0.,1.,0.,0., 0.,0.,1.,0., 0.,0.,0.,1.] }
    }
    fn near(actual: f32, expected: f32) { assert!((actual - expected).abs() < 1e-3, "{actual} != {expected}"); }
    #[test]
    fn prepared_camera_pose_commits_exact_matrix_and_rejects_recenter_loss_or_older_success() {
        let mut state = PoseState::default();
        submit(&mut state, identity(1)).unwrap();
        let first = prepare(&mut state).unwrap();
        assert!(state.last_applied_pose.is_none());
        assert!(commit(&mut state, first));
        state.reference = None;
        assert!(prepare(&mut state).is_none()); // Recenter cannot reuse the prior pose.
        state.reference = Some(first.native.stage_from_head);
        let mut moved = identity(2); moved.stage_from_head[12] = 0.25;
        submit(&mut state, moved).unwrap();
        let second = prepare(&mut state).unwrap();
        submit(&mut state, identity(3)).unwrap(); // A later submission cannot change this token.
        assert!(commit(&mut state, second));
        near(state.last_applied_relative_transform.unwrap()[12], 0.25);
        assert_eq!(state.last_applied_pose.unwrap().frame, 2);
        assert!(!commit(&mut state, first));
        let pending = prepare(&mut state).unwrap();
        recenter(&mut state); // Recenter during guest setter, with a newer unconsumed pose pending.
        assert!(!commit(&mut state, pending));
        assert!(prepare(&mut state).is_none());
        submit(&mut state, identity(4)).unwrap();
        let pending = prepare(&mut state).unwrap();
        let mut lost = identity(5); lost.tracked = 0;
        submit(&mut state, lost).unwrap();
        assert!(!commit(&mut state, pending));
        assert!(prepare(&mut state).is_none());
    }
    #[test]
    fn reference_and_yaw_pitch_translation_use_native_column_major_basis() {
        let mut state = PoseState::default();
        submit(&mut state, identity(1)).unwrap();
        let first = consume(&mut state).unwrap();
        near(first.heading_degrees, 0.); near(first.pitch_degrees, 0.);
        assert!(consume(&mut state).is_none());
        let mut left = identity(2);
        left.stage_from_head = [0.,0.,-1.,0., 0.,1.,0.,0., 1.,0.,0.,0., 0.2,0.1,-0.3,1.];
        submit(&mut state, left).unwrap();
        let update = consume(&mut state).unwrap();
        near(update.heading_degrees, 90.); near(update.pitch_degrees, 0.);
        assert_eq!(update.relative_position_meters, [0.2,0.1,-0.3]);
        let mut up = identity(3);
        let angle = 30_f32.to_radians(); let (s,c) = angle.sin_cos();
        up.stage_from_head = [1.,0.,0.,0., 0.,c,s,0., 0.,-s,c,0., 0.,0.,0.,1.];
        submit(&mut state, up).unwrap();
        near(consume(&mut state).unwrap().pitch_degrees, 30.);
    }
    #[test]
    fn recentered_rotation_and_translation_do_not_accumulate_world_offsets() {
        let mut state = PoseState::default();
        let mut pose = identity(8);
        pose.stage_from_head = [0.,0.,-1.,0., 0.,1.,0.,0., 1.,0.,0.,0., 8.,2.,-4.,1.];
        submit(&mut state, pose).unwrap(); consume(&mut state).unwrap();
        pose.frame = 9; pose.stage_from_head[12] += 0.4;
        submit(&mut state, pose).unwrap();
        let update = consume(&mut state).unwrap();
        near(update.heading_degrees, 0.); near(update.relative_position_meters[2], 0.4);
        state.reference = None;
        assert!(consume(&mut state).is_none());
        pose.frame = 10; submit(&mut state, pose).unwrap();
        let update = consume(&mut state).unwrap();
        assert_eq!(update.relative_position_meters, [0.,0.,0.]);
    }
    #[test]
    fn scene_deactivation_discards_pending_pose_and_blocks_late_tracking_callback() {
        let mut state = PoseState::default();
        submit(&mut state, identity(200)).unwrap(); consume(&mut state).unwrap();
        submit(&mut state, identity(201)).unwrap();
        set_state_active(&mut state, false);
        assert!(consume(&mut state).is_none());
        assert_eq!(submit(&mut state, identity(202)), Err(-5));
        set_state_active(&mut state, true);
        // A new renderer has a new frame epoch; its first pose is a fresh reference.
        let mut fresh = identity(1);
        fresh.stage_from_head[12] = 3.;
        submit(&mut state, fresh).unwrap();
        assert_eq!(consume(&mut state).unwrap().relative_position_meters, [0., 0., 0.]);
    }
    #[test]
    fn tracking_loss_old_frames_and_invalid_matrices_cannot_drive_camera() {
        let mut state = PoseState::default();
        submit(&mut state, identity(1)).unwrap(); consume(&mut state).unwrap();
        assert_eq!(submit(&mut state, identity(1)), Err(-4));
        let mut untracked = identity(2); untracked.tracked = 0;
        submit(&mut state, untracked).unwrap(); assert!(consume(&mut state).is_none());
        let loss_epoch = state.epoch;
        untracked.frame = 3;
        submit(&mut state, untracked).unwrap();
        assert_eq!(state.epoch, loss_epoch, "repeated untracked callbacks must not restart the tracking epoch");
        let mut bad = identity(4); bad.stage_from_head[0] = f32::NAN;
        assert_eq!(submit(&mut state, bad), Err(-3));
        bad = identity(3); bad.stage_from_head[0] = -1.;
        assert_eq!(submit(&mut state, bad), Err(-3));
        bad = identity(3); bad.stage_from_head[0] = 2.;
        assert_eq!(submit(&mut state, bad), Err(-3));
    }

    #[test]
    fn successful_snapshot_keeps_consumed_matrix_and_tracking_loss_cannot_revive_it() {
        let mut state = PoseState::default();
        let mut reference = identity(1);
        reference.stage_from_head = [0.,0.,-1.,0., 0.,1.,0.,0., 1.,0.,0.,0., 8.,2.,-4.,1.];
        submit(&mut state, reference).unwrap(); consume(&mut state).unwrap();
        let mut next = reference;
        next.frame = 2; next.stage_from_head[12] += 0.4;
        submit(&mut state, next).unwrap(); consume(&mut state).unwrap();
        // The compositor can submit a newer pose while the guest consumes frame 2.
        let mut newer = next; newer.frame = 3; newer.stage_from_head[12] += 4.;
        submit(&mut state, newer).unwrap(); record_result(&mut state, Ok(()));
        assert_eq!(state.last_applied_pose.unwrap().frame, 2);
        let exact = state.last_applied_relative_transform.unwrap();
        near(exact[0], 1.); near(exact[5], 1.); near(exact[10], 1.);
        near(exact[12], 0.); near(exact[13], 0.); near(exact[14], 0.4);
        newer.frame = 4; newer.tracked = 0;
        submit(&mut state, newer).unwrap(); record_result(&mut state, Ok(()));
        assert!(state.last_applied_pose.is_none());
        assert!(state.last_applied_relative_transform.is_none());
    }
}

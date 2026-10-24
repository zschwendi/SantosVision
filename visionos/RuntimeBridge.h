#ifndef SANTOSVISION_RUNTIME_BRIDGE_H
#define SANTOSVISION_RUNTIME_BRIDGE_H

#include <stddef.h>
#include <stdint.h>
#include "WorldBridge.h"

/* Implemented by the native Rust archive. UTF-8 output buffers are NUL
 * terminated. Return values: 0 success, -1 runtime error, -2 buffer too small.
 * Engine startup and module loading happen on the Swift worker queue.
 */
uint32_t sv_runtime_abi_version(void);
int32_t sv_runtime_probe(char *json, size_t capacity);
int32_t sv_runtime_start(const char *game_root, const char *save_root,
                         char *error, size_t capacity);
int32_t sv_runtime_status(char *json, size_t capacity);
int32_t sv_runtime_set_active(uint32_t active);
/* Bounded menu key pulse: 1 Escape, 2 Enter, 3 Space/Apply, 4 Yes/Y, 5 No/N.
 * Returns -1 for unsupported action or full queue. */
int32_t sv_runtime_menu_action(uint32_t action);
/* Atomic cumulative physical Start presses, shared with full status JSON. */
uint64_t sv_native_menu_press_count_v1(void);
/* Verified GTA front-end state for the Start panel: 0 unknown (unverified
 * guest; the panel stays modal), 1 gameplay, 2 GTA pause/Settings menu open. */
uint32_t sv_native_menu_context_v1(void);
/* UI D-pad holds: action8..11, pressed0/1. Refresh <250ms;0success,-1invalid. */
int32_t sv_native_utility_dpad_v1(uint32_t action, uint32_t pressed);
/* Swift input ownership: V1 remains0/1. V2:0game,1modal,2utility.
 * Utility still blocks gameplay actions; Rust admits character-wheel axes
 * only after its real held-Down mapping has emitted AltLeft. */
uint32_t sv_native_controls_owns_input_v1(void);
uint32_t sv_native_controls_input_policy_v2(void);
/* Pre-start live 2D window surface. Pass a live CAMetalLayer pointer on visionOS.
 * Metal retains the layer; Swift must keep its owning UIView alive and attached
 * for presentation. Returns 0 on success, -1 for null, repeated, unsupported or
 * during/after runtime or GPU startup. Call before bootstrap/start; no replacement. */
int32_t sv_runtime_set_window_layer(void *metal_layer);
/* Attachment/visibility gate; never replaces or suspends the retained layer. */
int32_t sv_runtime_set_window_visible(uint32_t visible);
/* Pre-start app request for the immutable frozen-depth handoff. 0/1 only;
 * explicit MUGUET_TRANSFER_FROZEN_DEPTH=0/1 overrides it for diagnostics.
 * Returns -1 during or after native startup. */
int32_t sv_world_set_frozen_depth_transfer(uint32_t enabled);
/* Pre-start app request for render preparation, uniform staging, and bind-group
 * preparation. Initially OFF. Exact 1/0 diagnostic environment values override
 * each experiment separately. Returns -1 during or after native startup. */
int32_t sv_runtime_set_draw_setup_overhead(uint32_t enabled);
/* Queue readback of the next actual game GPU presentation to a PPM file. */
int32_t sv_gpu_capture_next(const char *output_ppm_path);

/* Headset mailbox only. The game script thread consumes this snapshot; the
 * Swift compositor thread never calls guest camera functions directly.
 */
typedef struct {
    uint32_t version;
    uint32_t size;
    uint64_t frame;
    double presentation_time_seconds;
    float stage_from_head[16];
    uint32_t tracked;
    uint32_t reserved;
} SVXRHeadPose;
_Static_assert(sizeof(SVXRHeadPose) == 96, "SVXRHeadPose ABI layout must match Rust");
int32_t sv_xr_submit_pose(const SVXRHeadPose *pose);
void sv_xr_recenter(void);

typedef struct {
    float head_from_eye[16];
    float projection_from_eye[16];
} SVXREyeView;
typedef struct {
    uint32_t version;
    uint32_t size;
    uint64_t frame;
    double presentation_time_seconds;
    uint32_t view_count;
    uint32_t reserved;
    SVXREyeView views[2];
} SVXRViewRequest;
_Static_assert(sizeof(SVXRViewRequest) == 288, "SVXRViewRequest ABI layout must match Rust");
_Static_assert(sizeof(SVWorldFrame) == 288, "SVWorldFrame ABI layout must match Rust");
int32_t sv_xr_submit_views(const SVXRViewRequest *request);
int32_t sv_xr_world_camera_set_enabled(uint32_t enabled);

/* Current gameplay camera or tracked native weapon input ray; never an impact. */
typedef struct {
    uint32_t version, size;
    uint64_t sequence, view_epoch;
    double capture_time_seconds, age_seconds;
    double origin[3], direction[3];
    uint32_t flags;
    int32_t ped_view_mode, vehicle_view_mode;
    uint32_t reserved;
} SVXRAimSnapshot;
_Static_assert(sizeof(SVXRAimSnapshot) == 104, "SVXRAimSnapshot ABI layout must match Rust");
int32_t sv_xr_get_aim_snapshot(SVXRAimSnapshot *snapshot, uint32_t byte_size);
/* One actual tracked barrel ray and its independently qualified common reference.
   producer_camera_sequence is an observation tag, not an older image-pose proof. */
typedef struct {
    uint32_t version, size;
    SVXRAimSnapshot aim;
    uint64_t reference_camera_sequence, reference_request_frame, reference_head_frame;
    uint64_t producer_camera_sequence, scope_sequence;
    double game_from_stage[16];
} SVXRAimReferenceSnapshotV2;
_Static_assert(sizeof(SVXRAimReferenceSnapshotV2) == 280, "SVXRAimReferenceSnapshotV2 ABI must match Rust");
int32_t sv_xr_get_aim_reference_snapshot_v2(SVXRAimReferenceSnapshotV2 *snapshot, uint32_t byte_size);
/* 1=current qualified native scope, 0=ordinary, -1=unknown/stale. */
int32_t sv_xr_native_scope_active(void);
/* Fresh exact original game-camera far plane in metres, or 0 when unavailable. */
float sv_xr_native_far_plane_metres(void);
/* Immutable source optics belonging to one acquired world lease. */
typedef struct {
    uint32_t abi_version, byte_size;
    uint64_t lease, provenance_epoch, game_frame, request_frame, head_frame;
    uint64_t camera_sequence, scope_sequence;
    double presentation_time;
    float magnification, reference_fov_degrees, actual_fov_degrees;
    uint32_t source_eye, view_count, flags;
    uint32_t reserved[4];
} SVWorldOpticsFrame;
_Static_assert(sizeof(SVWorldOpticsFrame) == 112, "SVWorldOpticsFrame ABI layout must match Rust");
int32_t sv_world_frame_optics_v1(uint64_t world_lease, SVWorldOpticsFrame *optics);
int32_t sv_xr_set_reticle_ready(uint32_t enabled);
/* Qualified tracked-arm mode, including the signed build's normal-launch default. */
uint32_t sv_xr_motion_arms_enabled_v1(void);
/* Explicit left grip-to-native-hand calibration. phase: 0 idle, 1 card open
   waiting, 2 GTA's own left hand shown, 3 capturing, 4 captured. failure: last
   failed capture reason for result_session, cleared by the next capture.
   flags: 1 supported, 2 eligible, 4 committed calibration present, 8 controller
   fingerprint matches, 16 native hand shown, 32 calibrated target recently
   applied. A successful capture (phase 4) stays pending until committed. */
typedef struct {
    uint32_t version, size;
    uint64_t session, result_session;
    uint32_t phase, reason, samples, required_samples, flags;
    float translation_m, rotation_degrees;
    uint32_t failure;
} SVLeftWristState;
_Static_assert(sizeof(SVLeftWristState) == 56, "SVLeftWristState ABI layout must match Rust");
typedef struct {
    uint32_t version, size;
    uint64_t controller;
    uint32_t bone_count;
    uint16_t hand, aux;
    uint32_t samples;
    float forearm_m;
    float hand_from_aux[12], grip_from_hand[12];
} SVLeftWristCalibration;
_Static_assert(sizeof(SVLeftWristCalibration) == 128, "SVLeftWristCalibration ABI layout must match Rust");
/* GameController product category + vendor + fixed left hand fingerprint of
   the current left controller; 0 none. Not a per-unit serial: two units of the
   same model share it. A change restarts, and 0 fails, an active capture. */
int32_t sv_xr_left_wrist_controller_v1(uint64_t fingerprint);
/* Opening suspends left arm requests so GTA's own left hand is shown. */
uint64_t sv_xr_left_wrist_open_v1(void);
int32_t sv_xr_left_wrist_capture_v1(uint64_t session);
/* Ends the session and discards any uncommitted capture. */
int32_t sv_xr_left_wrist_close_v1(uint64_t session);
/* Adopts this session's successful capture and ends the session: 0 committed,
   -1 no such session, -2 nothing to commit (session stays open). */
int32_t sv_xr_left_wrist_commit_v1(uint64_t session);
/* Reading state is the open card's heartbeat. */
int32_t sv_xr_left_wrist_state_v1(SVLeftWristState *state, uint32_t byte_size);
int32_t sv_xr_left_wrist_export_v1(SVLeftWristCalibration *calibration, uint32_t byte_size);
/* NULL clears the calibration and returns to position-only left tracking. */
int32_t sv_xr_left_wrist_import_v1(const SVLeftWristCalibration *calibration, uint32_t byte_size);

#endif

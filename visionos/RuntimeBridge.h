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
int32_t sv_xr_set_reticle_ready(uint32_t enabled);
/* Qualified tracked-arm mode, including the signed build's normal-launch default. */
uint32_t sv_xr_motion_arms_enabled_v1(void);

#endif

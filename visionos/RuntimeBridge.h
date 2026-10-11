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

#endif

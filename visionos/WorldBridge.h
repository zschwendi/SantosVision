#ifndef SANTOSVISION_WORLD_BRIDGE_H
#define SANTOSVISION_WORLD_BRIDGE_H
#include <stdint.h>
#define SV_WORLD_FRAME_ABI_VERSION 1u

/* Immutable snapshots of distinct native game views. These are full viewport
 * world images with the matching real game depth, never a virtual-screen plane.
 * AER views can come from successive natural game frames. */
typedef struct {
    uint32_t abi_version;
    uint32_t byte_size;
    uint64_t lease;
    uint64_t game_frame;
    uint64_t request_frame;
    uint64_t head_frame;
    double presentation_time;
    /* Borrowed id<MTLTexture>; retained by this lease until explicit release.
     * Producer copies are GPU-complete before acquire can return success.
     * Color retains its native pixel format; depth is R32Float, game NDC z. */
    uint64_t color_texture;
    uint64_t depth_texture;
    uint32_t view_index;
    uint32_t view_count;
    uint32_t width;
    uint32_t height;
    /* 0 = source projection defines 0..1 depth; never assume reverse-Z. */
    uint32_t depth_convention;
    uint32_t reserved;
    /* Column-major float matrices, without SIMD ABI alignment assumptions. */
    float projection_from_eye[16];
    float game_from_eye[16];
    float game_from_stage[16];
    float depth_near;
    float depth_far;
} SVWorldFrame;

/* Caller sets version/size. 0 = ready, 1 = awaiting a qualified world frame,
 * -1 = invalid arguments/ABI, -2 = unavailable native Metal backend.
 * Bound to at most 12 outstanding leases; do not acquire indefinitely.
 * Compare both textures' Metal device with the compositor's device. */
int32_t sv_world_acquire_frame(uint32_t view_index, SVWorldFrame *frame);
/* Release ONLY after the compositor command buffer finishes reading textures.
 * Duplicate/unknown lease tokens return -1 and never release another frame. */
int32_t sv_world_release_frame(uint64_t lease);
/* Report a completed Metal compositor command while all leases are still live.
 * status is MTLCommandBufferStatus (4 means completed). The native side verifies
 * every eye token before counting a world frame; release leases afterwards. */
int32_t sv_world_complete_frame(const uint64_t *leases, uint32_t view_count, uint32_t metal_status);
#endif

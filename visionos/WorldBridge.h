#ifndef SANTOSVISION_WORLD_BRIDGE_H
#define SANTOSVISION_WORLD_BRIDGE_H
#include <stdint.h>
#define SV_WORLD_FRAME_ABI_VERSION 1u
#define SV_WORLD_PAIR_ABI_VERSION 1u

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

/* One GPU-complete render transaction for two distinct eyes of one frozen
 * simulation scene and one consumed native head pose. scene_epoch is issued
 * only by the certified guest paired-render producer; matching counters from
 * independent natural game frames do not qualify. Each returned texture is
 * retained by its own lease until explicit release after Metal completion. */
typedef struct {
    uint32_t abi_version;
    uint32_t byte_size;
    uint64_t scene_epoch;
    SVWorldFrame eyes[2];
} SVWorldPair;

/* Caller sets version/size. 0 = ready, 1 = awaiting a qualified world frame,
 * -1 = invalid arguments/ABI, -2 = unavailable native Metal backend.
 * Bound to at most 12 outstanding leases; do not acquire indefinitely.
 * Compare both textures' Metal device with the compositor's device. */
int32_t sv_world_acquire_frame(uint32_t view_index, SVWorldFrame *frame);
/* Caller initializes version/size. 0 = both eyes ready atomically, 1 = no
 * qualified pair (including when MUGUET_XR_PAIRED_RENDER is off), -1 = bad
 * ABI/arguments. Never assemble a pair from two acquire_frame calls. */
int32_t sv_world_acquire_pair(SVWorldPair *pair);
/* Release ONLY after the compositor command buffer finishes reading textures.
 * Duplicate/unknown lease tokens return -1 and never release another frame. */
int32_t sv_world_release_frame(uint64_t lease);
/* Exact immutable snapshot publication identity. Requires a current live
 * lease; does not retain/pin native resources. source_id is nonzero and changes
 * on every publication, even when the slot address and frame metadata repeat.
 * Caller still releases its existing lease only after command completion. */
typedef struct {
    uint32_t abi_version, byte_size;
    uint64_t lease, source_id, generation, provenance_epoch;
} SVWorldSourceFrame;
_Static_assert(sizeof(SVWorldSourceFrame) == 40, "SVWorldSourceFrame ABI must match Rust");
int32_t sv_world_frame_source_v1(uint64_t world_lease, SVWorldSourceFrame *source);
/* Report a completed Metal compositor command while all leases are still live.
 * status is MTLCommandBufferStatus (4 means completed). The native side verifies
 * every eye token before counting a world frame; release leases afterwards. */
int32_t sv_world_complete_frame(const uint64_t *leases, uint32_t view_count, uint32_t metal_status);
/* Default-off diagnostic only. All stages are timestamped by one native clock.
 * Commit observes validated live leases; it does not certify same-epoch stereo.
 * finish stops once and snapshots for bounded background create-new export. */
uint32_t sv_world_latency_enabled(void);
int32_t sv_world_latency_commit(const uint64_t *leases, uint32_t view_count);
void sv_world_latency_finish(void);

/* Optional HUD decomposition, borrowed for exactly the supplied world lease.
 * The original WorldFrame color is always the unmodified complete game image.
 * Use world_color_texture ONLY if every selected eye has valid HUD metadata
 * for the same provenance epoch, then show ONE chosen HUD in both eyes.
 * Otherwise use the original WorldFrame colors and no extra HUD layer. */
typedef struct {
    uint32_t abi_version;
    uint32_t byte_size;
    uint64_t lease;
    uint64_t provenance_epoch;
    uint64_t game_frame;
    uint64_t request_frame;
    uint64_t head_frame;
    uint64_t color_texture;
    uint64_t world_color_texture;
    double presentation_time;
    uint32_t width;
    uint32_t height;
    uint32_t source_eye;
    uint32_t flags; /* 1 = original UNORM numeric RGB, premultiplied coverage. */
    float angular_width_degrees;
    float distance_metres;
    uint32_t reserved[2];
} SVWorldHudFrame;
_Static_assert(sizeof(SVWorldHudFrame) == 104, "SVWorldHudFrame ABI layout must match Rust");
int32_t sv_world_frame_hud_v1(uint64_t world_lease, SVWorldHudFrame *hud);
/* Readiness is held by the current Swift world-camera owner. */
int32_t sv_xr_set_hud_ready(uint32_t enabled);
#endif

/* Proposed ABI contract only. No implementation or linkage exists in this app.
 * See IMMERSIVE-BRIDGE.md before implementing it in the Rust/game runtime.
 * This header is intentionally outside Sources and is not a bridging header.
 */
#ifndef SANTOSVISION_IMMERSIVE_BRIDGE_H
#define SANTOSVISION_IMMERSIVE_BRIDGE_H

#include <stdint.h>
#include <simd/simd.h>

#define SV_IMMERSIVE_ABI_VERSION 1u
#define SV_IMMERSIVE_MAX_VIEWS 4u

typedef struct {
    double x, y, width, height, z_near, z_far;
} SVViewport;

typedef struct {
    /* Borrowed Objective-C id<MTLTexture> addresses, NOT owned or retained. */
    uintptr_t color_texture;
    uintptr_t depth_texture;
    uint32_t texture_index;
    uint32_t array_slice;
    uint32_t color_pixel_format;
    uint32_t depth_pixel_format;
    SVViewport viewport;
    /* Column-major Apple right-up-back eye-space projection, reverse-Z. */
    simd_float4x4 projection_from_eye;
    /* Both transforms include stage-to-game conversion and unit scale. */
    simd_float4x4 game_from_eye;
    simd_float4x4 eye_from_game;
} SVImmersiveView;

typedef struct {
    uint32_t abi_version;
    uint32_t byte_size;
    uint64_t frame_index;
    /* Absolute ARKit/CompositorServices epoch seconds, not wall-clock time. */
    double presentation_time;
    /* Borrowed id<MTLCommandBuffer>; host alone presents and commits it. */
    uintptr_t command_buffer;
    /* Tracked native headset pose in stage space, meters. */
    simd_float4x4 stage_from_head;
    /* Explicit locomotion/recenter conversion from stage to game world. */
    simd_float4x4 game_from_stage;
    uint32_t view_count;
    /* 0 = built-in headset, 1 = recorder; do not assume two views. */
    uint32_t target;
    SVImmersiveView views[SV_IMMERSIVE_MAX_VIEWS];
} SVImmersiveFrame;

enum SVImmersiveResult {
    SV_IMMERSIVE_OK = 0,
    SV_IMMERSIVE_UNSUPPORTED = 1,
    SV_IMMERSIVE_BAD_ABI = 2,
    SV_IMMERSIVE_RUNTIME_ERROR = 3
};

/* Future entry point: encode one immutable simulation snapshot into ALL views.
 * Not implemented. The diagnostic never references this symbol.
 */
int32_t sv_game_encode_immersive_frame(const SVImmersiveFrame *frame);

#endif

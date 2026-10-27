# From two original draws to a finished same-eye region

The concrete next native span is **two consecutive qualified `VS_TransformD` / `PS_DeferredTextured` draws in the same four-MRT/D32S8 target domain**, with actual inherited target contents and an original after-span reference. The new capture/replay implementation handles that span. It does not close the complete scene or post-processing graph, and its G-buffer output cannot be displayed as a replacement for finished world color.

## Rechecked historical boundary

The preserved command artifact at `SantosVision/target/verification/reprojection-implementation-v1/guest-command-snapshot/runs/root-command-snapshot-v8-v1/command-snapshot.json` has SHA256 `2550b2cbf75dbec47da441b7c23e4a06e520121e77f4d25d193c234372fd7ebb`. Decoding its actual words confirms the four-MRT binding at ordinal 650 and next target binding at 7965. The interval contains 997 draws and 73 numeric shader pairs under owner sequence 24/list 19.

Two corrections to the earlier `native-draw-stereo/next-pass-plan-v1.md` matter:

- The clear at ordinal 652 is opcode 6 with operands `[1221, 0, 0, 3, 0, 7]`. Flags 3 clear **both depth to zero and stencil to 7** before the first draw at 662. Stencil is not inherited there. MRT0 has an explicit prior clear at 644; the other three colors still need their exact inherited contents.
- The first two draws, 662 and 670, use different vertex shader IDs, 4800 and 4801. They are not the already qualified family. The observed 1257/1288 family has 47 consecutive pairs later in the span, of which 11 have only SRV/IB/VB changes between them. Examples are 2074→2078 (2,472/2,049 indices) and 4550→4554 (42/18 indices). These historical IDs guide fixture selection but cannot certify a new run's shader identity.

The decoded inventory is retained privately at `target/native-span-dependencies/historical-pair-inventory.json`. It is metadata evidence, not a replayable capture: original material, other initial MRT contents, shader semantics and after-span target bytes are absent.

## Native closure implemented for the first span

`Gpu::do_draw` resolves the actual shader variant, pipeline, packed constant-buffer range, texture view, sampler, VB/IB, effective draw arguments, viewport, scissor, stencil reference and blend constant before the capture hook. The capture pins the actual translated shader texts, permits one direct indexed family, retains separate snapshots for each draw, and requires exact native attachment-parent equality and view identity across the span.

`ensure_pass` applies original pending clears as load operations. Closing that pass before the initial copy materializes those real values; reopening uses load. The capture rejects active queries or pending query resolves before splitting a pass. Original target copies precede draw one and follow draw two in the existing encoder; the producer does not submit independently. Packed constants come from the actual resolved GPU ring and selected offset, and immutable copies retain the material's raw mip bytes. Native completion and successful maps precede capture publication.

The consumer restores all six attachment aspects and checks their bytes before replay. It then executes both original draws into persistent attachments and compares every final byte against the original captured target. Raw compressed mip transfers use block-rounded physical copy extents while the manifest keeps logical dimensions. Static packed-buffer rebinding preserves the selected range and records the original dynamic offset; it does not reinterpret shader constants.

## Remaining graph required for genuine finished detail

The current game bridge publishes an already finished color image. A new region must carry a complete equivalent graph upstream of that publication, not merely more opaque geometry. The preserved different-scene frame-606 trace gives one exact late dependency chain: `CompositeExtraFX` reads HDR scene 1212, exposure 5828 and depth resources → BGRA8 6329; `AA_SM50` reads 6329 → 1229; pinned Lens reads 1229 → final target 1; HUD begins afterward. The same target 1229 is subsequently reused for minimap scratch. A target-ID graph without ordered allocation/subresource versions would therefore connect the wrong contents.

That trace omits earlier draws and non-draw writes. It does not certify every lighting, sky, water, transparency, SSAO, bloom or temporal dependency. A complete region producer must retain the following contracts before it can publish:

| Dependency | Required region behavior |
| --- | --- |
| Opaque geometry and depth | Same individual eye and scene, complete source list, retained per-material state, correct nested projection, stencil and all G-buffer attachments. Source-list LOD and visibility remain separate qualification. |
| Deferred lighting and shadows | Remap the region's depth/G-buffer inputs; preserve source shadow/light resources or rerender their required region dependencies. Reconstruct positions using the corresponding region projection and viewport. |
| Transparency, sky, water, particles | Preserve original ordering, blending, depth/stencil tests and any scene-color/depth feedback version. A late overlay cannot be composed over unfinished center G-buffers. |
| Screen-space effects | Carry actual texel sizes, resolution-dependent constants and texture-coordinate transforms for each target. Include filtering neighborhoods outside the displayed center; determine the halo from actual shader reads and downsample chains. Unknown/unbounded neighborhoods prevent closure. |
| Exposure, adaptation and temporal inputs | Preserve the reference's global exposure/history policy and frame identity. Recomputing exposure from only the center changes appearance even with the same tone-mapping shader. Histories need explicit regional mapping or a proven shared reference; do not silently start them at zero. |
| Sampling and derivatives | Keep exact sampler/mip/view formats and implicit-derivative fragment-quad phase. The earlier density fixture demonstrated that an odd reference-crop origin alone changes sampled colors. Nonlinear lens/AA coordinates need the original global-coordinate mapping plus a region transform, not a blind crop of constants. |
| Final color and HUD | Preserve the completed finishing chain, then publish that region's actual final color with its own depth/projection. Apply the already verified color transfer/P3 contract and compose HUD once. |
| Guest-visible side effects | Queries, readbacks, events and fences remain single-owner. Do not duplicate guest simulation/list construction or consume mutable render lists twice. |

The smallest next density test after original two-draw parity uses these same immutable inputs with an independently rendered high-resolution crop oracle and preserved fragment-quad phase. Its initial attachments must also be valid at that density. Upscaling an inherited low-density G-buffer does not supply the missing prior geometry or correct high-density depth/stencil; either start from proved clears or expand backward to include the commands that produced those pixels. This is the concrete reason to grow toward a closed opaque target span before extending its downstream graph.

There is no production region flag, alternating center/guard schedule or stereo shortcut in this implementation. A future completed region lease must retain both same-eye regions and their individual color/depth/projections through GPU completion. Neither a two-draw span nor an entire opaque span is a finished-image publication token.

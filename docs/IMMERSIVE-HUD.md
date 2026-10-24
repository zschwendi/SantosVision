# Immersive HUD and weapon-wheel surface

The immersive HUD uses one logical image on a finite head-relative canvas. Both
physical eyes sample that image through their own projections. The existing aim
reticle remains a separate projection of the actual gameplay ray; this extraction
does not change camera aim, native reticle suppression, weapon transforms or shots.

`Gpu` recognizes the original, SHA256-pinned final lens/post-FX writer and retains
its exact native final-target allocation and camera provenance. Before its first
qualified Scaleform HUD write it copies the completed world color. Only pinned
Scaleform writers or a certified map-scratch composite may contribute to the HUD
image. The scratch must begin with a full original clear and retain a closed UI/map
writer ancestry. A blit with RGB writes needs certified source ancestry. Alpha-only
scratch blits may supply the original game's real alpha mask, including its
DepthToAlpha pass; the scratch's raw target alpha is never called raster coverage.

The original final target remains render attachment zero and its fragment output
and blend are unchanged. An additional RGBA16Float render attachment accumulates
premultiplied HUD RGB and real coverage from the same surviving shader fragments,
discard, scissor, depth and stencil samples. The auxiliary source explicitly
saturates RGBA because the original UNORM target saturates its source before
blending. Straight/premultiplied source-over, opaque replacement and RGB-mask-zero
stencil/alpha-only operations are supported. Partial RGB masks and other blend
algebras reject extraction. The alpha blend uses One/OneMinusSrcAlpha rather than
the original target's SrcAlpha/OneMinusSrcAlpha: the latter squares source alpha
and is not compositing coverage. Color subtraction is not used.

All final-target writes are checked. Unsupported draws, nonzero final attachments,
resource replacement, copies, clears, uploads, UAV mutations and feedback reads
reject the candidate. Mutating a scratch allocation revokes its role. An optional
auxiliary pipeline failure or pending compilation retries the original draw rather
than suppressing it. The full final color is always preserved in `WorldFrame`.
Extraction is enabled only after the immersive consumer's ready handshake.

The bounded producer pool has six immutable image pairs, reused only when no
published world frame or compositor lease retains them. Publication waits for GPU
completion. `sv_world_frame_hud_v1` adds a 104-byte, 8-byte-aligned metadata record
without modifying WorldFrame ABI 1. Its premultiplied HUD texture and HUD-free world
texture are borrowed under the existing world lease. Metadata carries the same
scene epoch, game frame, request, head, eye and presentation timestamp. The consumer
uses HUD-free world images only if every selected world eye has a valid HUD record
from the same epoch, then draws one selected HUD image into both eyes. Otherwise
it uses both original full-color world images. Default canvas width is 60 degrees
at two metres and includes the whole source canvas, bringing edge HUD elements
into a comfortable field of view.

`world_hud` status reports actual GPU-complete publications and rejected candidates.
A successful synthetic fixture is not proof that an unrecorded game state uses the
certified path. A new shader/resource arrangement falls back to the original full
world image and may retain the old HUD problem until its actual writer is qualified.
No source images or game shader bodies are distributed by this implementation;
only source digests and routing code are retained.

Validation: the focused `xr_hud` Rust test filter includes original user shader
source-pin/fragment-ABI checks, blend/ABI/algebra tests, actual offscreen Mac Metal
MRT original-target byte identity and coverage/recomposition cases, and production
Gpu hazard/failure regressions. Physical acceptance still requires ordinary HUD,
weapon-wheel and WASTED cases in the headset. f16 and intermediate UNORM rounding
can differ slightly from original flat-screen composition; the same-source game
world stereo continues to use the existing renderer's provenance and is not made
same-update stereo by sharing the HUD.

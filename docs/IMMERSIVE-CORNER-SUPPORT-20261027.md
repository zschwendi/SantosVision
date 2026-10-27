# Direct immersive corner correction — October 27

New production change after `039aaad`: fixes a reproduced cause of fine reprojection cracks. It does not recover detail lost to 3× overscan or establish that all headset flicker is fixed.

## Change

`GameWorld.metal` tested one 3×3 depth neighborhood per geometric corner. An unrelated edge in its extra row/column could reject a continuous plane shared by all four corner owners. Flattening those pixels independently then introduced cracks during reprojection.

After the original fails, the shader now tries the other enclosing neighborhoods. Each contains all four owners and retains the original finite-depth, sky exclusion, four-ULP affine and projection checks. An additional four-owner consistency check avoids unnecessary work. Original successful fits, borders and optional smooth-surface behavior are unchanged. Source color, resolution, guard scale, graphics effects and independent-eye selection are unchanged.

Existing exact, source-order corner sharing now runs by default, retaining both original triangles per source pixel. Bounded allocation/encoder failure falls back to corner-only preparation, then direct calculation, with identical corrected pixels. Adaptive meshes and source reuse remain opt-in. Explicit `MUGUET_COMPOSITOR_ORDERED_CORNERS=0` and `MUGUET_COMPOSITOR_CORNER_PRECOMPUTE=0` select direct calculation for comparison.

## Fresh evidence

Private artifacts: `target/corner-support-20261027/`.

- `CornerSupportMetalValidation.swift`: 688 Metal checks; 18 old false rejections recover the expected plane. Real foreground/sky/invalid boundaries and previously accepted corners remain unchanged.
- `ordered/report.json`: six comparisons of both independently captured game eyes at three prescribed poses. 228 previously uncovered world pixels gain coverage, none lose it; the turned pair accounts for 218. Final RGB changes affect 2,696 left-eye and 2,863 right-eye pixels in that pair. Depth/ownership changes are intentional geometric corrections.
- `motion-frames/report.json`: twelve slow-motion poses of the same right-eye game source. Across frames, 1,675 previously uncovered pixel samples gain coverage and one loses it. These are sample counts, not unique world points or headset flicker measurements. The silhouette outlier remains in the before/after images. The improvement is small, not wholesale restored detail.
- All 18 registered comparisons confirm exact color/depth/ownership parity between new shared and direct calculations, including normal finite-sky and gap-fill passes.
- Serial alternating-order warmed M4 world-plus-fill medians: old default **22.388 ms**, corrected shared path **14.041 ms**, 37.3% lower. This excludes the game producer/live compositor queue and is not headset FPS.
- Corrected direct and corner-only fallbacks are slower than old direct rendering. The 384 MiB indexed pool fits three ordinary 1440-line sets, fewer than a full stereo triple queue. Corner-only fallback limits extra storage; sustained headset timing/allocation pressure remains unverified.
- New-shader pool/integration checks: 68 budget, 44 actual-Metal ownership/exhaustion, and 90 checks across 18 assembled renders pass. Physical visionOS Swift typechecking and Metal compilation pass; no simulator/headset used.

## Acceptance boundary

These are production-shader outputs from registered game RGB/depth with prescribed display poses, not fresh game frames or headset recordings. Source is local; the frozen signed visionOS V28 package remains unchanged.

Next headset test, when authorized: same bad scene, 1440 Clarity, windowed → independent-eye third-person; 15 seconds still, 15 seconds slow yaw, 15 seconds still. Check thin edges/dark surfaces/shimmer and completed command timing plus allocation fallback. Keep adaptive, common-target and single-source experiments off.

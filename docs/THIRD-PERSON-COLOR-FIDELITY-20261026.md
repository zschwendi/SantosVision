# V27.7 third-person finished-image color fidelity

This local candidate corrects demonstrated color and sampling defects in the actual immersive renderer. It does not yet establish windowed-quality headset acceptance. Source base: `177acd56c88b5e41132d1d42ddc3dd53d58bc053`, branch `codex/third-person-fidelity27`, checkout `/Users/zach/gaming/SantosVision-third-person27`. The canonical rollback checkout and frozen V27.6 package are preserved. No app was installed or launched, and no headset, iPad, simulator, game-save or graphics-preference action occurred.

## What changed

The guest's finished SDR image uses sRGB primaries and transfer. Compositor Services requires **extended-linear Display P3 shader values**, independent of whether its attachment stores plain linear values or sRGB-encoded values. The old renderer inferred the color contract from the destination format and omitted the primaries conversion. The historical physical BGRA8 UNORM → BGRA8 sRGB path already handled transfer correctly: **that run is not evidence of double gamma**.

`GameWorldColor` now prefers a supported BGRA8 sRGB target, then RGBA8 sRGB, RGBA16F, and finally linear 8-bit targets. All receive linear P3 shader values. This retains dark-tone precision on the usual four-byte output without raising source resolution.

Each retained final/world source obtains a zero-copy sRGB texture view. Hardware decodes each texel before bilinear filtering. If that view cannot be created, the shader decodes four source taps before interpolation. Previously UNORM sampling filtered encoded values and decoded afterward, darkening mixed edges. A black/white midpoint reproduced linear **0.214041 before versus the correct 0.5 after**. The sRGB-to-P3 matrix follows filtering. Ordinary, ownership, optional vertical, source-copy and gap-fill fragments share this contract.

The HUD sidecar contains an accumulated premultiplied contribution in the game's **encoded blend domain**. Unpremultiplying, decoding and blending linearly changes that image. The corrected shader fetches the displayed background, converts it back to the guest blend domain, applies the original contribution, then decodes and converts the finished result to P3. Fixed-function blending is disabled for this pass. For opaque background B, contribution H and alpha A, the reference is:

`linearP3 = sRGBToP3(decode(clamp(H + (1-A) * encode(P3ToSRGB(B)), 0, 1)))`

Transparent backgrounds retain premultiplication and output alpha. Native-supported RGB-only contributions at zero alpha are preserved. Final saturation occurs in guest sRGB primaries, before P3 conversion. No new full-screen color copy or post-processing pass is added.

Debug readbacks retain their raw numerical files and add ICC-tagged PNG companions. Guest bytes are tagged sRGB; compositor sRGB storage is tagged Display P3; linear output is tagged linear Display P3. Float output retains raw RGBA16F, with an explicitly clipped 16-bit PNG preview. Native publication format and sampling-view format are recorded separately.

## End-to-end ownership audit

The guest final target, HUD-free snapshot and contribution retain the existing native same-lease chain. The previously audited finished/world equality outside actual HUD contribution remains relevant; no post-processing pass was removed or replayed.

`GameWorldLease` retains its own color view and original depth/projection. HUD selection still chooses one logical overlay, while each eye substitutes **its own** `lease.hud.worldColor`. It never uses the selected HUD's world for both eyes. The native lease remains held through GPU completion. Derived-depth cache identities are unchanged. No demonstrated wrong-eye/world substitution was found.

## Local before/after evidence

Private artifacts are under this checkout, not the frozen rollback checkout:

- `target/color-pipeline-validation/final/`: frozen source inputs, independent reference results, actual before/after PNGs, crops, and comparison sheet.
- `target/color-fidelity27-7/`: independent source review, build recipe/logs, package verification and frozen candidate.
- `target/verification/color-export-20261026/`: export tests, corrected existing rendering regressions and receipts.

The Metal harness uses the original registered 2560×1440 airport imagery, Float32 depth, HUD contribution and matrices from two separate captured leases: eye0 game10156/lease68739; eye1 game10153/lease68740. It retains the original capture-readiness failure/deadline qualifications. The same inputs are rendered with frozen base177acd5 and the new production shader through normal per-pixel reconstruction, finite-sky support, gap fill and the selected shared HUD. Output is 1888×1792 per eye. Neutral and explicitly prescribed 5-degree yaw/25mm displacement are separate local replay poses, not newly captured headset motion.

The new suite passes **383 checks and 121 actual Metal renders** on Apple M4. All four eye/pose comparisons preserve every Float32 depth bit and ownership byte. Hardware sRGB views and manual filtering differ by at most one output byte on these full images. Independent Double sRGB→XYZ→P3 references give maximum linear RGB error below 0.000148 for hardware sampling and 0.00000085 for manual sampling. The frozen baseline fails the high-contrast filtering reference.

HUD tests include captured fractional contributions, saturation, additive zero-alpha RGB, partial/transparent backgrounds, and actual framebuffer fetch on BGRA8 sRGB, RGBA16F and linear BGRA8. The independent HUD reference differs by at most 0.706 byte; the frozen baseline misses by up to 64.25 bytes in the deliberate controls. Twenty-four actual captured HUD samples also agree with their original same-lease finished source within documented rounding.

The airport scene changes modestly: edge mixtures become brighter and color saturation follows the window's sRGB appearance rather than treating its primaries as P3. About 2.47–2.49 million pixels change per replay, but mean RGB code change is only 0.574–0.584 of a byte; maxima are 34–37 bytes. The pixel count is not a sharpness score or a claim of a dramatic visual improvement. Geometry and missing detail are unchanged.

Export validation passes 208 CPU checks, including actual ImageIO ICC round trips and exact RGB preservation. The existing depth-fill regression retains all covered color/depth/ownership and agrees with the independent texel/color reference on all 5,556 filled pixels. Updated vertical-reconstruction regressions preserve their sampling/geometry/fallback gates under the new color contract. Physical-target Swift 6 strict typechecking, Metal compilation and the complete signed visionOS app build pass.

## What this does not establish

Color correctness, spatial detail and performance are different results. This patch repairs the first and the filtering component of the second. It cannot recreate samples absent from the producer: the reported guard near3 still gives approximately 853×480 central source samples from a 2560×1440 image. Increasing interpolation quality cannot restore native material or shadow detail that never reached those source samples.

Matching the window's sample density across that guard would require changing the producer's allocation of samples—for example, a high-density central render plus a lower-density guard region, each retaining finished effects and per-eye color/depth/projection provenance. That is a larger rendering architecture change, not justified as a normal default by these static frames. The implemented bounded first step establishes a consistent finished-image color/sampling/compositing contract across all existing paths, with a real-image oracle to prevent a later density change from silently changing the game's appearance. No new density reconstruction path is deployed here.

The native archive, game/AOT, source dimensions, overscan/freshness policy, stereo behavior, graphics effects and performance-experiment defaults are unchanged. There is no measured M2 latency, FPS or flicker improvement. Overlapping historical GPU command intervals are not converted into FPS. The retained HUD contribution also cannot universally reconstruct arbitrary histories of intermediate UNORM saturation; its agreement is established for the registered fixtures and explicit blend controls, not every game scene.

## Single next headset acceptance test

When device work is resumed, use V27.7 in the same third-person scene and existing graphics profile through the ordinary window→expand route. Hold the camera still for 15 seconds, then make a gentle yaw for 15 seconds. Compare signage, shadow gradients, texture edges and HUD opacity, and check for new peripheral gaps or flicker. Record the actually negotiated source/sampling/output formats and distinct source eyes. Keep resolution, guard, cache flags and submission depth fixed, and leave debug readback off for the performance observation. This one comparison decides whether the demonstrated color corrections improve the user's current scene; it does not certify a broader performance change.

Official contract references: [Apple immersive Metal session](https://developer.apple.com/videos/play/wwdc2023/10089/), [Metal Shading Language specification](https://developer.apple.com/metal/Metal-Shading-Language-Specification.pdf), [sRGB texture-view exception](https://developer.apple.com/documentation/metal/mtltextureusage/pixelformatview). The private independent review records exact specification sections and source hashes.

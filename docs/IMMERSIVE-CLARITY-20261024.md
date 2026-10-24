# Immersive clarity audit and depth-edge gap fill — October 24, 2026

## Audit of the actual image path

Inputs: the registered AVP left-eye projection and 1888×1792 viewport from `device-session-20261020/candidate7/world-normal7-…-report.json`, the build21 report `claude-five-issues22/runtime-report.json` (Clarity, 2560×1440, 60 cap, Texture2/AF16, guard scale 2.175 at its last frame sample), and the build20 report (guard 1.844).

- **Sampling.** Each source texel is a quad sampled bilinearly with perspective-correct UVs; texel centres, corners and the Y flip are consistent with the source projection. No half-texel or orientation defect was found. Bilinear enlargement is soft, but sharper kernels multiply fragment cost in a mesh of sub-pixel triangles whose compositor command already measured 41.8 ms mean at 1920×1080. That trade was not taken without timings.
- **Colour.** The device negotiated `bgra8Unorm_srgb` (81) for an 8-bit gamma-encoded `bgra8Unorm` (80) source. `colorTransfer=2` decodes, and the target re-encodes. The round trip is exact apart from 8-bit rounding. The output is SDR; there is no HDR or tone-map stage of our own, and GTA's tone mapping is untouched. No gamma defect was found.
- **Texture filtering.** The host maps D3D anisotropic samplers to linear/linear/linear with the raw MaxAnisotropy clamped to 1–16. GTA's mip LOD bias has no wgpu equivalent and is dropped.
- **Resolution and aspect.** Source and display cover the same frustum. One source texel therefore spans `scale × displayPixels / sourcePixels` display pixels on each axis. The source is 16:9, while the eye drawable is about 1.05:1. With foveation off, the drawable resolves 12.06 px/° horizontally and 14.27 px/° vertically straight ahead. At 2560×1440, each texel spans this many display pixels (horizontal, vertical):

  | Guard scale | Display pixels per texel (H, V) |
  |---|---|
  | 1.0 | 0.74, 1.24 |
  | 1.30 (rest) | 0.96, 1.62 |
  | 1.844 (build20 sample) | 1.36, 2.30 |
  | 2.175 (build21 sample) | 1.60, 2.71 |
  | 3.0 (cap) | 2.21, 3.73 |

  At rest, the vertical axis is the bottleneck: horizontal already matches the drawable. At 3840×2160 and scale 1.3 the figures are 0.64, 1.08, so horizontal is oversampled. Only a taller immersive render target would recover vertical detail at equal cost, which needs native resize support. No resolution default changed.
- **Foveation.** Foveation is disabled, so the drawable is uniform at the density above. Enabling it would raise centre density, but the source is already the vertical bottleneck, and every pass would need rate-map handling.
- **Coverage guard.** Adaptive coverage widens both axes together. Splitting X and Y would gain only 3–6% for the registered frustum, because the isotropic cone around the 64° corner ray inflates both coordinates almost equally.

## Defect addressed: opaque black depth-edge gaps

The registered third-person capture shows a dotted black silhouette and cracks around the body, fence, plants and bridge edges. The source frame had none of them. The finite-sky ownership mask records them as owner 0: no source texel reached the pixel, so the drawable keeps its opaque black clear colour. 320,094 output pixels are exact black at cleared depth. 314,537 of them are the outer border outside the source image. The other 5,557 are interior, including all 1,557 in the audited body region. Head translation opens these gaps beside nearer geometry. They grow with nearness: a few pixels around a character at 1.8 m, more beside a first-person weapon.

`GameWorld.metal` adds `gameWorldDepthEdgeFillFragment`, which runs after finite-sky ownership and before HUD/reticle, in the existing overlay encoder. For owner-0 pixels only, it walks the pixel's current-eye ray through the source depth image. The walk goes from the ray's infinite point toward the eye along its epipolar line, for at most 64 source texels. The last place where the ray passes from behind to in front of a source surface is where it meets that surface. Beside an occluder, that surface is the adjacent background. That texel's colour is copied unchanged, with the same transfer rules as the world pass. Its source depth is extended along the ray, or the finite-sky support sphere is used for sky.

Left untouched:
- pixels whose infinite point lies outside the source image (turn borders, never stretched)
- directions behind either camera
- gaps wider than the walk
- near-fade discards inside 10 cm
- every covered pixel

The fill estimates hidden background from the same frame. It renders no new geometry.

The fill is **off by default**. It is controlled by:
- the launcher toggle "Fill gaps beside near objects (experimental)", persisted in `SantosVisionDepthEdgeFillEnabled`
- `MUGUET_COMPOSITOR_DEPTH_EDGE_FILL=1/0`
- `--sv-world-depth-edge-fill` / `--sv-world-disable-depth-edge-fill`; disable always wins

It needs finite-sky ownership. Runtime `immersive_lifecycle.image_quality` reports:
- per-eye encode outcomes
- a GPU counter sample at most once per second per eye, from successfully completed command buffers: uncovered, filled from finite background, filled from sky, and left uncovered
- per-eye `sampling_density` from the actual display projection/viewport and the leased source projection/size

Launch reports add `rendering_quality_requested` and `rendering_quality_effective`.

## Verification

`sh scripts/test-depth-edge-fill.sh OUT [capture-prefix]` runs:
- compile-only Metal ABI asserts for xros and macosx (240-byte fill uniforms; existing 224-byte world uniforms unchanged)
- production policy, uniform and density code
- `DepthEdgeFillMirror.swift`, a statement-for-statement CPU mirror of the shader walk

60 checks pass. On analytic scenes (±3 cm and the capture's 7 mm/2.9° pose, sky behind the occluder, standard and reversed depth):
- every in-image gap is filled
- no wide gap takes occluder colour
- no fill lands more than one texel from the true surface
- extended background depth is exact to 3e-7 relative
- the walk also recovers every visible surface

Off-image and behind-camera directions stay unfilled, and gaps beyond the walk bound stay black.

Applied to the device's actual registered output, the mirror filled:
- all 5,557 interior gaps: 4,779 from finite background, 778 from sky
- all 1,557 in the body region

It left the 314,537-pixel outer border black. Crops show the outline and cracks replaced by the adjacent wall, ground and jacket colours, with no visible smear.

A strict Swift 6 xros type-check of all sources passes, with only the existing `mac1` deprecation warning. Offline Metal compiles pass for macosx, xrsimulator and xros.

`scripts/test-depth-edge-fill-metal.sh` is the root GPU step. On the same capture it runs production ownership plus the fill and requires:
- covered pixels bit-identical
- fills matching the CPU mirror
- GPU counters equal to readback

It also records warmed Mac timings with and without the fill. It was compiled, not executed, by this work.

## Remaining physical acceptance

Run the Metal parity step, then build. With the toggle on, compare third-person and first-person immersive scenes while translating the head. Black silhouettes and specks should become background-coloured, with no smears, flicker or regression of covered pixels, HUD, reticle or third-person camera. Record the `image_quality` counters and the compositor GPU timing. Flip the default only after that A/B. Vertical source density, alternating-eye updates and first-person foreground depth remain separate constraints.

# Third-person image stability and central detail — V27.8

V27.8 retains the verified V27.7 finished-image color corrections and adds two bounded production fixes: stable source-coverage timing and depth-qualified sampling at repaired gaps. It does **not** restore the detail absent from a guard-3 source. Hardware image quality and flicker remain unaccepted. A separate same-eye density fixture implements the first rendering step toward a sharper center; it is outside the app and cannot publish a runtime frame.

## Source coverage

The former source-age peak decayed by 0.98 on every eye callback. Identical ages in a different order produced guard scales 1.612618 versus 1.629063; a duplicate young-eye callback reduced the scale to 1.596714. Over 0.2 seconds, equivalent 30/60/90/120 Hz age observations produced horizons of 0.235415/0.184734/0.144964/0.130000 seconds.

The renderer now reports the maximum valid age across its actual sources with the drawable timestamp. Decay uses elapsed time, derived from the former factor at one 60 Hz observation. Eye order, count and duplicate timestamps no longer change the coverage budget. All four tested cadences now produce 0.235415 seconds. This is deliberately conservative: extra coverage can persist longer after a stall. It is not a sharper-source or performance claim.

Lease admission and coverage now share the existing one-second limit. Previously, accepted sources just above 0.5 seconds lost their coverage signal; accepted positive ages through one second now raise the horizon to its existing 0.3-second cap. The 0.1–0.3-second horizon, 1.3–3 guard, instant expansion, 0.75-units/second contraction and angular-cone guarantee remain intact. No native mailbox, stereo acquisition or camera authority changes.

The production CPU suite passes **259,229 geometric and temporal checks**. Exact old/new harnesses, commands and source hashes are in `target/verification/temporal-guard-audit-20261026/`. Implementation: `3459c40`.

## Gap color and depth

The old filler copied the search-entry texel's color but placed filled depth at a different point on the viewing ray. On a sloping textured surface, color and geometry therefore came from different locations.

The new helper requires a finite, continuous 3×3 affine-depth patch within four Float32 ULPs, a complete 2×2 sampling footprint within that patch, positive bounded inverse distance, and valid positive output depth. It intersects the display ray with that plane, samples color there in linear light, and puts filled depth at the same point. Unsupported cases retain the original repair. Covered pixels, ownership, sky behavior and fill coverage remain unchanged. Independent review found and resolved exact-far-plane and normalized-coordinate rounding boundaries before final validation.

The final suite passes **465,036 assertions across 14 analytic and 47 direct boundary cases**, plus six registered before/after comparisons. In the saved airport scene at declared 5-degree yaw/25 mm translation, left/right images change only **29/19 color pixels** and **39/29 filled-depth pixels**. Every covered pixel, unsupported fallback, sky sample, ownership byte and finite-coverage decision remains exact. The visible improvement is a few repaired edge pixels, not broad scene detail or demonstrated live flicker removal.

High-frequency analytic planes provide the independent color/depth oracle; thin occluders, steps, invalid depth, source borders, both depth conventions, grazing planes and non-power-of-two texture widths exercise rejection. The full finished-image regression separately passes **391 checks across 121 actual Metal renders**, including both real eye leases, neutral/turned poses, HUD composition, Display P3 output, and hardware/manual linear sampling. Their maximum output difference is one byte; covered depth and all ownership remain exact.

A historical candidate-7 compatibility replay still fills 5,556 pixels and preserves covered pixels exactly. Its old adjacent-texel mirror has six differences within its pre-existing tolerance; it is not the correctness oracle for the new planar samples.

Ten alternating old/new serial command pairs after three warm-up pairs gave local M4 medians of **19.051 ms before and 18.987 ms after**, with overlapping ranges (18.311–19.806 / 18.465–19.775 ms). This bounded fixture reveals no measurable regression. It establishes neither a speedup nor headset FPS or isolated shader busy time.

Implementation: `f976c41`. Final same-shader receipts and visual crops:

- `target/reprojection-fill-sampling/final/receipt.json`
- `target/reprojection-fill-sampling/final/plane-validation.json`
- `target/reprojection-fill-sampling/final/registered/report.json`
- `target/reprojection-fill-sampling/visuals/registered-fill-before-after.png`
- `target/independent-gapfill-review/REVIEW.md`
- `target/fidelity-v27-8/color-regression-final/report.json`

Reproduce with a fresh output directory:

```sh
scripts/test-reprojection-fill-sampling.sh NEW_OUTPUT_DIR /Users/zach/gaming/SantosVision/target/verification/device-session-20261025/third-person27/registered-optical-comparison
```

## Implemented first step for central detail

At 2560×1440 with guard 3, roughly 853×480 source samples span the original physical frustum. Finished-image corrections cannot recreate detail never rendered. A high-density center plus a lower-density wide guard from the same individual eye is the concrete architecture under investigation.

Implementation `c538abb`: the new standalone [density fixture](../scripts/central-density-proof/README.md) executes two hash-pinned game shaders with retained geometry and constant buffers. The original sampled texture and complete draw state were not captured, so material, sampler and render-target state are explicitly synthetic. No game runtime is linked or launched.

Seven Metal renders validate the narrowed projection against an independent reference: unmodified original shaders/constant buffers rendered at 1920×1080, followed by an integer-aligned crop. All four color targets, stencil and coverage match exactly; the maximum depth difference is one Float32 ULP (2.33e-10). The selected draw covers nine wide-image samples versus 93 samples in the 3× nested region. It lies outside the physical center, which correctly produces zero samples; the tested region is explicitly off axis.

An odd-origin control exposed a fragment-quad phase mismatch and up to 12 bytes of color error. Aligning the crop on both axes fixes that comparison. This is a regional-rendering contract discovered by the fixture, not a proven live GTA defect. Duplicate wide rendering is only a determinism control, not parity with the original game renderer. Final evidence is `target/central-density-proof/final/{receipt.json,result.json}`.

This is a rendering implementation, but only for one draw. A publishable region producer still needs immutable complete material/state capture, an ordered target span, complete lighting/transparency/sky/water/SSAO/bloom/tone mapping and temporal dependencies, qualified LOD/culling, and same-eye region color/depth/projection leases. Alternating center and guard across natural game frames would lower cadence and mix simulation states; that shortcut remains excluded. The source audit does not prove a mesh-LOD defect. Exact implementation seams and limits are in `target/central-detail-review/REVIEW.md`.

## Package and next single headset acceptance test

The signed **V27.8** app is frozen at `target/fidelity-v27-8/SantosVision.app`. The xros build passes **34 package checks**: strict signatures, unchanged technical identity/entitlements/provisioning, exact original-27 game/AOT bytes with read-execute protections, unchanged pinned native archive, and all 51 recorded build inputs. The density fixture is not linked into this app.

Executable SHA256: `ac15bc66a30638441c842e34d9ec1e4457f4f8f718c5f8ea689249a61ae686b0`. Metallib SHA256: `99d7e88b9b42477f79ac0f8fba17b3f17eb6df7f8cf5606f3cc548a08be24578`. Recipe, final checkpoint and package verification are under `target/fidelity-v27-8/`.

V27.6, V27.7 and the canonical rollback remain preserved. The app display name is V; technical identity remains SantosVision. Original game/save identities, resolution, graphics settings and independent eye sources remain intact. No headset, iPad, simulator, installation, save replacement or game launch occurred in this local cycle. Build 27.3 remains the last user-reported installed build; no live device query was made.

After the local-only hold is lifted, perform **one same-scene window-to-immersive comparison** in normal third person at the existing 1440-line settings: 15 seconds still, 15 seconds of slow yaw, then 15 seconds still again. Compare matched static details, dark shading, thin edges, HUD and grid settling. Keep source-cache, common-target, single-source and other experimental modes off. Capture a matched completed source/output pair at the end for the actual bad scene; label capture-enabled timing separately. Actual wearer improvement and stereo behavior remain the acceptance gate.

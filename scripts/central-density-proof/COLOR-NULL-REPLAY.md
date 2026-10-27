# Exact color-null draw replay

This separate consumer imports one actual `VS_Blit` / `PS_BlitDepthToAlpha` draw, its resolved immutable inputs, and original initialized color/depth/stencil attachments. It does not change the two-draw span consumer or link the game runtime.

```
scripts/central-density-proof/replay-color-null.sh /absolute/private/color-null/capture.json /absolute/new/output --cpu-only
scripts/central-density-proof/replay-color-null.sh /absolute/private/color-null/capture.json /absolute/different/new/output
```

The first command validates CPU input admission without initializing Metal. The second requires the coordinated local GPU slot. Both require a new output directory and use offline pinned Cargo dependencies. Inputs, consumer source, patched wgpu-core source, executable and all artifacts receive hashes; original capture files are never modified. Private game inputs are not committed.

The corrected replay must import every original attachment byte exactly and reproduce every original after-draw byte. Its pipeline, write mask, blending, depth/stencil behavior, actual native dummy texture, real sampled texture, samplers, packed constants, vertex bytes and effective draw arguments come from the capture. The original draw must change at least one attachment; an entirely inert result cannot pass as meaningful pixel evidence.

A second branch changes only the demonstrated old t5 binding to reference the same native parent/subresource as color attachment zero. It must fail Metal/wgpu validation and leave all initialized targets unchanged. An additional **pass-discard control** invokes the same captured corrected draw, then the same captured draw with the historical alias, in one pass. This deliberately altered two-invocation sequence is a control, not original game chronology. The first invocation's measured nonzero contribution must also disappear.

Images show captured RGB and alpha separately. RGB bytes are reordered from BGRA8 and interpreted as sRGB for preview; this intermediate target's transfer encoding is not independently established, and the raw bins are authoritative. Alpha is numerical grayscale tagged gamma 1.0. Difference images contain unscaled absolute byte differences. Crops preserve one source pixel per output pixel. These are the original draw's target contents, **not finished-world or compositor screenshots**. If the effective mask writes only alpha, the report says so and does not claim an RGB improvement. `world_view:null` remains null: a UI/scratch draw is not assigned an invented immersive eye.

Native schema3 requires `kind:native_color_null_draw`, one `draw`, captured color0 and optional paired depth/stencil, explicit retained-parent alias proof, inactive queries, complete original initial/final copies and successful natural submission. The replay pins both translated shader hashes and rejects unrecognized binding shapes. The original capture's provenance and the full effective pipeline are retained in `replay/result.json`.

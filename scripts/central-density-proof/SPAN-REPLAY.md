# Real two-draw target replay

`replay_span` consumes an immutable `native_draw_span` schema-2 capture. It is separate from the earlier one-draw synthetic-material density proof. It does not link, start or publish frames to the game.

```sh
sh scripts/central-density-proof/replay-span.sh /absolute/private/manifest.json target/native-span/cpu --cpu-only
# Reserve the local GPU slot first.
sh scripts/central-density-proof/replay-span.sh /absolute/private/manifest.json target/native-span/replay
```

Both routes require a fresh output directory. The runner verifies and freezes the private manifest and all binary inputs, snapshots sources, builds the pinned dependency versions offline, and writes a hash receipt. Captured shaders, textures, buffers and target images remain private files outside Git.

The supported first span is two original direct indexed draws of one exact resolved shader family, one eye/view/epoch and one target-generation/subresource domain. It retains four BGRA8 MRTs and D32FloatS8, effective pipeline descriptors, per-draw packed constants, actual vertex/index ranges, sampled material mips, view/sampler descriptors, viewport/scissor, blend constant and stencil reference. Missing mip data, mutable resources, incomplete closure or mismatched eye/target evidence is rejected. The capture must retain actual initial and final attachment bytes; zero-cleared substitutes are not accepted as original parity.

The replay first imports the four color images and stencil bytes. wgpu forbids buffer-to-depth-aspect upload, so a fullscreen identity fragment writes the captured finite Float32 depth values into the depth attachment. A complete six-aspect readback must equal the captured initial bytes before the two original draws can run. Both draws then execute in order into the same persistent attachments with load/store preservation. Every byte of the final four color, depth and stencil aspects must equal the actual original after-span capture. The result records original and replay initial-to-final changed byte and pixel counts for each aspect; an entirely unchanged span cannot pass as meaningful drawing proof. A mismatch is a failed replay and is recorded; duplicate scratch determinism cannot substitute for this reference.

This establishes only a captured native source-list span. It has no center projection patch and does not establish a finished higher-density image, full opaque-pass completeness, LOD, visibility, later lighting/transparency/sky/water/post processing, temporal history or headset acceptance. The next density oracle must retain the same two original draws and compare against an unchanged-CB high-resolution reference with matching fragment-quad phase. Inherited low-density attachments cannot be upscaled and called a genuinely denser prior scene.

## Authored API validation

`span_fixture` is a separate, explicitly authored 32×24 test of the import/replay API. It draws a literal two-draw reference using four nonzero initial color targets, depth 0.25, stencil 63, real uniform/vertex/index/material buffers, partial write masks, blending, scissor and changing depth/stencil. Its reference renderer does not parse the replay manifest. Build it with the same pinned Cargo environment used by `replay-span.sh`, selecting `--bin span_fixture`; after reserving the GPU, run `span_fixture NEW_FIXTURE_DIRECTORY`, then replay the resulting `capture.json` through the runner. The optional `--bc` fixture uses three BC1 mip levels, including 2×2 and 1×1 logical tails, and an sRGB texture view. This checks physical block upload sizes and view reinterpretation; it does not certify that implicit LOD selects every tail mip.

These fixture manifests use `source.method=authored_api_validation_fixture`, `status=authored_fixture`, and `reference_submission_completed=true`; they never claim natural game submission or actual game imagery. Replay results explicitly distinguish them with `actual_game_capture=false`. Native captures require the pinned original vertex/pixel shader source hashes and `natural_submission_completed=true`.

The closure and complete-rendering dependencies are documented in [SPAN-DEPENDENCIES.md](SPAN-DEPENDENCIES.md).

## Actual source-list acceptance, October 26

The private `target/native-draw-capture/actual-game-v2/capture/capture.json` fixture (SHA-256 `46f92ddf9fbe759a92746eb62d187c9eabd219332d2dd13c744b0337db106a06`) contains two actual indexed game draws, 3,072 and 1,632 indices, from right eye 1 / request 69 / camera 84. Only SRV, index and vertex binding commands separate them. The 2560×1440 targets retain their real inherited contents.

`target/native-draw-capture/actual-game-v2/replay-final/receipt.json` records exact import of all six initial aspects and zero byte or pixel differences against all six original after-span aspects. The original and replay each change 1,388 pixels in color0, color1, color3 and depth; 1,227 in color2; and 31 in stencil. Sources and private inputs remain unchanged. This proves the native two-draw span consumer, including real material and state; it does not extend acceptance to later scene passes or the final image. The separate post-capture observation found a native resource/output overlap after this span, so this receipt must not be interpreted as a clean complete game run.

## Resource overlap regression

`src/bin/srv_overlap.rs` is another authored, isolated Metal test. Build `--bin srv_overlap` with the same Cargo environment and, after reserving the GPU, run `srv_overlap NEW_OUTPUT_DIRECTORY`. The directory must not exist. The result snapshots its source and hashes every successful output.

Four expected validation failures cover an overlapping sampled view, an unused declared binding, replacement of the overlapping binding before drawing, and an all-mip view overlapping one output mip. The failed passes must leave both targets unchanged. Five legal cases cover both binding orders, persistent null state after unbinding the output, and disjoint mip and layer ranges. Each legal pass executes two draws and checks exact pixels in both render targets plus preservation of an untouched texture.

Its explicit binding-state model follows Microsoft's [PSSetShaderResources](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-pssetshaderresources) and [OMSetRenderTargets](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-omsetrendertargets) contracts. It does not execute native guest commands or prove that the actual game's failing pass has the same cause. The native producer probe and a subsequent original-image replay are separate acceptance steps.

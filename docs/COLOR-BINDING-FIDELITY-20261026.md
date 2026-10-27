# Preserve native passes when color inputs overlap outputs

The native adapter now implements D3D11's persistent null-binding rule for color render targets and overlapping shader inputs. Previously, it forwarded both bindings to wgpu. A real game draw demonstrated that this invalidated the render pass; it was not a gamma, post-processing selection or resolution problem.

## Demonstrated failure

In the private unchanged-1440p Mac run `target/native-draw-capture/actual-game-v2/`, the first conflict occurred at present 537 with the GPU error counter still zero. The resolved pixel shader was `im::PS_BlitDepthToAlpha` (guest hash `7f531ca66548aefe`), drawing six vertices. Its actual texture input at group 1/binding 37, guest view 1230, retained the same native allocation as color target 0, guest texture 1229. Both selected mip 0/layer 0 of the 2560×1440 BGRA8 image.

The shader samples that input to derive output alpha; it is not an unused declaration. The following validation errors report incompatible `RESOURCE` and `COLOR_TARGET` usages, first on command encoding and then submission. The 20-second stats sample counted 710 errors. No claim is made that this is the sole cause of the user's headset complaint or that the fresh developer scene matches the bad headset scene.

Evidence is `first-color-feedback.json`, the corresponding `stderr.log`, and the run's source/binary-bound receipt. The diagnostic compares actual retained native parents and subresource ranges, rather than assuming guest IDs identify the same allocation. Captured original draws earlier in that run independently replayed exactly; later errors are not relabelled as whole-run health.

The first conflicting draw has `world_view: null`. This shader also appears in the known minimap scratch chain; an older frame-606 trace uses it with an alpha-only write mask. The subsequent exact-pixel experiment below confirms that the captured corrected draw is a minimap scratch operation and changes no pixels. Invalid submission can discard other encoded work, but the affected original command contents must be captured before attributing that loss to the finished world image. The verification establishes the binding correction, not that causal link.

## Correction

At an incoming shader-resource binding command (op24), an overlapping color input is stored as null. At a render-target binding command (op26), existing overlapping inputs in all three shader stages are unbound. The null state persists after the output is unbound until the game explicitly binds the input again. The existing renderer supplies its ordinary zero texture for that null binding.

This matches Microsoft's [PSSetShaderResources](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-pssetshaderresources) and [OMSetRenderTargets](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-omsetrendertargets) contracts. Substituting a dummy only at draw time would be incorrect because it would silently restore the old input when the output was removed.

The check compares current native texture allocations and the renderer's normalized mip/layer ranges. Disjoint mips, disjoint layers and different allocations retain their bindings. Cube and multisample view mappings follow existing renderer interpretation; a 3D mip is a whole subresource. Unsupported 1D arrays remain unsupported. Existing depth/stencil read-only and proxy behavior is unchanged. Shaders, graphics effects, draw order, source resolution, cameras, eye leases and compositor color/filtering are unchanged.

## Native verification

Independent tests execute actual `Gpu::exec` op24/op26/op32 commands, production bind-group construction, the production dummy binding, and real Metal draws. Ten cases execute two draws each with binding preparation off/on. They cover both binding orders, persistence after output unbinding, legal separate mips/layers and a previously cached input becoming null. All 20 draws match exact two-MRT pixel oracles; an unrelated texture remains unchanged and GPU validation errors are zero. All three input stages and depth flags 0/1/2/3 are checked. Source and binary hashes are in `target/native-span-api/native-overlap-final/receipt.json`.

A separate API control demonstrates why the change matters: an overlapping binding invalidates the complete pass, including another otherwise valid target. Replacing the offending group later does not repair already accumulated pass usage. Five legal cases pass, including disjoint mip/layer views. Evidence is `target/native-span-api/srv-overlap1/result.json`.

The correction also passes CPU range/admission checks and independent source review. Implementation is `ceaac5d`; capture identity verification is `09184fb`.

## Corrected actual game and V27.9

Two corrected private Mac runs reached the **identical translated shader**, SHA256 `21bd6330cd32748471b4f917ff7e3a3ea99838978391a2eafd096eb58f093e2d`. A bounded diagnostic records the actual op26 mutation of view 1230 to null, retains its native texture parent, and proves that the later resolved six-vertex blit uses the same output parent/subresource with the ordinary dummy input. Its complete bind-group key has nine words, rather than the previous ten-word real-texture binding. Explicit rebinding and ClearState revoke diagnostic attribution. This proof does not rely on the absence of another error alone.

The v3 run reports zero cumulative GPU validation errors through 30.10 seconds, compared with 710 by 20.03 seconds before the fix. Its actual captured draws independently reproduce all six initial/final aspects exactly, with up to 5,711 changed pixels. Its wrapper nevertheless rejected six duplicated matrix decimals that changed by one double-precision ULP during JSON parsing/reserialization. That failed receipt remains intact. The driver now binds the exact bytes it parsed using SHA256 and byte length; no numeric tolerance was introduced.

The final v4 run exits successfully after 35.09 seconds, completes its requested 20-second observation, verifies the exact raw capture identity, and reports no GPU validation errors. It reaches the same corrected shader and binding path. Its two original left-eye draws change 248 pixels in every one of the six attachment aspects; independent replay imports and reproduces every byte exactly. The earlier actual right-eye spans also pass independently. These are individual-eye span proofs, not a simultaneous stereo-pair acceptance claim. Original game inputs, graphics semantics and all recorded source/executable hashes remain unchanged across the run.

Final evidence:

- `target/native-color-binding-fix/before-after-contract.json` — matched shader/binding and error evidence, including the preserved v3 wrapper limitation.
- `target/native-draw-capture/actual-game-v3/independent-replay/receipt.json` — independently accepted immutable v3 capture.
- `target/native-draw-capture/actual-game-v4/independent-replay/final-acceptance.json` — clean final capture, corrected path, observed error-free run and exact replay.

**V27.9** is built and signed locally at `target/fidelity-v27-9/SantosVision.app`. All **40 package checks** pass: strict signatures, unchanged technical identity/entitlements/provisioning, original27 game/AOT bytes and RX protections, exact new native archive/source binding, compiled Swift/Metal input evidence, baseline feature defaults, and preservation of every recorded V27.6–V27.8 package file. The Mac capture feature and its diagnostic markers are absent from the headset package. Native archive SHA256 is `ce86d3f0062a953775417a20995dd4f7053940051a6c2dae21a1ab51859a662e`; executable SHA256 is `729703a9b4549989721664fa6eda13ced17b39bc1e452c7e8bb50160d985c8f6`. Build, verification and frozen-copy receipts are under that candidate directory.

## Visible result and remaining acceptance

The demonstrated local change is that formerly invalid bindings now execute with the specified null-input behavior. The native control tests write their expected pixels and the actual formerly failing shader reaches that corrected path without validation errors. The actual single-draw pixel capture below is a no-op, so those synthetic control pixels must not be presented as restored world shading. This has not been visually accepted in the user's bad headset scene. It is not evidence that reported grain, shading or flicker is solved.

V27.9 retains V27.7's linear-light/P3/HUD correction and V27.8's stable coverage and qualified gap sampling. The guard-3 detail limit remains roughly 853×480 central source samples at 2560×1440. The [real material/target-span replay](NATIVE-DRAW-SPAN-20261026.md) is the bounded architectural step toward a denser same-eye center; no unfinished region renderer is shipped. No headset performance or FPS claim follows from the Mac runs, especially because correcting rejected passes changes actual work performed.

No headset, iPad or simulator actions or installation occurred. The rollback, frozen earlier apps and existing saves are preserved. First-person aiming, Sense and hand tracking remain deferred.

After the local-only hold is lifted, the **single next headset acceptance test** is the same bad scene, windowed then normal third-person immersive on V27.9 at existing 1440-line settings: 15 seconds still, 15 seconds slow yaw, then 15 seconds still. Compare dark shading, texture detail, thin edges, HUD and flicker; keep experimental source-cache/common-target/single-source modes off. Capture a matched completed source/output pair at the end, with capture timing labelled separately. Wearer-visible improvement and normal stereo remain unaccepted until that test.

## Follow-up: actual pixels narrow the claim

A separate opt-in Mac capture mode (`fbfafad`) now retains one original `VS_Blit` / `PS_BlitDepthToAlpha` draw, its actual effective pipeline and immutable inputs, and all three initialized/final attachment aspects. The recorded inputs include the existing native zero dummy's real bytes, material mip bytes, both samplers, packed constants and the referenced vertex-buffer range. It uses the existing 256 MiB reservation and natural submission/completion path; it does not publish a world frame. The ordinary app excludes this feature, and the original two-draw capture remains the default.

The new isolated run `target/native-draw-capture/actual-color-null-v5/` exits successfully and completes five seconds of post-capture observation. Its capture contains ten blobs, 66,421,020 packed bytes; manifest SHA256 is `a01d908edfdc7c746efcc06c06b54f5e75e8698c3c2168d6d8ae77bfb5121da0`. Source, original inputs and graphics semantics remain unchanged. The actual target preview shows minimap scratch contents, and `world_view` remains null.

The effective write mask is 8 (alpha only), blending is disabled, and depth/stencil writes are disabled. **All 3,686,400 initial alpha values were already zero. The original draw changes zero RGB, alpha, depth or stencil pixels.** Independent Metal replay imports all three aspects exactly and reproduces every original final byte; the captured dummy is exactly `[0,0,0,0]`. Reconstructing the old alias produces the expected `RESOURCE` / `COLOR_TARGET` validation conflict and leaves all initialized targets unchanged. An explicitly altered two-invocation control also fails; because its first invocation is a no-op, it does not demonstrate the loss of earlier visible work.

The consumer deliberately rejects an all-attachment no-op as evidence of visible improvement. Its preserved `independent-replay/receipt.json` therefore has `run_returncode: 1` and `passed: false`; byte parity and expected rejection are separately recorded in `replay/result.json`. No gate was weakened and no retry selected a more favorable image. RGB previews interpret raw intermediate bytes as sRGB for display only; the intermediate transfer encoding is unverified, and raw bins are authoritative. Alpha and unscaled differences are exported separately.

The implementation passes 11 producer, 3 driver and 21 runner CPU tests, independent source review, and the ordinary Mac build check. Two consumer metric tests and exact PNG byte/tag checks also pass. An initial Mac check incorrectly disabled the required default JIT feature without supplying an embedded game; that invocation failed as designed and is retained beside the successful ordinary default check. No new headset candidate was built. Every recorded V27.6–V27.9 package file, the V27.9 native archive and original visionOS AOT remain hash-identical (`package-preservation-review.json`).

This result rules out a claimed visible contribution from this captured draw. It leaves the earlier rejected encoder's other work and the current bad headset scene unresolved. A source-density architecture still needs initialized complete scene targets and the finished processing graph; copying inherited low-density G-buffers into a denser region cannot supply that missing work. A separately checked per-axis guard change offered only modest gains away from the worst guard-3 case and was not implemented.

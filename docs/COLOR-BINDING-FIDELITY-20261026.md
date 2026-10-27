# Preserve native passes when color inputs overlap outputs

The native adapter now implements D3D11's persistent null-binding rule for color render targets and overlapping shader inputs. Previously, it forwarded both bindings to wgpu. A real game draw demonstrated that this invalidated the render pass; it was not a gamma, post-processing selection or resolution problem.

## Demonstrated failure

In the private unchanged-1440p Mac run `target/native-draw-capture/actual-game-v2/`, the first conflict occurred at present 537 with the GPU error counter still zero. The resolved pixel shader was `im::PS_BlitDepthToAlpha` (guest hash `7f531ca66548aefe`), drawing six vertices. Its actual texture input at group 1/binding 37, guest view 1230, retained the same native allocation as color target 0, guest texture 1229. Both selected mip 0/layer 0 of the 2560×1440 BGRA8 image.

The shader samples that input to derive output alpha; it is not an unused declaration. The following validation errors report incompatible `RESOURCE` and `COLOR_TARGET` usages, first on command encoding and then submission. The 20-second stats sample counted 710 errors. No claim is made that this is the sole cause of the user's headset complaint or that the fresh developer scene matches the bad headset scene.

Evidence is `first-color-feedback.json`, the corresponding `stderr.log`, and the run's source/binary-bound receipt. The diagnostic compares actual retained native parents and subresource ranges, rather than assuming guest IDs identify the same allocation. Captured original draws earlier in that run independently replayed exactly; later errors are not relabelled as whole-run health.

## Correction

At an incoming shader-resource binding command (op24), an overlapping color input is stored as null. At a render-target binding command (op26), existing overlapping inputs in all three shader stages are unbound. The null state persists after the output is unbound until the game explicitly binds the input again. The existing renderer supplies its ordinary zero texture for that null binding.

This matches Microsoft's [PSSetShaderResources](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-pssetshaderresources) and [OMSetRenderTargets](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-omsetrendertargets) contracts. Substituting a dummy only at draw time would be incorrect because it would silently restore the old input when the output was removed.

The check compares current native texture allocations and the renderer's normalized mip/layer ranges. Disjoint mips, disjoint layers and different allocations retain their bindings. Cube and multisample view mappings follow existing renderer interpretation; a 3D mip is a whole subresource. Unsupported 1D arrays remain unsupported. Existing depth/stencil read-only and proxy behavior is unchanged. Shaders, graphics effects, draw order, source resolution, cameras, eye leases and compositor color/filtering are unchanged.

## Native verification

Independent tests execute actual `Gpu::exec` op24/op26/op32 commands, production bind-group construction, the production dummy binding, and real Metal draws. Ten cases execute two draws each with binding preparation off/on. They cover both binding orders, persistence after output unbinding, legal separate mips/layers and a previously cached input becoming null. All 20 draws match exact two-MRT pixel oracles; an unrelated texture remains unchanged and GPU validation errors are zero. All three input stages and depth flags 0/1/2/3 are checked. Source and binary hashes are in `target/native-span-api/native-overlap-final/receipt.json`.

A separate API control demonstrates why the change matters: an overlapping binding invalidates the complete pass, including another otherwise valid target. Replacing the offending group later does not repair already accumulated pass usage. Five legal cases pass, including disjoint mip/layer views. Evidence is `target/native-span-api/srv-overlap1/result.json`.

The correction also passes CPU range/admission checks and independent source review. A fresh corrected game run, signed package and hardware acceptance are separate gates; their results will be added after completion. The frozen V27.8 package and original rollback are preserved.

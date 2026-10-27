# Exact native post-world compute replay

The private reconstruction path now executes an actual game compute dispatch with **byte-identical output**. It retains the successful final shader, actual resources and dispatch dimensions, instead of inferring a shader from historical guest IDs. This extends the original-density reconstruction work toward the finished lighting/postprocessing chain. It does not change the signed V28 app or establish a sharper finished image.

Producer checkpoint: `c368151`; consumer checkpoint: `63c85bd`. Earlier native clear promotion: `6cbcf63`, documented in `NATIVE-TARGET-SPAN-20261026.md`. The app title is **V**; both signed V28 packages remain uninstalled. No headset, iPad or simulator query/action occurred.

## Actual game evidence

The captured dispatch at command 108306, encoder epoch 836, reads two 2560×1440 images: R32Float depth and BGRA8 UNORM material data. It uses a 352-byte packed uniform range originally at offset 17558528 and a separate 96-byte constant buffer. The actual successful shader has local workgroup size 16×16×1 and dispatch counts 160×90×1. Its storage-format rewrite selects **RGBA16Float**, producing a 160×90 texture. The historical trace's inferred RGBA32Float format and 40×23 dispatch are not substituted for this observed invocation.

| Check | Result |
| --- | --- |
| Original output pixels changed by this dispatch | 14,400 of 14,400 |
| Original output bytes changed | 91,024 of 115,200 |
| Initial texture subresources reproduced exactly | 3 of 3 |
| Replayed final bytes differing from original | 0 |
| Nonfinite original output components | 0 |
| Original source, WASM/AOT and graphics inputs unchanged | Yes |

Capture artifacts: `target/native-draw-capture/actual-compute-v12/`. The natural GPU capture completed at 8.452 seconds; the owned process exited 0 and was reaped after 14.861 seconds total. These are tool wall times, not game FPS or isolated GPU execution times. Six payload blobs total 29,722,048 bytes. The full capture and staging remain within existing bounds. The existing diagnostic guard is 1.3, with fresh developer-mode state and synthetic two-eye requests; this is not the rejected physical scene.

Producer binary SHA256: `22e03aa6e35dc78b784195615b54177735f42967cc2cab2f5e5d3671b9b67110`.
Native capture SHA256: `001670d94972968244d426c4e1ff4fd082808df1d59a169f6c6e4130516b91f9`.
Actual final shader SHA256: `ac7bd423acd30069bae7e323c9386ee4965f8a907c94444a26b9b48b9c62c241`.
Final output SHA256: `5a44e7d64d1795bf0dcd790b3f26af208e04a6738f98a2003040edeac1818548`.

The independent consumer ran on Apple M4 Metal. Its native replay is at `target/native-compute-api-fixture/final2/native-v12-run/`, using binary SHA256 `0bf49355425e4e87eaff7ea591024c31ac0a3f9a216e1b6eb868c3eaadece1dc`. Its wrapper verifies original and frozen inputs and sources remained unchanged. Root independently rehashed all captured blobs and compared the actual final output file directly with the replayed file; see `root-native-verification.json` and `root-replay-verification.json` in the capture run.

The preceding main-world observation belongs to eye1/camera84/request70 in epoch2. It is retained as context only. The isolated compute scene deliberately has `eye: null`: an earlier draw does not establish the dispatch's ownership or a complete same-eye dependency chain. The accepted 1,277-draw world interval is a **different capture**. Native allocation IDs and versions cannot be joined across those processes.

## Implementation and verification

The feature-only producer snapshots actual resolved bindings immediately before the unchanged dispatch and snapshots the output immediately afterward in the original encoder. It exports only after natural GPU submission completion. Actual native buffer handles/ranges, texture parents, selected mip/layer/aspect, final rewritten shader module and output versions remain explicit. Writable buffers/counters, unsupported storage views, output-parent aliases, sampled/output aliases, missing provenance, active queries and GPU errors reject capture. Staging remains charged until both GPU and CPU owners release it.

The generic executor adds explicit compute pipelines and ordered dispatches on persistent textures. Storage output is limited to supported write-only D2 color views with one selected mip/layer. Readonly buffers, samplers and sampled textures preserve their real layouts. Every selected input/output version must exist, every output has exactly one advancing transition, and partial writes require original initial contents. CPU admission rejects oversized workgroup counts, stale/reused native versions and unsupported or unmodelled resources. No graphics effects or app resolution are changed.

An independent authored 13×7 oracle checks offset-256 uniform binding, selected mip1/layer1, partial-border preservation, untouched other mip/layer data, and a subsequent raster draw consuming the produced texture version. All six initial subresources and five final outputs match their independent CPU expectations. Its original successful binary is `70f8b82b07627027c8ac4ff6be62e92bf8aebdfc7347212dd1885b6c673165a2`; evidence is under `target/native-compute-api-fixture/final/oracle-run/`.

The first actual native wrapper attempt rejected a receipt filename mismatch before executing a GPU. The importer was corrected to emit the common `native-import.json` contract and its CPU test now checks that interface. The failed attempt remains preserved. Only the importer filename/test changed for the final2 build; executor, model, validator, authored fixture and wrapper hashes match the successful oracle. The oracle was not unnecessarily repeated.

Verification includes 22 native capture CPU tests, 8 resolved-binding CPU tests, 26 runner boundary tests and 18 integrated consumer CPU suites. Independent producer and consumer reviews found no remaining blocker after fixing dispatch-product and native-version collision checks. Review artifacts are under `target/native-compute-review/`, `target/compute-consumer-review/` and `target/native-compute-import/`.

## Remaining quality boundary

Color correction, sampling detail and performance are separate. V28 already contains the verified color/filtering/HUD correction and source-publication correction. This work changes the private reconstruction implementation, not the visible V28 image. It does not remove the physical guard3 central-sample limit, prove repaired headset shading/flicker, or establish M2 performance.

A later single-capture world/proxy/compute graph now passes exact replay: see [connected graph verification](NATIVE-CONNECTED-GRAPH-20261026.md). It uses its own actual native identities and bindings, without splicing these older captures. Later lighting, mip generation, copies and query-dependent work remain outside the accepted graph. Inherited color1/2/3 initialization still needs its real producers or a complete finished-image proof that the inherited pixels are unused. Keep original-density final-image equality as the gate before changing density; do not upscale inherited input bytes.

The next single headset acceptance, after the local-only hold is lifted, remains signed V28 with the front-glass icon in the same rejected scene at 1440 Clarity: window → normal third-person immersive, 15 seconds still → 15 seconds slow yaw → 15 seconds still. Compare texture detail, dark shading, thin edges, HUD and flicker using matched per-eye sources/output. Normal independent stereo stays enabled and experimental rendering paths stay off.

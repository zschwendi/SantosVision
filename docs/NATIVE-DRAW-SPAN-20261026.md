# Real native material and target-span replay

The same-eye regional-rendering prerequisite now reproduces **two original game draws with their real material and state**, including inherited target contents. All four color targets, depth and stencil match the original native GPU result byte for byte. This advances the earlier synthetic-material density fixture; it does not produce a finished higher-density scene or change the signed V27.8 app.

## Actual capture and replay

The private Mac fixture used the existing hash-pinned game WASM and Mac AOT, unchanged 2560×1440 Clarity graphics files, synthetic static two-eye input and a new disposable developer-mode save. No existing player profile/save was copied or replaced. The diagnostic source guard is fixed at 1.3; it is not production coverage tuning or headset acceptance. The immutable AOT loader checks exact content hashes and actual-engine deserialization and forbids compilation fallback.

The accepted span is right eye 1, request/head frame 69, camera sequence 84 and provenance epoch 2. Its two indexed draws use 3,072 and 1,632 indices. Only SRV/IB/VB state commands intervene. Both draws retain the same live native four-MRT/D32S8 attachments and exact eye/request metadata. Capture contains 34 immutable blobs, 154,944,488 packed bytes, and completed original before/after targets. Original game inputs, source/executable identities and disposable graphics semantics passed post-run verification.

The standalone Metal replay imports and reads back all six initial aspects exactly, then runs the original draws in order into persistent attachments. Every final byte equals the native reference. This is a meaningful draw result, not an unchanged-target control:

| Aspect | Pixels changed by original draws | Replay differences |
| --- | ---: | ---: |
| Color 0 | 1,388 | 0 |
| Color 1 | 1,388 | 0 |
| Color 2 | 1,227 | 0 |
| Color 3 | 1,388 | 0 |
| Depth | 1,388 | 0 |
| Stencil | 31 | 0 |

The replay reported no GPU validation errors. It used actual GPU uniform ranges, VB/IB bytes, sampled mip data, view/sampler descriptors and effective pipeline state; no synthetic material or zero-cleared substitute was accepted. The consumer rejects no-op evidence, unmodelled intervening writes, mismatched eye/target identity and incomplete inputs. An earlier controlled API fixture separately verified nonzero depth/stencil restoration and BC1 small-mip/sRGB-view transfers.

Private evidence under this checkout:

- `target/native-draw-capture/build-v2/receipt.json` — optimized executable, full frozen native source inventory and runner binding.
- `target/native-draw-capture/actual-game-v2/receipt.json` — capture-byte and original-input verification.
- `target/native-draw-capture/actual-game-v2/capture/capture.json` — SHA256 `46f92ddf9fbe759a92746eb62d187c9eabd219332d2dd13c744b0337db106a06`.
- `target/native-draw-capture/actual-game-v2/replay-final/receipt.json` and `replay/result.json` — independent actual-game parity.

The capture hook is macOS-only and requires the `native-draw-capture` Cargo feature. It encodes copies into existing command buffers and waits for normal submission completion; it adds no forced submit or GPU wait. All outstanding and abandoned readbacks share a 256 MiB live allocation limit. Metadata and attempts are separately bounded. Success is published atomically after complete readback. Ordinary builds compile out these hooks.

## A separate real rendering failure discovered during observation

The completed capture above precedes a later rendering error. During the following 20-second observation, the first offending draw was recorded before any validation error: `im::PS_BlitDepthToAlpha` sampled binding 37 (`t5`, view 1230) from native texture 1229 while writing that same native texture, mip 0/layer 0, as color target 0. The shader actually uses this sample to determine output alpha. The subsequent GPU validation error rejects conflicting `RESOURCE` and `COLOR_TARGET` usage.

This is not inferred from guest IDs: the record compares the retained sampled and attachment views' native parents and overlapping ranges. Evidence is `target/native-draw-capture/actual-game-v2/first-color-feedback.json` and its subsequent stderr. A controlled Metal regression independently demonstrates that such a binding invalidates the whole pass, including another otherwise legal target. Legal disjoint mip/layer cases remain valid.

The native adapter's original op24/op26 handling omitted D3D11's persistent nulling of overlapping input views. The relevant contracts are Microsoft's [OMSetRenderTargets](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-omsetrendertargets) and [PSSetShaderResources](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-pssetshaderresources). The separate [production correction, actual game verification and V27.9 package](COLOR-BINDING-FIDELITY-20261026.md) are now complete locally. Their contribution to the user's headset complaint remains physically unverified.

The first failed run is preserved at `target/native-draw-capture/actual-game-v1/`. It omitted consumed-request retention, so every capture candidate failed before encoding. Its later usage conflict therefore was not caused by capture copies/pass splitting. The corrected driver explicitly enables bounded request-receipt retention and never enables the old geometry-capture path. No failed run was relabelled as success.

## Remaining quality boundary

The demonstrated parity covers one shader-family source-list segment, one individual eye and one original target domain. It does not establish completed lighting, transparency, sky, water, SSAO, bloom, tone mapping, history, higher-density initial attachments, live stereo or hardware performance. Expanding backward to a closed opaque span and forward through the finished-image dependencies remains necessary before a regional image can be published. Upscaling inherited low-density targets is not a substitute. See [the concrete dependency plan](../scripts/central-density-proof/SPAN-DEPENDENCIES.md).

V27.6, V27.7, V27.8 and the canonical rollback remain preserved. No headset, iPad or simulator action occurred. The next physical acceptance is the single same-scene window-to-immersive test in the [V27.9 report](COLOR-BINDING-FIDELITY-20261026.md), after the local-only hold is lifted.

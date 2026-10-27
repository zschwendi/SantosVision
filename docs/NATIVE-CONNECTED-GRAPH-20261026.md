# Connected native world, depth proxy and compute replay

The private reconstruction now executes a connected interval from **one actual game capture**, preserving produced textures between stages. All 13 intermediate comparisons and all eight final outputs are byte-identical on the local Apple M4 Metal backend. This closes the previously separate world-pass and compute proofs for this interval. It does not change the signed V28 package, increase source density, or establish a finished HUD-free image or headset quality acceptance.

Implementation checkpoints: `b73c5ef` adds exact depth conversion and ordered checkpoints; `fd96318` captures the connected original operations; `cf07e3a` imports and replays their exact dependencies.

## Actual captured interval

Capture: `target/native-draw-capture/actual-graph-v14/`.

| Evidence | Result |
| --- | --- |
| Capture namespace | `native-graph-10145-158395` |
| Native present | 381 |
| World raster anchor | Eye0, epoch2, camera101, request/head78 |
| Main world interval | 1,334 draws, commands158383–168138 |
| Depth conversion | Allocation17/depth107178 → allocation6240/color107180 |
| Intervening raster interval | Two sparse-MRT draws, commands168138–168158 |
| Actual compute | Command168164; workgroups160×90×1 |
| Ordered native events | 1,342 |
| Immutable payloads | 10,265 blobs; 256,749,896 packed bytes |
| Actual staging allocation | 261,187,156 bytes, within the unchanged256MiB cap |
| Original GPU errors | Zero |

The depth conversion occurs after the main raster checkpoint and before the two sparse-target draws. Those draws bind color slots0/3 and read-only depth/stencil. Their output happens to be unchanged in this capture; both draws are still executed and checked. The compute stage samples the exact produced proxy at binding39 and writes allocation275/version107185. Binding40 is an actual captured1×1 dummy in this run. The older isolated v12 dispatch instead sampled a full-size BGRA texture there; its bindings must not be substituted into this capture.

The raster anchor is preserved on every actual draw. Compute remains explicitly unattributed: sharing an exact dependency chain and original present does not fabricate per-eye ownership for a dispatch. The diagnostic source uses the existing fixed1.3 guard and fresh disposable local graphics state. It is not the rejected physical guard3 scene or a coherent stereo frame.

## Executed proof

Frozen producer: `target/native-draw-capture/build-v14/native-span-capture`, SHA256 `e941bff6f64fd428db2abb19c47c0662a798758272055375eab27ebf93e99d4f`.

Raw capture SHA256: `376d12c3b8fb2de66aaf308b6dd730b6196bef5255cba7ef70bbc0a86d033c4d`.

Frozen consumer: `target/native-graph-import/final/native-scene-replay`, SHA256 `b394340914178feeb92520f916812897186b0595974bac72231abf6b10ec238b`.

Strict import and Metal replay: `target/native-graph-import/final/{actual-v14-import,actual-v14-run}/`.

| Replay result | Verified value |
| --- | --- |
| Raster pipelines / compute pipelines | 92 / 1 |
| Initial subresources exact | 5,393, including three real native clears |
| Intermediate comparisons | 13 across four ordered checkpoint commands; zero differing bytes |
| Final comparisons | Eight; zero differing bytes |
| Produced input initializers removed | Eight, replaced by their exact earlier produced texture versions |
| Original/frozen inputs and sources unchanged | Yes |
| Resolution-independent initialization / finished image accepted | False / False |

Root independently compared all eight emitted final files against their original reference bytes. See `actual-v14-run/root-verification.json`, `actual-v14-run/receipt.json` and `actual-v14-run/replay/result.json`. Independent producer review rehashed every capture payload and confirmed all3,686,400 original depth pixels match the produced proxy; see `target/native-joined-graph-audit/actual-v14-verification.json`. Source review and its exact build hashes are under `target/native-graph-consumer-review/`.

The importer preserves allocation IDs, subresource versions, shader text, buffer snapshot ranges and the original event order. A downstream captured input is removed only when its exact native version has an earlier producer and its bytes match the producer's checkpoint. It rejects version collisions across allocations, contradictory overlapping buffer snapshots, overlapping raster event intervals, stale attachment loads, cross-capture/present splicing and unmodelled operations. At least one actual raster→proxy→compute path is required; every recorded proxy and intermediate stage is still executed and verified.

The capture hook permits storage-texture binding changes between raster intervals. It rejects actual buffer-counter writes at the native write operation, while preserving the renderer's existing null/texture binding no-ops. It rejects resource-creation and skipped-draw warmup candidates rather than silently omitting them. The final accepted candidate needs neither omission nor a wider byte budget. Completed immutable captures survive unrelated later commands until natural submission completes.

Validation: 29 capture CPU tests, 27 runner boundary tests, 27 integrated consumer CPU suites, an ordinary-feature build check, and the actual Metal graph replay. The separately authored depth-proxy oracle also verified an intermediate mismatch fails acceptance even when all final outputs match. The first v13 graph attempt is retained as a rejected harness run; the missing binding-opcode handling was corrected before v14. No failed attempt is counted as visual or runtime acceptance.

## Remaining work and acceptance

This step preserves the original image data; it demonstrates **no new visible quality improvement** in V28. Color correctness, source freshness, sampling density and performance remain separate results. The existing V28 color/publication corrections retain their earlier evidence, while guard3's approximately853×480 central samples from a2560×1440 source remain a real clarity limit.

The next local implementation must extend this same capture through subsequent lighting/postprocessing, copies and any query-dependent work to the finished HUD-free endpoint. The inherited main color1/2/3 roots still need actual producers or a complete proof that their inherited pixels do not contribute to the finished image. Only then can a bounded density change be compared without scaling unknown initial images or losing graphics effects. The graph replay is an offline correctness implementation; its timings are not gameplay FPS or headset performance.

Both platform app titles and the signed visionOS build28 are **V**. The front-glass V28 package remains local and uninstalled at `target/fidelity-v28-front-glass/SantosVision.app`; canonical27 and frozen27.6 remain preserved.

The next single headset acceptance, after the local-only hold is lifted: use signed V28 with the front-glass icon in the same rejected scene at1440 Clarity; window → normal third-person immersive,15 seconds still →15 seconds slow yaw →15 seconds still. Compare texture detail, dark shading, thin edges, HUD and flicker against matched per-eye source/output images. Keep normal independent stereo and experimental renderer settings off. No headset, iPad or simulator action occurred during this work.

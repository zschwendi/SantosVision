# Finished HUD-free graph replay — October 27, 2026

The private capture and replay tools now implement the remaining original-image operations and an exact accepted-publication certificate. **A complete actual-game finished image has not yet been admitted or replayed.** Three bounded captures rejected incomplete evidence; their failures remain preserved. This checkpoint adds no new visible-quality or performance claim and is not another signed app candidate.

The app is **V** on both platforms. The preserved signed Vision Pro app remains **build 28**, with the front-glass icon and no FIVE text, at `target/fidelity-v28-front-glass/SantosVision.app`. Technical targets and bundle IDs retain SantosVision. No headset, iPad, simulator, installation, repository rename, save replacement or reset occurred. The canonical rollback checkout and V27.6 package were not edited.

## Implemented boundary

Schema 7 retains the actual ordered raster, context-raster, compute, depth-proxy, stencil-proxy, clear, copy and mip operations. Raw later raster contexts remain raw, including absent world metadata. Ordinary schema-4/6 qualification stays strict. Original queries and readbacks execute only in the original game; replay records their exact metadata without writing guest results or pretending to rerun result-dependent execution.

The graph ends at the actual full copy from the finished color parent into `HudImages.world`, immediately before HUD drawing. HUD rendering and publication remain original-only. A feature-only token verifies the exact native color/world/depth handles, selected depth VersionRef and WorldView, native present, eye and request identities. Acceptance occurs only after the normal callback inserts `Ready`, including exact `Arc::ptr_eq` equality of the retained HUD sidecar. The HUD-free sidecar is never equated with the HUD-bearing snapshot color. Normal publication remains unchanged if capture certification fails.

The disk spool receives only existing natural submissions. Readback memory stays charged until original GPU completion and the last CPU owner releases it. Per-batch polling credits last through mapping/writing. Payloads and final manifests are create-only. Rejection after an anchored attempt is terminal, preventing streamed-prefix name reuse or unbounded failed-candidate accumulation.

Finished mode has a fixed **512 MiB staging limit**, a **1 GiB cumulative packed-payload limit**, and a **256 MiB single-snapshot limit**. Every older capture mode retains 256 MiB staging. The replay's logical GPU allocation bound remains 256 MiB. The larger private staging bound followed an actual 256 MiB rejection before the first natural submission; it does not change source resolution, native submission cadence, or production headset memory policy. No automatic further increase is enabled.

## Verified local results

- Producer: 39 capture CPU tests pass; 3 driver tests pass; normal-feature library check passes. Final frozen producer `build-v17/native-span-capture` SHA256: `69a7da7dd6765d4bffc8b7636c3bc984543fc94de9c078619864a7570a840ebc`. Its build receipt confirms unchanged source and runner inputs.
- Consumer: 40 CPU suites pass. Schema-7-only inclusive command boundaries preserve exact global event ordering at native helper seams; legacy exclusive bounds remain unchanged. The native runner's 29 Python tests pass.
- Authored Metal operation oracle: **52 initial subresources, 52 final aspects and 11 ordered checkpoint comparisons are byte-exact**, across 21 commands. It covers partial color copies, combined depth/stencil copies, native mip filtering including sRGB, compute followed by context raster, and stencil-to-RG8 conversion with every stencil byte value 0–255. Unrelated mip/layer/depth contents remain exact. This is operation proof, not an actual finished game image.
- Authored spool oracle: 140 natural batches, **293,601,280 packed bytes**, all byte-exact; peak live and mapped staging **8,388,608 bytes**; final live reservations and polling credits zero. It proves streaming beyond the old cumulative bound, **not** a single encoder retaining more than 256 MiB.
- Independent reviews covered native helper bytes, operation/version ownership, descriptor retention across compute, shader identity, same-sidecar publication, query capacity, spool lifetime and manifest publication. Original mip and stencil shader bytes remain identical.

Private evidence:

- `target/native-graph-operations/receipt.json` and `target/finished-stencil-proxy/receipt.json`
- `target/finished-graph-endpoint-review/REPORT.md` and `receipt.json`
- `target/native-finished-graph/oracle-v2/run/{receipt.json,replay/result.json}`
- `target/native-stencil-oracle/receipt.json`
- `target/finished-spool-oracle-v1/{receipt.json,source-receipt.json}`
- `target/finished-staging-budget-assessment/assessment.json`

## Actual-game attempts and remaining work

All attempts used a fresh disposable graphics fixture, original pinned game/AOT inputs, normal two-eye requests, fixed private fixture guard and unchanged graphics effects. Each child exited and was reaped; runner receipts confirm original inputs unchanged. None produced an accepted final manifest or image payload. No occlusion-capacity exhaustion was logged in these runs.

| Attempt | Frozen producer | Rejection | Child wall time |
| --- | --- | --- | --- |
| `actual-finished-v15` | `da9c51206c00771b0fe47ad6a4df1226afab968cf24945ac119f81213c2496ba` | 256 MiB retained/live staging admission before natural submission | 21.648 s |
| `actual-finished-v16` | `85d68bf2e1dd527a07960e25a1cfd09c307ffcdaf0d9e9c96c76dc2b501e40a2` | Generic dirty-proxy guard; its exact rejected flag was not recorded | 22.070 s |
| `actual-finished-v17` | `69a7da7dd6765d4bffc8b7636c3bc984543fc94de9c078619864a7570a840ebc` | An original draw was skipped at command 2,563,294 | 19.018 s |

These wall times are capture lifecycle durations, not rendering performance. V16 does not independently prove a stencil defect; exact stencil support was added and tested without changing native rendering. V17 preserves the skipped-draw rejection rather than accepting a successful prefix.

The next local step is to classify the actual skipped draw and determine whether a fully compiled, complete original frame can be selected without changing rendering or weakening the completeness gate. Then capture the full pre-HUD graph, strictly import it, and compare actual game pixels through every ordered checkpoint and the accepted endpoint. A full capture must also reject any logged occlusion-query exhaustion, including original HUD postlude operations; the current auxiliary capacity hook stops at the pre-HUD graph boundary.

The earlier connected graph remains the accepted actual-game proof: 1,334 main draws, original depth conversion, two sparse draws and an original compute dispatch, with all checkpoints/final bytes exact. See [connected graph evidence](NATIVE-CONNECTED-GRAPH-20261026.md). This new checkpoint does not supersede that narrower successful result with a broader claim.

Color correctness, source sampling and performance remain separate. Production V28 retains the registered color/filtering correction and source-ownership correction. The measured color difference was modest; no restoration of missing central samples is claimed. At 3× guard, a 2560×1440 source still provides approximately 853×480 central samples. This private replay work has not yet implemented or accepted a higher-density finished center.

## Single headset acceptance test, after the local-only hold is lifted

Use the preserved signed V28 front-glass package in the same bad scene and 1440 Clarity profile: windowed → normal independent-eye third-person immersive; 15 seconds still, 15 seconds slow yaw, 15 seconds still. Compare matched source/output texture detail, dark shading, thin edges, HUD and flicker. Keep experimental caches, common-target and single-source paths off. Hardware quality remains unverified.

Implementation checkpoints: consumer `92bc68d`; producer `e8a74b0`.

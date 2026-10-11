# Atomic stereo pair bridge — October 11, 2026

An additive, default-off native bridge now publishes and acquires two GPU-complete eye snapshots as one transaction. It preserves the existing 288-byte `SVWorldFrame` v1 ABI. `SVWorldPair` v1 is 592 bytes: version, size, a scene epoch and two left/right frames. `sv_world_acquire_pair` returns 0 for success, 1 for pending/off/stale/capacity and -1 for invalid input. The caller initializes version/size on every poll and releases both leases only after compositor completion.

`MUGUET_XR_PAIRED_RENDER=1` requests the new mode. Pair publication checks common scene/request/head/presentation identity, valid distinct cameras, stage mapping, supported depth ranges, distinct native color/depth targets and active provenance. Both snapshots are copied in one queue submission and exposed together on completion. Acquisition cannot partly consume lease capacity. Resizing or session invalidation leaves held snapshots releasable; mixed pair epochs cannot acknowledge stereo completion.

**This is bridge infrastructure, not completed paired GTA rendering.** `CertifiedPairEpoch` has no production constructor. Today's alternating natural game frames cannot mint it or satisfy pair acquisition. The real producer must render two eyes from one game state with qualified culling, deferred passes, targets and resource lifetime. Static inspection found that the available guest render entry points mutate portal/culling/list state or consume lists/fences. Host geometry replay also needs camera-uniform semantics and conservative culling. The mode therefore remains pending for actual gameplay and must fall back to the existing renderer.

Sol implemented the bridge; root checked the ABI, lifetime design and source-bound test results. Two CPU checks passed for metadata rejection and ABI/pending behavior. Two serial Apple Metal fixtures passed: the new paired snapshot lifetime fixture and the existing legacy immutable-snapshot fixture. The new fixture uploaded distinct eye color/depth, changed the sources, verified held snapshot bytes, resized both eyes, verified the new bytes, rejected mixed epochs, filled the 12-lease pool without partial acquisition, invalidated the session and released old/new leases. Test certification tokens exist only under `cfg(test)`.

Local evidence is under `target/verification/reprojection-implementation-v1/`. GPU test source manifests before/after are identical; root independently checked both raw log hashes and their passing results. These fixtures validate bridge behavior, not game geometry, physical tracking, headset stereo or 4K60 performance.

| Evidence | SHA-256 |
| --- | --- |
| `stereo-gpu-pair-lifetime.log` | `0ffcb93f539eb369eacabb4906b4ae937b7d51d610969d58da88405de600644e` |
| `stereo-gpu-legacy-lifetime.log` | `c77a21dc1f1b4cf8fbd1bddc1c0827a43497fc298fba8c4e04db8b38120de329` |

The same directory retains CPU logs, `stereo-pair-contract.md`, `render-boundary-certification.md` and `gpu-stereo-feasibility.md`. No game app or headset was run for this checkpoint. Parent implementation work `w-ab947ade438e427bb263135cabb1ac55` remains open.

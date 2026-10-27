# V image-quality checkpoint — October 27

**There is no newly proven visible improvement over the existing signed V28 in this stretch of work.** The app title and front-glass icon are complete. The additional work below improves local capture/replay verification; it has not restored central detail or demonstrated a fix for headset flicker. The full visual-quality objective remains incomplete.

## What is available to judge

The preserved, signed `target/fidelity-v28-front-glass/SantosVision.app` is **V**, build **28**, bundle ID `com.zephyrstudios.santosvision`. It remains uninstalled. Its executable and assets were rehashed unchanged:

- Executable: `dde735e5f31326b19a123707b069f67ad1402390f64b7c8c5d7407b701d0530b`
- Assets: `5fe10adb8705b7964e35cbffff9637b612e72ae3aa579200ba2cdb208a0b3060`

V28 already contains the earlier color/filtering and source-publication corrections. Their local evidence does not establish that the user's grainy headset scene is fixed. The recorded 3× guard still implies about 853×480 central source samples from a 2560×1440 image. No claim of restored detail, improved headset frame rate or hardware acceptance is made.

## What this work accomplished

- Corrected a private test-driver defect: finished-image capture had never enabled the existing HUD-readiness flag needed for its endpoint.
- Identified an original nonzero tessellation draw omitted by the shared renderer because translated control-point/hull/domain support is absent. The original shader asset exists. The request was 5,742 indices × one instance; pixel visibility and matching windowed-scene occurrence remain unproved. No shader substitution or graphics-effect disabling was applied. See [exact evidence](ORIGINAL-TESSELLATION-OMISSION-20261027.md).
- Added an explicit executed-image replay contract that records the known omitted draw and reports `all_requested_draws_executed=false`. The strict contract still rejects omissions.
- Added bounded storage and fresh memory/disk admission for that private mode; corrected a stale 256 MiB output-monitor limit. These changes do not alter app resolution or normal rendering.
- Added exact float-to-depth helper replay. The original 376-byte shader is unchanged. An authored Metal oracle passed 18 initial, 18 final and 25 checkpoint comparisons byte-for-byte. The consumer's 49 CPU suites include adjacent-context/version continuity and rejection of cross-eye substitutions.
- Added capture-stage splitting only when raw context changes while the exact native targets and domain remain equal. Target changes still reject. Its 47 producer CPU tests pass.

These are verification foundations, not a completed visual fix. No experimental rendering path was deployed.

## Latest actual result

With float-to-depth capture implemented, actual game capture `actual-executed-finished-v22` rejected `span eye, camera or native target changed`. It produced no accepted finished-image manifest. The new context-splitting implementation has not yet been exercised in the game.

The next prepared capture, `actual-executed-finished-v23`, **did not launch a game process**: its fresh preflight found 5,932,957,696 disk bytes available, below the fixed 6 GiB floor. The refusal and exact observations are preserved in `target/native-draw-capture/actual-executed-finished-v23/host-admission.json`. No cleanup or threshold reduction was performed to force it through.

Latest frozen native tool: `target/native-draw-capture/build-v23/native-span-capture`, SHA256 `d678066f827924272267c2f8f4d4aa0aaa2be71fe7b39299d02e772052282fd5`. Latest frozen consumer: `target/native-finished-graph/context-v1/native-scene-replay`, SHA256 `2a09229b9e73cda142d9df81d1016fbe22c129e0a3a742a202a339ba43bcae34`. Their source-bound receipts are beside the binaries. Full actual finished-image capture/import/replay is still unaccepted.

Implementation commits: `e201eff` (driver prerequisite), `2914faa` (separate executed-image contract), `1571016` (bounded producer/storage), `712616c` (float-to-depth), `dc36006` (context boundaries).

## Next single headset acceptance test

Only after the local-only hold is lifted: use the existing signed V28 in the same bad scene and 1440 Clarity settings. Compare windowed → normal independent-eye third-person immersive, then 15 seconds still, 15 seconds slow yaw, 15 seconds still. Judge texture detail, dark shading, thin edges, HUD and flicker. Keep experimental caches, common-target and single-source paths off. This is the quickest way to judge the existing candidate; the remaining quality fix has no reliable completion ETA yet.

The canonical rollback checkout, frozen V27.6, original game assets and saves were preserved. No headset, iPad, simulator, installation, GitHub rename or push occurred.

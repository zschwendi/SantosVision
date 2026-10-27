# Private native draw capture

This diagnostic feature runs the actual game without a desktop window, using a fresh developer-mode save and synthetic static two-eye input. It captures a natural two-draw span for material/state replay. It does not reproduce the current bad headset scene, steer an airport fixture, or establish finished-world/stereo/hardware quality.

`--capture-kind two-draw` is the unchanged default: schema 2, `native_draw_span`, exactly two original draws. Explicit `--capture-kind color-null` selects the feature-only corrected color-binding capture: schema 3, `native_color_null_draw`, one original draw with its resolved resources and initial/final attachments. Its `world_view` may be null; it establishes neither a same-eye world span nor a finished scene. Both kinds still require independent semantic replay after byte admission.

Explicit `--capture-kind target-span --scene-provenance --same-ray-alias` captures schema 4, `native_target_span`: a complete heterogeneous four-MRT/depth/stencil interval between original target-binding commands. It records every executed draw, actual emitted arguments, final shader modules, resolved bindings and immutable input snapshots, plus all six original initial/final target aspects. Unsupported commands, skipped draws, changed eye/world/targets, active queries or incomplete intervals reject the entire candidate. Ordinary producer submissions retain their snapshots until final natural completion. This is an original-density pass, not a dependency-closed finished image or a high-density initializer. Its strict adapter is documented in `scripts/native-scene-replay/NATIVE-TARGET-SPAN.md`.

Whole intervals retain the 256 MiB aggregate staging/payload cap, including aborted in-flight snapshots until both GPU and CPU owners release them. Exact pipeline, binding, full world-view and target-domain objects are interned into four `shared_v1` tables; ordered per-draw references are expanded before unchanged semantic checks. Different eye/lease identities never collapse. The limits remain 4096 draws, 65536 intervening commands, 16384 blobs, 128 MiB expanded metadata and 16 MiB serialized manifest. Target-span payloads are written and closed before one synced atomic manifest publication, avoiding thousands of individual filesystem barriers. Every later admission rehashes every blob; an interrupted or crash-damaged bundle is rejected. The two older capture kinds retain their original per-blob sync behavior.

The selected kind is passed as `MUGUET_NATIVE_DRAW_CAPTURE_KIND`, checked against the driver's optional eighth argument, and recorded in preparation, invocation and result receipts. A missing eighth driver argument selects `two-draw`; unknown choices or a mismatched manifest schema/kind fail. The runner supplies the argument and environment explicitly, so parent-shell variables cannot switch capture kinds. The `color-null` byte gate also requires a single original draw, inactive queries and zero reported GPU errors at capture; this does not assert whole-run GPU health.

The synthetic eyes use the geometry fixture's fixed projection and a **1.3 source guard**. That is an explicitly recorded diagnostic field-of-view choice, not a change to production coverage policy or evidence that this guard is sufficient on hardware.

`MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT=1` retains the exact consumed request in the existing bounded history before camera/GPU work; the driver requires it. Geometry capture stays disabled. Camera tracing alone does not enable request retention. A stopped or rejected run writes a failed runner receipt and preserves its original logs; no partial output is accepted.

Optional `--observe-after-capture 20` continues the existing static requests for up to 20 seconds after the first complete capture. The allowed range is 0–30 seconds, default 0, and the original runtime deadline still applies. Initial/final native status and the existence of `first-color-feedback.json` are recorded separately. A later runtime failure ends observation early; it does not invalidate already completed capture bytes. Successful runner status remains `captured_bytes_verified`, explicitly **not** whole-run GPU health acceptance.

Optional `--scene-provenance` enables two private metadata extensions. The allocation ledger records actual native texture lifetimes, per-aspect versions and observed clears without inventing missing initialization. The shader registry records WGSL at successful native module creation, so pipeline references select the actual final flat/remapped/alpha/HUD variant or actual base-module fallback. Only sources referenced by the captured draw pipelines are exported. Metadata exhaustion is explicit; it cannot certify a complete render graph. The original two capture kinds and production rendering remain unchanged. Parent environment variables cannot enable either extension. Source retention is bounded at 4096 modules/32 MiB, exported shader JSON at 8 MiB, ledger JSON at 1 MiB, and the complete serialized manifest at 16 MiB.

Optional `--same-ray-alias` selects the verified same-eye coordinate correction for a matched local comparison. This runner defaults to the historical policy for an explicit baseline; ordinary and visionOS builds always use the correction and cannot read this private override. Both private modes record the same bounded classifications. Only exact same-eye camera/stage/projection evidence can share the oldest coordinate identity and actual depth convention; genuine camera or eye changes still reject publication. This option is separate from metadata capture and recorded in the run identity. Actual local source-publication recovery is documented in `docs/WORLD-PROVENANCE-FIDELITY-20261026.md`; headset benefit remains unverified.

Optional `--exact-viewport-match` tests a separate exact-first stored-history lookup. It remains private and off because actual v7 runs changed no selections. It is not required for the same-ray correction.

`native-span-capture` is available only with `jit,native-draw-capture`. Build with the exact compile-time certificate:

```sh
MUGUET_MOTION_AOT_SHA256=dc4806cba7b44366fd039dc5212a20fbfcc96139d3208047b5e52e41db2fdfff
```

The feature-gated host helper admits the original WASM and pinned Mac AOT, retains the compiled bytes in process, and refuses compilation fallback. It creates no cache copy. The driver copies only three pinned graphics files into the new save: the 2560×1440 Clarity configuration, its applied marker and settings XML. The disposable developer-mode setting is newly generated. No player profile or old save is read or replaced.

The runner defaults to **read-only admission**. A build receipt must contain `success: true`, `source_unchanged: true`, exact `source_sha256` from `source_inventory(checkout)`, `binary_sha256`, `binary_bytes` and `runner_sha256`. The source inventory includes every Rust file under `src`, relevant vendor Rust/Cargo/build files, and root Cargo/build/config files. Rebuild/rebind after source changes; do not reuse a stale receipt.

Supply the active checkout, newly built binary, build receipt, original Mac game root, graphics seed, original AOT and a nonexistent run root:

```sh
python3 scripts/native-draw-capture/run.py \
  --checkout /Users/zach/gaming/SantosVision-third-person27 \
  --binary /ABSOLUTE/NEW/BUILD/native-span-capture \
  --build-receipt /ABSOLUTE/NEW/BUILD/receipt.json \
  --game-root /Users/zach/gaming/SantosVision/target/verification/device-session-20261025/ads-offline29/mac-runtime/game \
  --graphics-seed /Users/zach/gaming/SantosVision/target/verification/device-session-20261025/third-person27/resume-20261025 \
  --aot /Users/zach/gaming/SantosVision/target/verification/device-session-20261025/ads-offline29/mac-runtime/fast-precompile/game-motion-mac.cwasm \
  --run-root /Users/zach/gaming/SantosVision-third-person27/target/native-draw-capture/NEW-RUN \
  --deadline 180
```

Only the parent-coordinated local run adds `--execute`. It creates the run directory exclusively, whitelists the environment, mutes audio, isolates host HOME/tmp/cache and native save state, and owns one child process group. The inner runtime deadline is at most 180 seconds; the wrapper allows 20 seconds for input preparation and five seconds for termination before killing/reaping only its child group. The capture payload bound is 256 MiB, manifest 16 MiB and combined logs 128 MiB.

`capture.json` is published after all original natural-submission readbacks complete. The runner rejects partial/rejected results, unsafe or duplicated payload paths, missing/extra files, hash/length mismatches, source drift, changed seed files or changed disposable graphics semantics. Its successful receipt means **captured bytes verified**. Run the independent semantic/replay admission afterward; the byte manifest alone does not prove the renderer reproduces the game.

The driver's `capture_identity` records SHA-256 and byte length from the exact manifest read used to admit completion. The runner verifies those original bytes. The embedded `capture` object is for inspection only (a compact summary for target-span): parsing and reserializing JSON matrix decimals can change the last digit, so it is not the identity comparison. Byte changes still fail even if the parsed JSON is equivalent.

CPU-only boundary tests (fake bytes and one owned Python child, no game):

```sh
python3 -m unittest discover -s scripts/native-draw-capture -p test_run.py -v
```

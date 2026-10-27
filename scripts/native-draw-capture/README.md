# Private native draw capture

This diagnostic feature runs the actual game without a desktop window, using a fresh developer-mode save and synthetic static two-eye input. It captures a natural two-draw span for material/state replay. It does not reproduce the current bad headset scene, steer an airport fixture, or establish finished-world/stereo/hardware quality.

The synthetic eyes use the geometry fixture's fixed projection and a **1.3 source guard**. That is an explicitly recorded diagnostic field-of-view choice, not a change to production coverage policy or evidence that this guard is sufficient on hardware.

`MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT=1` retains the exact consumed request in the existing bounded history before camera/GPU work; the driver requires it. Geometry capture stays disabled. Camera tracing alone does not enable request retention. A stopped or rejected run writes a failed runner receipt and preserves its original logs; no partial output is accepted.

Optional `--observe-after-capture 20` continues the existing static requests for up to 20 seconds after the first complete capture. The allowed range is 0–30 seconds, default 0, and the original runtime deadline still applies. Initial/final native status and the existence of `first-color-feedback.json` are recorded separately. A later runtime failure ends observation early; it does not invalidate already completed capture bytes. Successful runner status remains `captured_bytes_verified`, explicitly **not** whole-run GPU health acceptance.

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

The driver's `capture_identity` records SHA-256 and byte length from the exact manifest read used to admit completion. The runner verifies those original bytes. The embedded `capture` object is for inspection only: parsing and reserializing JSON matrix decimals can change the last digit, so it is not the identity comparison. Byte changes still fail even if the parsed JSON is equivalent.

CPU-only boundary tests (fake bytes and one owned Python child, no game):

```sh
python3 -m unittest discover -s scripts/native-draw-capture -p test_run.py -v
```

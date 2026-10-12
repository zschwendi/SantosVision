# Metal pre-pass qualification — October 12, 2026

Source `bca1787` includes Opus's conservative Metal pre-pass skip and empty-indirect-batch refinement, the XR reset fix, and typed launcher wiring. It preserves genuine initialization, discarded-surface clears, query resets, indirect validation, resource tracking and deferred query ordering. No original game/WASM/AOT, native ABI or Cargo dependency change. Both Metal skips follow the existing rendering-overhead choice; exact diagnostic `0`/`1` values independently override it. Public upload pooling and op47 dedup remain separate, off by default.

The full native library passed 173 tests OFF and 173 ON in each upload route, including real Metal bytes, render/compute order, query/discard/initialization fallback and requested-mode assertions. Queue receipt: `prepass-launcher-native-v1/receipt.json`, SHA-256 `9d08ccb2ad63003919e418f8a796f20defba6761ffc1528c0a4e633778c9a3b9`. Pool supplemental receipt: `prepass-launcher-pool-native-v1/receipt.json`, SHA-256 `d6db0cad68d059641ac825e70cf8369265334b91d026996a3b6db31e7b8ab91c`. Read-only source review: `prepass-final-review-v1/note.md`.

The built simulator app binds the same source/compiler and original VERSION5 AOT: `prepass-launcher-app-v1/build-manifest.json`, SHA-256 `ae425f501a60a460dec97856b338f43e38dcb2e4b53058d11b3519ae2b0895c6`. Installation preserved original game data, audio, graphics and preferences. The source binary includes the concurrent XR reset fix.

Fresh `prepass-v2-off-v1` and `prepass-v2-on-v1` runs passed their first 90 loaded seconds, qualified airport/noon/CLEAR scene, zero validation errors, source-image captures, owned-process cleanup, exact preference restoration and unchanged source/tools. Both use queue uploads, empty queue-Transit skip ON, uniform/XR shortcuts ON, dedup OFF, Low 960x540, 60 cap and synthetic head movement. The comparison is `prepass-v2-comparison-v1.json`, SHA-256 `0ddeef56a21808795df36ce8be0e470b55d64d80ba9607a9d97216af3026137f`.

| Complete-window observation | Pre-pass OFF | Pre-pass ON |
| --- | ---: | ---: |
| Source FPS | 45.40 | 49.63 |
| Draws/source frame | 429.65 | 419.91 |
| Reported passes/source frame | 39.86 | 39.58 |
| Rate-normalized core HAL input command buffers/source frame | 116.90 | 73.07 |

ON avoided 156,775 empty pre-pass command buffers in complete paired backend intervals: 146,344 render and 10,431 compute, retaining 10,126 query-bearing pre-passes. This is 93.9% of pre-pass decisions. The approximately 37.5% lower HAL-input rate is a command count, not GPU time. Backend and FPS intervals have independent phases; the extractor brackets complete intervals and labels rate normalization. Traffic, actor/model, streaming, shader mix and head phase vary; the observed FPS difference is descriptive, not a controlled causal gain. Captures are archived in `captures-prepass-v2-v1/` and the optimized source image was inspected.

The combined public pool experiment passed at the same compiled source, but observed 38.05 FPS with 532.11 draws/frame versus the queue ON run's 419.91. It reduced upload API wall again and had no validation/map/poll errors, without establishing a frame-rate benefit. Pool remains off. Single-run evidence: `prepass-pool-diagnostic-v1.json`, SHA-256 `d488487647a11c121fc2475e44e0e6c29be071090f7cd445251f749c6e0ed7c9`.

The saved-choice proof `saved-launcher-proofs/saved-launcher-on-v1/proof-result.json` passed after omitting all seven diagnostic overrides from the actual owned process environment. Saved choice, native/Swift effective flags, uniform hits, XR fast emissions, both Metal skips, actual scene/capture and clean shutdown agreed. Original preferences were restored by the proof. Its disclosed wrapper hooks are execution-bound; the proof is not used as a performance comparison. The failed original `prepass-off-v1` run remains untouched: a test-script counter-name mismatch stopped it before loading, without an app crash. V2 fixtures exercise the actual log schema and catch the old mismatch.

All evidence paths are relative to `target/verification/render-hotpath-20261011/`. These are one-view simulator results. High-settings native 4K60 with two rendered stereo streams remains incomplete and unverified. Next performance work should target actual encoding/texture-upload costs with matched workloads, rather than promoting pooling on small API timers alone. Physical headset testing remains deferred at the user's request; master volume is zero and banked resets are untouched.

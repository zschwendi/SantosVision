# V27.10 source publication correction

V27.10 repairs a demonstrated source-publication failure. Different camera callback IDs could describe exactly the same eye rays within one original game presentation. The former global ID check rejected that finished image, even though its color and depth coordinates agreed. The corrected path retains the oldest complete coordinate identity and each draw's actual depth convention. It changes no rendered pixels, projection, resolution, material, post-processing or stereo policy.

## Actual local before and after

The same frozen Mac executable, original game/AOT, 2560×1440 Clarity settings, synthetic static two-eye input and fixed diagnostic guard1.3 were used. Fresh natural scenes are not pixel-registered. Each run continued for approximately20 seconds after its initial original-draw capture:

| Observation-window counter | Original admission | Corrected admission |
| --- | ---: | ---: |
| GPU-complete source publications | 46 | 972 |
| Camera-conflicting presents | 1,083 | 46 |
| Presents admitted through exact-ray identity | 0 | 928 |
| Snapshot drops | 0 | 0 |

Whole-run completed publications were56 and980 respectively. Both final eyes had independent completed sources; no publication error or correlation failure was reported. The corrected run retained138 genuine camera-change refusals. These are source counters, not headset FPS, isolated GPU busy time or a matched visual-quality comparison. The runs include loading and different natural game progress; the improvement is supported by the actual928 admissions that the former ID rule would have rejected, not just the ratio between two runs.

Private evidence:

- `target/native-draw-capture/build-v7/receipt.json`: executable `ae402e20e02b3645c6e9b8ff68fbde310dc4dbaeaa90dc3ac06cc1601fe4f5a9`, exact source and runner identity.
- `target/native-draw-capture/actual-provenance-v7-exact-off/`: baseline; both private candidates disabled.
- `target/native-draw-capture/actual-provenance-v7-alias-on/`: strict same-ray correction enabled; exact-first lookup disabled.
- `target/same-ray-alias-production-review/`: independent comparison, original-input checks and actual floating-point matrix audit.
- `target/same-ray-production-cpu-tests.log`:40 passing tests. The promoted geometric predicate and consumed-request retention are identical to the actual v7 tested implementation.

## Exact admission boundary

Within one original present, admission requires identical eye/count/epoch, bit-identical camera and stage matrices, exact optics, and identical projection X/Y/W rows. The remaining depth row must be identical or the already-certified exact forward/reverse-Z conversion. The actual draw's depth projection is retained. Times and request/head/camera identities must be monotonic; the oldest complete identity is retained, never a newer timestamp. This identity describes the common coordinate basis, not a claim that every draw consumed the oldest callback.

Normalization happens before the GPU can consume the draw's ring command. Existing final color/depth/HUD joins remain strict. Different eyes, poses, scopes, near/far planes, out-of-order evidence, missing required request receipts, foreground attribution and marker/queue failures still reject. No single-source stereo or new rendering path is enabled. Ordinary and visionOS builds use the correction; only the private Mac capture feature can select the former policy for comparison.

The separate exact-first viewport lookup remains private and disabled: actual v7 runs observed zero corrected selections. Its synthetic discriminator is not evidence of a live benefit.

## Signed local checkpoint

V27.10 is signed and frozen at `target/fidelity-v27-10/SantosVision.app`, built from `dc838560bb709d5c4cc234b884250ac48bd77ce5`. All41 package checks pass, including strict signatures, original game/AOT bytes and RX protection, technical identities/entitlements, unchanged47 compositor source files, and exclusion of private diagnostic switches. All V27.6–V27.9 rollback files and the V27.9 archive remain exact. No installation occurred.

- Executable SHA256: `b931b65c1a450f8308457fa28253edcf50f0f41d5a1c5f2a0d2ff68937151a8f`.
- Native archive SHA256: `9b7e4740957f8e8bac39e42a2d17ef88126cd0c674f294a403692472de042bc7`.
- Package receipt: `target/fidelity-v27-10/candidate-receipt.json`; independent final file checks: `target/fidelity-v27-10/root-verification.json`.
- Supporting commits: `b8f2d5c` ordered native replay; `6cc8a15` bounded native resource/shader capture.

## Supporting implementation and verification

The private native capture now records actual texture lifetime/subresource versions and exact successful native shader-module sources. Missing initialization, inputs and evicted predecessors remain explicit. Capture-time dependency expansion is bounded; normal passes retain only a small list of roots, and tracking stops after capture completion. An initial expensive/oversized ledger attempt is preserved as negative evidence in `target/native-draw-capture/v6-provenance-comparison.json`; its timing is not used for the correction's comparison.

The [generic ordered replay](../scripts/native-scene-replay/README.md) implements persistent attachments, heterogeneous original draws, actual clears, copies and immutable bindings. Its final frozen build passes16 adverse admission cases, a mixed-pass Metal oracle, and exact initial/final raw aspect comparisons for three real game captures. Full finished-scene capture and initialization closure remain unfinished. The older color-null minimap draw remains an explicit no-op and is not presented as improved shading.

## What remains unverified

Color correctness remains the independently verified V27.7 correction. This change repairs source availability; it does not add texture samples. The known guard3 central sampling limit remains approximately853×480 from a2560×1440 source. Fresher source availability may reduce required guard in live use, but this diagnostic held guard fixed and did not measure that effect.

No headset, iPad or simulator action or query was performed. The reported bad headset scene was not reproduced here. HUD publication and compositor presentation were inactive in this local fixture; their strict join predicates were checked in CPU tests, not accepted interactively. The old physical27.3 report recorded zero camera conflicts, so this local defect is not established as the sole cause of the user's grain, shading or flicker complaint. M2 source/compositor contention and the extra valid snapshot-copy workload still need hardware acceptance.

After the local-only hold is lifted, the next single test is V27.10 in the same bad scene at1440 Clarity: window→normal third-person immersive,15 seconds still,15 seconds slow yaw,15 seconds still. Compare texture detail, dark shading, thin edges, HUD and flicker, and retain a matched completed source/output pair with timing scope labeled. Keep experimental cache/common-target/single-source paths off. V27.6–V27.9 remain rollback packages; no save or graphics-setting replacement is required.

# Complete native world-pass reconstruction

The private native adapter now reconstructs a complete actual game target interval, with **1,277 draws, 102 pipelines and 94 effective shader pairs**, and produces byte-identical results for all four color targets, depth and stencil. This is a concrete first step toward allocating more native samples to the central view. It is not yet a higher-density finished image or a new runtime rendering path. V28 and its new front-glass icon package remain unchanged by this feature.

Producer commit: `2e008d1`. Consumer commit: `9dd9e05`. The Mac-only `native-draw-capture` feature is excluded from ordinary app builds. The original two-draw and corrected-null capture modes remain supported. No device, iPad or simulator action or query occurred.

## What was implemented

The producer captures every executed draw between two original target-binding commands, rather than selecting a successful prefix. The selected interval has four BGRA8 UNORM targets and D32FloatStencil8 at 2560×1440. Every draw retains its actual emitted draw arguments, viewport/scissor, pipeline, exact successful shader modules, resolved binding layouts and native parent identities. Buffer uploads have independent revisions; dynamic uniform ranges are copied exactly. Material snapshots retain original mip/layer/aspect versions and real dummy resources. D1 and D3 support preserves actual dimensions and complete volume mip data instead of treating volume slices as a 2D array.

Full individual-eye world and target identities must agree throughout the interval. A changed eye, camera, request, target parent or material version, skipped draw, active query or unsupported operation rejects the whole candidate. Ordinary native submissions preserve order; the snapshots remain retained through the final natural completion. Initial target contents are captured, not invented or inferred from an incomplete clear history.

The combined staging limit remains 256 MiB. Aborted snapshots remain charged until both their GPU command ownership and CPU/readback ownership release them. Review found and fixed an unrelated private retention defect: uncaptured buffer uploads no longer retain native buffer handles in the revision map. Its 10,000-key CPU test checks actual reference retention.

Whole intervals repeat immutable metadata. Lossless `shared_v1` tables store exact pipeline, binding, full world-view and target-domain objects once; each draw retains ordered references. Structural equality is checked after hash lookup. The consumer expands references before its existing semantic checks, rejects mixed/missing/out-of-range references, bounds expansion before cloning and preserves the original encoded capture bytes and SHA. Different eye or resource-version objects remain different. The actual manifest shrank from 19,012,292 to 10,067,158 bytes, within the unchanged 16 MiB limit.

All payload files are fully written and closed before one synced, atomic, exclusive manifest publication. This removes thousands of per-file filesystem barriers. Later byte admission still reopens and hashes every payload, rejects extra/missing files and requires successful child completion. A crash may leave an unusable partial bundle; it cannot make it pass admission. Older small capture modes retain their per-payload sync behavior.

## Actual before/after and replay evidence

The accepted run is `target/native-draw-capture/actual-target-span-v11b/`, using frozen producer `build-v11/native-span-capture`, SHA256 `259b0b5b806044780f0004efb0c9eaf53d0f63c8826c2f173fe062e8f1055e52`. It completed in 40.276 seconds with exit 0, no capture GPU errors, unchanged original WASM/AOT/graphics inputs and a reaped owned child. This elapsed time is capture-tool wall time, not game FPS or headset performance.

The complete interval is commands 186825 through 196057, eye 0, provenance epoch 2, camera 81, request/head 116. It includes 128 exact final shader modules and real sampled D2/D3 resources. It exports 9,925 blobs containing 209,210,800 raw bytes; padded staging totals 213,971,408 bytes. The source guard is the existing fixed 1.3 diagnostic fixture value, not a production policy change or proof of sufficient physical coverage. The game retains third-person camera mode. This is a fresh developer-mode scene, not the user's rejected headset scene.

Strict CPU admission passed. The independent generic executor then ran the actual ordered pass on Apple M4 Metal, using binary SHA256 `a6bd8577df6c644bcc4ee2c1fba0f51190adfd3986c26d6a130e4f1213914297`. All 5,318 initial texture subresources round-trip exactly, and all six final aspects match the original native output byte for byte. Source, original inputs and frozen inputs remain unchanged. Root separately hashed each output against the native final blob and verified that all six original final aspects differ from their initial contents; this pass has substantial actual image content.

| Aspect | Pixels changed by the original pass | Replay bytes differing from original final |
| --- | ---: | ---: |
| color0 | 2,898,811 | 0 |
| color1 | 2,428,833 | 0 |
| color2 | 1,544,246 | 0 |
| color3 | 766,378 | 0 |
| depth | 2,898,813 | 0 |
| stencil | 2,898,813 | 0 |

The actual imagery is under `generic-previews-v1/`, including `color0-initial-rgb.png`, `color0-original-final-rgb.png` and `color0-replayed-final-rgb.png`, plus all other target RGB/alpha views. Root visually inspected the original and replayed color0 images: the third-person character, underpass, ground textures and CarTune sign agree. The black sky and flat material appearance belong to this **intermediate target**; these previews are not finished lighting or compositor output. PNG previews use an explicitly declared sRGB display interpretation; raw target bytes remain the numerical authority.

Key receipts in the accepted run:

- `receipt.json`: successful native byte admission. Native capture SHA256 `eacd4556127d6abe4286b9dff0e32756688ecf95e5daf410ca2325a37f6dcef6`.
- `generic-import-v1/native-import.json`: raw native capture to generic scene identity.
- `generic-replay-v1/receipt.json` and `replay/result.json`: successful actual Metal execution and exact comparisons. Result SHA256 `ab68e810f432e74a7dcc98e9afaab42afa197b2c8a6b51d8baced196c8659163`.
- `root-replay-verification.json`: independent six-aspect file verification.
- `generic-previews-v1/receipt.json`: before/after pixel counts and preview provenance.

Verification also includes 21 native capture CPU tests, 25 runner boundary tests, six strict-adapter CPU suites, the separate D1/D3 Metal oracle, and independent producer, exporter and consumer reviews. Failed v8–v11 runs remain preserved. They exposed unsupported volume resources, slow per-file export, oversized repeated metadata and a bounded run that found no eligible interval. The final implementation does not raise payload/metadata limits or admit partial intervals to bypass those failures.

## Quality boundary and next step

Color correctness, source detail and performance remain distinct. V28 already includes the registered-image linear filtering/Display-P3/HUD correction and the separately verified same-ray source-publication correction. This new private feature proves faithful reconstruction of a heterogeneous world pass. It does not yet reproduce the complete finished lighting/postprocessing chain, add central samples to the app, establish stereo presentation quality or measure M2 performance.

Follow-up commit `6cbcf63` now uses three proven native clears: color0 is BGRA8 zero, depth is Float32 positive zero and stencil is 7. Promotion requires a direct ledger root with matching unique native identity, version, descriptor, view, aspect and chronology, plus an exact comparison against every original initial byte. Those reference blobs remain mandatory comparisons on replay. Only these three initializer origins differ from the accepted scene; every other field remains equivalent. All 5,318 initial subresources, including the three real API clears, and all six final aspects still match exactly. Evidence is in `target/native-scene-replay/uniform-clear-v1/qualification.json`; frozen consumer SHA256 is `92ae8b8366c374c8f4fe5d054f76622042e7f0afd0396cc90974b0bf6a6516fb`.

Color1/2/3 remain `Captured`: their inherited pixels survive this world pass, so inventing empty inputs would be incorrect. The overall resolution-independent initialization gate remains false. A higher-density center must start from a complete target initialization/dependency graph and preserve subsequent lighting and postprocessing, including screen-size-dependent uniforms. The next local implementation step is to extend the adapter through those original target transitions and compute work, with the same exact original-density pixel gate before any density change. Upscaling inherited target bytes or treating this flat intermediate image as a finished world would violate the fidelity goal.

The current signed app for eventual acceptance is the separate **V28 front-glass package**, described in `V28-FRONT-GLASS-PACKAGE-20261026.md`. After the hardware hold is lifted, use the same rejected scene at 1440 Clarity: window to normal third-person immersive, 15 seconds still, 15 seconds slow yaw, 15 seconds still. Compare texture detail, dark shading, thin edges, HUD and flicker with matched per-eye sources and compositor output. Keep normal independent stereo and experimental cache/common-target/single-source paths off. No hardware acceptance is claimed here.

# Correctness qualification — October 11, 2026

The frozen diagnostic/readback/query-lifetime batch passed the full native regression, simulator and device builds, local development signing checks, and bounded muted simulator gameplay observations. This is a prototype qualification record. The allocator crash cause remains unknown; a nonreproduction does not establish a crash fix. Physical headset operation and binocular stereo were not tested.

The parent project remains open under `w-478e719b3be844d28b469ea1782b0347`. Implementation used Claude Desktop Opus 5.5, Extra effort; Sol reviewed and integrated. Fast mode was off. No experimental performance flags were enabled during qualification.

## Frozen inputs and regression

The final worker freeze is `/Users/zach/gaming/SantosVision/target/verification/readback-e2e-v1/freeze.json`, SHA-256 `40be19c6b9cca98733ef7397fb77ec4b511c965c4b6bc67172b7113325dd03f1`. The merged native binding contains 113 inputs, including every Rust source and the newly added autopsy and guest-write fixtures; its content hash is `83f4120c56fbbd9f091a8ccc7c85ee7772402f85bf1158832b5f08b6708a2ba7`. Qualified app sources and original guest/AOT files were pinned separately.

The original official regression ran the complete library suite serially, with two build jobs and one test thread:

| Mode | Passed | Failed | Ignored | Filtered |
| --- | ---: | ---: | ---: | ---: |
| Uniform packing OFF | 100 | 0 | 0 | 0 |
| Uniform packing ON | 100 | 0 | 0 | 0 |

All 41 required diagnostic/readback/query/audio tests were present. Actual Metal fixtures compared the whole guest memory image, guarded destinations, and checked completion flags; destroy/reuse fixtures exercised queued query completions. Existing shader-word and workload equality checks passed. These are 200 test executions of 100 tests, not 200 distinct tests. The subsequent Git checkpoints did not change qualified source bytes and did not require repeating this regression.

The fixes prevent false-clean diagnostic results at incomplete traversals, bound readbacks to the declared guest destination, and cancel stale query completions with a per-instance lifetime token. Software-decoded BC readback intentionally completes without writing data; it is not complete BC conversion support. Query GPU fixtures assign slots directly and do not exercise a real guest draw pass or nonzero occlusion visibility. The unchanged readback-after-guest-timeout lifetime window remains a limitation. No autopsy ran against a real trapped guest heap in these observations.

## Build and signing verification

Both `aarch64-apple-visionos-sim` and `aarch64-apple-visionos` native archives built sequentially with two jobs, using the unchanged qualified AOT artifacts. Both native Swift/Metal apps built. Pinned link verification checked ARM64/platform identity, required native ABI symbols, executable `__GAME` protections, exact embedded AOT bytes, native archive/source receipts, app source identity, and Metal library hashes.

The device app was signed with the previously authorized development configuration. Local signature/profile verification passed for team `V729QMY3V6`, development entitlement `get-task-allow=true`, and profile expiry October 4, 2027. The device app was not installed or run on a headset.

Two tooling errors were preserved rather than hidden: the prepared builder omitted the isolated Xcode project parent directory; its narrow recovery then omitted a variable used only in receipt finalization after both app builds succeeded. Separate recovery scripts verified the unchanged original regression-bound scripts, source binding, successful archive/app command logs and hashes, then completed the original link checks. No production source was changed and no archive or test success was inferred from a failed command. Recovery receipts and failure logs remain with the build evidence.

## Muted simulator observations

The known task-owned simulator was used exclusively, with no concurrent compilation, GPU test, profiling session, or other task app runtime. Both runs used the Low preset, 60 FPS cap, identical simulated pose arguments, start mode 2, queue buffer upload with 64 KiB pages, and all unproven optimization flags set to zero. Gameplay scenes and timing evolved naturally and were not controlled identically between runs.

| Observation | Low | 4K stress |
| --- | ---: | ---: |
| Source resolution | 960 × 540 | 3840 × 2160 |
| Observed gameplay | 600.980 s | 304.540 s |
| Source present cadence | 31.6511 FPS | 13.2598 FPS |
| Compositor completion cadence | 59.1375 FPS | 11.8442 FPS |
| Compositor GPU weighted mean | 0.1429 ms | 75.1103 ms |
| Compositor GPU maximum sample | 89.1722 ms | 219.5562 ms |
| Deduplicated GPU windows / samples | 57 / 33,723 | 30 / 3,539 |
| Process CPU observation, median | 185.8% | 81.6% |

Source FPS is the change in native source `presents` divided by the report uptime interval after gameplay connection. Compositor counts are separate; a reused source image can produce multiple compositor completions. The ps CPU observations are lightweight process observations, not per-stage execution measurements. No p95 frame time is inferred from report deltas or coarse wall-time histograms.

Both runs completed without a reported trap, native/world/view error, or detected source stall. GPU timing windows reported zero command errors, invalid samples and unavailable samples. Guest heap remained 3 GiB. Each owned app terminated successfully; exact prior Low graphics JSON and game settings XML were restored, and master volume remained zero. The 4K run was gated on the pinned successful Low result. These were one-view simulator runs with `stereo=false`; they establish neither matched stereo pairs nor hardware 4K60 feasibility, visual acceptance, or long-term stability.

## What the compositor timing measures

The qualified `GameWorldRenderer.swift` source still matches its app freeze. Its completion handler passes the **whole command buffer** `gpuStartTime` and `gpuEndTime` to `GameWorldGpuTiming` (`GameWorldRenderer.swift:379`; `GameWorldGpuTiming.swift:95`). The measurement includes the encoded compositor render passes and `encodePresent`, plus a debug blit if requested. These qualification runs requested no debug capture; recorded debug-blit counts were zero. The interval excludes the separate guest/wgpu queue and system display work. It is not a vertex-only, fragment-only, mesh-only, or compute-only duration, nor an end-to-end source-frame latency.

Source inspection shows one fixed `MTLRenderPipelineDescriptor` using `gameWorldVertex` and `gameWorldFragment` (`GameWorldRenderer.swift:180`). There is no compute encoder, Metal mesh-shader pipeline, or alternate compositor fallback pipeline in this qualified path. The renderer calls `drawPrimitives(.triangleStrip, vertexCount: 4, instanceCount: width × height)` per successfully leased view (`GameWorldRenderer.swift:454`). This is an instanced pixel grid: 518,400 four-vertex instances at Low and 8,294,400 at 4K, a 16-fold nominal geometry increase. That means 33,177,600 nominal vertex invocations and 16,588,800 nominal triangles per successful 4K view draw, before shader rejection/rasterization. This is source-derived workload, not a captured hardware vertex/triangle counter. Qualified acknowledgements and one-view presentation reports demonstrate successful compositor command completion, but no GPU capture or exact command-buffer draw-count counter was collected; additional compositor drawables would also fall inside the measured buffer.

The guest renderer is a different path. Its final logged interval had 1,501 draws/source frame and 437 pipelines at Low, versus 898 draws/source frame and 279 pipelines at 4K; both reported zero draws waiting for compilation and zero validation errors in that final interval. Earlier startup skips existed. These counters do not identify shader variants or exclude every guest fallback; they also demonstrate that the guest workloads were not identical. They must not be confused with the compositor's single pipeline and instanced draw.

For the last 20 native timing windows, Low command replay occupied 93.54% of GPU-thread wall time and producer idle wait 4.55%; at 4K those figures were 24.87% and 72.42%. Uniform slot waits were negligible. These are CPU-thread wall spans; inclusive parents contain child work and must not be summed. Native guest GPU execution duration was not measured. The compositor GPU increase is a concrete lead for critical-path investigation, especially given the dense grid, but it does not by itself prove which shader stage, scheduling interaction, or implementation change would improve performance.

## Evidence references

All paths below are local evidence retained outside Git build inputs, beneath `/Users/zach/gaming/SantosVision/target/verification/readback-bound-integration-v1/`.

| Relative artifact | SHA-256 |
| --- | --- |
| `regression-final-v1/correctness-runs.json` | `49c42ae865ca79ab0810b1575b8220764bccba0af3d7fc7f709a5e19802252f8` |
| `build-final-v1/build-success.json` | `087cb790d49e96f34b4a79144c60da4b67b7a5d042af873cd438ac5fcc29205b` |
| `build-final-v1/build-manifest.json` | `c2c8e28ae21132fa2aa25a0747a4a2adda4b4bb0d4131fccc2db54f9eb109d9e` |
| `sign-final-v1/signing-verification.json` | `1eda9732de1735da15cbb99b4ecd26aeb555da1779db9e2c5c5882bdca176289` |
| `sign-final-v1/build-manifest.json` | `5d040b1bd8ef5d8513ccc2ad0c83978217c93c5df31afcad18cbacbb184e743a` |
| `low-final-v1/result.json` | `9b8b4f244e6d6b6c22c1cfaa433abb673fc47ab74a4e4a5e2238afdcf5c8286f` |
| `fourk-final-v1/result.json` | `6709707fc6455add20a097e789a4056ab858be1c8574182d6a652df7b3e35148` |
| `runtime-timing-summary.json` | `f5e9c3b371ac9c5cd84213a2be660ffae2b4142fd099704367996dead740e8f1` |
| `INTEGRATION-HANDOFF.txt` | `40799eace61f0525cdf461dd4d03407ec1f9d9bd1990a5a8b31fb0a17c1a9940` |

Each runtime directory also retains full stdout/stderr, report snapshots, observations, original settings, `native-timing.json` and `diagnostic-extract.log`. The extract contains allocation-region evidence; no crash autopsy or readback-bound warning was triggered. Original worker fixture/negative-control evidence remains in `/Users/zach/gaming/SantosVision/target/verification/readback-e2e-v1/`.

This record adds documentation only. No production edit, build, launch, staging or commit was performed for this documentation scope; root owns inspection and the separate documentation checkpoint.

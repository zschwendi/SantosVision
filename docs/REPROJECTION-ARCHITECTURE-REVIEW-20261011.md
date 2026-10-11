# Reprojection architecture review — October 11, 2026

The game image does not have to become one quad per pixel. That is the current implementation of application-side depth reprojection: reconstruct a small surface from each source color/depth texel, then rasterize it from the newer head pose. It uses the rasterizer for coverage, clipping and depth testing. It cannot reveal content that was absent from the source image.

At 3840 × 2160, the current draw requests 33,177,600 vertices and 16,588,800 triangles per eye. These are source-derived workload counts, not captured hardware invocation counters. The optional exact corner-depth precompute keeps that geometry and gave only modest provisional synthetic gains. The previous bit-identical gate belonged to that experiment; it is not a permanent product requirement.

Claude Desktop Fable 5.1, Extra effort, Fast OFF reviewed alternatives. Sol independently checked the source-pose contract and adaptive tile boundaries; root checked the proposals against the source and requested corrections. This record covers architecture review only, with no new production implementation, build, GPU measurement, app launch or headset test.

## Candidates

| Candidate | What it changes | Qualification needed |
| --- | --- | --- |
| Retained source anchor and fullscreen color/depth submission | Replaces our dense mesh with one fullscreen triangle per eye and asks the system to perform its documented display-time pose adjustment | Old-anchor acceptance, rotation and translation behavior, projection/depth agreement, and coherent stereo source inputs remain unproven |
| Conservative adaptive planar mesh | Keeps full-resolution color and depth-aware application reprojection; uses larger patches on smooth surfaces and fine geometry at edges | All covered samples must qualify; boundary subdivision, coverage, clipping and fade need image/depth tests before timing |
| Output-pixel gather or source-pixel scatter | Replaces mesh geometry with texture/compute work | Occlusion search, magnification holes, disocclusion, atomics/bandwidth and edge quality need bounded prototypes; geometry removal alone does not establish a speedup |
| Paired game rendering from one simulation epoch | Produces two coherent eye images rather than alternating natural game frames | Guest camera/render separation, deferred passes, scheduling and resource lifetime need substantial work; this remains necessary for proper stereo regardless of warp choice |

Foveation, shared stereo preparation and separate sky handling can complement these changes. They do not alone remove the dense vertex workload. No measured sky fraction or multiview speedup is claimed.

## Native pose correction is a probe, not an established replacement

Apple documents that `Drawable.deviceAnchor` names the pose used to render a frame, and that Compositor Services adjusts its pixels at display time when this differs from the actual device pose. The inspected documentation and XROS 27 interfaces do not establish arbitrary historical depth-aware 6DoF reprojection, accepted anchor age or disocclusion repair. A drawable has one anchor shared by its views. Sources: [deviceAnchor](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/deviceanchor), [immersive Metal rendering](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), [depthTextures](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/depthtextures).

Today Swift discards the actual ARKit anchor after submitting its matrix. The consumed pose is identified by a native `head_frame`; the independently generated `request_frame` is not a substitute. A retained-anchor implementation must bind the actual anchor to the exact submitted head-frame key and bound its lifetime. A matrix cannot simply be relabeled as an ARKit anchor.

The current left and right source slots may come from different head poses and different game frames. Warping them to a common presentation pose does not repair moving-object temporal disagreement. One newer anchor cannot truthfully describe both source images. A correct production copy path requires coherent pose, projection and scene inputs for both eyes.

The smallest platform experiment should first render a frozen analytic scene into two coherent source-eye textures using one retained actual anchor. Include a near object, a distant plane and sky. Submit those textures at controlled source ages, comparing the existing mesh and fullscreen path under rotation and lateral translation on the headset. Record source/target projections, anchor age, color/depth/coverage, stereo alignment and motion-to-pose behavior. A non-nil anchor getter proves assignment, not successful display correction. Evidence must observe the displayed result; reading the app's pre-compositor texture cannot validate the system warp. Simulator identity tests can qualify copying and depth/color conversion, but cannot qualify the system's display-time correction.

Retain the mesh as the default and rollback path until those gates pass. Removing our draw does not remove system compositor work or prove that game FPS will reach 60.

## Adaptive mesh fallback

Four tile corners cannot certify a surface: an interior foreground object can be missed. The independent check requires all 64 centers of an 8 × 8 tile, a halo and boundary corners to agree with an affine NDC depth plane. Invalid, clear-depth, discontinuous or near-clipped regions fall back to existing texel footprints.

A naive two-triangle tile meeting fine geometry can create T-junctions. The conservative first proposal retains one-pixel perimeter subdivision with a 32-triangle fan. If every 4K tile qualifies, this requests 4,147,200 triangles and 12,441,600 unindexed vertices per eye; at 50% accepted whole tiles, 10,368,000 triangles and 22,809,600 vertices. An indexed fan has 4,276,800 distinct tile vertices in the all-qualified case, but actual cache reuse must be measured. All-fallback retains today's geometry and adds classification work, so it can regress.

An ideal two-triangle tile would give 259,200 triangles when all 129,600 tiles qualify. That is an arithmetic lower bound, not a seam-safe mixed-scene implementation or measured result. Transition topology remains future work. The proposed tile metadata and two ID lists are bounded to 1.48 MiB per eye, excluding existing textures and optional corner scratch.

Before timing, frozen-source fixtures must cover hidden interior depth islands, shallow steps, coarse/fine seams, sky, invalid depth, image borders, near clipping, fade and head motion. Compare full color, depth and written-pixel masks with the original path and an independent plane oracle. Keep source color at full resolution. Define visual tolerances explicitly; the candidate has no bit-identical raster guarantee. The flag-OFF reference must remain exact.

## Performance boundary

The qualified one-view simulator observations were **Low settings**: 31.65 source FPS at 960 × 540 and 13.26 at 3840 × 2160. The 4K whole compositor-command GPU mean was 75.11 ms, excluding guest/wgpu GPU execution and system display work. These were naturally evolving scenes, not controlled paired performance comparisons or headset stereo runs.

A separate synthetic macOS Apple M4 experiment found approximately 35 ms in the vertex-processing stage envelope and 13–17 ms in the fragment envelope at 4K. These are not exclusive shader ALU figures and do not precisely apportion the simulator observation. They support investigating geometry reduction. The game source-rendering limit remains separate; neither a faster warp nor native late correction creates 60 new simulation frames or matched stereo pairs.

High 4K60 stereo remains the goal, with no feasibility or performance claim from this review. Audio settings and production flags were not changed.

## Local evidence

Review artifacts are retained under `target/verification/novel-reprojection-review-v1/`: Claude's revised `review.md`, `status.json` and `handoff.txt`, independent `source-pose-contract-sol.txt` and `adaptive-mesh-check-sol.txt`, and the downloaded official Apple references. Root independently verified all 25 status bindings covering consulted sources, SDK files, documents, revision inputs and outputs. This verifies provenance, not correctness of every proposed algorithm. Gather occlusion and multiview performance remain hypotheses requiring tests.

| Artifact | SHA-256 |
| --- | --- |
| Revised Claude `review.md` | `a556b80dcf905d84774d12f5a1cad2ecfd5f7ca9a5f994aac3d98ffe95e75e71` |
| Revised Claude `status.json` | `82886f41316562c173372062a24e60440396d47eb95538c925f24b8caf7e4fcd` |
| Independent source-pose contract | `4be023a8d0bfed24c17efec723c05440961e6a2d8bcafde0cf884058692b4e18` |
| Independent adaptive-mesh check | `b3d63af2aa885bd41c451cf33e9a59eba54aca8cb59aaf868e6e5748cb8b3fda` |

Existing performance evidence remains under `target/verification/compositor-performance-v1/`, and qualified app observations are recorded in `docs/CORRECTNESS-QUALIFICATION-20261011.md`.

Reviewed production base: `b619fe6462f85b5ad0e48b0f38d517f808c180fb`. Parent project `w-478e719b3be844d28b469ea1782b0347` remains open. The next bounded decision is the retained-anchor platform probe, with the conservative adaptive mesh as a fallback if native correction cannot preserve the required head-motion behavior.

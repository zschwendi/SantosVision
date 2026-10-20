# Finite sky support experiment — October 20, 2026

This is a default-off test of one possible contributor to the stepped internal
sky holes. It is not a demonstrated headset fix. The normal vertex and fragment
functions, normal depth range, graphics settings, game source and AOT stay
unchanged.

Enable explicitly with `MUGUET_COMPOSITOR_FINITE_SKY_SUPPORT=1` or
`--sv-world-finite-sky-support`. The experiment declines copy, anchor probe,
adaptive and corner-precompute flags, and never runs on the single-source stereo
diagnostic. It uses each actual native source eye independently through the fine
mesh. `finite_sky_support_experiment_depth_mesh` in a debug capture means output
eye 0 actually used this path; a request alone does not establish use.

The first pass uses the original source vertex arithmetic and matching RGB/fade
arithmetic, with the original reverse-Z depth test/write. A second integer MRT
records the final surviving source owner: 0 untouched, 1 exact-clear sky,
2 finite world. It receives the same depth visibility and discard decisions as
color. No color, blackness or alpha inference is used.

After every world fragment has finished, a depth-only pass touches only owner 1.
It reconstructs the actual target pixel's eye ray using the actual inverse
projection, places a support point at 0.8 times the declared far distance, then
projects through the unchanged actual target projection. Only strict finite
depths inside `(0,1)` are written. At far 1000m this defines an 800m sphere,
giving margin for late view adjustment. Original sky color/rotation remains;
the support introduces finite-distance parallax and is not true infinity.

Owner 2 retains its original depth even when geometry is 900m away, behind the
800m proxy. Completing world visibility before repairing sky is essential: a
single-pass nearer sky depth could occlude that geometry. Owner 0, including
unseen black areas and invalid/disoccluded source regions, is never filled or
assigned sky depth. Fast head-turn missing coverage remains a separate problem.

The ownership pass ends before repair, and the ordinary reticle runs afterward
in a separate single-color-attachment pass. The reticle never writes the mask.
Debug output readback is later on the same command buffer, so it captures the
repaired depth, not an earlier intermediate.

Masks are private `r8Uint` targets, at most six for three in-flight two-eye
commands. Each is capped at 16MiB, so the pool cannot exceed 96MiB. Masks stay
exclusively leased until GPU command completion, with synchronized pool access;
idle wrong-size entries are dropped. Allocation/encode failure uses the original
render path. Source/destination extents and single-sampling are qualified, and
the caller's render-pass descriptor is copied before adding the MRT.

Run `sh scripts/test-finite-sky-support.sh` for actual offscreen Mac Metal tests.
The production Swift uniform ABI is extracted verbatim. Tests run the actual
optional helper and Metal functions, compare all RGB bytes and all finite or
uncovered depth bits to the original production pipeline, and reconstruct the
final GPU sky depth independently to verify the 800m support sphere.

Results in `target/verification/finite-sky-support-20261020/final/result.json`:

- Registered physical source replay: 291,334 sky pixels gain support depth;
  2,641,082 finite and 450,880 uncovered pixels retain bit-identical depth;
  every RGB byte remains identical. Maximum reconstructed sphere error is
  0.000306m.
- Sky, 900m geometry, black finite geometry and invalid source regions pass all
  three existing color-transfer modes. Each preserves 14,044 visible finite
  pixels beyond 850m while leaving 720 uncovered pixels untouched.
- Explicit overlapping sky/900m source layers in both draw orders preserve
  12,166 checked 900m final-depth pixels per order, while repairing surviving sky.
- Six in-flight masks, refusal of a seventh/oversized mask, completion lifetime
  and resizing without releasing busy resources pass.

The replay uses the registered October 20 first-qualified physical capture. It
is a different earlier viewpoint than the user's new screenshots. XMP dates
qualify IMG_0631–0633 as historical October 13 images of an unknown build;
IMG_0636 onward are current October 20 feedback, including the large stepped
sky holes. Neither this startup replay nor synthetic tests establish that
system composition caused those holes. No simulator or device install was used
for this experiment. A full signed app and physical A/B comparison remain the
parent task's next work.

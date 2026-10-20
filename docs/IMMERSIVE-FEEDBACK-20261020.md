# Physical feedback and next fixes — October 20, 2026

## Build 7 live test, 10:35–10:42 MDT

Exact signed candidate `743ba8f` (executable SHA256 `d31aa8fb7c9272dc25ac3208f130f4116be9352c1b5e62666866bd2e16fb1515`) installed in place successfully. Own-app inspection confirms version 7 and retained game data. The normal run presented both immersive eyes. Its current-camera crop restoration advanced to 1,035 restores with zero failures, unqualified callbacks or dropped ownership. This establishes live qualification; it does not by itself establish visual aim correctness. One 10-second sample measured 37.02ms mean compositor GPU command-buffer time, excluding guest rendering and system display.

The user's new physical report is more specific:

- Flat play and third-person immersion look good. Preserve third-person head free-look: avatar, weapon and aiming remain stick driven.
- First-person needs selectable actual head-directed aim versus head-check free-look. The visible reticle appears binocularly aligned, but is often absent or leaves the view while the gun points elsewhere. Couple the actual shot ray, weapon and reticle; moving only the dot is insufficient.
- Weapon wheel and WASTED remain doubled. Map, health and money are outside the useful immersive field. They need a qualified HUD route and comfortable placement, separately from viewmodels.
- Fast turns still expose top/bottom/peripheral black borders; some sky holes remain closer to the viewer. Increase useful source coverage and reduce source/compositor delay independently.
- Immersive appearance improved, but black speckles on the character and much worse clarity than flat remain unacceptable.

Registered normal-run analysis now shows black cracks on the third-person character and other depth boundaries after reprojection, while the source character is clean. A visually selected output body region contains 1,557 black pixels, all at exact-clear depth; the separately selected source body region contains none. All finite source texel centers reconstruct at least 1.773m from the current eye, so the 0.15–0.45m ordered near-body fade cannot explain this captured episode. Isolated texel footprints at depth discontinuities are the supported mechanism; actual newly exposed background remains unavailable. The first-person close-body case can additionally invoke that dotted fade and needs separate qualification. Exact analysis and crop evidence: `candidate7/analysis-normal-ready/` and `candidate7/body-speckle-audit-20261020/`.

A separate finite-sky-only process presented on the physical headset. The user reports the sky holes are **much smaller and only in the distance**, a partial improvement rather than complete resolution. Its unique schema-2 capture completed and was compositor-acknowledged with `rendering_method=finite_sky_support_experiment_depth_mesh`; all four registered source/output color/depth files are local and analyzed. That captured viewpoint looks down and contains only 294 source clear-depth pixels, so it cannot certify the user's distant sky result or both eyes by itself. No guard band, corner-precompute, adaptive mesh or single-source mode was enabled. Receipts and captures are in `target/verification/device-session-20261020/candidate7/`, including `normal-ready-feedback-receipt.json` and `analysis-sky-ready/`. First-person head aim, full HUD routing, coherent stereo and performance acceptance remain unfinished.

The earlier signed r6 session produced two registered source/output color and Float32 depth images and a matching late-color trace. The user's subsequent 19 screenshots are preserved, with hashes and capture metadata, in `target/verification/device-session-20261020/feedback-images/manifest.json`. Originals remain in Downloads. The folder was copied through Finder after direct Downloads reads stalled; no unrelated media was opened.

## Screenshot provenance and findings

`IMG_0631`–`0633` were captured October 13 and are historical build-unknown evidence. `IMG_0636`–`0651` are October 20 captures, 02:05:00–02:09:40 local, during the installed r6 session. All are single-view 1920×1080 screenshots; binocular behavior and felt latency additionally rely on the user's direct headset report.

- Current `0636`, `0637`, `0640`, `0642` and `0643` show large stepped black sky regions, including interior gaps bounded by otherwise rendered buildings. This is more than the ordinary outside-frustum border.
- `0638` and `0641` show ground detail; the user reports black floor/peripheral areas during turns and delayed catch-up. The first registered pre-system capture already contains a large black left/bottom border. In an every-fourth-pixel pure-rotation check, 98.38% of sampled black/clear output pixels lie outside the old source frustum after a 4.295° pose change. This diagnoses that captured border, not every sky hole.
- `0639` shows the returned flat game window. A source-qualified ownership defect can retain XR crop offsets after immersion; observed headset offsets would displace forward projection by approximately 257 horizontal/47 vertical pixels at 1920×1080. Post-fix physical return still needs testing.
- `0644`–`0650` show weapon/hand silhouette distortion, dark/dithered gun appearance, and shooting/reloading. The user reports divergent binocular reticle/impact behavior and a reticle that fails to steer with head movement. R6 intentionally uses the original controller ray and a fixed 40m direction marker, not head-directed game aim or an authoritative hit point.
- `0651` shows displaced WASTED text and launcher controls. HUD remains mixed into the world color and reconstructed using world depth; it is not yet a qualified independent HUD layer.
- User reports excessive softness/stylization and weak stereo. Requested source remains 1920×1080 Low; the captured eye output is 1888×1792, approximately 1.66× vertical enlargement. No compositor gamma defect was confirmed. Natural eye updates use different game frames; stereo strength is not a substitute for correcting this mismatch.

## Changes under validation

The source crop restoration uses current live game-camera callbacks and exact current SetWindow semantics. It preserves fresh game lens/depth settings and declines unknown custom geometry. It does not write saved guest pointers from the UI. Forty-eight integrated tests pass; physical geometry qualification and flat aiming require a new headset build.

The default-off finite sky support experiment records actual final sky/finite/uncovered ownership and changes only surviving sky depth, preserving all finite geometry and uncovered areas. Six actual offscreen Mac Metal cases pass, including registered source replay and overlapping 900m geometry in both draw orders. This tests the finite-far/system-composition hypothesis; it has no physical acceptance yet. See `FINITE-SKY-SUPPORT-EXPERIMENT-20261020.md`.

The source-only guard band is opt-in via `MUGUET_XR_SOURCE_GUARD_BAND`, a finite scale from 1.0 to 1.2 (default 1.0). It expands the source request, retaining that exact projection through the lease while leaving the physical display projection and eye spacing unchanged. At fixed texture size it trades angular resolution for limited turn coverage; it does not render the unseen hemisphere, repair translation disocclusion or reduce pipeline delay. 110,946 CPU ray/depth checks and independent provenance review pass. Test at most one rendering experiment at a time.

Native status now distinguishes GPU-ready age from source-relative pose lag and snapshot submission-to-completion wait. The later r6 snapshot shows source pose timestamps 111–156ms behind the newest primary-camera pose; this is not an atomic motion-to-photon measurement. Nine focused tests pass, including actual normal/paired Metal publication. Do not label ready age as head latency.

## Required next physical sequence

Build and sign current committed source with the unchanged original AOT, preserving frozen r6/build5. Start normal window first; verify game data/saves/controller. Observe `primary_crop_restoration` qualification and test immersive→flat aim. Then independently compare normal sky with the finite-sky flag; require current source/output capture and the user's view. Separately compare corner precompute with compositor GPU timing, then a small source guard band if performance permits. Never enable single-source stereo for normal play.

Head-directed gameplay aim still needs a center-head game ray, acknowledged native camera contribution and residual render transform so the head rotation is applied exactly once. Moving the dot alone is insufficient. Matching gun/arms transforms and depth, independent HUD coverage and coherent same-update stereo remain required. Exact plan: `target/verification/device-session-20261020/head-aim-integration-plan-20261020.txt`.

No simulator, banked reset, original AOT regeneration, uninstall or automatic background continuation is authorized by this record. Parent work `w-c6e3be74eb8743d1ab27b2ebffa59406` remains unfinished.

# Match the finite native world depth range

The previous compositor configuration used a fixed1000m far plane. That same
projection became the actual GTA source projection through the typed camera
setters, so native finite world geometry beyond1km was clipped. The native
original lens recorded in the stale build18 report at
`target/verification/device-session-20261024/quality20/feedback-runtime-report.json`
(timestamp05:25:53UTC) has near0.15m and far10000m. This is concrete range loss,
but does not prove the cause of the user's remaining black squares or a total
black frame. That report is not evidence that build19 ran.

Before each XR override, the certified primary camera callback now records a
far-plane observation only if the current original projection is an exact
`RegeneratePerspective` result with its exact legitimate SetWindow crop. The
observation carries no guest pointers. `sv_xr_native_far_plane_metres()` returns
that finite native range only while at most2seconds old and within1..20000m;
unknown, future-dated, stale, custom or out-of-bounds observations return0.
Normal lifecycle invalidation clears it.

The observation is available during flat gameplay: on visionOS, camera tracing
starts enabled and is never switched off. The original snapshot is sampled even
with immersive world-camera ownership disabled. The compositor layer is created
when opening GameWorld immersive space after normal window preflight, so live
window gameplay supplies the lens before `makeConfiguration`. If freshness or
qualification fails, configuration preserves the previous1000m fallback.

The compositor selects the observed native far plane (minimum1000m), retaining
its existing supported near-plane policy. The resulting actual SDK projection
then travels through source requests, exact retained eye geometry, native typed
setters, immutable GPU leases, scope admission and display reprojection. No
unrelated culling, visibility or occlusion tests are bypassed. Camera setters
still regenerate native frustum planes and dependent matrices. Scope zoom and
whole-clip calibration tolerances are unchanged. Infinity remains unsupported.

Native/Swift finite projection parsers allow reconstructed far up to20512m.
Nominal native authority remains at most20000m. The additional range admits f32
forward-Z regeneration and reverse-depth flips near p22=-1 (up to about2.5%
range reconstruction error at near0.05m and nominal20km); it does not relax the
whole-clip ray/depth error budget or request a farther nominal world.

Validation:

- 1678 Swift checks cover finite fallback/selection, both captured calibrated
  eyes, regenerated1/10/20km depth rows at near0.05/0.1/0.15m, guard1.3/2/3,
  scope optical scales1/3.146/9.487 and both depth directions. They also expose
  old1km clipping of otherwise valid distant native geometry.
- Actual native scalar-rebuild and exact reverse-clone admission passes10/20km
  in both depth directions. All28 adjacent xr_view tests pass. Separate current
  native lens/crop qualification and getter freshness/invalidation tests pass.
- `scripts/test-depth-range.sh` reproduces the pure Swift tests in Swift6 strict
  concurrency mode. The registered X/Y/W calibration is from an actual SDK
  capture; extended depth rows are mathematical fixtures, not newly captured
  SDK10/20km matrices.

Physical acceptance requires a fresh source-qualified build and a new normal
window-to-immersive run. Confirm native_original_far_plane_metres, actual source
and compositor range, scope behavior and a moving source/output capture. The
new range cannot create rays outside source coverage, remove temporal alternate
stereo, fill translation disocclusions or by itself prove black-square removal.

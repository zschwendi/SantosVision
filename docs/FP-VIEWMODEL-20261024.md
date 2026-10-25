# First-person view model in immersion — October 24, 2026

Update after physical build25: the earlier cache-stage observations below do
not establish same-request solved-hand or visible tracking acceptance. Exact
completed native goals now match the last sampled target positions, but remain
before Solve. The actual same-eye/frame gun placement differs by172.766deg,
while arms agree. [Build26](GUN-FIRST-PERSON-WORLD26-20261024.md) adds a qualified
gun-only queued-world path; the old O-derived correction remains for arms and
unqualified draws. Physical hands/arms/gun and firing agreement remain unaccepted.

Worker `w-708b69ad7cb1407d952c27ea341b7404`; parent
`w-14dea40fb6e248138ad507f85cf4384b`. Source change only. Root owns the
nine-body artifact, AOT, signing and physical acceptance.

## Why tracked arms cannot be seen today

Build 21's own observations
(`device-session-20261024/claude-five-issues22/runtime-report.json`) show the
native arm solver working: at the cache stage the actual hand bone equals the
native goal exactly for both arms (`goal_active_byte` 1, blend 1.0). Original
38548 writes `goal.position = hand + blend * (inverse(R) * target - hand)`, so with
blend 1 the goal is the converted tracked target. The bones do move.

They are drawn in the wrong frame. Original 44415 (StreamPed, the player's arms)
and 44355 (skinned attachments, the gun) take a first-person branch that clones
the current viewport, zeroes its translation (`Bz`) and builds
`M = Bz * S * inverse(O)` from the camera-relative extension `O`. The view of that
clone is `inverse(Bz)`, so `view * M = S * inverse(O)`. Headset rotation, head
translation and eye offset all cancel. Both eyes get the same head-locked image
of the arms and gun, with zero disparity. A tracked hand is drawn where it would
be if your head were still at its reference pose. Looking down at your hands
can never bring them into view. Source certificate:
`immersive-recording-20261013/viewmodel-transform-report.md`.

## Change

At the two certified completed-model seams (`barrel-reticle21/completed-model-seams.json`),
the nine-body factory `src/xr_viewmodel_patch.rs` adds one host call. The call
happens only when the original first-person branch has saved a parent viewport:
44415 before `0x01a9c894` and 44355 before `0x01a63f6e`.

The host, in `src/xr_viewmodel.rs`, checks all of the following:

- the original context and draw-data first-person predicates;
- that the clone is the zeroed parent basis;
- that the parent's twelve camera coordinates are a headset eye installed by
  `xr_view::camera_post`, from which `R = inverse(B0) * Beye` is recovered;
- that the stack model equals the original `Bz * S * inverse(O)`.

It then rewrites only the twelve model coordinates:

`M' = Bz * inverse(R) * inverse(Bz) * M`, so `view * M' = inverse(R) * S * inverse(O)`.

The result is the native flat first-person placement relative to the game
camera `B0`, seen from the physical eye. The world already uses `Beye = B0 * R`.
The 44355 code reloads its cached translation locals 20/19/18, so native LOD
consumes the corrected model. Any rejection leaves the native matrix untouched.
Third person, flat mode, unmatched views and other families are unaffected. No
game camera, aim, bone, goal, weapon or firing state is written.

The correction is opt-in. It requires the certified motion AOT, which sets
`MUGUET_MOTION_AOT_SHA256`, plus `MUGUET_XR_FP_VIEWMODEL=1` or the compile-time
`MUGUET_XR_FP_VIEWMODEL_DEFAULT=1`. Status is reported under
`motion_aim.native_arms.first_person_viewmodel`. It includes per-family
first-person hook and applied counts, the last gate, the head/eye transform that
was restored, and an observation: the distance between `B0 * S * inverse(O)` and
the draw data's own world matrix. That distance shows whether the camera
extension is stale relative to the actual barrel.

## Evidence

- Exact original certificate: body SHA, ABI, local types, landmarks, and seam
  ordinals 500/686. PASS.
- The exact original 44415/44355 model arithmetic was executed with calls
  stubbed by their original types, under the production seam instrumentation
  and the production host code. In this test:
  - the original result equals the host's `Bz * S * inverse(O)` qualification;
  - corrected `view * M' = inverse(R) * S * inverse(O)` within 5e-4;
  - 44355 LOD distance consumes the reloaded translation;
  - non-first-person and unmatched views keep native bits.

  PASS (ignored test; needs `MUGUET_TRACE_ORIGINAL_WASM`).
- Host mathematics: a world-fixed tracked grip appears at `eye_from_grip`.
  Identity `R` preserves model bits. Padding and every other word stay
  untouched. Each unqualified branch writes nothing.
- Synthetic factory: the host is called only for a saved parent; downstream code
  reads the corrected model; traps propagate; nontarget bodies and data are
  preserved; double instrumentation is rejected.

## Root actions

1. `MUGUET_TRACE_ORIGINAL_WASM=<original> cargo test --lib exact_original_nine_bodies -- --ignored`
   This is the full parser/re-encoder preservation check. It is large, so run
   it centrally. Then
   `--precompile-visionos-motion-arms-cache-viewmodel <original.wasm> <new.cwasm>`
   to a new path, and build the host with the manifest's `aot_sha256`.
2. Preserve candidate 21, the seven-body artifact and the 20261013 checkpoint.

## Physical acceptance still required

These are measured on hardware, in immersive first person with Sense controllers
and a weapon equipped. Unarmed, neither arm is requested: the right arm reports
`equipped_physical_weapon_absent`, and the left arm requires the right
attachment to qualify.

- Look down and around. The arms and gun stay fixed in the world, not on the
  face, and appear where the controllers are.
- Both eyes show plausible disparity, with no doubling.
- Head turns without moving the grip leave the gun still.
- The reticle stays on the drawn barrel near and far.
- Check `camera_relative_vs_draw_world` while standing and while running.
- Depth and reprojection: the gun does not swim or ghost during head motion.
  The foreground depth path is not certified.
- Near clip: the rear sight does not clip during ADS. The corrected sight
  picture is binocular and centred on the game camera, so a dominant-eye policy
  may be needed. That belongs to the separate ADS task.
- Third person and flat first person are unchanged.

The fixed-FOV family 44395 is not changed.

At the build 21 cache stage, the native goal sat 16–19 cm forward of the desired
grip for both hands. Blend was 1.0 and reach was within range (0.40–0.42 m from
the upper arm). This is consistent with the entity reference moving between
solve and render-list read. Confirm with a stationary capture. Full left-wrist
orientation is still not implemented. It needs an explicit per-hand
grip-to-hand calibration, captured while the user matches the now-visible
native hand. It must not use guessed axes.

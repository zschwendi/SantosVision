# Native-goal diagnostic build 25

This is a diagnostic candidate, not a verified hand-tracking fix. The user's
visible Sense-driven hands, arms, weapon/barrel/reticle agreement and unarmed
controller hands remain unfinished. Accepted third-person behavior is retained.
Parent: `w-14dea40fb6e248138ad507f85cf4384b`; integration:
`w-3f4d23f4b3a64ce6bdd2920b69c14ac3`.

Source `0d47441a77291f2f03d4eb66412df110b69bea49`, tag
`los-santos-native-goal-diagnostics25-20261024`. Native implementation is
`5e5966d`; that implementation was completed by the local worker after the
verified Opus 5.5/xhigh run exhausted its turn limit without a patch. The root
retains independent gun/arm observations instead of allowing the latest arm
sample to overwrite the gun's diagnostic identity. Launcher footer displays 25.

The new read-only callback observes each qualified native arm goal immediately
after its construction, before Solve. It reuses the existing target signature
through explicit bit 31, with per-invocation pairing, owner/cookie/generation
checks and consume-once cleanup. Optional `ped+4256` skeleton comparison is
fail-soft; draw selection remains **unknown**. See
[the native-goal protocol](POST-NATIVE-GOAL-DIAGNOSTICS-20261024.md).

The build-24 capture already established that the recorded gun/arm depth reaches
the published image without foreground merge. The broad missing-depth diagnosis
was withdrawn for that arrangement. Do not enable foreground merge as a hand fix
or infer an alternate rig is used merely because a candidate pointer differs.

## Verification and frozen artifact

- Focused native-arm tests: 39 passed, four ignored; final changed-read tests:
  two passed. The separate root per-family regression also ran and passed.
- Independent review found no blocker. Executable fixtures preserve all guest
  bytes and TRUE/FALSE/repeated-arm behavior. The host-memory fixture is not a
  full host-hook dispatcher test; that limitation remains explicit.
- Exact original complete-chain parser/reencoder test passed in 132.41 seconds.
  Patched WASM SHA256:
  `2a5d560d54b48d2c137c9f7d099e14807d6f09c98cdff64da441df8c320dd8f6`.
- Release tool, separate visionOS AOT, native archive, full signed visionOS app,
  strict signature, device provisioning, iCloud entitlements, complete AOT
  embedding and executable RX segment checks passed. Original WASM/AOT and
  frozen prior apps are preserved. No simulator or banked reset used.

Frozen candidate:
`target/verification/device-session-20261024/post-goal25/integration/SantosVision.app`.
Executable SHA256:
`ad71c23e023302c697c724a22438711a6f759f61e5b1f5e5d07ae6d659ad5811`.
AOT SHA256:
`9db6a260f6727f3b9935a0b95d172c06319531ba5ec7b2de4efc2632e7d381d6`.
Exact recipes, logs and qualified deployment state are in that directory's
`candidate-receipt.json`. Foreground merge and image capture remain default off.
Normal controller/arm/viewmodel choices and graphics defaults remain unchanged.

## Deployment and next physical observation

Installed **in place** and verified as build **25** on the physical Vision Pro.
The first45-second transfer timed out; one180-second bounded retry succeeded.
No uninstall or save replacement occurred. Ordinary foreground launch succeeded
without special environment flags. The fresh00:22:00Z report shows active window
presentation,1736 window frames, no last error and the new post-native-goal schema.
It has not entered immersive and both arm sample counts remain zero. This is
window startup evidence only; visible tracking remains unaccepted.

## Next test

Use the ordinary launch and enter immersive first person with a supported gun.
Keep the head still and move the right Sense controller, then move the head
independently. No special capture setting is needed. Read this app's own fresh
`Documents/native-runtime-report.json`; require the new
`status.payload.world_view.motion_aim.native_arms.geometry_observations.{left,right}.post_native_goal`
fields with accepted, current-run samples, and retain gun/arm family identities
separately. Completed goals are before Solve, so they do not certify solved skin,
consumed palette, barrel/shot agreement or visible acceptance. If palette
ownership remains the missing link, the bounded follow-up design is saved in
`target/verification/device-session-20261024/post-goal25/palette-next-observation-design.md`.

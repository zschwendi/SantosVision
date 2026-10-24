# Los Santos motion controls

The intended on-foot experience separates looking, body movement and weapon
aiming. The headset controls the view, the dominant hand controls the weapon,
and both eyes project the same gameplay aim. Third-person and flat-window
controls retain their existing behavior.

## References inspected

- [VRAge](https://www.gta5-mods.com/scripts/vrage-jaspergaming) is the closest
  GTA V behavior reference: the author describes tracked wrists/arms and
  independent weapon aiming. No public implementation source was established.
- [GTAVR](https://github.com/DeployAbi/GTAVR/tree/f2a5e5357e5a2f2e7210013f065c3316a921192c)
  provides distribution documentation, without a verified public implementation.
- The MIT-licensed [UEVR San Andreas DE weapon implementation](https://github.com/Holydh/UEVR_GTASADE/blob/f2326ab0afd9235dac3a7833c86b9400fe4970f9/WeaponManager.cpp)
  demonstrates separate hand-bound weapon placement and a calibrated weapon ray.
  Its Unreal interfaces and offsets do not apply to this GTA V WASM runtime.
- R.E.A.L. is a head/gamepad reference; its documented release does not provide
  motion-controller support.

Detailed source and license evidence is retained in
`target/verification/device-session-20261023/vr-motion-references/`.

## Implemented source milestones

1. Separate left/right spatial controllers supply sticks, triggers, grips and
   face buttons. Hardware handedness is resolved through ARKit. Hold the left
   stick click while moving the stick for D-pad navigation. See
   [Sense controls](PSVR2-SENSE-CONTROLS.md).
2. One ARKit session supplies head, accessory grip/aim and bare-hand wrist
   poses. Immutable samples carry the original source time and accepted head
   generation. Loss, reconnect and renderer changes invalidate old poses.
3. A separate, exact-source diagnostic AOT supplies the right accessory aim
   frame to GTA's original gun-direction and calculated-shot consumers. It
   preserves native shot corrections, ammo and firing logic. Tracking loss
   blocks motion fire and requires trigger release before rearming. See
   [native motion aim](MOTION-AIM-DIAGNOSTIC-20261023.md).

The diagnostic requires both the matching embedded AOT and explicit launch
opt-in `MUGUET_XR_MOTION_AIM=1`. A normal launch keeps ordinary aiming. A tracking
preview alone does not enable motion shooting. Bare wrist poses are not treated
as calibrated gun barrels.

## Remaining native integration

The current gun-direction hook reduces its target to torso yaw/pitch. Independent
weapon translation and roll require the native arm-goal matrix solver and its
weapon attachment chain. These are distinct from moving a rendered gun image.
Weapon-to-controller calibration, deliberate offhand support, bare-hand actions
and motion-aware HUD/reticle routing remain unfinished.

## Headset acceptance

Check head looking away from a stationary gun, independent hand translation and
roll, near/far shots and one shared reticle in both eyes. Then check loss while
holding the trigger, release/reconnect, recentering, menus/phone/wheel,
third-person, flat mode, vehicles, cutscenes and death. Build success and native
fixtures do not establish physical weapon, bullet or reticle agreement.

Signed candidate 9 is the original-AOT Sense button/pose milestone. Candidate 10
is the separate native-aim diagnostic. Read each frozen receipt before installing;
neither label alone establishes that it is installed or physically accepted.

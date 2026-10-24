# PS VR2 Sense button controls — 2026-10-23

This milestone adds native visionOS button/stick input for two independently
connected spatial controllers. It does not apply tracked controller poses to the
gun or game camera. Physical acceptance is still required.

## API and identity

Apple's spatial-controller guide specifies a separate `GCController.input`
profile: each half has A/B, grip, trigger, thumbstick click, Menu and a thumbstick.
On Sense, left A/B mean Square/Triangle; right A/B mean Cross/Circle. The previous
`extendedGamepad`-only poller did not handle this profile.
See [Apple's input guide](https://developer.apple.com/documentation/gamecontroller/discovering-and-tracking-spatial-game-controllers-and-styli).

The installed XROS27 SDK declares `ARKit.Accessory(device:)` and
`Accessory.inherentChirality` at visionOS26. This supplies fixed hardware
left/right identity; discovery order, translated product names and which hand
currently holds the controller do not select control roles. Only one unambiguous
device per hand is used. Menu and thumbstick click are optional: missing either
leaves core movement, face buttons and triggers working, with separate status.

`SpatialGamepadInput.shared.trackingAccessories` exposes these resolved
accessories on the main actor, with `accessoriesDidChange` notifications on
resolve/disconnect, for the separate permissioned tracking implementation.
Constructing this input registry does not run an accessory tracking provider.

## Mapping

| Physical control | Gameplay | GTA menus |
| --- | --- | --- |
| Left stick | Move/steer | Navigate |
| Right stick | Look using existing 75% sensitivity | Existing behavior |
| L2 / R2 | Aim/fire; brake/accelerate in vehicles | None; left free for native-button select |
| L1 grip | Weapon wheel | Previous tab (Q, FRONTEND_LB) |
| R1 grip | Cover; handbrake in vehicles | Next tab (E, FRONTEND_RB) |
| Square | Jump | Apply (Space, FRONTEND_X) |
| Triangle | Enter/exit vehicle | GTA's Y action (Tab, FRONTEND_Y) |
| Cross | Sprint/confirm | Select (Enter) |
| Circle | Reload/back | Back (Backspace) |
| Right Menu/Options | Pause | GTA pause/back (Escape) |
| Left Menu/Create, if exposed | Cycle camera, replacing absent touchpad | Inactive |
| Right stick click | Existing rear-view action | Existing behavior |
| Hold left stick click + deflect left stick | Virtual D-pad; suppress movement | Directional navigation |
| Tap left stick click without directional use | Normal L3 click on release, held120ms | Existing behavior |

The Menu aliases follow Apple's documented API. Whether the physical left Create
button reaches `.menu` on this paired device remains a hardware test, and its
availability is reported separately. The virtual D-pad provides phone/radio and
other directional actions; holding its Down direction preserves the existing
300ms character-switch behavior. L3 is a tap after release with this profile,
including a brief horn in vehicles. The DualSense mapping stays unchanged.

Current input is read afresh at every native poll. A missing half contributes no
keys, axes or triggers. Replacing/disconnecting the left device, changing input
backend or suspending the runtime clears pending modifier/tap state. A click held
at connection/resume must release before becoming a new chord. Full/partial
disconnects go through the established key/button release mapper.

GTA menu keys follow GTA's fixed PC front-end bindings in the shipped
`x64/data/control/fixed.meta`; DualSense uses the same menu keys. While GTA's
own pause menu is open, the Start panel no longer owns the controller. See
[GTA menus with Sense](SENSE-GTA-MENUS-20261024.md).

## Pairing and validation

Pair each half individually in Vision Pro Settings → Bluetooth. Apple documents
PlayStation+Create on the left and PlayStation+Options on the right to enter
pairing mode. See [Apple's pairing instructions](https://support.apple.com/guide/apple-vision-pro/connect-spatial-accessories-tan7a3f2c9d3/27/visionos/27).

`scripts/test-pad-routing.sh` uses the actual production pad modules with recording
host-input sinks and no game runtime, graphics, simulator or device. Fifteen
tests pass, including emitted disconnect/suspend releases, D-pad versus movement,
tap timing, reconnect/identity changes and existing menu/phone regressions.
Artifacts: `target/verification/sense-controls-20261023/`.

The new Swift bridge passes Swift6 strict-concurrency typechecking against the
physical XROS27 SDK at deployment26. Full native archive/Swift/Metal linking and
actual paired-controller gameplay are required before physical acceptance.

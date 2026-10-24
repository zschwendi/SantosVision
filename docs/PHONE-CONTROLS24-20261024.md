# Los Santos build 24 — October 24, 2026

Installed in place on the paired Vision Pro; the bundle query confirms 24.
No launch, uninstall or save replacement occurred. The freshly queried own
process matched the installed app; its report had both app activity flags
false before its ordinary SIGTERM and update. No unrelated app was stopped.

The immersive renderer no longer receives a utility atlas. Its tracking-area
textures clear each frame and its spatial-event callback does nothing, so the
Phone button, phone directions and invisible hit regions are absent. Windowed
Phone/Close-phone and its directional ornament are unchanged. Existing pause
and camera controls remain. The launcher footer reads the bundle build number.

- Client source `04456addc943da51f4770fd0fa163762a7bb2e4a`;
  tag `los-santos-phone-ui-candidate24-20261024`.
- Native source `889bfe784ff1223d0964ec7516d4650276846f91`, using the frozen
  23-r2 archive unchanged; no new native compile is claimed.
- Executable SHA256 `60892282c7b490691c4a6159386a4129dc9e02054ceea1867f0f940d6cccaa24`.
- AOT SHA256 `ec56cf2308faca2a49a58edf46a7337ef3885d0bd75852c9f4d14b6023601fe2`.
- App, build, signature, entitlement, install and version-query evidence:
  `target/verification/device-session-20261024/phone-removal24/`.

Signed client build and independent source/package review pass. Complete AOT
bytes, RX-only `__GAME`, strict signature, team, physical provisioning and cloud
entitlements were verified. No simulator or banked reset was used.

## Tracking is still unfinished

The player reports that visible Sense-driven hands/arms do not work. Installed
22's retained report shows both tracked accessories, native arm requests and
model-correction hooks. It does not correlate a matching request to a displayed
skeleton. Its corrected first-person draws have no supported foreground-depth
publication in the ordinary path. Unarmed Sense hands also remain unimplemented.

Build 24 carries the conditional foreground experiment but leaves it off.
It must not be presented as a hand-tracking fix. Read
[the foreground plan](FP-FOREGROUND-DEPTH-20261024.md) before changing that default.

## Next actual headset test

Check no Phone utility in immersive, and working Phone/Close-phone in the game
window. A separate diagnostic launch must set
`MUGUET_XR_FP_DEPTH_CAPTURE_DIR=Documents`, with foreground merge still off.
Once gameplay is live, enter immersive first person with a gun, hold the head
still and move the right Sense, then hold the Sense still and move the head.
Require a written manifest and registered colour/depth images from the same
draw sequence. Capture the actual target/order/blend/depth case before repairing
or enabling its merge. Ask for this test when the player is ready; do not
interrupt their other foreground app. Normal launch remains capture-free.

Then compare actual visible barrel, reticle and native shots. Only visible
success closes tracking acceptance; pose/target/hook counters do not. Left
calibration, unarmed hands and broader issues remain in the
[request audit](USER-REQUEST-AUDIT-20261024.md).

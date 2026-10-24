# First-person candidate 23-r2 — October 24, 2026

**Historical signed native baseline; this exact app was never installed.**
[Build 24](PHONE-CONTROLS24-20261024.md) now carries this frozen native archive
and AOT, with newer client UI, and its installed version was verified. The
user's build-22 feedback failed visible hand/arm tracking. Physical acceptance
remains open; default-off foreground depth is not a delivered tracking fix.

Parent `w-14dea40fb6e248138ad507f85cf4384b`, topic `santosvision-immersive`.

## Frozen app

- Source `889bfe784ff1223d0964ec7516d4650276846f91`.
- Tag `los-santos-first-person-candidate23-r2-20261024`.
- App: `target/verification/device-session-20261024/first-person23-r2-device/SantosVision.app`.
- Executable SHA256 `cabbbd9cf54290685d6c95633e8c6a85556ba73ab20c6c761b893ebcf1fd4a8c`.
- AOT SHA256 `ec56cf2308faca2a49a58edf46a7337ef3885d0bd75852c9f4d14b6023601fe2`.
- Receipt: `first-person23-r2-device/candidate-receipt.json` under the same
  verification root. It verifies signing, provisioned physical UDID, iCloud
  entitlements, complete AOT byte identity and RX-only `__GAME` protections.

The earlier signed 23 at source `2b8628d` is also preserved. Use **r2** for any
foreground-depth experiment: it additionally rejects unsupported cross-target
depth and correctly identifies enabled destination-replacing blends. Original
AOT, signed 22 and `los-santos-immersive-checkpoint-20261013` remain intact.

## Changes carried forward

- Build 22's serialized scene return and explicit GTA-pause-to-window fallback,
  Sense GTA-menu routing, first-person sights shortcut, scope registration and
  first-person completed-model correction remain.
- **Calibrate left hand** in Settings requests a temporary card on the next
  qualified immersive first-person gun frame. The native solver measures the
  grip-to-hand frame; it does not guess wrist axes. Confirmation needs a real
  native capture, then explicit commit. Cancel, Crown/window exit, tracking or
  controller loss cannot silently replace the previous calibration. A product,
  vendor and handedness fingerprint distinguishes controller models, not units.
- The new foreground path retains actual first-person depth, either in live
  world depth or through a qualified before/after merge with the frozen world.
  It is **off by default**, including capture. It cannot yet certify GTA's real
  material/pass order, depth-only coverage or translucent layers.
- Review repairs cover cached depth after clear, stale attribution after epoch
  reset, colour without depth, unsupported depth on another target and enabled
  destination-replacing blends. Unsupported opaque coverage refuses the whole
  present rather than publishing it over background depth.
- Launcher duplicates/decorations and the redundant View ornament are removed.
  Physical camera shortcuts, Phone/Close-phone and phone-only directions remain.
- Mailbox counters add bounded startup evidence without changing scheduling.
  A structural source certificate is conditional; zero host notifications does
  not exclude the separate atomic-notify path or explain black startup pixels.

## Verification and limits

Logs/recipes live in `first-person23-integration/` under the verification root.

- Full original-game artifact preservation and separate AOT generation pass.
- Reviewed calibration: 11 native state tests, 2 factory tests, 46 Swift-flow
  checks. The original native-flag slice checks passed in the specialist run.
- Foreground: 16 CPU checks; five Metal/classifier checks using the production
  GPU helpers. A real Metal regression failed before the depth-clear repair
  (cached 0.6 after a 0.0 clear) and passes after it. Tests also cover exact
  merged values, untouched background, queue-ordered pool reuse and rejection
  without an overlay / after live combining / on a different depth target.
- Full native visionOS archive, Swift/Metal app build, signing, provisioning and
  embedding checks pass. These are host/source checks, not AVP frame-rate,
  visible arm alignment or physical stereo acceptance.
- No source-resolution, graphics-preset, far-plane, stereo-IPD or performance
  default was increased. Higher display resolution and depth-edge filling stay
  default off. Existing saved choices are not overwritten.

Independent Opus 5.5 sessions and follow-up reviews are retained in
`claude-five-issues22/`, `left-wrist23-review/`, `foreground-depth23-review/`
and `startup-mailbox23/` under the verification root. The final foreground
addendum's R1/R2 cases are repaired at `889bfe7`.

## Next physical test

This is the original 23-r2 recipe. Use the installed build-24 equivalent in
[the current notes](PHONE-CONTROLS24-20261024.md), preserving this frozen app.

1. Resolve the pending **installed-22** check: normal startup, Start from
   immersive returning to the real GTA menu, Sense directions/tabs/Apply, then
   close GTA's menu before expanding again. Keep third-person behaviour intact.
2. When the headset is reachable, verify the own-app activity, quit safely and
   install the exact signed **23-r2** in place. Never uninstall or replace saves.
3. In immersive first person with a gun, compare stationary head / moving right
   Sense, then stationary Sense / moving head. Accept visible gun, barrel,
   reticle and native shot agreement, not target counters alone. Run the left
   calibration and check wrist roll/pitch/yaw and exits/reconnects.
4. Separately create a unique own-app Documents capture directory and launch
   with `MUGUET_XR_FP_DEPTH_CAPTURE_DIR` pointing there, **merge off**. Enter
   immersive only after real gameplay is live. Require a written manifest and
   registered raw images, inspect omissions, actual colour/depth order, blend
   words, families and eye/frame identity. A capture can alter resource usage;
   compare against normal rendering if the artifact changes.
5. Only when that evidence supports a handled pass arrangement, run a separate
   merge-on QA attempt. Equal-pass/prepass arrangements may fail closed;
   capture-only and successful counters never prove visible alignment.

Startup black pixels, scope/HUD fallback, peripheral coverage, image quality,
temporal stereo, unarmed Sense-driven hands, original intro playback and an
actual story/cloud-save round trip remain open in the
[request audit](USER-REQUEST-AUDIT-20261024.md). No simulator, banked reset,
unrelated media, uninstall or unverified physical-performance claim was used in
this candidate batch.

The user confirmed Sense-driven hands, arms and gun as the first tracking
priority. Natural bare-hand interaction is deferred; it is separate from the
unfinished unarmed Sense-driven avatar hands.

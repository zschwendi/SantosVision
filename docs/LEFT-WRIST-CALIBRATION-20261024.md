# Left wrist calibration — October 24, 2026

Worker `w-708b69ad7cb1407d952c27ea341b7404`; parent
`w-14dea40fb6e248138ad507f85cf4384b`. This is a source change on branch
`codex/los-santos23-left-wrist`, based on `db5f612`. Root owns:

- the full artifact preservation check;
- AOT generation, signing and installation;
- physical acceptance.

## Behaviour

The left Sense controller now drives GTA's own full-pose arm solver, wrist
orientation included, once the player has deliberately calibrated it. Until
then, and whenever the calibration does not apply, the existing fallback is
unchanged: the hand follows the grip position (native flags 0) and keeps the
game's own wrist angle.

### Player steps

1. Open Settings → Controller → **Calibrate left hand**. Settings closes. If
   GTA's pause menu is open, close it, then enter immersive view.
2. Switch to first person while holding a gun. The calibration card opens by
   itself in the existing controls window. Gameplay input is blocked; head and
   controller tracking continue.
3. While the card is open, no left request is issued, so GTA's own left hand is
   shown. Put the left controller exactly where that hand is, at the same
   angle. Hold still, look at **Confirm** and pull the right trigger.
4. The card reports progress. A successful native capture is then explicitly
   committed, and only after that does the card show a brief **Left hand
   calibrated** and close.
   Cancel, Circle or Start cancels the card. Crown, Window, GTA's pause menu,
   tracking loss and quitting also close or fail the card cleanly, and the
   previous setting is kept.
   If the Crown or Window comes before the commit, even right after a successful
   capture, the capture is discarded and nothing is saved or claimed.

## Native semantics

### Native hand qualifier

PostIkUpdate (38488) clears the per-arm request bytes every frame, and
original 38481 then blends request+92 toward zero. The calibration card relies
on this. The left hand counts as native at the post-IK producer when either:

- the blend is 0; or
- the request cookie is not our last left token, meaning a stock IK request now
  owns that slot.

### Capture

The capture is read at the post-IK cache stage, from the genuine local
first-person weapon producer:

`grip_from_hand = inverse(game_from_stage * stage_from_grip) * actual_native_hand_world`

It uses the live exact left packet and the actual hand bone in the same
skeleton reference.

Twelve fresh camera generations are averaged with a sign-aligned quaternion
mean. Every sample must stay within the existing pose-agreement bounds (3 cm
and 3°) of the first one. Any of the following restarts the still window:

- a change of tracking or view epoch;
- a change of rig;
- motion beyond those bounds;
- the left IK still blending out.

The capture is rejected if the measured offset is longer than the measured
native forearm.

Controller fingerprint during a capture:

- The capture is pinned to the controller fingerprint it started with.
- A fingerprint change restarts it: samples are never mixed.
- Losing the fingerprint (0) fails it.

The 4 s capture deadline is enforced on every state access, including the
card's heartbeat, not only when a post-IK observation arrives. So if the
observer stops (gun holstered, mode change), the capture still ends, with
`no_post_ik_observation_before_deadline`. Retrying needs a fresh native hand;
Cancel always works.

Commit and discard: a successful capture stays **pending** in the session.

- `commit` adopts it and ends the session.
- `close`, heartbeat expiry and every other exit discard it.

The app saves to UserDefaults only after a successful commit.

No anatomical axes, angle constants or arbitrary offsets are used: the
translation is the real measured offset.

### Validity checks

A calibration applies only while all of these match:

- **Controller fingerprint**: GameController product category, vendor name and
  fixed left hand. GameController exposes no persistent per-unit serial, so
  this identifies a controller **model**, not a physical unit. Two units of the
  same model share it, and a calibration applies to any left controller of that
  model. It measures how the player holds that model.
- **Rig**: bone count, converted left hand and auxiliary bones, and the live
  hand→auxiliary transform.
- **Artifact**: the running module is the left-wrist artifact.

Persisted values in UserDefaults are re-validated natively on import.

### Transport

- The target is G·grip_from_hand.
- The new layer passes **native flags 256** to 38506 for that lease only.
- Original 38548 maps flags 256 to ArmGoal+90 bit 0, and 3551 copies the target
  basis.
- The right arm (256), its gun, muzzle and shot authority, the 104/280-byte
  ABIs, third person and flat play are unchanged.

## Artifact

`src/xr_left_wrist_patch.rs` adds a separate generator, manifest
`muguet.motion-left-wrist.v1`, built on the preserved nine-body view-model
artifact. In the already-instrumented 38481 it rewrites only one instruction:
the left 38506 flags constant 0 becomes
`muguet_motion_arms_v1.left_request_flags(token, TLS) -> i32`. That import
returns the flags of the host's exact pending lease.

- The exact injected request sequence is source-pinned.
- Exactly one right site (256) and one left site (0) are required.
- One import is appended.
- All other bodies and native sections are preserved.

CLI: `--precompile-visionos-motion-arms-cache-viewmodel-left-wrist`. Older
artifacts lack the import, so the host keeps left requests position-only and
the Settings action stays hidden.

C ABI (in `RuntimeBridge.h`, structures of 56 and 128 bytes):

- `sv_xr_left_wrist_controller_v1`
- `_open_v1`, `_capture_v1`
- `_commit_v1`: adopts this session's successful capture, then ends the session
- `_close_v1`: ends the session and discards any uncommitted capture
- `_state_v1`, the card heartbeat; a session expires 1.5 s after polling stops
- `_export_v1`, `_import_v1`

## Evidence (CPU-only, private original read-only)

- **Exact original 38548, ordinals 278..=3457.** This runs from the 38487-TRUE
  branch through the arm-0 goal writes, with every call stubbed by type and
  none executed. With a calibrated full-pose target and blend 1, the goal pose
  in the world equals the target within 0.01° and 0.5 mm. Orientation bit 0 is
  set only for flags 256, and the goal becomes active.
- **New-layer test.** The real arms-instrumented 38481 fixture is executed with
  the new layer. Left 38506 receives exactly the host-returned flags for
  (token, TLS); right stays 256. Shape and index mismatches and double
  instrumentation are rejected, and Iterate is preserved.
- **Capture mathematics.** The captured relation is recovered within 0.05° and
  2 mm. The checks cover:
  - stillness, epoch and rig restarts;
  - blend-out waiting and the forearm bound;
  - a missing controller fingerprint, a fingerprint change restarting a
    capture, and loss failing one;
  - session-only acknowledgement, failure and timeout;
  - the capture deadline firing with no new observation;
  - a successful capture discarded by close or expiry, and adopted only by
    commit;
  - heartbeat expiry;
  - controller, rig and artifact mismatch falling back to position-only;
  - persistence round trip and corruption rejection;
  - native-hand qualification from memory.
- **Swift flow.** 44 checks covering:
  - the pending entry and the open gate;
  - Confirm only once native-ready;
  - success only after a native phase-4 result for this session is committed;
  - Crown or Window right after success, before and after the commit;
  - a refused commit;
  - failure and retry, and every exit path;
  - the model-scoped settings wording.
- **Regressions.** All Swift sources typecheck against the physical XROS27 SDK
  in both game and diagnostic configurations; the only warning is the existing
  `mac1` deprecation. Existing suites pass: xr_motion 46, xr_view 60, xr_aim 10,
  view model 9, xr_foreground 17, pad 35.

## Root actions

1. Run the full chain:
   `MUGUET_TRACE_ORIGINAL_WASM=<original> cargo test --lib exact_original_left_wrist_layer -- --ignored`.
   This is the full parser/re-encoder over the nine-body chain and is large.
2. Precompile the new artifact to a new path, then build the host with its
   `aot_sha256`.
3. Preserve candidates 22/21/20, the seven- and nine-body artifacts and the
   original checkpoint.

## Not yet accepted

These steps were not done and need hardware:

- Physical calibration, then visible left-wrist following. Roll, pitch and yaw
  should follow the controller, with the hand staying at the controller.
- Weapon swaps and character changes; with a different rig the calibration
  falls back.
- Native reach limits.
- The left IK blend in and out around the card.
- Confirming the stock left-hand IK behaviour while the card is open.

Still unfinished:

- unarmed or bare-hand tracking: the left arm still requires a qualified right
  weapon attachment;
- certified combined foreground depth;
- weapons without the ordinary gun_muzzle producer, which provides the
  post-IK capture point.

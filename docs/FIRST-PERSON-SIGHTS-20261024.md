# First-person sights with PS VR2 Sense — October 24, 2026

The user reports that aiming down sights fails in first person, both in the
flat window and in immersion, while third person is good. This change restores
GTA's own first-person sights toggle for Sense. Physical acceptance is still
required.

## Root cause in the original game

Function numbers are original `game.wasm` indices (seven-body index − 16).

- L2 already reaches GTA as the right mouse button (`INPUT_AIM`, control 25).
  `CPlayerInfo::IsSoftAiming` 41260 reads that control, and
  `CTaskGun::GetOverriddenGameplayCameraSettings` 76864 picks the first-person
  aim camera from it, exactly as in third person. Press, hold and release each
  produce one edge.
- First-person sights are a separate state. `CPlayerInfo::ProcessFPSState`
  41680 sets the aiming FPS state to 3 only when `CPlayerInfo+4664` value 2 is
  set; otherwise L2 gives state 2, the ordinary first-person aim. Only
  `INPUT_ACCURATE_AIM` (control 50) changes that flag, and only while aiming.
  A pad press toggles it. A keyboard/mouse source (device −4) is read as a
  signed, unbounded axis: negative turns sights on, positive turns them off.
  This is the mouse wheel. The flag persists across aims. A GTA Settings
  preference seeds it through `CPauseMenu::SetValueBasedOnPreference` 36174.
- GTA's pad layout puts this "zoom aim" on R3 together with `LOOK_BEHIND` in
  every FPS control type (`data/ui/controls.xml`, label group 17). Sense R3
  sent only C (`LOOK_BEHIND`), and the guest never receives a gamepad. Nothing
  Sense could do reached control 50, so first-person sights could not turn on.

Third person is unaffected. In third person, control 50 is only
`camControlHelper` 26310's optional accurate-aim zoom.

## Change

While L2 aim is already held, on foot, in first person (fresh script-tick
`GetFollowPedCamViewMode` 4), with no menu, phone, vehicle or utility/modal
ownership, a fresh Sense R3 press sends one mouse-wheel notch instead of C.
Releasing R3 is required before another notch can be sent. In every other case
R3 still sends C. DualSense and the window paths are unchanged. The scoped
sniper camera also keeps C, so its wheel stays sniper zoom.

- **Direction.** When the exact-ABI guest read is available, the notch toggles
  the actual native flag. The read follows `GetMainPlayerInfo` 47305:
  ped+4328, then +4664. Without that read, presses alternate.
- **Sign.** The guest's `CControlMgr::Update` 65444 swaps our wheel word and
  copies its sign into rage's mouse dZ (+1 is wheel up). Which wheel direction
  GTA's keyboard/mouse binding treats as negative is binding data, not code.
  The first sign tried is +1. If a full 300 ms aimed window leaves the native
  flag unchanged, the next press tries the other sign. The first observed
  native change locks the sign.
- **Aim hold.** The aim button stays down for 120 ms after a notch, so the
  wheel step is not read as weapon selection. `CWeaponWheel::Update` 35905
  suppresses wheel weapon selection only for the scoped aim camera.

Status `controller.first_person_sights` shows:

- aim button edges;
- routed R3 presses and up/down notches;
- the learned and locked sign;
- confirmed and unchanged native results;
- the last native flag and the view-mode freshness.

These values describe input delivery and GTA's own flag. They do not show hand
pose, weapon geometry or the rendered view.

## Not changed

In immersive first person, tracked arm targets can still replace the native
hand pose, including in the sights state. The headset projection also replaces
GTA's sights FOV, except for qualified scope optics. Visible immersive sights
geometry therefore depends on the tracked-arms and rendering work. GTA's
keyboard/mouse toggle-aim preference is left as the user set it.

## Checks and physical acceptance

`scripts/test-pad-routing.sh` runs the production pad modules with recording
sinks. 37 tests pass, including:

- aim press/hold/release for both profiles;
- one notch per R3 press, with C suppressed;
- the 120 ms aim hold;
- third-person, unknown-view and DualSense look-behind;
- no notch through pause, phone, vehicle, utility, modal or loss transitions;
- native-flag direction, sign learning and sign locking;
- the guest read of the flag and the scoped camera.

The `mp.rs` call needs the normal full build.

On the headset, start in flat first person with Sense:

1. Hold L2, then click R3. The view should go down the sights. Click R3 again
   to return to ordinary aim.
2. Check that `native_sights_changes_confirmed` increases and that
   `wheel_sign_locked_by_native_change` is true. If only the second press
   works, the sign was learned as −1. Root may then pin it.
3. Confirm that no weapon switches when L2 is released right after R3.
4. Confirm that third-person R3 is unchanged.
5. Repeat in immersion. There, judge the visible result separately from
   tracked-arm acceptance.

# GTA's own menus with PS VR2 Sense — October 24, 2026

Parent work `w-14dea40fb6e248138ad507f85cf4384b`. Source change only; physical
acceptance is still required.

## Root causes

1. **The Start panel owned the controller while GTA's menu was open.** Start
   sends Escape, which opens GTA's pause menu, and the same press toggled the
   app Start panel. Since `7b85d60` that panel set modal ownership (policy 1),
   so the Rust poller discarded every physical input until the panel closed,
   and closing it sent Resume. GTA's pause and Settings menus were therefore
   never controller-navigable in window or immersive mode, for Sense or
   DualSense. The October 24 copied runtime report shows exactly that state:
   `menu_context_source=verified_guest_pause_state`, `menu_navigation=true`,
   `native_controls_owns_input=true`, `native_controls_input_policy_v2=1`.
2. **Menu shoulder and Triangle keys had no GTA binding.** The guest only
   receives keyboard/mouse. GTA's fixed PC front-end bindings, read from the
   shipped `x64/data/control/fixed.meta`, are: accept Enter; cancel Backspace
   or Escape; pause Escape; X Space; Y Tab; LB/RB Q/E; LT/RT PageDown/PageUp;
   directions are the arrow keys. The mapper sent Tab for L1, N for R1 and Y for
   Triangle, so header-tab switching (Q/E) was unreachable and Y/N did nothing.

No mouse motion reaches the guest in menus on visionOS (`pad.rs` is the only
writer and looks only in gameplay), so GTA's front end stays in keyboard focus.

## Change

- Menu context maps L1/R1 to Q/E and Triangle to Tab. Cross, Circle, Square,
  Start and stick/D-pad directions are unchanged. Triggers still emit nothing in
  menus, so a Sense trigger can select native buttons without reaching GTA.
  Gameplay and phone bindings are unchanged for both controller profiles.
- Additive `sv_native_menu_context_v1()`: 0 unknown, 1 gameplay, 2 GTA menu,
  from the same exact-AOT guest getter that already routes menu keys. Swift
  reads it in the existing 50ms Start poll.
- `NativePauseCompanionPolicy` decides ownership while the panel is visible:

| GTA state | Panel | Controller |
| --- | --- | --- |
| Unverified guest | Start toggles, as before | Panel (modal) |
| Start pressed in gameplay | Opens | GTA, for a 2s pause-open grace |
| GTA menu open | Companion; closes when GTA's menu closes | GTA |
| GTA never opened a menu | Stays | Panel (modal), as before |
| Los Santos settings sheet | Sheet | Sheet (modal) |

  Start inside an open GTA menu is GTA's Escape and never toggles the panel.
- Beside GTA's menu the panel shows only app actions: Window (immersive),
  Los Santos settings and Quit game. GTA's own Back already resumes, so Resume
  is hidden, and nothing is highlighted. Its caption maps GTA's keyboard prompts
  to buttons: Enter ✕, Backspace ○, Space □, Tab △, Q/E L1/R1. No new buttons
  or overlays are added.
- A companion activation within 250ms of a physical Cross press, or while Cross
  is held, is attributed to GTA's menu and ignored by the panel.

## Validation

Narrow host checks only; no app launch, device, simulator or full build.

- `scripts/test-pad-routing.sh`: production `pad.rs` in a recording harness.
  New tests drive both DualSense and Sense samples through a verified GTA-menu
  context and check the emitted front-end keys, no gameplay leaks, unchanged
  gameplay bindings, the context ABI values and that a modal panel blocks the
  same input. Reverting only the menu keys fails both new tests.
- `scripts/test-native-controls.sh`: companion-policy transitions, actions and
  the Cross guard.
- All `visionos/Sources` typecheck against the physical XROS27 SDK.

## Hardware acceptance still required

In window and immersive mode, with Sense and with DualSense: Start opens GTA's
pause menu; L1/R1 change tabs; stick and Cross enter Settings; categories,
values, Circle back, Square apply and the confirmation all respond; Start or
Circle closes GTA's menu and the panel with it; Los Santos settings, Window and
Quit respond to look plus trigger or pinch; Cross pressed while looking at the
panel does not activate it; phone and Change view utilities work after resume.
Report fields `guest_menu_context` and `pause_controls_follow_game` should
read 2/true while GTA's menu is open.

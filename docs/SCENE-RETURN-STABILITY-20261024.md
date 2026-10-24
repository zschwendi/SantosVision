# Immersive Start and window restoration

The device build-21 report at 08:08:29Z records GTA's verified pause menu open,
our panel owning all controller input, lost qualified world completions and a
window recovery. The matching own-app crash is a main-thread SwiftUI
`AppSceneDelegate.makeSceneHostWindow` assertion. Disassembly of the matching
device framework identifies **Missing app graph.** It is not a game-renderer
stack failure.

A separate tiny scene-only app reproduced a main-thread assertion in that same
SwiftUI scene-creation function without game code or assets. Its old sequence
opened main, immediately dismissed immersion, then requested main again from
the disappearing compositor while also dismissing controls. This reproducer
qualifies a scene-lifecycle defect independently of any native graphics issue;
the complete device Start experience still needs physical acceptance.

## Change

- App scope owns open/dismiss window actions and one bounded return task.
- `GameWindowReturnPolicy` coalesces Start, watchdog and disappearance requests.
  It requires the real game view attached to an active window before closing
  immersion; queued `openWindow` alone is insufficient. An old ticket cannot
  close a new renderer. A failed attachment keeps immersion open rather than
  destroying the remaining presentation.
- A return remains accounted for when `onDisappear` arrives **after** the
  asynchronous dismiss call returns. That order occurred in the isolated test.
  Late disappearance no longer opens another window.
- The main window is the launch scene; the controls window explicitly suppresses
  launch/restoration. Unique `Window` scenes still prevent duplicates.
- GTA's own paused front end currently returns to the actual game window,
  preserving pause and giving the controller to GTA. It has no new qualified
  stereo world/depth pair to display. This is an explicit usable fallback,
  **not** a claim of a working immersive GTA front-end compositor. Close GTA's
  menu before expanding again. No stale image gets a new world timestamp and no
  freshness/depth check is weakened.
- The redundant View ornament is removed in both presentations. The physical
  camera shortcuts, Phone/Close-phone and phone-only directional pad remain.

The menu input repair is separate: see `SENSE-GTA-MENUS-20261024.md`.

## Evidence and limits

Private receipts: `target/verification/device-session-20261024/startup-black22/`.
The scene-only test uses its own simulator and bundle; no other project's
simulator or device app was changed. It proves scene sequencing, not actual
game output, performance, headset tracking or startup reliability.

The transition policy has 20 passing checks, including delayed disappearance,
background/quit gates, real attachment, timeout, duplicate requests and stale
tickets. All app sources typecheck against the physical visionOS SDK. Existing
native controls checks pass; the changed utility layout passes actual Metal
orientation/hit-area assertions. Full application build and physical acceptance
belong to candidate integration.

The fixed scene-only app completed three immersive/control/window round trips
with three return requests, three confirmed active attachments before dismissal,
no late duplicate and no timeout. The previous immediate-dismiss sequence
crashed on its first round trip. This is a bounded lifecycle regression check,
not a game or physical-headset test. The owned test simulator was shut down
afterward; other simulators were left alone.

Intermittent black **cold startup remains unresolved** until a new failing
startup has actual pixel/surface evidence. This repair addresses a reproduced
scene failure and makes launch-scene selection explicit; it is not proof that
every reported black image shares that cause.

## Original startup intro

Read-only inspection found the original engine's path
`platform:/movies/rockstar_logos`. None of the supplied fourteen game archive
parts contains that movie, any movies directory or a Bink movie file. The
launcher cannot restore the original audiovisual intro from this package alone.
No replacement Rockstar asset has been downloaded or bundled, and the game's
legal screens have not been patched out. The original movie from the user's own
copy is the next asset dependency for that separate request.

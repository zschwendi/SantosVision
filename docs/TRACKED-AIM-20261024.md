# Tracked aiming and visible arms — October 24, 2026

The user's current acceptance priority is a visible gun and arms that follow
PS VR2 Sense poses, with a reticle aligned to the actual barrel in both eyes.
Rendering screenshots and quality changes are deferred until this works.

## Confirmed build 20 evidence

The own-app runtime report at
`target/verification/device-session-20261024/tracked-aim21/feedback-runtime-report.json`
was generated at `2026-10-24T06:17:18Z`. It confirms the installed build 20
configuration, 2,616 accepted motion captures, 1,166 left and 1,133 right private
arm-target writes, and 1,226 accepted genuine cached-muzzle producer calls.
The report was written after leaving immersion; its historical counters do not
establish current tracking, final skeleton movement, visible gun movement, or
physical aiming acceptance. The user reports that those remain broken.

## Candidate 21 changes

- Revoke a lost controller independently. Preserve the healthy hand's original
  exact-head packet and prevent an in-flight ARKit query from restoring a
  revoked source. A provider update barrier is not itself tracking loss.
- Supply an additive, immutable 280-byte native aim/reference tuple. Both eyes
  use its common game-to-stage reference rather than each eye's older source
  reference. The existing 104-byte ABI remains available. The ray still comes
  from qualified native weapon geometry, never a fabricated screen-center dot.
- Retain bounded value-only observations at the existing target and genuine
  muzzle-cache hooks: desired hand, native goal and actual skeleton hand,
  original blend/activation/orientation state, request-cookie association and
  independently qualified drawable skeleton when available. These observations
  are evidence, not new pose or firing authority.

This candidate does not yet establish that the native solver's output reaches
the displayed first-person model. The existing first-person draw transforms can
cancel headset rotation; see the source certificate in
`target/verification/immersive-recording-20261013/viewmodel-transform-report.md`.
The left hand still uses tracked translation with the native wrist orientation.
Full independent left-wrist rotation remains unfinished.

## Required physical check

Use first-person immersion with Sense controllers. Keep the head still and move
each grip independently, then rotate the right wrist. Compare changing source
poses with the requested target, native goal and actual skeleton output from
the same request cookie. Do not count a private target write as visible success.
Next keep the right grip still and turn/lean the head: the gun and reticle must
remain mutually aligned in both eyes. Check a near wall and a distant surface;
the native ray is not an impact certificate and the display marker's chosen
distance must not be reported as a measured bullet impact.

If actual skeleton output follows while the displayed gun does not, repair the
qualified first-person draw transform and matching depth. If the native goal or
bone output does not follow, use the retained blend/reference/activation values
to repair that native stage first. Do not hide either failure by moving only the
reticle or writing the desired controller ray into the actual-barrel field.

Preserve build 20 and the original immersive checkpoint. No save/game-data
replacement, uninstall, simulator acceptance claim, or banked reset is needed.

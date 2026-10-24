# Los Santos tracked Sense candidate 13 — October 24, 2026

Parent work `w-14dea40fb6e248138ad507f85cf4384b`.

The app Start panel contains Resume, Window, Settings and Quit game. A fast
monotonic menu counter avoids the former one-second status-poll delay; actual
Button hit targets take priority over the controller-navigation fallback.

Phone, Change view and four D-pad directions occupy a compact lower-left
Sense gameplay strip, in both the game window and compositor world. Directions
support real holds, including Down for the native character wheel. Utility
ownership blocks ordinary gameplay actions while allowing right-stick wheel
selection only after the actual wheel-open input. Full modals retain complete
input ownership. Release, cancellation, pause and tracking loss release holds;
a gesture begun or interrupted while unavailable must end before it can rearm.

The separately compiled six-body AOT uses native arm requests and the game's
original IK/attachment path. Right-hand position/orientation and attached gun
follow the accepted Sense pose; the left hand follows grip position with the
native wrist orientation. This is not full independent left-wrist tracking.
The tracked reticle/fire input uses actual native weapon geometry after pose
agreement, preserving native firing calculations and corrections. An input
ray is not a certified impact. See `native-tracked-arms-20261023.md` for source
qualification and physical acceptance requirements.

Use the new exact AOT and matching native archive together. Original game AOT,
build 12 and accepted projection checkpoints remain unchanged. Current launch
uses `MUGUET_XR_MOTION_ARMS=1`, old motion aim off, and motion acquisition enabled.
Do not label this candidate physically accepted from compilation or host tests.

Evidence and deployment receipts:
`target/verification/device-session-20261024/tracked-sense13/`.
The next headset test is Start and each of its four actions; Phone/View and
held/cancelled directions; first-person right arm/gun/reticle near/far alignment,
independent head motion and wrist roll; left grip translation; tracking loss,
weapon changes, menu/phone/vehicle/third-person and flat-window fallbacks.

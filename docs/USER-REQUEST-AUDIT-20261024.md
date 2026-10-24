# User-request audit — October 24, 2026

This is an acceptance audit, not a list of completed code changes. The current
headset feedback supersedes earlier passing source tests and target counters.
The coordinator owns integration and the complete player experience. Parent:
`w-14dea40fb6e248138ad507f85cf4384b`, topic `santosvision-immersive`.

## Current blockers and unfinished requests

| Request | Current status | Owner / next acceptance |
| --- | --- | --- |
| Startup succeeds consistently instead of going black roughly two times out of three | **Unfixed.** User reports repeated quit/restart needed. | Coordinator: trace native engine, surface, scene ownership and actual presented frame state. |
| Start in immersive must not crash; our settings must remain usable | **Unfixed.** New physical crash reported. A matching own-app crash log exists at 02:08:30; retrieve and symbolize it. The preceding lifecycle also records lost qualified frames and automatic return. | Coordinator with menus specialist; distinguish the process crash from frame-loss recovery and repair both where necessary. |
| First-person ADS in flat and immersive | **Unfixed.** L2-to-RMB source mapping exists; native first-person aim/camera behavior is under review. | Dedicated Opus 5.5 `ads` task; test both first-person views. |
| Sense can navigate GTA Settings and controls in every view | **Unfixed.** Our own overlay works but GTA's front end does not. | Dedicated Opus 5.5 `menus` task; actual directions, tabs, confirm/cancel and Apply/save. |
| Actual tracked gun, hands and arms | **Unfixed.** Actual Sense source poses move, but private target writes are not visible arm acceptance. | Dedicated Opus 5.5 `hands` task; actual skeleton, attachments and first-person draw transform must agree. |
| Reticle always agrees with visible barrel and actual native aiming | **Incomplete.** Candidate 21 repairs an independent binocular-reference bug; it does not establish visible gun alignment. | Hands/ADS integration; near/far and independent head/hand movement. No fabricated center dot or desired-ray authority. |
| Independent left wrist rotation | **Unimplemented at candidate 21.** Left hand currently has translated grip with native wrist orientation. | Hands task; proper calibrated frame and native orientation transport. |
| First-person immersive sniper is single and correctly zoomed | **Unfixed.** User still reports double vision. Earlier synthetic collimation check is insufficient. | Dedicated Opus 5.5 `sniper` task. |
| Crisp immersive graphics closer to the flat presentation | **Unresolved.** Clarity profile and 1440p are applied, but the user is dissatisfied. | Dedicated Opus 5.5 `quality` task; source/display evidence and explicit controls, no unsupported performance claims. |
| Cannot outrun the immersive viewport into black edges | **Unresolved.** Wider adaptive source coverage exists; user-visible residuals remain. | Quality/renderer work after core aiming and stability. |
| Residual distant black sky squares | **Unresolved.** Earlier near holes improved; no complete physical acceptance. | Renderer follow-up; do not silently reopen accepted third-person camera behavior. |
| Weapon wheel and Wasted are not duplicated; HUD stays visible | **Not fully accepted.** Prior reticle improvement does not prove all HUD elements. | Explicit UI/stereo checks; minimap lower without moving farther left, right HUD lower and inward. |
| Phone/view utilities are minimal and out of the way | **Needs revision.** User now dislikes them and points out the duplicate camera shortcut. | Coordinator: trim redundant camera ornament; settle minimal phone access without reintroducing the old window phone trap. Preserve the physical camera button. |
| Phone arrow pad only appears with phone, as an upright directional ornament; phone button closes phone | **Partial.** User previously accepted window behavior; immersive phone/open-transition failures remain. | Controls/lifecycle regression; preserve the working window path. |
| Front of app feels concise and deliberate | **Needs revision.** User asks to trim front-end clutter again. | Coordinator after startup/menu crash repair; no replacement wall of diagnostic controls. |
| Original police-siren/Rockstar startup presentation instead of the current legal-only experience | **Not implemented.** Check the supplied game copy's own intro resources and movie/start flow first. | Coordinator; use the user's installed game assets, without embedding new proprietary assets in the app. |
| Reliable iCloud save sync with redundant recovery | **Source exists; full save/restore acceptance remains unverified.** | Inspect existing CloudSaveModel/recovery evidence and test actual save, restore and overwrite recovery. |
| Vision Pro on the app icon instead of sunglasses | **Still pending.** The shipped custom FIVE sunglasses asset does not satisfy the later icon revision. | Asset follow-up after playability blockers. |

## Behavior explicitly worth preserving

- User is happy with immersive third person. Preserve its camera/aiming behavior;
  it is not a proxy test for first-person flat or first-person immersive.
- Flat gameplay and the separate game window have been reported working. The
  launcher hides and duplicate windows are constrained.
- DualSense touchpad camera cycling was explicitly confirmed working earlier.
- The working windowed phone controls must survive utility cleanup.
- Los Santos naming, normal sound, 75 percent look sensitivity and the developer
  mode control already have source implementations. Verify their current saved
  state when touching startup; do not assume they solve any blocker above.

## Completion rule

Each row needs a concrete change, appropriate source checks and the stated
physical/player acceptance. A successful build, valid controller pose, private
target counter, diagnostic marker or source certificate closes only that narrow
check. Do not report the complete request as fixed while the user's visible
behavior remains broken. Keep this table current at integration and deployment.

Existing specialist checkouts and launcher receipts are documented in
`FIRST-PERSON-REPAIR-20261024.md`. Preserve candidate 21, candidate 20 and the
original committed immersive checkpoint for rollback.

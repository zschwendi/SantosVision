# User-request audit — October 24, 2026

This is an acceptance audit, not a list of completed code changes. The current
headset feedback supersedes earlier passing source tests and target counters.
The coordinator owns integration and the complete player experience. Parent:
`w-14dea40fb6e248138ad507f85cf4384b`, topic `santosvision-immersive`.

Build **24** is installed in place and the installed bundle version was verified.
Client source is `04456ad`, tag `los-santos-phone-ui-candidate24-20261024`.
It links the frozen 23-r2 native archive from `889bfe7`, with its exact AOT.
The signed client build, strict signature, physical provisioning, cloud
entitlements, complete AOT embedding and RX segment checks passed. No uninstall,
save replacement or foreground launch occurred. Details and next test:
[build 24 notes](PHONE-CONTROLS24-20261024.md). Deployment receipt:
`target/verification/device-session-20261024/phone-removal24/candidate-receipt.json`.

The [23-r2 source notes](FIRST-PERSON-CANDIDATE23-20261024.md) remain the native
baseline. Foreground merge/capture remain off for normal launch. Signed 22,
23-r2 and the original build-5 checkpoint are preserved. The last tested
build-22 experience failed visible hand/arm acceptance; installing 24 does not
close that failure.

The user confirmed **Sense-driven hands and arms first**. Visible hands, arms
and gun must follow the controllers, with barrel, reticle and native shots in
agreement during independent head movement. Natural bare-hand interaction is
deferred; unarmed Sense-driven avatar hands remain within the controller scope.

Latest player feedback: **visible hand/arm tracking does not work**. Treat this
as failed acceptance, not a completed implementation awaiting confirmation.
The normal immersive publication path still lacks qualified foreground depth
for the corrected first-person gun/arms; the separate candidate's conditional
foreground merge remains off until actual pass ordering is captured and handled.
The user also withdrew the immersive Phone button: phone utilities now belong
only to the windowed presentation, reached through the existing pause/window flow.

## Current blockers and unfinished requests

| Request | Current status | Owner / next acceptance |
| --- | --- | --- |
| Startup succeeds consistently instead of going black roughly two times out of three | **Unfixed.** User reports repeated quit/restart needed. | Coordinator: trace native engine, surface, scene ownership and actual presented frame state. |
| Start in immersive must not crash; our settings must remain usable | **Source repair integrated; device acceptance pending.** Matching device crash is SwiftUI Missing app graph. The old sequence also crashes an isolated scene-only app; serialized restoration passes three cycles. GTA pause currently uses an explicit window fallback, not an immersive front-end compositor. | Coordinator with menus specialist; see SCENE-RETURN-STABILITY-20261024.md. Test Start, GTA tabs/Apply/save and return to immersive after closing GTA menu. |
| First-person ADS in flat and immersive | **Source repair integrated; visible ADS unaccepted.** Sense L2 + R3 now reaches GTA's separate native sights toggle in both first-person views. Original L2-to-RMB mapping was already correct. Tracked-arm/sights interaction and wheel sign need a device check. | Dedicated Opus 5.5 `ads` task; test both first-person views. |
| Sense can navigate GTA Settings and controls in every view | **Source repair integrated; hardware check pending.** GTA keeps the controller while its menu is open; L1/R1/Triangle send its actual Q/E/Tab bindings. Combined menu/ADS routing passes 39 tests. | Dedicated Opus 5.5 `menus` task; actual directions, tabs, confirm/cancel and Apply/save. |
| Actual tracked gun, hands and arms | **Player acceptance failed; unfinished.** Installed 22 corrects head/eye cancellation in the two skinned model families, but its ordinary immersive publication lacks a qualified foreground-depth route. Signed 23-r2 adds calibrated left-wrist orientation and conditional actual foreground-depth integration, disabled by default. Five Metal/classifier and sixteen foreground CPU checks pass; actual game draw-order evidence is still needed. Unarmed Sense-driven hands remain unfinished. | Dedicated Opus 5.5 `hands` task; actual skeleton, attachments and first-person draw transform must agree. |
| Reticle always agrees with visible barrel and actual native aiming | **Incomplete.** Candidate 21 repairs an independent binocular-reference bug; it does not establish visible gun alignment. | Hands/ADS integration; near/far and independent head/hand movement. No fabricated center dot or desired-ray authority. |
| Independent left wrist rotation | **Carried into installed 24; not physically accepted.** Explicit grip-to-native-hand calibration transports native flags256. Full artifact preservation, AOT generation and signed-app integration passed; 11 native state tests, 2 factory tests and 46 UI-flow checks pass after independent fixes. Uncalibrated play keeps native wrist orientation. | Hands task; proper calibrated frame and native orientation transport. |
| First-person head looking, body turning and weapon aiming can be separated or chosen | **Partially implemented, choice unfinished.** Sense weapon aiming and headset view have separate source paths. There is no normal Settings choice for the requested first-person head-check/body-turn experiment. Preserve accepted third-person behavior. | Hands/aiming follow-up after actual barrel alignment is accepted. |
| First-person immersive sniper is single and correctly zoomed | **Partial source repair; hardware pending.** Qualified scoped world now shares the graticule axis and uses a 1/m virtual baseline. Baked HUD fallback can still double when real HUD isolation is rejected; new counters identify it. | Dedicated Opus 5.5 `sniper` task. |
| Crisp immersive graphics closer to the flat presentation | **Unaccepted.** New default-off display-resolution choice and experimental gap fill; Mac Metal parity passes for the fill. Source vertical density and alternating-eye limits remain. | Dedicated Opus 5.5 `quality` task; source/display evidence and explicit controls, no unsupported performance claims. |
| Cannot outrun the immersive viewport into black edges | **Unresolved.** Wider adaptive source coverage exists; user-visible residuals remain. | Quality/renderer work after core aiming and stability. |
| Residual distant black sky squares | **Unresolved.** Earlier near holes improved; no complete physical acceptance. | Renderer follow-up; do not silently reopen accepted third-person camera behavior. |
| Near geometry fades instead of fighting the player's body | **Source exists; physical comfort not accepted.** Current fade range is 5–10 cm. This does not establish that all first-person arms, sights or nearby walls behave correctly. | Check alongside actual tracked arms and ADS; do not restore the old broad 15–45 cm fade. |
| Stronger or more convincing stereo | **Unaccepted.** Use calibrated physical eye transforms; alternate-eye timing and foreground/HUD errors remain. No artificial IPD-strength setting has been added. | Accept coherent aiming/scope geometry before experimenting with perceived depth strength. |
| Weapon wheel and Wasted are not duplicated; HUD stays visible | **Not fully accepted.** Prior reticle improvement does not prove all HUD elements. | Explicit UI/stereo checks; minimap lower without moving farther left, right HUD lower and inward. |
| Phone/view utilities are minimal and out of the way | **Installed 24 removes immersive Phone utilities entirely**, including their tracking hit areas. Phone/Close-phone and phone-only directions remain in the game window; View ornament was already removed in both presentations. Physical camera shortcuts remain. | Verify the requested removal and preserve working windowed phone access. |
| Phone arrow pad only appears with phone, as an upright directional ornament; phone button closes phone | **Partial.** User previously accepted window behavior; immersive phone/open-transition failures remain. | Controls/lifecycle regression; preserve the working window path. |
| Front of app feels concise and deliberate | **Trimmed in installed 24; player check pending.** Removed duplicate title/status and generic launcher decoration. Play, Settings, Quit and controller status remain. Footer includes the actual bundle build number. | Coordinator; no replacement wall of diagnostic controls. |
| Original police-siren/Rockstar startup presentation instead of the current legal-only experience | **Missing asset confirmed.** The engine references platform:/movies/rockstar_logos, but none of the fourteen supplied archives contains the movie. Requires the original intro from the user's own copy; no new Rockstar asset bundled. | Coordinator; use the user's installed game assets, without embedding new proprietary assets in the app. |
| Reliable iCloud save sync with redundant recovery | **Configured in signed build 22; story save/restore remains unverified.** CloudDocuments container/entitlements are present. Last received play configuration is Sandbox/developer mode ON; it does not establish a story-save round trip. | Inspect existing CloudSaveModel/recovery evidence and test actual save, restore and overwrite recovery. |
| Vision Pro on the app icon instead of sunglasses | **Implemented and visually checked.** Asset catalog selects santos-five-visor.png, introduced by 5baebf1 in build 12 and retained in signed build 22. The prior audit entry was stale. | Keep the selected visor asset; APP-ICON.md provenance corrected. |

## Behavior explicitly worth preserving

- User is happy with immersive third person. Preserve its camera/aiming behavior;
  it is not a proxy test for first-person flat or first-person immersive.
- Flat gameplay and the separate game window have been reported working. The
  launcher hides and duplicate windows are constrained.
- DualSense touchpad camera cycling was explicitly confirmed working earlier.
- The working windowed phone controls must survive utility cleanup.
- Last received device configuration (build 21, 08:16:09Z): master volume
  100 percent, look sensitivity 75 percent, developer mode ON / Sandbox,
  Clarity 2560×1440 and a 60 FPS cap. Build 22 preserves those saved choices.
  Turn developer mode off before Play for an actual story-save test; the
  developer toggle exists near the bottom of Settings.

## Completion rule

Each row needs a concrete change, appropriate source checks and the stated
physical/player acceptance. A successful build, valid controller pose, private
target counter, diagnostic marker or source certificate closes only that narrow
check. Do not report the complete request as fixed while the user's visible
behavior remains broken. Keep this table current at integration and deployment.

Existing specialist checkouts and launcher receipts are documented in
`FIRST-PERSON-REPAIR-20261024.md`. Preserve candidate 21, candidate 20 and the
original committed immersive checkpoint for rollback.

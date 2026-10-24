# Native motion aim diagnostic

This is a separate AOT experiment. The ordinary patch5 path, original game AOT,
and normal controller/window/third-person behavior are preserved. No physical
motion-aim acceptance, impact correctness, or complete six-degree-of-freedom
weapon attachment is claimed.

## Implemented native path

`xr_motion_patch::generate` accepts only original WASM SHA256
`11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`.
It verifies exact names, ABIs and SHA256 bodies for the native local-ped getter,
first-person predicate, and four modified functions:

| Original function | Intervention |
| --- | --- |
| 76735 PostCameraUpdate | Redirect its selected frame before the first read. |
| 82184 CalcFireVecFromAimCamera | Redirect both initial and dominant frame alternatives. |
| 76770 CalculateFiringVector | Save incoming identities/output addresses and observe original successful output after all native corrections; preserve its result unchanged. |
| 76778 FireGun | Synchronous allow-fire at entry before original allocation/writes, and after its calculated-shot call before the existing failure branch. |

The two frame helpers originally own 16 bytes on their per-instance guest stack.
Their exact allocation/restoration constants become 96, preserving those original
16 bytes and using the extra 80 bytes for a private camFrame. Original shared
camera bytes and calculated shot vectors are never overwritten. No pointer to
that private frame escapes its native invocation. Recovered guest errors discard
child calculation scope and firing authorization before stack restoration.

The live local ped is exactly the original 41056 recipe: load64 at 20159720,
then load64 at factory+8. The native first-person query is original 45913 with
arguments `(ped,0,0,0,1,0)`, matching the existing gun-IK branch and avoiding its
reset-list mutation branch. False results also pass through the observer so third
person restores native control without a cached UI-mode guess.

The on-foot task identity is source qualified: original factory 76779 stores
vptr 5968184 at 0x0261c576. Its virtual slot+240 maps through table slot76420 to
76818 PostCamera; slot+760 maps through table slot65423 to 76770 calculated shot.
76778 calls the latter at original 0x0261b5e6 (body-relative642). Original task
local0 and ped local1 are not overwritten before that call. The late gate keeps
the calculated function's original result/output and chooses whether FireGun
continues through its existing success or failure branch.

The source proofs are in `target/verification/device-session-20261020/`:
`head-aim-gun-task-vtable-20261020.txt`,
`head-aim-onfoot-postcamera-fire-bodies-20261020.txt`, and
`head-aim-native-frame-fire-helpers-20261020.txt`.
The new bounded local-ped/first-person bodies are under
`target/verification/device-session-20261023/motion-aim/`.

## Shared generation and loss handling

After qualified primary-camera setters and head-pose commit, camera_post requests
the exact accepted head frame from the Swift immutable pose ring. It retains the
right accessory aim matrix, original source age, tracking epoch, sample sequence,
original game-camera matrix, and game-from-stage transform together. Bare wrist
tracking does not qualify as a calibrated weapon aim matrix.

Each native read requires the current selected native camFrame to equal that
accepted original camera after the certified axis permutation/sign conversion.
It requires the same Store/pthread/TLS identity and live local player. One
immutable hand frame is reused across IK and both calculated-shot alternatives.
New camera generations cannot silently replace a calculation's initial lease.

Before use, the exact same Swift head-frame packet and IDs are revalidated;
tracking loss, session change, hotplug and ring invalidation therefore fail
closed. Original anchor age plus elapsed time may not exceed150ms; capture does
not restart that interval. A late native dominant-frame mismatch replaces all
initial origin/forward values in the original helper before downstream math,
and its motion qualification is cleared. Native fallback output remains observable
as native output, but synchronous FireGun gating prevents firing it in motion mode.

For spatial controllers in immersive on-foot first person only, loss requires
right-trigger release before rearming. Standard pads, windows, menus, vehicles
and actual native third person retain their existing routing. Unknown motion
context does not authorize firing. The native entry/late gates close the interval
between controller polling, tracking loss and the native shot consumer.

Status labels the last successful native calculation as historical and provides
its age; it is neither a current aim sample nor an impact observation.

## Build and verify

1. Build the host with the repo-local toolchain and ordinary locked dependencies.
2. Run `target/debug/muguet --precompile-visionos-motion ORIGINAL_WASM NEW_CWASM`.
   The output must not already exist. This path never replaces the ordinary AOT.
3. Read `NEW_CWASM.motion.json`. Build the native archive with
   `MUGUET_GAME_CODE=NEW_CWASM` and `MUGUET_MOTION_AOT_SHA256` equal to its exact
   `aot_sha256`. Use the same five-GiB compiler/runtime heap configuration.
4. The embedded bytes are hashed before accepting the motion imports. The exact
   source-derived diagnostic can additionally use the existing source-qualified
   pause/input field layouts; other AOTs retain the original hash rejection.
5. Enable the separately opt-in Swift motion provider when launching this
   diagnostic. A normal AOT never enables gameplay motion from a preview flag.
   Gameplay additionally requires launch environment `MUGUET_XR_MOTION_AIM=1`.
   A matching diagnostic AOT without that flag passes original frame pointers,
   reports no calculation scope, allows native firing and leaves triggers alone.
   Status reports AOT certification and gameplay activation separately.

Generation validates the entire output module, every untouched function against
normal function-index remapping, original type/import/function relationships,
all preserved data/global/table/element sections, and original exports. Executable
tiny shared-memory64 fixtures additionally instantiate the rewritten functions,
verify stack restoration and native corrections, and simulate tracking loss
between calculation and actual firing. Tests do not instantiate the whole game.

## Physical acceptance still required

76735 sends a hand-derived direction target through the original gun IK. Its
torso solver reduces that request to native yaw/pitch; it does not retain
independent grip translation or roll. Full tracked arm/weapon attachment requires
the separately qualified ArmGoal/bone consumer and native attachment transforms.
Identity accessory-aim calibration is an initial diagnostic convention, not a
measured model-to-controller/muzzle calibration. Reticle/impact, assisted/native
alternate aim paths, recoil, near-wall behavior, loss/reconnect, camera changes,
and natural body/weapon alignment all need actual headset evidence.

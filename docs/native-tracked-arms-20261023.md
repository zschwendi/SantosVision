# Native tracked arms candidate — October 23, 2026

Worker `w-3535bdd91b0e43818fc8d0007894b2cd`; parent
`w-14dea40fb6e248138ad507f85cf4384b`. Physical candidate integration and acceptance
remain with the parent. This source change does not install or sign anything.

## Implemented behavior

`MUGUET_XR_MOTION_ARMS=1`, together with the matching separately compiled AOT
certificate, activates a native right arm target and a native left arm position
target. This launch gate is independent of the older motion aim experiment.

The right hand origin follows the exact accepted Sense **grip** position. Its
orientation accounts for the current native hand-to-attachment-bone transform
`K` and retained weapon attachment `A`. The desired native weapon basis maps its
source-qualified +X firing direction to accessory aim -Z. The +Z-to-aim-Y choice
is an explicit upright/roll policy, requiring physical wrist checks. The
resulting weapon translation is the tracked grip plus the rotated live `K*A`
offset; the controller aim origin is not substituted for the hand origin.

The left hand follows the accepted left Sense grip position. Native flags0 and
the original target basis preserve the game's wrist orientation policy. There
is no left weapon or assumed right-hand calibration on this arm. Independent
left wrist rotation remains a separate physical/calibration follow-up.

Both targets enter original38481 after stock automatic requests and before
blend/activation. Original38506 creates/registers the solver normally. Right
flags256 retain native orientation transport; left flags0 retain normal wrist
policy. In original38548, the hook changes only the invocation's private target
matrix after successful original38487, before the native reference conversion,
offsets, blending and goal construction. Native CanUpdate, clip gates, arm
reach/bend constraints, original3551 and descendant attachment updates remain.
No shared camera, final arm goal, bone bank or physical weapon matrix is written
by the host bridge.

## Qualification and firing

The bridge validates the current local ped, initialized manager and actual
solver/skeleton identities, converted native hand/auxiliary bone indices,
descendant bounds, the actual hierarchy, local/global bank consistency, native
weapon/physical/attachment identities and current epoch. Retained attachment
quaternions use the executed native3159 xyzw convention. Cached muzzle flag4,
alternate attachment orientation bit0x8000 and other stable unsupported native
branches fall through to original native behavior. Changing pose banks,
missing/stale tracking or packet disagreement do not authorize firing fallback.

Requests carry a finite, non-recycled token in Vector3 padding, certified through
the complete original38487 absolute target path for flags0 and256. Leases retain
only values and exact camera/tracking/hand generation, never guest addresses.
Original38506 must return before a lease commits. Each Store journals requests
and applied targets until the outermost guest boundary succeeds; error, trap,
longjmp or panic cleanup revokes child values before guest recovery. A successful
nested call remains subject to a later outer failure.

Firing and the tracked reticle sample the **actual** current native weapon
matrix, re-deriving and double-checking its live address chain. They wait for
agreement with the requested pose within 3 cm and 3 degrees. FireInitial freezes
that actual frame for its own calculation; FireDominant reuses that frame even
across nested calculations. The original native calculation and corrections
still produce origin/target. The existing late native FireGun gate rejects a
missing or mismatched motion acknowledgement before firing effects. A current
input ray is never labeled an impact.

Status exposes `left_position_active`, its native orientation policy, actual
ray readiness/pose agreement, and unsupported-weapon native fallback.

## Evidence

Local evidence directory:
`target/verification/device-session-20261023/motion-native-arms/`.

Final source-qualified both-arm WASM:

- `six-body-both-arms.wasm`, 63,223,024 bytes.
- SHA256 `6f560f3001523f26b7c545c034e62100c378dd625ff5d0dee17ced8c6346ce06`.
- Its adjacent JSON records the exact six changed original bodies:
  76735, 82184, 76770, 76778, 38481 and 38548.
- Exact original parser/reencoder test passed in 121.64 seconds. It verifies
  every untouched function, original import/type relations, and non-code
  sections including data, globals, tables, elements and exports. Regenerating
  the preserved four-body artifact before/after gives identical bytes.

Focused verification passed: 19 motion tests, five reticle tests, and 12 guest
call cleanup tests. The executable memory64/shared-memory reencoder fixture
checks stock-before-request-before-blend order, both native arm request enums,
original parameters despite local reuse, balanced private stack allocation,
TRUE-only target substitution, native post-target correction and false fallback.
Host fixtures check actual K/A identities, alternate/cached branches, pose
agreement, padding/metadata preservation, left basis preservation, nested frozen
shot frames and committed/applied-token rollback after an outer failure.

Additional executed original-source certificates are in
`motion-weapon-live-contract/` (native3159, original3551 child propagation,
native82185 +X and complete original38487 flags0/256 cookie proof),
`motion-aim/numeric-arm-*` (literal native inverse and attachment composition),
and `motion-weapon-coupling/pre-ik-seam/` (request/IK dependency order).

## Separate AOT and remaining acceptance

Build the updated Mac compiler, then use
`--precompile-visionos-motion-arms <original.wasm> <new-separate.cwasm>`.
The output must be a new path. Its manifest gives the AOT SHA required by
`MUGUET_MOTION_AOT_SHA256` in the host build. Keep the original AOT and the
four-body diagnostic unchanged. This worker generated the held WASM and source
factory; the parent owns final AOT compilation and the physical candidate.

Required physical checks remain: right translation/roll and gun axis; near/far
ray agreement; independent head motion; left grip translation/native wrist;
native reach limits; weapon changes/unsupported branches; release after loss;
menus, phone, vehicle, third person and flat controls. The 3 cm/3 degree readiness
threshold may need physical tuning. No physical acceptance is claimed here.

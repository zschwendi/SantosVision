# Exact native arm goal observations

Parent `w-14dea40fb6e248138ad507f85cf4384b`; implementation
`w-1414aa86e8604a45872eb20698247d64`. This is read-only diagnostic progress;
visible Sense-driven arms, gun alignment and firing accuracy remain unaccepted.

The separately requested arm artifact now observes original38548 immediately
after `0x01609911`, the native goal-active store (body-relative `0x4f8c`),
before the arm loop continues. The original goal vectors, orientation flags,
blend and active state are already constructed. This is before Solve and does
not certify the solved hand, arm palette or displayed pixels. The factory pins
the original body hash and checks the exact opcode at both the target and
completed-goal seams.

The existing nine-argument `muguet_motion_arms_v1.target` import signature is
unchanged. Ordinary arm0/1 calls retain their existing private-input behavior;
bit31 in the arm argument explicitly selects the read-only completed-goal
phase. The manifest describes this protocol extension. Old artifacts remain
compatible with the updated host. Collecting the new observation requires a
new separately compiled artifact and its matching AOT certificate; root owns
that build and physical integration.

A private WASM marker resets before every native CalculateTargetPosition call,
is set only after the ordinary target hook, and is consumed before the new
callback. Original local28 supplies arm identity because local40 has become
orientation flags. The native FALSE-target branch runs unchanged and cannot
borrow an earlier arm/invocation's marker. Each Store retains at most two
pending observations; normal target attempts clear the corresponding entry,
post-goal consumes it once even on rejection, and outer completion/recovery
clears pending state.

The callback requires the exact applied cookie, request XYZ/flags, generation,
tracking epoch, invocation stack/TLS, local owner/manager/solver, passed and
resolved skeletons, goal/request identities and unchanged reference. Reads must
remain coherent and within the existing150ms bound. This branch never calls
guest functions, writes native memory, publishes arm input, changes firing
authority or changes fallback/graphics defaults.

Runtime status is under
`motion_aim.native_arms.geometry_observations.{left,right}.post_native_goal`.
It retains bounded call/accept counts, last gate, and one historical sample:
age and target-to-goal elapsed time; applied target token/generation; completed
reference/world matrices; translation delta; native flags/active/blend; and
exact diagnostic identities. The existing later cache trace remains separate
and keeps its own cookie relationship.

`last_sample.first_person_candidate` optionally reads `ped+4256`, a possible
first-person arm palette skeleton. It reports skeleton/bank/count identities
and a hand pose only when metadata and bone count/range agree with the manager
skeleton. It also compares the manager hand, desired target and completed goal.
These optional reads have their own coherence check: missing/invalid candidate
data does not reject the main sample. `draw_selection` explicitly remains
`unknown`; pointer presence or inequality alone does not establish actual draw
use or a disconnected rig.

Focused fixtures verify exact ownership rejection, stale cookies/generation,
changed reads, no guest writes, optional-rig failure, consume-once cleanup,
both-arm TRUE/FALSE/repeated invocation pairing, local40 reuse and callback
ordering. The executable stand-in compares every guest-memory byte and restored
stack against its uninstrumented native output. Exact full-original factory
preservation, AOT generation and physical capture remain root-owned checks.

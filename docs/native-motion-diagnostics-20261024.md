# Candidate 14 hand tracking diagnosis

The reports available at 03:40 UTC do not isolate an active tracking failure.
`sense-ornaments14/initial-runtime-report.json` was captured at startup: the
providers were initialized, no motion packet had been published, and the native
camera had applied no headset overrides. `warm-runtime-report.json` and
`sense-input15/feedback-runtime-report.json` were captured after returning to the
window, when camera/arm inputs had been invalidated normally. Their cached last
renderer report records both Sense pose flags (3), two running authorized
accessories, and native head frame 996. The native camera had reached first
person (mode 4), with 859 headset overrides. This proves prior pose publication
and first-person camera use, not successful native IK or current tracking.

The host now reports bounded process-lifetime diagnostics without changing any
pose acceptance, native fallback, firing policy, or native AOT hooks:

- `motion_aim.capture_diagnostics`: attempts, accepted count, invalidations,
  last capture decision and last headset-applied decision. The latter survives
  later window cameras. Each observation includes the accepted head generation,
  returned motion packet flags/IDs, original right-source age, and presentation
  timestamp difference. Missing exact packets are distinct from packets lacking
  a tracked right accessory, bare wrists, stale source, and invalid transforms.
- `motion_aim.scope_diagnostics`: current world/spatial/gameplay/vehicle gates,
  native first-person mode age, IK permit age, and last native candidate gate.
- `motion_aim.native_arms.hook_diagnostics`: per-arm hook/local-ped hook counts,
  requests, committed native requests, applied private native targets, and last
  local request/target stage including the native first-person argument.
- Candidate and right input generations/ages make a camera-generation mismatch
  visible. Historical counters indicate callback activity, not rendered success.

These records retain values and stage names only. Leaving immersion still clears
all actionable candidates, leases, inputs and permits. A historical successful
observation cannot authorize motion or extend a source timestamp. The original
3 cm / 3 degree actual-weapon agreement requirement is unchanged. Original AOT,
candidate 13 rollback, all six patched guest bodies, and provider code remain
unchanged.

Focused Mac host tests pass: `xr_motion_aim::tests` (11) including classification
and return-to-window retention, and `xr_motion_arms::tests` (8). Full AOT
preservation fixtures were not repeated for this host-only observation change.
The next active first-person report must identify the earliest failed gate before
selecting a behavior fix. Parent work: `w-14dea40fb6e248138ad507f85cf4384b`;
bounded diagnosis: `w-d4257e6867c44a12a6a20c9bda091884`.

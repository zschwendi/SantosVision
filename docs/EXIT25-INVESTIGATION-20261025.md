# Exit after the build25 Sense test

The user reports that Los Santos might have crashed. The cause is **unconfirmed**.
The last own-app report is `2026-10-25T00:27:45Z`: immersive, running, return code0,
no last error and successfully serialized native-goal observations. Later own
process enumeration found no SantosVision process. Subsequent report copies
returned the same historical file; they do not establish ongoing health.

A device listing of matching SantosVision crash files found none near the exit.
The later own-crash refresh timed out. The current Jetsam listing succeeded,
but its newest entry was `2026-10-24T01:35:11Z`, before this test. No unrelated
crash contents were copied. Missing records do not rule out a crash, memory
pressure, background termination or an intentional close.

Independent bounded source review found no concrete new observer panic, guest
write, unbounded retention or lock cycle. The41KB status payload fit the64KB
buffer. Existing status FFI lacks local panic containment, but no new reachable
panic trigger was established. These findings do not identify the exit cause.

Build26 remains installed and its signed frozen app is preserved. An attempted
console launch received no confirmation and no bytes. The coordinator stopped
only its verified owned host devicectl observer using an uncatchable host signal;
no remote app termination was requested. The observer's recorded exit code-9 is
**not** an app crash or app exit signal. A build26 relaunch was not confirmed.

Artifacts: `target/verification/device-session-20261024/exit25-20261025/`, including
`observer-exit-audit.md`, filtered crash/Jetsam metadata, console recipe/result
and the explicit host-observer cancellation receipt. No app code, settings,
saves or graphics defaults were changed for this investigation.

Next step: when the headset is worn/unlocked and reachable, retry the ordinary
build26 launch with bounded console capture, then its immersive first-person
Sense test. Preserve build25/26 rollback artifacts. The readiness question is
pending; do not infer that the headset is ready from elapsed time.

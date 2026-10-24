# First-person repair scopes — October 24, 2026

The user clarified that immersive third person is satisfactory. Preserve its
camera, aiming behavior and presentation. First-person flat and immersive need
separate acceptance; a working third-person test does not close either.

Five independent Claude Opus 5.5 sessions own the five issues below. The local
Claude launcher reported the actual canonical model `claude-opus-5-5`; each
started session must retain that identity in its initialization record. Effort
is xhigh and fast mode is disabled. Implementation checkouts start at
`d0742fc4d2dac08c77fda74d80bd1a82d0c88fc8`. The coordinator reviews and integrates
the resulting commits, resolves overlapping input edits and owns final builds
and physical acceptance.

| Owner | Issue | Required acceptance |
| --- | --- | --- |
| `ads` | Aim down sights in both first-person views | Sense aim press/hold/release reaches GTA; sights and aim mode work in flat and immersive; third person remains correct. |
| `menus` | GTA's own menus with Sense in all modes | Navigate Settings, change values/tabs, confirm/cancel and apply changes; our overlay does not consume GTA input. |
| `sniper` | First-person immersive scope stereo | One coherent scope graphic/reticle with correct binocular placement and optical zoom; preserve flat and third-person scope behavior. |
| `hands` | Visible tracked hands, arms and gun | Independently moving Sense controllers changes actual skeleton/attachments and visible gun; head movement does not break barrel/reticle alignment. Full left-wrist rotation remains unfinished at the baseline. |
| `quality` | Immersive clarity and graphics | Correct source-to-display sampling/color/scale or expose a justified quality control; compare source/display evidence without unsupported frame-rate claims. |

Worktrees are under `/Users/zach/gaming/SantosVision-workers-20261024/` with the
owner names above. Durable launcher records and reports are under
`target/verification/device-session-20261024/claude-five-issues22/`. Each worker
edits and commits only its own checkout. The first wave is hands, ADS and menus;
sniper and quality have separate queued sessions. No extra nested workers are
needed.

The parent work is `w-14dea40fb6e248138ad507f85cf4384b`, topic
`santosvision-immersive`. Preserve candidate 21, candidate 20 and the original
working immersive checkpoint. The additional rollback tag is
`los-santos-before-first-person-repair22-20261024`.

Passing source tests, receiving poses or writing private native targets does not
prove visible hands, successful ADS, impact accuracy or physical stereo. Keep
these outcomes distinct in each handoff. The user's optional clarification on
bare-hand interaction is pending; Sense-driven visible arms remain the initial
hands scope. Rendering screenshots are deferred until aiming and arms work.

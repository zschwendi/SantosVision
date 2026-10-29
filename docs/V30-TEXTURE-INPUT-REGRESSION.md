# V30: preserve texture inputs across native passes

The wearer rejected V29: the game looked washed out even in the normal window immediately after Play, before entering immersive. The immersive image appeared sharper, but its brightness was also unacceptable. V29 is not a visually accepted build.

The window's display and color-transfer code had not changed since the accepted 27.3 window. The only unconditional shared rendering behavior change was persistent nulling of shader inputs when they overlapped color-target bindings. That made the shared native path the first correction target; it did not establish a gamma or exposure-math defect.

V30 restores the bridge's original retained input state. Binding a color target no longer erases texture inputs needed by a later pass. Feedback protection now applies while resolving the current graphics draw: an input that actually aliases that draw's color output receives the existing zero texture. This substitution never changes the retained guest binding. Compute dispatches do not have graphics color attachments, and independent depth/color proxy allocations remain readable. Cached binding paths use the same rule.

The immersive sharpness, coverage, stereo and color-conversion changes remain in place. No gamma, exposure, graphics setting, resolution, save file or game-code change is part of this correction. Implementation and build checkpoint: `7bd22fb` (V30).

## New before/after verification

Two new checks execute the production native command and Metal paths with authored source pixels. They test a later legal graphics read without rebinding and a real graphics draw followed by a compute read of its output. Both fail against V29's implementation: expected source bytes `[51, 102, 153, 204]` become `[0, 0, 0, 0]`. Both pass with the correction.

All nine targeted checks pass, including 45 guest graphics draws and six compute dispatches, with zero GPU validation errors. They also cover true simultaneous feedback, disjoint mips/layers, retained bindings, preparation/cache modes, unrelated targets and a distinct float-to-depth proxy. These are native pixel regressions, not a replay of the user's game scene or proof of restored headset appearance.

The signed V30 package preserves the original game AOT byte-for-byte and its executable protections, V29 entitlements, technical identity and provisioning. Independent verification confirms the package's source hashes and unchanged frozen rollback packages. V30 was installed in place on the M2 Vision Pro on October 28, 2026. Private build and verification receipts are under `target/visionpro-v30-20261028/` and `target/v29-whiteout-regression/`.

The previous real-game captures showed the same compute shader receiving a full-size texture in one capture and a zero dummy in another, but did not retain enough binding chronology to prove this caused the reported washout. The next acceptance check is therefore the cold game window in the same scene, followed by immersive: confirm normal shading and brightness first, then confirm the sharper immersive image remains. Hardware visual acceptance is pending.

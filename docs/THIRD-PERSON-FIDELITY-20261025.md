# Active baseline: build27, third-person immersive fidelity

The user explicitly rejected31, requested a return to27, and deferred Sense-specific controls and first-person immersive optimization. This direction supersedes previous first-person/ADS/hand-tracking test plans. Active source work is this worktree, branch `codex/third-person-fidelity27`, based on `75be389c8e7f0e120cfb640689ebc88fd01dc64f` / `los-santos-polish-candidate27-20261025`. Later source and isolated drafts are preserved separately; do not merge them into this baseline.

## Restore

The exact signed27 app was installed in place and the own-app query verified bundle version27. Executable SHA256: `6da3681d857138a93f433d862b5df60925e0edc3a338dbc7e7749614847098fc`. No uninstall, game-data replacement or save replacement occurred. Evidence is retained in the original checkout under `target/verification/device-session-20261025/polish27-device/restore27-1792952796494567000/`.

The attempt to launch27 with its previously used gap-repair configuration timed out. The copied report still belongs to an old31 session, so it does not establish a fresh27 launch or effective settings. Installation verification is separate. Do not ask for another Sense or first-person wearer test.

## Quality findings and active corrections

- The27-to31 third-person world shader, source coverage and fill-policy files are unchanged. A known launch-setting regression did occur:27 explicitly requested depth-edge gap repair, while the31 normal launch omitted it and actual telemetry reported repair unavailable/off. Restoring the binary alone does not persist the earlier launch override. This can account for some returning black outlines/speckles; it does not explain all softness.
- The first new quality-source correction makes repair the normal restored27 profile when no preference exists. Explicit saved off and QA disables still win. Real isolated UserDefaults reads/writes and renderer-selection checks pass10 cases. This is a source correction for the next quality build; it is not a claim that the installed frozen27 app changed or rendered new pixels.
- The window presents the final game pixels directly. Immersion redistributes them over a wider view and a much taller eye viewport. Registered third-person captures and actual viewport/projection measurements show a shortage of vertical source samples. More display pixels or post-sharpening cannot recover missing game detail.
- A controlled opt-in2560×2304 source experiment is being qualified. It must preserve native camera/HUD geometry and window aspect, use automatic game aspect, and retain the current16:9 choices/defaults. Actual game compatibility remains a required separate check.
- A separate default-off adaptive-mesh prototype will retain the existing finite-sky ownership, depth repair and owner0 gap fill. Image, depth and ownership parity must pass before measuring complete-command savings or spending the budget on more source detail. The existing incompatible adaptive toggle must not simply be enabled.

The acceptance target is a usable third-person immersive image approaching the window's clarity, lighting and material detail, with stable head-turn coverage. Display resolution, guard-band, source-resolution and shader-sharpening defaults remain unchanged. The normal unset repair preference now follows the intended27 profile; experimental source-aspect/adaptive paths remain opt-in and unaccepted. There is no sustained4K stereo60 claim.

# Equipped first-person gun placement — build 26

Build 25's physical snapshot at `2026-10-25T00:27:45Z` reported a gun-family discrepancy of **0.260376 m / 172.766°** between camera-relative placement and queued native world placement. The arm family agreed within **2.10e-8 m / 0.023544°**, on the same camera2296/epoch14/eye1. This is one snapshot, not a movement series or a displayed tracking acceptance.

Original producer44501 builds camera-relative O through `GetOffsetFromParent4652(..., false)` at0x01ab09b6, using the ordinary skeleton in the direct bone path. Its queued world attachment can instead select ped+4256 at0x01ab0c83..88. The existing model correction preserved O, including its discrepancy. Source evidence and the physical report are retained in `target/verification/device-session-20261024/post-goal25/gun-arm-transform-physical-analysis.md`.

`src/xr_viewmodel.rs` now uses the **existing 44355 completed-model hook** to place a qualified local equipped gun from queued `Wdraw` at draw+0:

`Mnew = Bz * inverse(Beye) * Wdraw`

The original clone still uses view `inverse(Bz)`, so the result is `inverse(Beye)*Wdraw`. It does not apply the older head correction again. Only the same12 model coordinates change; padding and the existing downstream translation-local reload remain intact. This is a host change using the existing hook ABI and build25 AOT.

The new policy requires arms/gameplay enabled, spatial controls selected, verified gameplay, and on-foot scope, in addition to all existing installed-eye, native first-person branch, original-model and coherent-snapshot checks. It qualifies the current local ped, manager/solver owners, equipped physical/runtime cross-link, physical type3, standard direct attachment to that ped and its native auxiliary bone, embedded transform source, finite attachment offset/unit quaternion and rigid queued world matrix.

Draw+144 is a **packed entity ID**, not a drawable pointer. The policy recomputes original46596's type3 ID for the exact equipped physical pointer through its registry/hash chain, using checked arithmetic, a64-node limit and cycle rejection, then matches draw+144. It also matches low16(draw+128) to the equipped physical's archetype model index. Failed qualification retains the existing camera-relative correction. A separate candidate readset prevents a rejected nonlocal prop from borrowing mutable weapon state; selected gun placement validates both readsets before writing.

Queued Wdraw is the authority for this visual placement. No current FP bone matrix, physical-root agreement, discrepancy threshold, cached muzzle or retained diagnostic sample selects the model. Such comparisons can cross asynchronous animation generations and switch between very different placements. This policy does **not** certify which FP bank generated Wdraw, consumed skin/palette identity, muzzle/barrel equality, reticle alignment, firing or native IK. Those systems are unchanged. StreamPed arms, flat/third-person draws and nonspatial controls retain their existing behavior.

Bounded per-family `last_applied` telemetry preserves legacy fields and adds:

- `placement_source`: `equipped_gun_queued_world` or `camera_relative`.
- `placement_qualification`: accepted identity/attachment reason or the exact fallback reason.
- `applied_vs_camera_relative_world` and `applied_vs_draw_world`: translation/rotation deltas reconstructed from the actual corrected model. Legacy `camera_relative_vs_draw_world` continues to describe the original source discrepancy.

Focused pinned offline tests exercise the production qualifier with an owned gun whose queued world differs by over170° from O, both eyes under head rotation/translation, all non-model bytes and padding preserved, scope/arm fallback, wrong ID/model/type/owner/auxiliary bone/attachment, malformed/cyclic/overlong registry chains, address overflow and mutable snapshots. Common qualification and selected-gun incoherence reject without writes; new-policy identity rejections match the legacy memory result exactly. The original private full-WASM, AOT and Metal suites are outside this host-only change. Physical validation must still confirm that accepted gun placement follows the intended grip and stays attached to the right hand. Reverting this source change restores the former O placement without replacing the AOT.

Validation: pinned `cargo +1.98.1 test --lib --locked --offline -j3 xr_motion_arms::viewmodel -- --nocapture` passed **13 tests**, with2 private-original slice tests intentionally ignored. The separate `xr_viewmodel` patch filter passed **3 tests**, with2 private-original certificate tests ignored. `git diff --check` passed. Independent read-only review found no remaining blocker after separating the candidate/fallback readsets.

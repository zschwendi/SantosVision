# Pending-mailbox hazard: structural reachability certificate (2026-10-24)

Parent `w-14dea40fb6e248138ad507f85cf4384b`, topic `santosvision-immersive`. This is a source/parser result only. It is not a proof that no mailbox sender can ever run, it does not explain the cold-start black screen, and it is not physical acceptance. The delivery policy (`deliver_pending_mailbox::<false>`) is unchanged.

## Scope and assumptions

The certificate is structural, against the exact original `game.wasm` (SHA256 `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`). It closes over direct calls and over literal table slots appearing in code, data or global initializers. It relies on three assumptions it cannot discharge:

- **A1 — function pointers.** Every table index that reaches `call_indirect` or host `invoke_*` originates from such a literal. Computed pointer arithmetic, corrupted memory or other out-of-band index values are not excluded. C/C++ semantics make them undefined behaviour, but that is not a proof.
- **A2 — export names.** Every host export lookup is a string literal or an `mp:`-prefixed name. This is enumerated below for this commit's source. The test guard is lexical and cannot see constructed strings.
- **A3 — module identity.** The running module derives from this original through factories that keep the named bodies canonical. The only change among them is patch5's Emit550 midpoint.

**Under A1–A3, no guest code calls `_emscripten_notify_mailbox_postmessage`.** So `deliver_pending_mailbox` never runs guest mailbox code: not at the Emit midpoint (`muguet_xr_gpu_command`), not at Reserve551's two `wasm_now_ms` calls, and not at any other import. Within that scope, the October 13 fixture hazard is unreachable, so there is nothing to defer.

All indices below are original indices. Factories append imports after the original 85, so a defined function `f` becomes `f + (production function imports − 85)`. The runtime tripwire matches name-section names, so it does not depend on that shift.

## Checked facts (`host::mailbox_reachability`)

1. Import73 has exactly one call site: `do_proxy`90842 at `0x2e38a79`. The call sits in the branch taken when `waiting_async(pthread+204)` is 0. When the flag is nonzero, `do_proxy` only executes `memory.atomic.notify`.
2. `em_task_queue_enqueue`90837 is called only by `do_proxy`.
3. **The sender island.** Starting from `do_proxy`, close over direct callers and over every function that materializes a member's table slot as a code literal. The result:
   - Entries: only the original exports `_emscripten_run_js_on_main_thread`90856 and `_emscripten_run_js_on_main_thread_done`90855.
   - Members: `emscripten_proxy_finish`, `emscripten_proxy_sync_with_ctx`, `cancel_ctx`, `call_then_finish_task`, `cancel_active_ctxs` and `init_active_ctxs`.
   - No member slot literal appears in data or global initializers, and no member is used by `ref.func` or as the start function.
   - The module contains no table-mutating instruction.
   - `init_active_ctxs` slot89331 is never materialized (its pthread_once call is directized).
   - Game code contains no `emscripten_proxy_async/sync`.
4. **`mp:` names.** No original export starts with `mp:`. patch5 exports `mp:` names only for `mp::COMMANDS`, and none of those targets is an island member.
5. **Yield's crash branch.** `_emscripten_yield`90743 reaches imports37/38 only when crash word29595928 is nonzero. The word's only literal writer is the export `_emscripten_thread_crashed`90742, which has no callers.
6. **Host export lookups (A2), at this commit.**
   - `host.rs` `export_call[_kind]`: `_emscripten_check_mailbox`, `_emscripten_thread_init`, `_emscripten_thread_free_data`, `emscripten_stack_get_current`, `_emscripten_stack_restore`, `setThrew`.
   - `lib.rs` `call_export`/`get_func`/`get_typed_func`: `_emscripten_tls_init`, `__wasm_call_ctors`, `__main_argc_argv`, `emscripten_stack_set_limits`, `_emscripten_stack_restore`, `_emscripten_thread_init`, `_emscripten_thread_exit`, `_emscripten_stack_alloc`.
   - `mp.rs` `call` (14 sites): seven `"mp:…"` literals, four `format!("mp:{cmd}")`, and three stack-helper literals. `mp.rs` also uses direct `get_export` with the literals `mp:GetGameplayCamCoord`/`Rot`.
   - `benchmark.rs` `invoke`: `format!("mp:{name}")` and three stack-helper literals.
   - `xr_view.rs`: stack-helper and `mp:XRSetViewport*` literals.
   - Non-function lookups: `__indirect_function_table` and `__muguet_motion_stack_pointer`.
   - `xr_aim` invokes only certified constant table slots.
   - Under A1, `invoke_*` reaches only guest-materialized slots.

If any assumption fails, the islands above would still need a sender to take the `waiting_async==0` branch. Note that the host arms `waiting_async=1`:
- on main, through ctor `_emscripten_init_main_thread_js(29596104)` → `_emscripten_thread_mailbox_init` → import53;
- on every pthread, in `run_pthread`.

After arming, a send takes the atomic-notify branch, which the host never observes. Such a notification would be lost and invisible, not delivered under the lock.

## What remains under the producer mutex (8348864)

The structural scan finds exactly three functions whose atomic RMW names the mutex by literal offset: Init549, Emit550 and EmitData552. All three are direct-call-only, and the address literal does not appear in data or globals. **EmitData552 was missing from earlier reports.** It calls Reserve551 under the mutex (`0x17c317`). Any post-release hook must cover it too; `xr_emit_completion_patch` covers Emit only.

| Under-lock site | Import / callback | Host effect under A1–A3 |
|---|---|---|
| Reserve551 `0x17c22f`, `0x17c266` (from Emit and EmitData) | import0 `wasm_now_ms` | Pending check is one load; returns time; no guest call. |
| `wasm_wait_begin`137 inside Reserve | import0 | Same. |
| `_do_futex_wait`90735 `0x2e348e6/0x2e348f6` | import12 `emscripten_get_now` | Only if `supports_wait==0`; the host initializes it to 1 on main and pthreads. |
| Main-thread `_emscripten_yield` | imports37/38; system-queue callback | No crash-word writer is called (fact 5); no task is ever enqueued (fact 2). |
| Emit550 patch5 midpoint | `muguet_xr_gpu_command` | Pending check is one load; `xr_view::gpu_command` makes no guest call but takes the host world-state mutex. |
| Init549 | `aligned_alloc` closure; import8 `wgpu_start_worker` | No pending mailbox; `gpu::start` spawns a Rust thread once. |

## Safe delivery boundary

Within A1–A3 the boundary question is vacuous. Outside them, today's host offers no boundary that is both proven safe and proven live:

- A Wasm-stack check can show that no Init/Emit/EmitData frame is live, but it cannot guarantee a later eligible import. Emit and EmitData have no post-release import.
- Long-running threads never return to an empty stack, which is Emscripten's own safe point.
- The atomic-notify branch bypasses the host entirely.

A real fix would need a separate qualified factory with post-release joins in both Emit550 and EmitData552, plus a decision about notifications sent after arming. The 9-body factory and its AOT must not be overwritten.

## Tripwire (host.rs) and what a zero means

`_emscripten_notify_mailbox_postmessage` increments a counter and prints one stderr line the first time. Each resulting delivery is counted and its Wasm stack classified:
- producer frame by exact name;
- clear;
- unqualified: 20 or more frames, or a nameless frame.

Delivery itself is unchanged. Root will add `"mailbox": crate::host::mailbox_status_json(),` to `native::sv_runtime_status`.

`notifications=0` and `deliveries=0` on one run means only that **this host-delivery path (import73 → pending set → `_emscripten_check_mailbox`) did not occur on that run**. It says nothing about the separate atomic-notify branch, which the host cannot see. It neither excludes nor explains any other cold-start failure. A nonzero value means an assumption failed for that build or run.

Observing the atomic-notify branch would need a different probe: reading each live pthread's mailbox notification word (`load64(pthread+192)` → `+0 == 2`, pending) with lifetime tracking. That probe is not implemented.

## Tests

- **`host::mailbox_publication_tests` (2, serialized).** These are the existing fixture-policy test and a new one. The new test checks that production delivery still runs inside an Emit-named frame and is counted as a producer frame. It also checks clear, deep and truncated classification and the status counters.
- **`host::mailbox_reachability::host_source_has_no_literal_proxy_or_crash_export_name`.** Lexical A2 guard; a mutation probe containing the literal makes it fail.
- **`host::mailbox_reachability::private_original_sender_island_closed_over_calls_and_literal_slots`.** Run with `--ignored`; parses the private original only.

The receipt with hashes and logs is `target/verification/opus23-startup-mailbox/REPORT.md`.

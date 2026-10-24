# Pending-mailbox hazard: reachability qualification (2026-10-24)

Parent `w-14dea40fb6e248138ad507f85cf4384b`, topic `santosvision-immersive`. Source/parser result only. This does not explain the cold-start black screen, and it is not physical acceptance.

## Result

With the exact original `game.wasm` (SHA256 `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`) and this host, no reachable code can send a host-visible mailbox notification. `deliver_pending_mailbox::<false>` therefore never calls `_emscripten_check_mailbox`. That covers the Emit midpoint (`muguet_xr_gpu_command`), Reserve551's two `wasm_now_ms` calls and every other import. The October 13 fixture hazard is real for its model, but production cannot reach it. The deferral question is moot for this module: nothing can be pending, so there is nothing to defer or deliver. The production policy is unchanged.

All indices below are original indices. Every production factory appends imports after the original 85, so a defined function `f` becomes `f + (production function imports − 85)`. The 9-body viewmodel factory leaves every function named here canonical. The one exception is patch5's Emit550 midpoint. The runtime tripwire matches name-section names, so it does not depend on that shift.

## Proof chain (checked by `host::mailbox_reachability`)

1. Import73 `_emscripten_notify_mailbox_postmessage` has exactly one call site: `do_proxy`90842 at `0x2e38a79`. That call is in the `waiting_async(pthread+204)==0` branch. When the flag is nonzero, `do_proxy` only performs `memory.atomic.notify`.
2. `em_task_queue_enqueue`90837 is called only by `do_proxy`. No other code enqueues into a mailbox or the system queue.
3. Closing over direct callers and over every code constant that materializes a member's table slot gives an island. Its only entries are the JS-glue exports `_emscripten_run_js_on_main_thread`90856 and `_emscripten_run_js_on_main_thread_done`90855. The island contains `emscripten_proxy_finish`, `emscripten_proxy_sync_with_ctx`, `cancel_ctx`, `call_then_finish_task`, `cancel_active_ctxs` and `init_active_ctxs`. No member slot appears in active data, global initializers or `ref.func`, and the module has no table-mutating instruction. `init_active_ctxs` slot89331 is never materialized at all, because its pthread_once call was directized. Game code contains no `emscripten_proxy_async/sync`.
4. The host never names those exports. Its export calls use fixed runtime names or the `mp:` prefix, and `invoke_*`/`xr_aim` reach only guest-materialized or certified slots. A source test fails if any `src` file gains such a literal.
5. Consequently `em_task_queue_execute`90833's indirect callback (`0x2e385a7`) never has a task, whether reached from `_emscripten_check_mailbox` or from main-thread `_emscripten_yield` → system queue.
6. `_emscripten_yield`90743 reaches imports37/38 (`set_strongref`, `exit_with_live_runtime`) only when crash word29595928 is nonzero. Only the export `_emscripten_thread_crashed`90742 writes that word, and nothing calls it. On a normal host, imports37/38 and the `Unwind` paths in `run_engine`/`run_pthread` are therefore unreachable.

## What remains under the producer mutex (8348864)

The only functions with atomic RMW on the mutex are Init549, Emit550 and EmitData552. All three are reachable only by direct call: none is exported, in a table or used by `ref.func`, and the mutex address does not escape to data. **EmitData552 was missing from earlier reports.** It locks the same mutex and calls Reserve551 under it (`0x17c317`). Any future post-release hook or emission-completion factory must cover EmitData too; `xr_emit_completion_patch` covers Emit only.

| Under-lock site | Import / callback | Host effect now |
|---|---|---|
| Reserve551 `0x17c22f`, `0x17c266` (from Emit and EmitData) | import0 `wasm_now_ms` | Pending check is one load; returns time. No guest call. |
| `wasm_wait_begin`137 inside Reserve | import0 | Same. |
| `_do_futex_wait`90735 `0x2e348e6/0x2e348f6` | import12 `emscripten_get_now` | Only if `supports_wait==0`; the host initializes 1 on main and on pthreads. |
| Main-thread `_emscripten_yield` | imports37/38; queue callback | Unreachable (items 5–6). |
| Emit550 patch5 midpoint | `muguet_xr_gpu_command` | Pending check is one load; `xr_view::gpu_command` makes no guest call (it takes the host world-state mutex). |
| Init549 | `aligned_alloc` closure; import8 `wgpu_start_worker` | No mailbox; `gpu::start` spawns a Rust thread once. |

## Safe delivery boundary

For this module the requested boundary is vacuously satisfied. If a future build or host makes `do_proxy` reachable, none of today's options is both proven safe and proven live:

- The host arms `waiting_async=1` on every thread: main via ctor import53, pthreads in `run_pthread`. Such sends would then use `memory.atomic.notify`, which the host never observes. They would be **lost** (a liveness failure), not delivered under the lock. Import73 could fire only in the pre-arm window: between `__pthread_create`'s `_emscripten_thread_mailbox_init` and the child's `mailbox_await`.
- A Wasm-stack check can prove "no producer frame on this pthread". But it cannot prove a later eligible import, because Emit/EmitData have no post-release import.
- Emscripten's own safe point is an empty guest stack (an event-loop return). Long-running host threads never reach one.

A sound fix would then need a separate qualified factory with post-release join hooks in both Emit550 and EmitData552, plus a decision on post-arm notification. Do not overwrite the 9-body factory or AOT for this.

## Tripwire (host.rs)

`_emscripten_notify_mailbox_postmessage` now increments a counter and prints one stderr line the first time. Each actual delivery is counted and its Wasm stack classified as producer frame / clear / unqualified, where truncation at 20 frames or a nameless frame counts as unqualified. Delivery itself is unchanged. `host::mailbox_status_json()` is not yet in the runtime report. Root integration needs one line in `native::sv_runtime_status`: `"mailbox": crate::host::mailbox_status_json(),`. Expected on device: `notifications == 0`.

## Tests

- `host::mailbox_publication_tests` (2, serialized): the existing fixture-policy test, plus a new test. The new one checks that production delivery still runs inside an Emit-named frame and is counted as producer, that clear/deep/truncated stacks are classified, and the counters/status.
- `host::mailbox_reachability::host_source_never_enters_proxy_or_crash_exports`: a mutation probe with a forbidden literal fails it.
- `host::mailbox_reachability::private_original_has_no_host_reachable_mailbox_sender`, run with `--ignored`. It parses the private original only.

Receipt with hashes and logs: `target/verification/opus23-startup-mailbox/REPORT.md`.

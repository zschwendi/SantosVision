# Bounded source-to-compositor latency trace

This default-off diagnostic observes the existing rendering pipeline. It changes no camera, source resolution, guard, graphics, input, eye selection or rendering policy. It does not certify coherent stereo, isolated GPU duration or motion-to-photon latency.

Set `MUGUET_XR_LATENCY_TRACE=1` before native startup. Optionally set `MUGUET_XR_LATENCY_TRACE_PATH` to a **new absolute file path** inside the app's own writable Documents directory. The directory must already exist. Relative paths reject capture; existing files are never overwritten. Without a path only bounded summary status remains after collection ends.

The native runtime initializes one capture session/origin per process. A live duplicate runtime start is rejected by existing runtime policy. A failed startup retry before runtime creation retains the original diagnostic session; it does not rearm an already completed trace. Every world-camera acquisition unconditionally advances the existing provenance epoch and resets native frame generation, so successive immersive renderers remain distinct within the one capture session. Recenter/tracking invalidation also advances provenance. The renderer caches the cold environment request, allowing later native initialization to arm commit events; it does not cache a possibly premature native `false` result.

At8192 events collection stops. The last event is retained, `record_limit_reached=true`, and later calls are unobserved. `observed_dropped_events` counts rejected observations before collection stops (for example a regressing injected clock); it is not a count of unobserved calls after stopping. Renderer stop/loop exit also finishes a shorter trace if that renderer actually releases current camera ownership. The additive `sv_world_latency_finish()` function can explicitly stop a QA capture. Pending callbacks/compositor completions arriving after that stop are unobserved; stop is not a GPU-drain barrier. Capture the head-turn interval while immersive is still running, then exit normally. No launcher button or rendering shutdown policy is changed. Collection does not automatically rearm.

One background worker and a one-slot bounded channel are created at cold trace initialization. At cap/finish, the bounded event snapshot moves out of the capture lock and a nonblocking once-only send hands it to that worker. No thread spawn, serialization or filesystem operation happens on the caller under frame/world locks. The worker creates one NDJSON file with header, complete event lines and footer. The file cannot exceed4MiB. If the file limit is reached, the footer reports `persistence_omitted`; a missing footer is a partial/failed export. Status reports collecting/pending/written/failed/not_requested and is capped at2KiB, including error text. It contains no full event payload.

Every event carries a native capture session ID and captured provenance epoch. Available request/head/camera/game/eye/lease IDs remain explicit, and unknown values stay null. In particular a request-receipt event has no invented head/eye identity. One native `Instant` origin timestamps all calls, including the Swift compositor-commit bridge. Predicted presentation timestamps remain separate metadata and are never subtracted from native elapsed nanoseconds.

The stage observations are:

- `request_received`: accepted latest-view request.
- `camera_consumed`: successfully applied qualified native camera and retained pose.
- `ring_camera_dequeued`: first replay marker for a consecutive camera identity. Raw head/tail and the original GPU-start size argument are sampled. Nonwrapped distance is reported when valid; wrapped occupancy stays null because that unused argument's wrap-extent contract is not certified here. Concurrent producer progress can immediately change these samples. This is not a per-command log.
- `snapshot_submitted`: immutable color/depth snapshot queued after the final game work.
- `snapshot_callback_observed`: callback entry before frame-lock acquisition or lifecycle validation. It retains the original source epoch even if a new lifecycle is already active.
- `snapshot_completed`: successful ready publication after original epoch/generation validation. An old callback may have an observed event and no successful publication.
- `snapshot_rejected_slot_limit`: publication could not reserve a snapshot slot; current existing slot/lease/drop counters are included.
- `source_acquired`: successful native acquisition with a new live lease.
- `compositor_commit`: Swift commit call with native-validated live leases, immediately before `commands.commit()`.
- `compositor_completed`: original completion validator accepted live lease identities; inspect raw Metal status (`4` means successful completion). An error status is not a successful presentation.

Existing pending-snapshot, slot, lease and dropped-snapshot counters are sampled under the locks already held by publication/acquisition/completion. The trace never takes a frame-state lock itself; the commit accessor uses a frame lock only when tracing is enabled. Asynchronous callbacks use captured source identities. Neither the current epoch nor the latest camera is substituted for an old source.

With tracing off, hot hooks take only an atomic/boolean branch: no trace clock, string/Vec allocation, environment access, mutex, file access or per-command recording. The one environment read and buffer allocation occur at cold native startup only when requested. The Swift environment request is read once when constructing its renderer, with no per-frame FFI call when unset. The native renderer lease/completion APIs and their existing production validation remain unchanged.

Run `python3 scripts/analyze_world_latency.py <trace.ndjson> --output <new-analysis.json>`. The parser bounds the input, rejects incomplete exports, mixed capture sessions and nonmonotonic records, and joins only exact per-eye session/epoch/request/head/camera/game/lease identities. Missing stages produce null intervals. Source reuse produces separate lease observations; it is never turned into a same-update stereo pair. Stage intervals include scheduling/queue/callback overhead. A still-camera observation or a fresh ready timestamp alone cannot locate head-turn source lag.

The next actual test is one bounded normal third-person session with moderate and fast head turns, no single-source diagnostic and no guard/default changes. Require the new complete export and its exact candidate/source/config provenance. Compare camera-to-ring, ring-to-submit, callback-to-publication and compositor intervals before selecting a latency fix. Source, CPU and typecheck results do not establish installed behavior or physical improvement.

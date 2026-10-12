# XR emission fallback reset ordering

The enabled emission path snapshots a render thread's current view before taking `STATE`. If a concurrent activation reset completes before the callback acquires that lock, the snapshot belongs to the previous epoch. The previous fallback filtered stale headset views but still accepted non-headset views; an old draw could repopulate `frame_world` after invalidation and cause a false conflict with the next valid camera sequence.

The fallback now compares the captured thread epoch with `EPOCH` while holding `STATE`. A mismatch rejects both the view and its token as `(None, 0)`. The flag-off lock/TLS/record order remains unchanged. If reset begins after that locked epoch read, its subsequent locked invalidation clears the recorded metadata, preserving the legacy ordering.

The mutex shortcut may cross a reset after its atomic checks, but it only increments a diagnostic count and cannot restore markers, frame state, tokens or presents. Draws and presents always take the locked fallback; a new binding has a distinct token. No additional lock is needed on the shortcut.

`emission_fallback_rejects_snapshot_taken_before_concurrent_reset` uses two barriers, a local epoch and a local state mutex to force snapshot → completed reset → fallback for both headset and non-headset views. It compares markers/presents with the legacy reset sequence and requires the next valid presentation without a conflict. `emission_fast_skip_crossing_reset_cannot_restore_old_binding` covers the shortcut's reset ordering.

Prepared from `bd509bd1ae513d9ff94a7a7a1a9b0837b3799d75` in an isolated checkout. Source review and `git diff --check` completed; the worker ran no builds, tests, GPU work or apps. Root owns fresh qualification before integration. Parent work: `w-ab947ade438e427bb263135cabb1ac55`; fix scope: `w-34dc16d09a8f40289888cc60f3353cde`.

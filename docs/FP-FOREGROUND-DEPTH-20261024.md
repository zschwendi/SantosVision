# First-person foreground depth — October 24, 2026

Opus 5.5 source worker for parent `w-14dea40fb6e248138ad507f85cf4384b`
(topic santosvision-immersive). Source change only, default OFF. Root owns
AOT, signing, Metal/device validation and physical acceptance. No change to
the nine-body factory: the existing seam import and patch5 hooks are reused,
so the installed22 AOT (`3874410a…`) is unaffected.

## The problem this fixes

Installed22 corrects the completed first-person model in 44415 (StreamPed,
arms) and 44355 (skinned attachment, gun), so the raster becomes
`view*M' = inverse(R)*S*inverse(O)` (docs/FP-VIEWMODEL-20261024.md). But those
draws run through the original zero-translation clone viewport. The GPU
producer's SetCurrent hook cannot match that clone to any qualified world
view, so every first-person command reaches the GPU worker unlabelled. Its
depth writes are then "unqualified writers": they revoke the world depth
record, or happen after the pre-UI world copy was frozen. Either way the
published depth cannot describe the gun and hands, and reprojection or the
reticle cannot place them. Relabelling the frozen world depth would be wrong.

## Producer: exact attribution of the corrected invocation

The corrected draws are the native placement `B0*S*inverse(O)` seen from the
parent eye `inverse(B0*R)` with the parent's own projection. So the parent
WorldView is the actual raster state of those draws, provided the clone's
basis, projection and projection state are the parent's, bit for bit.

`src/xr_foreground_depth.rs` (rules in `src/xr_foreground_depth_core.rs`):

- `xr_view::current_view` records, per guest thread, the viewport each
  SetCurrent made current and the view it carried. SetCurrent(clone) therefore
  remembers the displaced parent pointer and its exact WorldView.
- At the certified seam, after `qualify_and_correct` succeeds,
  `bind_after_correction` requires: family 44415 or 44355 (44395 never binds);
  the clone (frame+128) is the current viewport and matched no view; the
  displaced pointer is the seam's saved parent; the displaced view is the
  installed headset eye the correction used (same epoch, camera sequence, eye
  and camera coordinates); a known Store and pthread; and, read from guest
  memory, parent camera/projection equal to that view and the clone equal to
  the parent with zero translation, identical projection, projection scalars
  (+872..916) and state bytes (+916, +928).
- The binding gets a never-recycled generation. At every `wasm_gpu::Emit` while
  bound (host reads only, under the Emit mutex, no guest call), it must still
  hold: same epoch, Store and pthread; guest stack pointer at or below the
  invocation frame; clone camera, clone projection and corrected model unchanged
  bit for bit. Any mismatch revokes it permanently.
- It ends at the next SetCurrent on that thread: `balanced` when that is the
  original SetCurrent(parent), otherwise `other_set_current`.
- `guest_call_scope::call` holds a `Boundary` guard for every host-to-guest
  call. A binding opened inside a call that exits without its restoration
  (normal return, error, LongJmp, trap, panic) is revoked when `call` returns,
  before any stack_restore/setThrew recovery. Mailbox calls and their
  descendants suspend attribution; they neither carry nor revoke it.
- Each tagged Emit records an immutable `ForegroundMark { generation, family,
  view }` keyed by the exact ring command address, only on change, next to
  the existing view markers. Reset converts pending marks to "ended" in place.
  A lost transition sets `correlation_failed` (images refused), as for views.
  The existing view markers are not changed: first-person commands still carry
  no world view, so with the merge flag off every GPU rule is unchanged.

## GPU: actual foreground depth, never a relabelled world

`src/gpu.rs` consumes marks in ring order. An attributed draw that writes depth
is accepted only if it is opaque and projection-exact: full single-sample depth
target, full viewport and [0,1] depth range, no fragment depth, no depth bias or
clamp, no blended colour target, and a nearer-passes (or Always) compare in the
view's depth convention. Others take the existing generic path.

Its relation to the target's qualified world record decides the case. "Same"
means a bit-identical raster: identity, camera, stage, optics and projection
including its depth row.

1. **Live world depth, same raster** — written into the live world depth.
   The live target already holds the actual combined raster, including native
   first-person occlusion. It is kept rather than revoked. A later pre-UI copy
   includes it.
2. **Frozen pre-UI world, same raster** — written after GTA's UI clear. Before
   the first such write, the actual target depth is copied (`before`). The
   overlay closes when any non-attributed writer is about to touch the target:
   draw, clear, copy, upload, resolve, destroy or replacement. All of these are
   hooked before the mutation is encoded; the actual depth is copied then
   (`after`), or at present if nothing else wrote it. At publication a new
   R32Float is produced: frozen world wherever `before` and `after` are
   bit-identical, otherwise the actual foreground depth. This preserves
   GTA's intended first-person-over-world occlusion. There is no epsilon,
   colour test or invented mask. A foreground fragment that stored exactly the
   cleared value is indistinguishable; that is the projection limit.
3. **Anything else** — different raster, no world record, an attributed write
   after a foreign writer, a target replaced mid-overlay, a failed snapshot,
   two overlays per present, or the merge failing. The existing generic rule
   applies. If an accepted overlay cannot merge, its frozen world is withdrawn
   at publication, exactly as the generic rule would have revoked it. First-
   person colour is never published with world-only depth where the foreground
   wrote.

Snapshots use the existing exact depth proxy (`textureLoad` to R32Float). The
merge is a fullscreen pass with an explicit unfilterable layout. Its WGSL
validates with the naga that wgpu uses, and `merge_reference` is its CPU
reference. Bracketing images are pooled; reuse is ordered on the one queue.

## Bounded capture to settle the actual render order

The source cannot certify which case GTA actually uses for the gun and hands.
They might be before or after the UI clear, on which target, with which state,
or written by other families (44395 props, effects). So the build includes a
one-shot capture. It is not a metadata trace claiming replay: the manifest says
`replay_complete:false`, `command_snapshot_complete:false` and
`physical_visibility_certified:false`.

`MUGUET_XR_FP_DEPTH_CAPTURE_DIR=<existing dir>` arms it (absolute, or relative to
the app HOME). It requires `MUGUET_XR_FP_VIEWMODEL` and the motion AOT, like the
correction. Attribution alone does not change any published image. The capture
records the present that contains the first attributed draw:

- raw images in command-stream order: Float32 depth and colour target 0
  immediately before the first attributed draw, the same after the last
  attributed segment, the frozen world at trigger, the published depth and the
  final colour. Native Metal handles and descriptors are included;
- ordered events for that present: every depth-writing draw (target, state,
  shaders, viewport, world and foreground marks, live/frozen records), every
  attribution change, the pre-UI freeze, every depth-target mutation and every
  overlay decision.

Caps: 8 images, 96 MiB each, 448 MiB total, 8192 events. Omissions are listed.
It writes on a background thread into a new `fp-depth-<ms>-<pid>` directory with
`create_new` files. It never overwrites, retries or re-arms in-process. Status
is reported under `first_person_viewmodel.foreground_depth.capture`.

## Switches

- `MUGUET_XR_FP_FOREGROUND_DEPTH=1` (or compile-time
  `MUGUET_XR_FP_FOREGROUND_DEPTH_DEFAULT=1`) enables attribution plus the GPU
  merge. It is off by default.
- `MUGUET_XR_FP_DEPTH_CAPTURE_DIR` (or compile-time `…_DIR_DEFAULT`) enables
  attribution plus the one-shot capture, with no merge.
- Both depend on the existing `MUGUET_XR_FP_VIEWMODEL` correction and motion AOT.
  Third person, flat mode, menus and 44395 never bind.

Status lives at `motion_aim.native_arms.first_person_viewmodel.foreground_depth`,
which reports bindings, rejections and end reasons, plus GPU combined/overlay
writes, generic reasons, withdrawals and merges handed to publication. The
xr_view status adds `first_person_foreground_markers`. These are bookkeeping,
not visibility claims.

## Evidence (CPU only)

- The standalone core, `rustc --edition 2021 --test -D warnings
  src/xr_foreground_depth_core.rs`, passes 10 tests. They cover the bitwise
  clone certificate and its mutations; displaced-parent tracking; balanced,
  other, nested, epoch, store/pthread and memory endings; mailbox suspension;
  boundary revocation on every exit path; overlay open/continue/close/reject
  and capacity; a reverse-Z raster merge simulation; and capture budgets.
- In-crate focused tests pass: `xr_view::foreground_depth::` (15),
  `xr_view::` (73), `guest_call_scope::` (12), viewmodel host (6, +2 ignored)
  and viewmodel patch (3, +2 ignored). The new integration tests read actual
  guest memory in a SharedMemory, revoke through the real
  `guest_call_scope::call` guard (LongJmp error, unbalanced return, panic,
  nested, mailbox), revoke after a real wasm trap through guest frames, check
  positional ring markers, validate the merge WGSL with naga, and check
  bit-identical raster matching.
- No GPU, game, device, AOT or signing action was performed. The Metal merge,
  snapshot and capture code compiles but has not run on a GPU.

## Physical qualification still required (root)

1. Capture first: immersive first person, Sense controllers, weapon equipped,
   with `MUGUET_XR_FP_VIEWMODEL=1` and `MUGUET_XR_FP_DEPTH_CAPTURE_DIR`. Read
   `manifest.json`. Decide which case the arms and gun use, from the attributed
   draws, their targets, the freeze event and the before/after depth difference.
   Also check whether any non-attributed writer (44395 props, muzzle, effects)
   touches the foreground pixels.
2. Then merge: `MUGUET_XR_FP_FOREGROUND_DEPTH=1`. Confirm `combined_live_writes`
   or `merges_handed_to_publication` rise, and that `withdrawn_world_depth` and
   `generic` reasons are understood. Look down and around and move the head
   without moving the grip. The gun and hands must not swim or ghost under
   reprojection, the reticle must stay on the barrel near and far, and nothing
   may smear into the background.
3. Third person, flat first person, menus, phone and vehicles: unchanged
   (no bindings).

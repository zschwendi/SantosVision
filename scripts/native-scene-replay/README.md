# Generic immutable native scene replay

This standalone Mac Metal consumer executes an ordered scene of original clears,
heterogeneous draws and full color copies on persistent attachments. It is the
consumer seam for a complete captured source pass. It does **not** supply a complete
game capture, establish finished-frame dependency closure, increase game resolution,
or change the production renderer.

`model.rs` defines the resolved IR, `validate.rs` admits bounded immutable inputs,
`execute.rs` runs them, and `import.rs` adapts the two existing narrow capture
schemas. `fixture.rs` authors a small independent API oracle.

## Resource and provenance contract

- One persistent texture record represents one native allocation. Native records
  require unique native allocation IDs; local IDs are lookup names. Buffer records
  are immutable snapshots and may represent different versions/ranges of one native
  allocation. Every binding points to an exact snapshot.
- Each texture mip/layer/aspect has an explicit initial version and origin:
  `unknown`, exact `captured` bytes, or an actual `uniform_clear` with raw floating
  point bits and view format. Unknown cannot be loaded or sampled. A partial draw
  never establishes initialization. Captured target bytes support original-density
  parity only; they are not higher-density initialization.
- Every ordered attachment write advances an explicit version. Selected sampled
  views name every visible subresource version. Stale versions and sampled/output
  feedback fail CPU admission, with legal read-only depth/stencil distinguished.
- Native scenes require one identical eye/epoch/camera/request/head identity on
  every draw, completed original submission, no omitted commands and zero original
  GPU errors. Pipeline shader text must be the **actual final compiled variant**,
  not a guest program before rewrites. Root's native shader registry supplies the
  future effective shader references; the legacy adapter accepts only its pinned
  previously reviewed shader families.
- Producer ledger `VersionRef { allocation_id, content_version, subresource }`
  maps directly to texture allocation identity and IR `Version`. A future native
  adapter must reject missing/evicted predecessors, allocations, inputs or disabled
  ledgers; preserve clear bits; and resolve the exact final shader references.
  Debug-format spellings require an explicit enum mapping. This crate does not yet
  claim that adapter or the 997-draw historical span has been captured.

Supported runtime behavior includes vertex/fragment sampled textures, exact static
uniform/read-only storage buffer bindings, samplers, optional vertex slots,
indexed/nonindexed/instanced draws, color write masks and blends, depth/stencil,
viewport/scissor, sRGB views, 2D arrays/cubes and selected mip/layer views, BC raw
mips, ordered clears and equal-format full-color copies. Inputs are capped at
256 MiB and persistent allocation estimates/readbacks are also bounded. No compute,
queries, storage writes, dynamic binding offsets, binding arrays, multisampling,
3D textures, indirect draws, depth copies or omitted operations are silently
emulated. These require explicit new IR/executor support before admission.

The executor splits draws into serial Load/Store passes. This preserves the tested
ordinary raster contents, but is an offline correctness implementation, not a
production timing, batching, shader side-effect or feedback model. The report always
keeps `finished_frame_or_density_accepted: false`.

## Reproduce locally

Use the checkout's existing offline Rust toolchain. These commands never launch a
game. Only `run` creates a Metal device. Coordinate the GPU slot with other workers
before that command. All output directories must be new.

```sh
export CARGO_HOME=/Users/zach/gaming/SantosVision/.tools/cargo
export RUSTUP_HOME=/Users/zach/gaming/SantosVision/.tools/rustup
export CARGO_TARGET_DIR=target/central-density-proof/span-build
"$CARGO_HOME/bin/cargo" test --offline --locked --manifest-path scripts/native-scene-replay/Cargo.toml
"$CARGO_HOME/bin/cargo" build --offline --locked --manifest-path scripts/native-scene-replay/Cargo.toml
BINARY=target/central-density-proof/span-build/debug/native-scene-replay
"$BINARY" make-mixed target/native-scene-replay/new-mixed
"$BINARY" validate target/native-scene-replay/new-mixed/scene.json target/native-scene-replay/new-admission
python3 scripts/native-scene-replay/run.py "$BINARY" target/native-scene-replay/new-mixed/scene.json target/native-scene-replay/new-run
```

`run.py` freezes the exact admitted manifest and blobs, snapshots this crate's source
and lockfile, binds the replay binary hash, then executes the generic consumer. It
preserves failures and verifies both original and frozen input identity afterward.
It does not compile or launch any game/app. The result contains exact raw aspect
hashes and byte-difference counts; private payloads remain outside Git.

Legacy import is explicit:

```sh
"$BINARY" import-legacy /absolute/capture/capture.json target/native-scene-replay/new-legacy
```

The adapter preserves raw blobs, labels the scene `legacy_captured`, leaves native
allocation IDs absent and uses local replay version bookkeeping. It does not invent
original clears, runtime allocation IDs, complete initialization or a new-density
source. The v5 color-null capture is an all-aspect no-op; exact replay of it remains
API parity and cannot demonstrate visual restoration.

## Authored mixed-pass oracle

The 16×16 fixture starts target contents as Unknown, then performs actual ordered
color/depth/stencil clears. Three different pipelines run: a nonindexed draw uses a
vertex texture plus a selected fragment sRGB array mip, an indexed draw writes
depth and stencil, and an indexed two-instance draw changes the bottom half. A
depth-only clear between draws must preserve color/stencil. Uniform snapshots change
per draw and a final color copy reads the current version. Expected RGBA, Float32
depth and stencil bytes are calculated independently on the CPU. This is explicitly
an authored API test, not game imagery.

## Complete native target intervals

Schema 4 uses `import-native`, with real native allocation/version identities and
actual final shader sources. See [NATIVE-TARGET-SPAN.md](NATIVE-TARGET-SPAN.md) for
its strict producer contract, emitted draw arguments, selected views and complete
interval requirements. Imported target bytes remain original-density evidence.

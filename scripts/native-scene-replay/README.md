# Generic immutable native scene replay

This standalone Mac Metal consumer executes an ordered scene of original clears,
heterogeneous draws, supported compute dispatches and full color copies on persistent attachments. It is the
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
  GPU errors. An isolated native compute capture has no eye attribution; its
  preceding world observation cannot certify a later raster scene. Pipeline shader text must be the **actual final compiled variant**,
  not a guest program before rewrites. Root's native shader registry supplies the
  effective shader references; the legacy adapter accepts only its pinned
  previously reviewed shader families.
- Producer ledger `VersionRef { allocation_id, content_version, subresource }`
  maps directly to texture allocation identity and IR `Version`. Native adapters
  preserve captured initial contents when earlier producers are unavailable;
  they do not infer a closed dependency graph. Direct clear promotion requires
  exact native provenance and whole-image byte verification. The complete actual
  1,277-draw interval and a separate actual compute dispatch have been replayed;
  they are different captures, not one joined frame.

Supported runtime behavior includes vertex/fragment sampled textures, exact static
uniform/read-only storage buffer bindings, samplers, optional vertex slots,
indexed/nonindexed/instanced draws, color write masks and blends, depth/stencil,
viewport/scissor, sRGB views, 2D arrays/cubes and selected mip/layer views, BC raw
mips, ordered clears and equal-format full-color copies. Inputs are capped at
256 MiB and persistent allocation estimates/readbacks are also bounded. Selected
D1/D3 sampled textures retain their real dimensions. Compute supports readonly
buffers and distinct write-only D2 color storage views, with one selected mip/layer
per output and explicit before/after versions. Queries, writable storage buffers,
other storage access modes, dynamic binding offsets, binding arrays, multisampling,
indirect draws, depth copies or omitted operations are not silently
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

## Native compute and compute-to-raster oracle

`import-native-compute` admits schema 5 with an exact successful final module,
actual dispatch dimensions, resolved binding layouts and immutable selected
resource ranges. It preserves the original capture as `native-capture.json` and
uses the same hash-bound `native-import.json` contract as the wrapper. Unknown
operations, writable buffers, unsupported views, sampled/output parent aliases,
stale or reused native versions, and missing output transitions reject admission.
The original output's initial bytes are required even for a write-only shader,
because a shader may preserve unwritten texels. Workgroups are bounded to 65,535
per axis and 1,048,576 total.

`make-compute` authors an independent 13×7 CPU oracle. A compute shader reads a
sampled image and offset-256 uniform, writes mip1/layer1, preserves a border and
unselected subresources, then a raster pass consumes that exact produced version.
Five final comparisons and all six initial subresources match on Metal. A separate
actual game dispatch also matches all 115,200 RGBA16Float output bytes; see
`docs/NATIVE-COMPUTE-REPLAY-20261026.md` in the checkout for identities and limits.
Neither result establishes a complete finished frame or permits a density change.

## Executed-image contract with original omissions

Native schema 8 (`native_executed_finished_graph`) is a separate admission mode
from strict schema 7. It retains every finished endpoint, resource identity,
version, initialization and byte-comparison requirement, while recording one
narrow original engine omission as an ordered no-GPU event. The accepted case is
the unavailable control-point vertex shader `6edc9adb5e47f1e0`, topology 35,
direct indexed positive count/instances, identical requested/resolved words,
absent index/source, and zero pending pipelines or GPU errors. The original early
return precedes rendering preparation. Other skip reasons remain rejected.

Each record retains exact native command/event identity and raw draw arguments.
It creates no shader, buffer, texture or output. At most 1,024 records of 64 KiB
are admitted. Schema 6/7 reject both these records and the new top-level contract.
The producer must explicitly report `all_requested_draws_executed: false`, exact
`original_omission_count`, and
`execution_contract: executed_original_image_with_recorded_omissions`.

After successful original-density replay, schema 8 may report
`executed_finished_hud_free_image_byte_parity: true`; the existing
`finished_hud_free_graph_byte_parity` remains false. This proves reproduction of
the current native engine's executed image only. It does not restore omitted
effects, establish windowed/headset equivalence, certify stereo or accept a new
density path. Original and frozen capture bytes remain authoritative.
Use `import-native-graph` for strict CPU admission; it creates no GPU device.
`python3 -m unittest discover -s scripts/native-scene-replay -p test_run.py`
checks wrapper identity with its subprocess explicitly mocked, without Metal.

Schema 8 accepts only an explicit live staging/map receipt limit of 512 MiB or
1 GiB, with both observed peaks at or below that declared limit. Schema 7 retains
512 MiB only. This is a producer readback-staging admission limit, not a larger
render target or replay allocation budget: individual blobs remain at most
256 MiB, packed finished payload/import remains 1 GiB and logical persistent GPU
allocation remains 256 MiB. No limit is increased automatically.

## Exact float-to-depth proxy

Finished schemas 7/8 can retain the original `float_to_depth` helper passes.
Each selected source mip/layer is loaded by the exact native WGSL shader; its red
channel becomes `frag_depth` in a distinct Depth32Float allocation. The pass clears
to 1, writes with Always comparison, and has no color or stencil attachment. Native
full dimensions, mip count, layer count, selected indices and actual initial/final
bytes are preserved. Other graph schemas reject the operation.

`make-float-depth NEW_DIRECTORY` authors an independent finite Float32 oracle:
selected mip/layer isolation, neighboring contents, downstream depth sampling,
then all passes in native mip-major/layer-minor order. It tests exact in-range bits
and finite out-of-range depth behavior against CPU expectations. It makes no
NaN/infinity/subnormal or game-image claim. Creating/admitting it is CPU-only;
running it through `run.py` requires the serialized GPU slot.

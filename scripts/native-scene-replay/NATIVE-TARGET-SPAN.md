# Native target interval adapter (schema 4)

`native-scene-replay import-native /capture/capture.json /new/output` imports a
complete original-density fixed-target interval into the generic executor.
The interval starts at its original target-binding command and ends immediately
before the next one. Every executed draw, including its exact final emitted
arguments and resolved resources, must be captured. An unsupported command, skipped
draw, query or missing identity invalidates the whole interval. Ordinary natural
queue submissions may occur within it: the producer preserves their original
ordering and retains immutable snapshots through the final submission's completion.
A submission boundary does not authorize omission or replacement of any draw or
resource version. No successful prefix is accepted as complete.

The producer manifest has `schema: 4`, `kind: native_target_span`,
`status: captured`, source backend `Metal`, and
`normal_frame_publication_changed: false`. Closure requires:

- `complete_target_interval`, `first_executed_draw_captured`,
  `last_executed_draw_captured`, `initial_contents_captured`,
  `original_final_captured`, `natural_submission_completed`, `queries_inactive`
  all true; `unmodelled_commands` and `gpu_errors` zero.
- Original `interval_start_command` and `interval_end_command`, with every draw's
  `command_sequence` strictly increasing between them.
- Identical complete `world_view` and `target_domain` on every draw, including
  stage/camera/projection matrices, eye, epoch, request/head identity, presentation
  time and optics. No missing world metadata is synthesized.

`target_domain.attachments` contains unique roles `color0`…`color7` or
`depth_stencil`, actual `native_allocation_id`, selected `mip`/`slice` and view
`format`; `depth_flags` is explicit, including zero. Top `attachments` contains
all corresponding aspects, including targets no shader writes. Each has actual
full native `descriptor`, selected width/height, view format, mip/slice, native ID,
`initial_blob`, `final_blob`, `initial_version` and `final_version`.

Every texture version is the producer's exact
`{allocation_id, content_version, subresource:{mip,layer,aspect}}` reference.
Each draw's `writes` is an ordered array of `{before,after}` references. Conservative
native invalidations are accepted even if a write mask preserves bytes, provided
they affect bound writable aspects and include every actual write. No transition
initializes an unknown target. Input-material versions do not impose ordering on
unrelated attachment-write counters, including the explicit dummy namespace.

The optional lossless `draw_metadata_encoding: "shared_v1"` stores exact full
objects in `shared_draw_metadata.pipelines`, `.bindings`, `.world_views` and
`.target_domains` arrays. Encoded draws contain `pipeline_ref`, ordered
`binding_refs`, `world_view_ref` and `target_domain_ref` indices instead of their
four inline fields. All other draw metadata stays inline and unchanged. The
adapter rejects unknown encodings, mixed inline/reference forms, missing tables,
invalid index types/ranges and malformed records. It expands references before
existing semantic validation, while retaining the original encoded capture bytes
and their SHA in the frozen evidence. Expansion permits at most 4,096 draws,
4,096 pipeline/world/domain records each, 65,536 binding records, 256 bindings per draw and 262,144
expanded binding references. A conservative pre-expansion byte calculation,
including the raw manifest, is capped at 128 MiB. File/payload limits are unchanged.

The resolved draw payload extends the old capture shape:

- `pipeline` carries actual primitive/depth/stencil/multisample/target and vertex
  layouts. `effective_shaders` references the mandatory complete source registry,
  exact successful module SHA, entry point and default compilation options.
  Guest source before remap/flat/fold rewrites is insufficient.
- `draw` has explicit `kind` (0 direct, 1 indexed), `count`, `instances`,
  `first_vertex`, `start_index`, `base_vertex`, `start_instance`. Values are the
  final emitted arguments after VID/IID folding; unused direct/index fields are
  zero. Viewport and scissor are the actual resolved values.
- Buffer bindings and vertex/index inputs have authentic native allocation ID,
  immutable `content_version`, original `source_offset`, blob and normalized
  binding offset. Buffer revisions/snapshot ordinals have their own namespace.
  Different captured extents of the same native buffer remain distinct snapshots.
- Texture bindings carry actual native parent ID, full descriptor, exact view,
  and every selected subresource's mip/layer/aspect, raw blob, packed row bytes
  and VersionRef. Native allocation plus version identifies repeated materials;
  incompatible descriptors or differing bytes for one version reject the span.
- `uniform`, `buffer`, `readonly_storage`, `texture`, `sampler` bindings are
  interpreted through their typed layouts. Writable storage/dynamic offsets and
  other unsupported executor features fail admission.

Sampled D1, D2 and D3 native allocations retain their dimensions. D1 requires
height/depth one and one mip, matching the current native API. A D3 version refers
to the entire mip volume at logical layer zero; `depth` is mandatory on each
selected subresource and must equal the original depth shifted by mip. Packed
bytes include every z plane with the original row/image stride. Volume mip levels
are not recast as 2D array layers. D3 views require base array layer zero and one
logical layer. Mutable render targets remain D2; unsupported compressed or depth
formats on D1/D3 fail admission. Existing generic scenes without `dimension`
continue to mean D2. The full bound color domain is retained for every draw. A no-fragment pipeline
with an empty color-target list cannot execute in a color-bound pass in this
wgpu version; admission rejects that mismatch instead of dropping bound targets.

The adapter deduplicates identical complete pipelines (including binding layouts
and effective shader sources) and actual material allocations. It does not rebase
mips/layers or substitute dummy data. Unselected subresources remain explicitly
Unknown with version zero meaning *unobserved and unusable*, not a fabricated
native version. They cannot be sampled until authentic content is supplied.

Initial targets normally remain Captured. A direct `capture_start` ledger root
may instead become UniformClear only when its exact allocation, version,
subresource, view format and operation match a live native record, with no
contradictory missing provenance, and its complete original bytes agree. The
current byte oracle supports BGRA/RGBA8 UNORM endpoints, Float32 depth and 8-bit
stencil; unsupported formats or fractional color clears remain Captured. Uniform
pixels alone, a prior clear ancestor, or missing producer history never qualify.

Every promoted clear retains an optional `reference_blob` at the original extent.
The executor clears through the real API and reads the entire aspect back before
draws, preserving the original byte check. No density mode may silently strip this
reference. The actual v11b fixture promotes color0, depth and stencil; inherited
color1/2/3 remain Captured and the overall independent-initialization gate stays
false. Promotion evidence is recorded in `native-import.json`; unchanged original
ledger and image bytes remain in the frozen capture. Complete source-graph
initialization and final postprocessing remain separate work.

`native-import.json` binds the exact original capture bytes to the generated
`scene.json`; `run.py` verifies and freezes both alongside all payloads and source.
The generic `Native.source_sha256` field denotes the immutable capture document's
SHA, not a claim about source-code provenance. The external native build receipt
provides the source-code/binary binding. No GPU is created during import or CPU
validation. Original-density GPU replay still needs a serialized slot.

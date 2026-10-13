# Offline host command analysis

Run against a copied, complete schema1 command trace from the default-off host diagnostic:

```sh
python3 scripts/analyze_world_commands.py --trace /absolute/path/commands.json --output /absolute/path/new-analysis
python3 -m unittest discover -s scripts -p test_analyze_world_commands.py -v
```

The output directory must be new and its parent must exist. No file is overwritten. Input is bounded to17MiB; each output file is bounded to64MiB. A rejected input creates no analysis directory. A persistence/output-cap failure can leave a partial new directory; the CLI exits2 and does not report success.

`timeline.json` contains every retained input command in order, compact decoded details, source camera markers, potential accesses and boundary candidates. `timeline.csv` provides ordinal/opcode/marker and candidate summaries. `graph.json` lists analyzer-local resource lifetimes and potential dependency edges. A SHA256 binds the exact input file. No executable, game assets, GPU, device, simulator or native runtime is needed.

The analyzer rejects a truncated prefix, omitted records, ordinal gaps/duplicates, inconsistent counts, incomplete raw operands, malformed extents, unsupported schema/state, duplicate JSON keys and missing inherited state. It requires exactly two inclusive present endpoints and consecutive game-present counters. These checks establish the observed host command interval only. Unqualified closing markers and different endpoint epochs remain explicit limitations; neither interval order nor matching camera epochs certifies a frozen simulation scene.

Opcode decoding follows the current `src/gpu.rs` executor: texture/buffer/shader/state/layout/view creation, destroy, all bindings, render/depth targets, uploads, clears, draw/dispatch, copies/resolve, query/readback and wrap/wait/no-op commands. Unknown opcodes remain visible and make candidate-access reconstruction incomplete. Payload extents are checked against raw operands. Partial or missing fingerprints are separately reported; a fingerprint is not resource-content certification. Raw operands may already contain complete inline payloads even when a budget prevented hashing them.

Each observed create or replacement advances an analyzer-local generation. A destroy retires it. Resources that existed before the opening present retain an unknown generation. Potential write versions advance once per resource per command; reads precede writes, preserving self-copy and render-target load/blend dependencies. These versions describe possible host-op effects, not successful GPU writes. Initial contents, aliases, subresource coverage, async readbacks/query results and native implicit work remain unknown. Host configuration inputs use a separate `possible_host_configuration_read` label; bound SRVs/UAVs are conservative possibilities, without claiming shader consumption.

Each possible write also records its previous potential version/writer with `prior_contents_may_survive=true`. A partial upload, copy or clear can preserve earlier contents. This conservative chain does not establish a real GPU read, full overwrite or subresource coverage. The graph cannot authorize dead-pass or write elimination.

Views retain a numeric resource-ID mapping. Texture SRV/UAV paths can also retain a native TextureView cache whose key does not establish the allocation generation. Across ID reuse, edges therefore identify the current raw-ID candidate, possible prior lifetimes and an unresolved cached native allocation. Buffer lookup candidates are labeled separately; native binding allocation remains uncertified. Captured buffer identities can expose a conflicting lifetime change, but do not grant immutable ownership.

Limits include32768 lifetime nodes,200000 retained graph edges,128 displayed accesses per command and256 issue examples. All decoded accesses still advance potential versions when the per-command display cap is reached. Edge/display omissions are explicit. Edge-list retention, displayed access completeness and host candidate reconstruction are separate fields; `native_resource_graph_complete` and `snapshot_complete` always remain false.

Target/shader/camera transitions, requested blending and depthless draws are boundary candidates only. Blending does not establish transparency, HUD or first-person foreground. A render target's previous contents can remain relevant through load, blending and partial coverage. The analyzer performs no alpha subtraction, foreground extraction, pixel reconstruction, render replay or stereo-pair certification. Physical paired capture and qualified draw-family/pass evidence remain necessary.

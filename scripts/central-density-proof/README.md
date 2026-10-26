# Same-eye central-density proof

This standalone local Metal fixture executes two pinned game shaders with a privately captured draw. It implements the smallest rendering step toward separate high-density center and low-density guard images. It does not link or start the game, alter app rendering, publish a world frame, access saves/settings, or use a device/simulator.

The source draw, complete interleaved vertex/index buffers, eight constant buffers, original camera/projection and shader WGSL are read from a supplied private capture. The capture and both shader sources must match hard-coded SHA-256 admission hashes. No game assets or shader contents belong in this directory or Git.

```sh
sh scripts/central-density-proof/run.sh /absolute/private/frozen-game-draw.json target/central-density-proof/run --cpu-only
# After reserving the local GPU slot:
sh scripts/central-density-proof/run.sh /absolute/private/frozen-game-draw.json target/central-density-proof/metal
```

The CPU-only route validates capture, shader hashes, matrix contract and same-eye preservation without creating a GPU instance. The Metal route uses the repository's pinned wgpu 30.0.1 and local wgpu-core patch. Each run writes its own receipt and hashes; keep output directories distinct.

## What is retained and what is declared

Retained from the hash-admitted capture:

- Shader hashes `6a6a0898a51cf403` / `75cf2d53bf9497c9`, original WGSL and all active CB bytes.
- 1,506 indices, 502 triangles and the full 36-byte interleaved vertex stream. The shader uses locations 0–3 from the retained stream; the inactive instance layout entry is not required.
- Source eye 0, original `game_from_eye`, projection, request identity and reverse-Z convention.
- Original clip-CB Z/W components (rows 2/3 across all columns) and camera row; center rendering changes only clip X/Y.

The capture did **not** retain its sampled texture, sampler, complete raster/depth/stencil state or initial target contents. The fixture declares a deterministic synthetic gradient/checker mip chain, a linear/trilinear repeating sampler, four cleared BGRA8 MRTs, RGB-only writes for MRT0, no blending/culling, D32FloatS8 with GreaterEqual reverse-Z, and retained clear stencil. These are explicit fixture inputs, not claims about original game state. G-buffer previews are diagnostic values, not display-ready game imagery.

## Rendering and independent reference

1. Render the same wide draw twice at 640×360. Exact all-attachment equality tests standalone duplicate-render determinism. It is **not** independent parity with the original natural game renderer.
2. Test the physical 3× center crop. This captured draw lies outside it, so the expected control has zero covered samples.
3. Render a declared nested 3× region around the captured draw. Its center is snapped so the corresponding high-resolution crop origin is even on both axes. This preserves 2×2 fragment-quad phase for implicit derivatives.
4. Independently render the **unmodified original shaders and constant buffers** across a 1920×1080 reference, then crop the corresponding 640×360 rectangle. This is the primary density oracle. The temporary larger target is confined to this test and does not change game resolution.
5. Separately compare with a narrow-projection reference built from `P_narrow × inverse(game_from_eye) × object`. Report original Float32 producer rounding explicitly. A secondary calculation carries that measured source residual; it must not be substituted for the stronger unmodified-shader raster oracle.

The primary oracle requires exact coverage and every byte of all four MRTs and stencil. Depth alone permits at most two Float32 ULPs: clip Z/W and camera are unchanged, but equivalent raster positions can round depth interpolation differently after the clip-X/Y transformation. The receipt reports both byte differences and the actual maximum ULP/absolute error; it never labels approximate depth as bit-identical. The initial aligned run observed one ULP (`2.33e-10`) with every color/stencil byte equal. The nominal sample-density ratio is 3× per axis (9× area); covered-pixel counts for a tiny triangle set also depend on edge quantization.

The first odd-X crop control is retained under `target/central-density-proof/metal1`: coverage agreed, but implicit-derivative sampling differed by up to 12 RGB bytes because the reference and candidate fragment quads started on different pixel phases. This is a discovered **regional-rendering contract**, not a claimed live GTA defect. Never silently move or filter an odd-origin region and call it reference parity.

The current experiment handles one draw with synthetic material. It does not provide a completed dual-density game image, prove game LOD/culling coverage, preserve an entire post-processing chain, or improve production performance. A future region producer must preserve those properties and retain independent per-eye region color/depth/projections through completion before any app publication is eligible.

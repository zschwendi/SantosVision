# Original unsupported tessellation draw — October 27, 2026

The local finished-image investigation found an actual, nonzero draw request whose vertex shader is absent from the supplied translated shader set. This is a shared native-renderer capability limit, not evidence that immersive selects an unfinished post-processing target. Matching windowed-scene occurrence and the omitted draw's pixel visibility have not been established.

In `target/native-draw-capture/actual-finished-v19`, native present 501, command 2,502,847 requests indexed drawing with 5,742 indices, one instance, start index/base vertex/start instance zero. Topology 35 is a three-control-point patch list. Vertex shader `6edc9adb5e47f1e0` is cached as unavailable. Pending pipeline jobs, pending cached pipelines and GPU errors are all zero at this seam. The original function returns before foreground/HUD preparation, depth proxies, pass creation, capacity clamping or drawing. Consequently the request is nonzero, but valid buffer fetches and pixel coverage are not proved.

The original game asset contains the shader. In the pinned `data/common/shaders/win32_40/normal_diffspec_detail_dpm_tnt.fxc`, the 2,260-byte DXBC chunk at offset 56,540 has exactly that FNV-1a64 hash and the original name `VS_northVertexInputBump_PNTri`. Its outputs are seven control-point user semantics, with no `SV_Position`. The associated hull and domain stages are also present in the original asset, but their translated-index records explicitly report unsupported stages. The domain shader requires original matrix/material constant buffers and tint/height texture bindings. The current native adapter retains only VS/PS/CS stages and cannot execute the required hull/domain path.

A faithful repair therefore requires original control-point, hull and domain execution with their actual bindings, plus tessellation or an independently verified equivalent. A vertex-shader alias, ordinary triangle rendering, or turning off tessellation would not preserve that effect. None was applied. The graphics seed and effective settings retain Tessellation=1 and are byte-identical, SHA256 `3acd159e5195c30b4347ce12919170d2441e76a9016cfa9131f37bdc862a80ea`.

## Verification changes

The private driver previously never enabled the existing HUD frontend readiness flag. That independently prevented the required `HudImages.world` endpoint from being available. Finished-image modes now enable the existing flag before native startup and write a create-only receipt of observed readiness. Other modes keep their prior readiness state. This corrects the fixture, not app rendering.

All 14 original skipped-draw paths now retain exact reasons in private capture builds. The original renderer's return paths, counters and HUD rejection remain unchanged. Requested draw words are recorded before indirect fallback; effective direct arguments are recorded only when available. Unresolved indirect buffer identifiers are not presented as element counts. Failure records are bounded and create-only.

Preserved captures:

| Run | Frozen producer SHA256 | Result |
| --- | --- | --- |
| v18 | `e1d17bac4d649c4c1c252004472d704923b191ddf99926afc57d3d56330e06a1` | Exact missing-VS branch; HUD readiness observed true |
| v19 | `138f96a5cf53c01437cedb6e2004dac90a77b1bb1315bbbf4486119f1ae11e59` | Same missing-VS branch with actual 5,742 × 1 request |
| executed v20 | `f443a4cfd63d5933fe7f327ada2396594673a31205ce81d3281ba5d45ff8f36e` | Reached the private staging limit before its first natural submission |

The v18 and v19 children exited and were reaped; original input hashes stayed unchanged. Neither run produced an accepted finished-image capture. Each run's `root-verification.json` binds its logs, rejection, frontend receipt and lifecycle. Original DXBC extraction and independent source review are under `target/native-missing-shader-review/` and `target/missing-tessellation-shader-audit/`.

The separate executed v20 child also exited and was reaped with original inputs unchanged and no accepted image payload. Its rejection records 503,472,192 live staging bytes plus a 29,491,976-byte next snapshot and 29,491,200 reserved final bytes, exceeding the previous 536,870,912-byte ceiling. This is private capture memory, not an app resolution or performance measurement.

## Two distinct replay claims

Strict `finished` capture (schema 7) continues to reject an omitted original draw. A separate private `executed-finished` contract (schema 8) is being verified for reproducing the pixels the existing engine actually executed. It must record the known omission explicitly in global command order and report `all_requested_draws_executed=false`. It supplies no replacement shader, performs no omitted draw, and cannot certify that the tessellation effect was rendered.

Only the demonstrated early absent control-point shader branch is eligible: exact hash, stage, topology, direct indexed positive counts, equal requested/effective words, absent index/source, cached failure, no pending pipelines and no GPU errors. Other skip reasons still reject. The finished HUD-free endpoint and accepted individual-eye publication checks remain required. Pixel parity is not accepted until the actual complete capture, strict import and all final/intermediate byte comparisons succeed.

The signed **V build 28** front-glass package remains unchanged and uninstalled. No headset, iPad, simulator, settings change, original-asset edit, save replacement or deployment occurred. Central source sampling loss and hardware flicker/latency remain separate unresolved acceptance items.

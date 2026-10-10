# Proposed game rendering bridge

Status: **unimplemented**. `ImmersiveBridge.h` declares a proposed future C ABI; it is not compiled or imported by the diagnostic. No game FFI symbol is called or linked. A passing diagnostic build does not establish that the existing Muguet runtime can satisfy this contract.

The desired GTA experience requires the engine to render surrounding 3D world geometry from the player's first-person position. Both camera orientation and position must incorporate the predicted headset pose, with independent stereo eye offsets, projections, color targets and matching depth. Displaying the existing flat game image on a screen or panorama cannot satisfy this contract.

## Frame ownership and timing

1. Swift owns the `ARKitSession`, `WorldTrackingProvider`, `LayerRenderer`, frame lifecycle, command queue and final presentation. It stops submitting when paused or invalidated and stops ARKit when the test closes or app backgrounds.
2. After `optimalInputTime`, query a tracked device anchor at the drawable's `frameTiming.presentationTime`. Convert `LayerRenderer.Clock.Instant.epoch.duration(to:)` to seconds. Set that same anchor as `drawable.deviceAnchor`. Do not reuse an untracked or stale pose to claim head tracking.
3. Advance game simulation once per frame, before eye encoding. Every eye and recording target must read the same simulation snapshot. A second eye render must not run a second simulation step or consume input again.
4. The Swift host creates and owns the command buffer. All texture and command-buffer addresses in the C struct are borrowed Objective-C object pointers valid only for this synchronous call. The engine may encode into the supplied buffer but must not retain resources for a later frame, present, commit, or wait for it. Swift calls `drawable.encodePresent(commandBuffer:)`, commits, and balances `startSubmission` / `endSubmission`.
5. The bridge must reject unknown `abi_version`, incompatible `byte_size`, unsupported pixel formats, missing depth, or too many views. A nonzero result keeps game launch unavailable and supplies an actionable error to the UI. No fake success fallback.

## Coordinate and destination contract

Matrices are column-major `simd_float4x4`, multiplied by column vectors. `stage_from_head` is the ARKit `originFromAnchorTransform`, in meters. Its convention is right-handed: +X right, +Y up, -Z forward. `game_from_stage` must explicitly combine player position, locomotion orientation, recenter offset, engine axis conversion and unit scale; do not overwrite a rendered game camera with raw headset translation. The bridge should expose first-person camera control without changing simulation state per eye.

For a drawable view, compute `stage_from_eye = stage_from_head * view.transform`, then `game_from_eye = game_from_stage * stage_from_eye` and `eye_from_game = inverse(game_from_eye)`. The projection is the actual `drawable.computeProjection(convention: .rightUpBack, viewIndex:)`. Engine eye-space conventions must agree with that projection. There is no hardcoded IPD or assumed symmetric field of view.

Use each view's `textureMap.textureIndex`, `sliceIndex` and `viewport` rather than assuming separate 2D textures. Support dedicated, layered, and shared layouts, or negotiate one supported layout before launch. Shared texture passes clear a texture once and load it for the second viewport. Layered passes target the assigned array slice. The current diagnostic disables foveation; a future game implementation must negotiate and handle rasterization-rate maps before enabling it.

Color and depth refer to the exact compositor textures. The depth range is reverse-Z: near is `depthRange.y`, far is `depthRange.x`; clear depth to zero, compare greater-or-equal, write actual scene depth in the projection's convention, and store it for compositor reprojection. A flat-depth approximation does not meet the surrounding world requirement.

`SV_IMMERSIVE_MAX_VIEWS` is a versioned upper bound, not a promise of two views. Each drawable may target the headset or recording output. Handle supported targets separately and reject unsupported view counts explicitly. The header's proposed `target` values are bridge-defined, not raw CompositorServices enum values.

## Remaining implementation

The Rust/game runtime still needs native visionOS compilation, compatible Metal destination import, an immutable frame snapshot, camera override hooks inside the loaded game, per-eye world rendering and depth export. A compatible game ZIP is also required. None of those game capabilities is supplied by the calibration app.

Primary references: [Apple's Metal immersive rendering guide](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), [predicted device pose](https://developer.apple.com/documentation/arkit/worldtrackingprovider/querydeviceanchor(attimestamp:)), and [per-view projection](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/computeprojection(convention:viewindex:)). API names were also checked against the installed Xcode 27 SDK interfaces and headers.

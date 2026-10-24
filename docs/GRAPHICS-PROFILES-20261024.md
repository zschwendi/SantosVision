# Explicit graphics quality profiles

The native launcher and both settings screens offer Existing GTA, Performance and Clarity. The selected profile is a one-time request, recorded separately from the actual XML values in runtime status. Unchanged launches preserve subsequent GTA edits, except that MSAA remains zero for the current immersive depth path.

Clarity changes only TextureQuality to 2, AnisotropicFiltering to 16, DoF to false and MotionBlurStrength to 0.000000. It preserves the current ShaderQuality, FXAA, shadows, reflections, SSAO, tessellation, LOD and other choices. In particular, the copied physical settings with ShadowQuality 2 are not reduced to the low baseline. Performance explicitly applies the established low quality values. Existing GTA preserves current quality; switching to it does not restore a previous backup. Legacy `low` selections migrate to Existing GTA because the old launcher preserved existing XML.

Resolution and the 30/60 frame cap are independent. The built-in 960×540 / 60 defaults remain unchanged; candidate20's proposed 2560×1440 / 60 / Clarity selection is a persisted launcher choice. With no prior marker, an explicit Performance or Clarity request also applies its requested video dimensions; Existing GTA adopts existing XML. A missing XML receives complete inexpensive defaults, then the selected Clarity changes if requested. A frame cap is not a measured frame rate guarantee.

Every changed existing XML gets a byte-exact, flushed backup. XML replacement and marker replacement each use a flushed temporary file and atomic rename. Validation finishes before either write, and failed temporary replacement removes its temporary file. The existing transactional mechanism is retained. If interrupted between XML and marker replacement, the pending profile can be applied again safely. Runtime status reports `graphics_preset`, `quality_action`, changed tags, backup path and observed `actual_graphics_values`, rather than claiming the selection describes all current quality.

## Source qualification

Original game WASM SHA-256: `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`.

The original passive-data metadata array at 5470448 names index8 `AnisotropicFiltering`. Its matching offset array at 0x5376f0 assigns offset40. Original65672 (`CSettingsManager::ApplySettings`) reads settings graphics+40 at 0x2059deb–0x2059df0, then forwards the raw value to the sampler descriptor at local4+32 (0x2059fbc–0x2059fc0). The descriptor begins at local4+12 (0x205a014–0x205a018), making this its MaxAnisotropy field. There is no enum conversion: XML 16 requests 16×. The host sampler in `src/gpu.rs::make_sampler` uses that raw field and clamps it to 1…16. TextureQuality2 is within the original accepted range. Clarity preserves the already qualified ShaderQuality1 / `win32_40` choice and does not enable MSAA or higher shader variants.

## Verification

`cargo +1.98.1 test native_graphics_tests --lib`: 15 passed. Tests cover profile transitions, exact preservation of the copied quality values, later user edits, missing-marker video application, missing XML defaults, invalid/malformed no-write behavior, byte-exact backup and no leftover temporary files. `NativeGraphicsSettingsValidation.swift`, compiled with Swift6: 83 checks passed, covering every profile/resolution/cap combination, CLI selection, legacy migration, invalid files and preserving the prior file on an invalid save. `git diff --check` passed. No AOT, GPU, renderer, device or archive changes are part of this scope. Physical quality and performance acceptance remain with the parent candidate20 work.

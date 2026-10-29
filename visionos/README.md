# V native visionOS integration

For a first installation, start with **[From zero to V on Vision Pro](../docs/INSTALL-VISION-PRO.md)**. It covers the repository-local Rust toolchain, compatible game package, signed build, data transfer, and first launch.

## Build modes

| Mode | What it builds |
| --- | --- |
| `./make-visionos.sh game device` | Physical-device native game shell, Rust runtime, embedded visionOS AOT code, game window and immersive renderer. |
| `./make-visionos.sh game simulator` | Separate arm64 simulator game build; not interchangeable with a device archive. |
| `./make-visionos.sh diagnostic device` | Head-tracking/calibration app without the GTA runtime. |
| `./make-visionos.sh diagnostic simulator` | Simulator calibration app. |

Without signing variables, the helper builds unsigned and does not install or boot anything. For a signed physical-device game build, provide `TEAM`, `BUNDLE_ID`, and `MUGUET_GAME_WASM` as shown in the installation guide. The default signed route uses local saves. Set `MUGUET_CODE_SIGN_ENTITLEMENTS` only when deliberately using an entitlement file compatible with your signing team.

`MUGUET_GAME_WASM` selects the source module. `MUGUET_RUNTIME_ARCHIVE` can instead select an already-built archive; it must match the destination and embedded game code. The default development module path under a sibling `game-assets/` directory is a maintainer convenience, not a fresh-clone prerequisite.

The native helper compiles host tools with JIT enabled for ahead-of-time preparation, then the device library with `--no-default-features`. The final game link includes `SV_GAME_RUNTIME`, `RuntimeBridge.h`, the force-loaded native archive, and executable protection on `__GAME`. These are command-line build settings; the generated Xcode project alone does not retain them.

Build products are under `target/visionos-xcode/DerivedData-<mode>-<destination>/`. The Xcode configuration is named Debug, with `-O` Swift optimization in game mode. Use `MUGUET_SWIFT_OPTIMIZATION=-Onone` only for intentional Swift debugging.

## Runtime and presentation

The app starts in the native game window. Its embedded-module check, complete game-data installation, engine startup, and first displayed game frame are separate milestones. After the world loads, the expand control opens third-person immersion using the same game session.

The immersive renderer consumes independently retained eye color/depth/projection snapshots and uses ARKit / CompositorServices for tracked presentation. Entering immersion is not equivalent to rendering the same source image for both eyes. Preserve the eye/lease relationship when changing this code.

Data lives under `Documents/game`; local saves and graphics configuration live separately under `Documents/saves`. The current visionOS launcher has no ZIP picker. Use the actual host importer and copy its prepared result as documented in the installation guide.

## Current graphics behavior

Fresh settings are **960×540**, a **60 FPS cap**, and **Existing GTA**. Presets are **Existing GTA**, **Performance**, and **Clarity**. The 2560×2304 tall source is experimental. Existing user settings are preserved; the old “always force Low” description is historical.

Choose launch settings before Play. Quit and reopen V before changing an active game's launch configuration. A cap is not an FPS measurement; overlapping GPU command intervals must not be inverted into game FPS.

## Development evidence

- [Third-person fidelity checkpoint](../docs/THIRD-PERSON-FIDELITY-20261025.md): current accepted evidence, remaining image-quality limits and next hardware comparison.
- [Corner-support correction](../docs/IMMERSIVE-CORNER-SUPPORT-20261027.md): bounded local before/after and fallback/performance limits.
- [Graphics profiles](../docs/GRAPHICS-PROFILES-20261024.md): preservation and profile semantics.
- [Scene return](../docs/SCENE-RETURN-STABILITY-20261024.md): window/immersion lifecycle.
- [Save sync](SAVE-SYNC.md): local archives, optional iCloud provisioning and data preservation.
- [October 10–11 integration archive](../docs/VISIONOS-INTEGRATION-HISTORY-20261011.md): older simulator measurements and debugging instructions, retained as historical evidence.

Do not use historical diagnostic switches or experimental flags as a player's default installation recipe. A successful source build, an offscreen image comparison, or a simulator run does not certify a physical headset result. Device testing follows the current task's authorization.

Apple references: [CompositorServices](https://developer.apple.com/documentation/compositorservices), [Metal immersive rendering](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), and [ARKit](https://developer.apple.com/documentation/arkit).

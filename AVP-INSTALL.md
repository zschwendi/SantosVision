For the IPA route and its current limits, see [sideloading](docs/SIDELOADING.md).
The guide below covers building from source.

# Install V on Apple Vision Pro

V is the native Apple-platform GTA V port built on
[Muguet](https://github.com/c22dev/muguet). It supports windowed play and
experimental third-person immersion. The remaining presentation bugs are in
immersive mode; see [current status](README.md#graphics-and-immersion).

## Requirements

- An Apple silicon Mac with Xcode 26 or newer and the visionOS SDK.
- Apple Vision Pro running visionOS 26 or newer, paired with Xcode with
  Developer Mode enabled.
- Your own Apple signing team and bundle identifier, plus a compatible gamepad.
- Git, Python 3, XcodeGen, and repository-local Rust 1.98.1.
- Space for your game ZIP, prepared data, and build output. The previously
  tested ZIP is about 21.2 GB and its prepared data about 15.7 GB; the headset
  needs the prepared data, app, and additional working space.

## Game files

Supply your own compatible GTA V **web-port package**. It must contain
`data/manifest.json`, `b/<build-id>/game.wasm`, and the matching
`b/<build-id>/shaders/`. This repository contains no game files.

A retail PC installation or console disc image cannot be imported directly.
This project does not include a retail-to-WASM converter. Use only a compatible
package you have the right to use; see [Game files](docs/GAME-FILES.md).

From the repository root, validate your package:

```sh
python3 scripts/validate-game.py /absolute/path/to/game.zip
```

Use that same package for compilation and the later data import.

## Build from an existing checkout

If you came from the AVP Ports Index, keep the exact scanned commit it checked
out. **Start at step 3 of [the full Vision Pro walkthrough](docs/INSTALL-VISION-PRO.md#3-set-up-the-mac-tools)**,
skipping its source-clone/update instructions. Do not switch branches or pull
newer source as part of this installation.

1. Set up the repository-local Rust toolchain as described in step 3.
2. Follow step 4 to import your validated ZIP into a new Mac staging folder.
   Record its actual build directory and set `V_WASM` to that build's
   `game.wasm`.
3. Follow step 5 to pair the headset and set `V_TEAM` and `V_BUNDLE_ID`
   to identifiers you control. The documented build uses local saves.
4. Build and sign the game app using those variables:

```sh
TEAM="$V_TEAM" BUNDLE_ID="$V_BUNDLE_ID" \
  MUGUET_GAME_WASM="$V_WASM" \
  ./make-visionos.sh game device
```

The app is written to
`target/visionos-xcode/DerivedData-game-device/Build/Products/Debug-xros/SantosVision.app`.
Use **game** mode: a diagnostic build is a tracking scene, not GTA.

## Install on Apple Vision Pro

Follow [steps 7 and 8](docs/INSTALL-VISION-PRO.md#7-install-v-on-the-correct-vision-pro)
to select the physical headset, install the signed app with `devicectl`,
and copy the prepared game folder to the same app's `Documents/game`.
Wait for the entire transfer to succeed. The Vision Pro launcher has no ZIP
import button; installing the app alone does not supply the data.

Open **V**, pair a gamepad, and choose **Play**. Once the game world is loaded
and GTA's pause menu is closed, use **Enter immersive view** in the upper-right
corner. Follow [first launch](docs/INSTALL-VISION-PRO.md#9-first-launch-window-first-then-immersion)
for permissions and graphics settings.

## Controls, limits, and updates

Use a conventional gamepad. Immersive image quality, depth edges, flicker, and
performance remain unfinished; Sense motion controls and hand tracking are
not the supported setup path. Windowed play is the current visual reference.

Back up app data before updates. Keep the same signing team and bundle ID and
install over the existing app to preserve saves. Do not delete the app or
replace `Documents/saves` to recover a data-transfer problem. See
[the walkthrough](docs/INSTALL-VISION-PRO.md#troubleshooting) for troubleshooting.

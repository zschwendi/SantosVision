# Muguet
A native Mach-O iOS/macOS port of GTA V, with multiplayer

## SantosVision fork — visionOS work in progress

The requested goal for this fork is GTA V surrounding the player with head tracking
and stereoscopic world rendering on Apple Vision Pro. The inherited game runtime
still targets iOS and macOS. Full immersive GTA gameplay is not implemented or verified.

- Game input is a compiled **GTA V web-port ZIP**, with `data/manifest.json`,
  `b/<build>/game.wasm`, `b/<build>/shaders/index.json` and matching `.wgsl` shaders.
  A retail PC installation or console ISO/ROM does not supply that WebAssembly build.
  The repository includes no retail-to-web-port converter.
- Run `python3 scripts/validate-game.py /path/to/game.zip` before building.
  See [game-file requirements](docs/GAME-FILES.md).
- The [full-game-v1 snapshot release](https://github.com/alihammoud21/playgta5/releases/tag/full-game-v1)
  provides 14 independent game archives. `scripts/assemble-game.py` verifies them
  and combines their game files into one Muguet ZIP; the linked game-file guide
  covers the required metadata and two known source-manifest repairs.
- `python3 scripts/precompile-visionos.py /path/to/game.zip` uses a built host
  compiler to produce visionOS AOT code. It does not create a playable app.
- `cargo build --locked --bin inspect-game`, followed by
  `target/debug/inspect-game /path/to/game.wasm`, inventories actual function
  names and signatures without executing the game, for camera-hook research.
- The native target in `visionos/` is a separate **head-tracking calibration**
  checkpoint. It contains no GTA engine or game assets. See its README for build
  instructions and the remaining integration work.

On October 10, 2026, the real snapshot module (build `8b0b5899ed`, SHA-256
`11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`)
passed read-only WASM validation and Muguet hook inspection, and compiled for
`aarch64-apple-visionos` with this fork's host compiler. These checks do not
establish game launch, shader compatibility, stereo rendering, or headset tracking.

The upstream instructions below describe the existing iOS/macOS port.

## How does this work ?
this whole port relies on the WebAssembly compilation of the game. in a browser, that `game.wasm` runs inside the JS engine, behind WebGPU, Web Audio, web workers...
Muguet throws that layer away and runs the same `game.wasm` as a regular native app, with [wasmtime](https://wasmtime.dev) for the code and [wgpu](https://wgpu.rs) on Metal for the graphics

On macOS, the game code is compiled to native arm64 the first time you launch it, then cached. On iOS, we can't properly do that (i dont rlly want you to deal with the hassle of enabling JIT), so the game code is compiled when you build the app and baked into it

On top of that, Muguet adds a small UDP multiplayer (for other people who have the same port to play with you)

## Game files
Because I care about me not getting sued, you will need your own copy as a `.zip` : the folder holding `data/` and `b/<build>/` (with `game.wasm` and `shaders/`)

It's basically the same source as the web port.

Muguet itself will then asks for that zip

## Requirements
- Apple Silicon Mac, macOS 14 or later
- Rust toolchain
- Xcode 26 or later

For iOS, also :
- `rustup` with `rustup toolchain install 1.98.1` and `rustup target add aarch64-apple-ios --toolchain 1.98.1`
- `xcodegen` (`brew install xcodegen`)
- an Apple developer account
- a device on iOS 15 or later, ideally 8 GB of RAM

## Building
macOS :
```
./make-app.sh
```
gives `build/Muguet.app`

iOS :
```
TEAM=<your team id> ./make-ios.sh game.zip --install
```
`--ipa` makes `build/Muguet.ipa` instead. With a free account, add `FREE=1` (+ a `BUNDLE_ID` of your own)

Please install this app with AltStore. This probably won't work well with Sideloadly (or even SideStore, as per [this issue])(https://github.com/SideStore/SideStore/issues/1616)

The app also works with a paid developer account.

## Multiplayer
One player hosts from the launcher (or runs `muguet-server` on any machine), the others join with that address. UDP port 47520

## Limitations
- iOS needs the increased memory limit entitlement
- Minimap doesn't show
- Story missions are single player only


Contributions are welcome !

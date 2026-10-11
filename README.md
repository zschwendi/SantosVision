# Muguet
A native Mach-O iOS/macOS port of GTA V, with multiplayer

## SantosVision fork — visionOS work in progress

The requested goal for this fork is GTA V surrounding the player with head tracking
and distinct views for each eye on Apple Vision Pro. The fork now has a native
visionOS SwiftUI/Rust/Metal runtime, with the actual game executing in the visionOS
simulator. Actual compositor captures now show the immersive world, using real
scene color and depth with simulated head motion. The first performance target is
a smooth 60 FPS. Physical head tracking, binocular stereo and headset performance
have not been verified.

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
- `./make-visionos.sh game simulator` builds the native game shell and signed
  AOT runtime; `game device` builds for a physical headset. Provide the compatible
  WASM module through the build helper and matching imported data in app Documents.
  Game assets are not included in this repository.
- The native target also retains a separate **head-tracking calibration** mode.
  See [visionOS build instructions](visionos/README.md) and the
  [current runtime audit](docs/VISIONOS-RUNTIME-AUDIT.md).

On October 10, 2026, the real snapshot module (build `8b0b5899ed`, SHA-256
`11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`)
passed WASM validation and hook inspection, compiled to ARM code, and ran in the
owned visionOS simulator. Source captures show actual outdoor gameplay and the
standard third-person camera with reference-relative head translation/rotation.
The camera hook preserves the game's selected primary camera, including cutscenes;
near-camera geometry fades between 45 and 15 cm. The simulator exposes one view;
earlier source gameplay was roughly 10–13 FPS at 1280×720. The v5i run kept
publication active through gameplay and its actual compositor readback contains
the reprojected world. Uncovered regions and black outlines at depth discontinuities
remain visible. This is a prototype; hardware stereo and performance remain untested.

The native launcher now defaults to **Low, 960×540, with a 60 FPS cap**. It allows
640×360, 960×540 or 1280×720 and a 30 or 60 FPS cap before starting the engine;
quit and reopen to change settings after startup. Low reduces textures, shadows,
reflections and grass to level 0 and disables antialiasing, SSAO, tessellation,
depth of field and motion blur. Shader quality remains at the engine's level-1
minimum. The saved settings are backed up before modification. The cap is a
limit, not a measured frame rate. A six-minute 960×540 test averaged 18.5 game FPS
in its warmed intervals and did not repeat the earlier simulator crash. A
three-minute 640×360 comparison averaged 29.8 game FPS in its warmed intervals.
These one-view simulator tests do not establish smooth 60 FPS or headset stability.

Native rendering changes are built into the app and apply to compatible imported
game data; they do not rewrite the ZIP, WASM module or cached game AOT. The first
reusable buffer-upload experiment passed correctness checks but reduced simulator
source FPS: about 21.7 versus 38.7 and 31.3 in the original-path runs bracketing it.
The original queue upload path remains the default. Experimental 64 KiB/1 MiB
staging pages and a conservative Metal optimization that skips empty transition
command buffers are available for comparison. All 46 integrated Rust tests pass,
including actual Metal initialization, discard, query and upload-order checks.
The Metal toggle measured 31.75 FPS off, 34.47 FPS on and 37.70 FPS on the off
repeat, so its apparent gain was not consistent. A 64 KiB public-pool attempt
failed during guest texture loading before reaching the immersive world and
does not provide a world-FPS result. Both experiments remain off by default.
The Metal change removes an eligible command buffer in those focused tests;
that result alone does not establish a gameplay speedup. See the
[runtime audit](docs/VISIONOS-RUNTIME-AUDIT.md) for measured performance and limits.

A further source experiment reuses small Metal upload allocations inside the
original queue encoder, without adding submission or wait triggers. It is
controlled by `MUGUET_METAL_INTERNAL_UPLOAD_POOL=1` and remains off by default.
Both modes pass the 46 app tests, eight focused Metal tests and ten allocator
lifetime/concurrency tests. The updated simulator and device apps build and
contain the same game AOT; the device build has not been installed on a headset.
Its Low 640×360 simulator sequence measured 17.17 FPS off, 37.52 FPS on and
32.38 FPS off again. The enabled run exercised real reuse, but differing scenes
and command workloads prevent attributing the FPS difference solely to it.
It remains experimental; smooth 60 FPS is unverified.

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

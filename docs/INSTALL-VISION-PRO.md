# From zero to V on Apple Vision Pro

This is the source-build walkthrough for **V**, our iOS/iPadOS/visionOS take on the GTA V WASM port. It ends with the native app installed, matching game data in its container, and the ordinary game window ready for a first launch.

**Immersive third-person mode is experimental.** Installation does not certify image quality, performance, or full-game compatibility. Start in the window; try immersion after the game world is running.

## The route at a glance

**Compatible game ZIP → validate → prepare data on the Mac → compile and sign V → install on Vision Pro → copy data → Play → expand into immersion.**

The current Vision Pro launcher has **Play** and **Settings**, but no ZIP picker. The Mac-side import below is required. iPhone/iPad use a different launcher with **Import game data**.

## 1. What you need

- An **Apple silicon Mac** with full Xcode and the **visionOS SDK** installed. This project targets **visionOS 26.0 or newer**; use Xcode 26 or newer with a compatible SDK.
- **Apple Vision Pro**, paired with that Mac, with Developer Mode enabled.
- An Apple development signing team and a **bundle identifier you control**.
- A compatible game controller for the first play session.
- **Git, Python 3, XcodeGen, and Rust 1.98.1**. The steps below set up the repository-local Rust installation required by the native build helper.
- A **compatible GTA V web-port ZIP** with matching WASM, game assets, and WGSL shaders. Read [Game files](GAME-FILES.md) first if you are starting with a PC install, disc image, or multipart download.

Plan storage for the ZIP, the imported data, AOT output, and Rust/Xcode build caches at the same time. The previously tested ZIP is about **21.2 GB** and its prepared mobile data about **15.7 GB**; builds need additional space. The headset needs room for the app and the entire prepared data folder, plus headroom for saves and caches. There is no small streaming installer.

## 2. Get the V source

The main branch contains the V Apple-platform port. Clone the complete repository; the GitHub source ZIP is not a game-data package:

~~~sh
git clone https://github.com/zschwendi/avp-gta.git V
cd V
~~~

The repository is named avp-gta and the app is named V. The internal target remains SantosVision. Work from the repository root in the same Terminal session for the commands below.

If you have an older Muguet-only checkout, use a fresh clone or update it before following this guide. The V source includes `visionos/`, `make-visionos.sh`, and this guide.

## 3. Set up the Mac tools

Open Xcode once, finish its first-run setup, add your Apple account, and install the visionOS platform component. Confirm that command-line tools point at the full Xcode installation:

~~~sh
xcode-select -p
xcrun --sdk xros --show-sdk-path
~~~

If that reports only Command Line Tools or no visionOS SDK, select your full Xcode installation in Xcode's Settings before continuing.

Install XcodeGen and Python if they are not already available. With Homebrew:

~~~sh
brew install xcodegen python
~~~

The current native runtime helper expects Rust under **this checkout's** `.tools/`. A global Cargo installation alone does not satisfy that requirement. Set up the isolated toolchain using [rustup's supported custom locations](https://rust-lang.github.io/rustup/installation/index.html#choosing-where-to-install):

~~~sh
export CARGO_HOME="$PWD/.tools/cargo"
export RUSTUP_HOME="$PWD/.tools/rustup"
export PATH="$CARGO_HOME/bin:$PATH"

mkdir -p .tools
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  -o .tools/rustup-init.sh
sh .tools/rustup-init.sh -y --profile minimal \
  --default-toolchain 1.98.1 --no-modify-path

rustup target add aarch64-apple-visionos --toolchain 1.98.1
cargo --version
~~~

Keep these environment variables set in later Terminal sessions when using this checkout's tools. `.tools/` and build output are ignored by Git.

## 4. Validate and prepare your game package

Set the path to your complete, compatible ZIP:

~~~sh
V_ZIP="/absolute/path/to/game.zip"
python3 scripts/validate-game.py "$V_ZIP"
~~~

Stop if validation reports missing data, mismatched sizes, multiple builds, or unsupported shaders. [Game-file preparation](GAME-FILES.md) covers the expected layout and the assembly helper for the previously tested snapshot. A source-code download from GitHub is not the game package.

Build the host tool, then import to a **new, isolated directory**:

~~~sh
cargo build --locked --bin muguet -j 3
mkdir -p target/setup
V_GAME="$PWD/target/setup/game"
target/debug/muguet --import "$V_ZIP" "$V_GAME"
~~~

The explicit destination must not already exist; it protects an earlier import. If you are repeating setup, use a new name such as `target/setup/game-2`. Omitting the destination uses the inherited desktop location and can replace its existing game data, so keep the explicit destination shown above.

This invokes the real importer rather than simply unzipping: it prepares the supported data subset and adjusts its manifest. It does not start the game.

Find the single build directory inside `"$V_GAME/b/"` and use its actual name:

~~~sh
ls "$V_GAME/b"
V_BUILD="REPLACE_WITH_THE_BUILD_DIRECTORY_NAME"
V_WASM="$V_GAME/b/$V_BUILD/game.wasm"
test -f "$V_WASM"
test -f "$V_GAME/b/$V_BUILD/shaders/index.json"
test -f "$V_GAME/data/manifest.json"
~~~

For the previously tested snapshot the build ID is `8b0b5899ed`. Do not mix its shaders with a different WASM module.

## 5. Pair your headset and configure signing

Follow Apple's [device pairing guide](https://developer.apple.com/documentation/xcode/pairing-your-devices-with-your-mac) and [Developer Mode instructions](https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device).

On Vision Pro, open **Settings → General → Remote Devices** to make it available for pairing. Pair it in Xcode's device manager / Device Hub, accept the pairing prompts, and enable Developer Mode on the headset when requested. Keep the device available to the Mac during installation and transfer.

Get your Apple **Team ID** from your development account and choose your own app bundle ID:

~~~sh
V_TEAM="YOUR_APPLE_TEAM_ID"
V_BUNDLE_ID="com.yourname.v"
~~~

Use the same bundle ID for building, installing, copying data, and later updates. A different ID has a different app container.

This guide builds with **local saves**. It does not claim the maintainer's iCloud container. To enable cloud sync in your own fork, configure a container owned by your team and update both the entitlement identifiers and the app's container identifier; see [Save sync](../visionos/SAVE-SYNC.md). A free team's capabilities may differ; this guide does not promise eligibility for every account.

## 6. Build and sign the actual game app

~~~sh
TEAM="$V_TEAM" BUNDLE_ID="$V_BUNDLE_ID" \
  MUGUET_GAME_WASM="$V_WASM" \
  ./make-visionos.sh game device
~~~

The helper builds the matching visionOS Rust runtime, compiles the WASM ahead of time, embeds that code, and links the native game app with the required executable-segment settings. Its build output is:

~~~text
target/visionos-xcode/DerivedData-game-device/
└── Build/Products/Debug-xros/SantosVision.app
~~~

The configuration is named Debug, but game-mode Swift compilation uses optimization. A first build can take substantial time and memory; later builds can reuse matching AOT artifacts. Wait for a successful exit.

~~~sh
V_APP="$PWD/target/visionos-xcode/DerivedData-game-device/Build/Products/Debug-xros/SantosVision.app"
codesign --verify --strict "$V_APP"
~~~

**Game mode matters.** `diagnostic` builds a tracking/calibration scene, not GTA. The generated Xcode project does not persist the command-line game-runtime/linker settings: reopening it and pressing Run without those settings is not equivalent to the helper command above.

With both `TEAM` and `BUNDLE_ID` unset, the helper makes an **unsigned** build for local compilation checks. It never installs or launches an app automatically.

## 7. Install V on the correct Vision Pro

List devices and select your **physical Vision Pro**, not a simulator, phone, or iPad:

~~~sh
xcrun devicectl list devices
V_DEVICE="YOUR_PHYSICAL_VISION_PRO_IDENTIFIER"

xcrun devicectl device install app --device "$V_DEVICE" "$V_APP"
xcrun devicectl device info apps --device "$V_DEVICE" \
  --bundle-id "$V_BUNDLE_ID"
~~~

The installed name should be **V**. The app still needs the imported game folder. Leave it closed during the transfer.

For an update, install over the existing app with the same identity. Do not uninstall first; that removes local data.

## 8. Copy the prepared data into V

Copy the **game folder produced in step 4**, not the ZIP, raw source download, or Mac saves:

~~~sh
xcrun devicectl device copy to \
  --device "$V_DEVICE" \
  --source "$V_GAME" \
  --destination Documents/game \
  --domain-type appDataContainer \
  --domain-identifier "$V_BUNDLE_ID" \
  --remove-existing-content false \
  --timeout 7200
~~~

This is a large transfer. Keep the headset connected and available, and wait for the command to finish successfully. A visible `manifest.json` or a partly populated folder does not prove the copy finished.

If the connection drops, reconnect, wait until the failed command has exited, and rerun this same non-removing copy. Keep the source package/build unchanged. Do not delete the app or replace `Documents/saves` to recover a game-data transfer.

Inspect the final destination:

~~~sh
xcrun devicectl device info files \
  --device "$V_DEVICE" \
  --domain-type appDataContainer \
  --domain-identifier "$V_BUNDLE_ID" \
  --subdirectory Documents/game --recurse
~~~

The layout must be:

~~~text
Documents/
├── game/
│   ├── data/manifest.json
│   ├── data/…assets…
│   └── b/<build-id>/shaders/…
└── saves/                         # created/managed separately by V
~~~

There must not be another `game/` folder between `Documents/game` and `data/`. Check that the copy command succeeded and the file paths/sizes agree with the imported Mac folder before playing. If using a separately supplied app, its embedded code must match your data package.

## 9. First launch: window first, then immersion

1. Pair a compatible controller in the headset's Bluetooth settings.
2. Open **V**. Once the embedded-module check and data check pass, the launcher shows **Ready to play**.
3. Open **Settings** if you want to choose graphics before starting. A new setup defaults to **Existing GTA**, **960×540**, and a **60 FPS cap**. Your previously saved choices take precedence.
4. Choose **Play** and allow the game to load in its normal window.
5. Confirm movement, camera, and audio in the actual game world.
6. Close GTA's pause menu, then select the upper-right expand icon, **Enter immersive view**. Allow the world-sensing permission needed for head tracking.
7. Wait for the first immersive game frame. If the transition fails, use **Return to window**.

Stay with ordinary third-person/gamepad play for the first acceptance test. Tracked first-person aiming, Sense controllers, and hand tracking are unfinished. A 60 FPS cap does not guarantee 60 rendered game frames each second.

Launch graphics changes apply on the next game start: choose **Quit game**, reopen V, change settings, then choose Play again. Leaving and re-entering immersion does not restart the engine.

## Troubleshooting

| What you see | What to check |
| --- | --- |
| Missing `.tools/cargo/bin/cargo` | Complete the repository-local Rust setup in step 3. |
| Signing/profile or iCloud-container error | Use your own team and bundle ID. The guide's signed build uses local-save entitlements; custom iCloud needs your own provisioned identifiers. |
| Tracking/calibration scene instead of GTA | Build `game device` with the native helper; do not use a diagnostic project run. |
| **Game files required** / disabled Play | Complete the copy into the same bundle ID's `Documents/game`. Check for a nested `game/game` folder, missing manifest/shaders, or a failed embedded-module check. |
| Import rejects an existing destination | Choose a new staging path. The explicit destination is intentionally non-replacing. |
| Copy fails or the device disappears | Restore the connection, let the failed copy exit, then resume the same source with `--remove-existing-content false`. Preserve saves. |
| The game reports a module or shader error | Verify that the app was compiled from the same build as the imported data; a new ZIP cannot replace embedded AOT code. |
| Immersion looks worse than the game window | This is an active development issue. Record build, graphics settings, scene, and whether the defect appears while still or moving; increasing resolution alone is not a universal fix. |
| Changing graphics has no effect | Quit and relaunch the game; make the change before Play. |
| “The app installed” but the game has not run | Installation, complete data transfer, windowed gameplay, and immersive acceptance are four separate checks. |

For a reproducible failure, include the source commit, headset/OS, build mode, and exact error in a [bug report](https://github.com/zschwendi/avp-gta/issues). The Vision Pro runtime writes `Documents/native-runtime-report.json`; inspect it for personal paths before sharing. Do not attach game assets or saves.

## Updating your build

Keep the same team/bundle ID and back up your app data. Pull the source you intend to use, repeat the build, and install over the existing V app. Reuse the current data only if the new embedded game build is compatible. Keep the source ZIP until you have a confirmed working replacement.

The latest renderer and touch-control source may be newer than a frozen package labeled V28. Record the **source commit and package**, not just the number, when comparing results.


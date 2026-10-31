# V on iPhone and iPad

This is the complete iPhone/iPad setup for **V**, from a Mac source build with your own game ZIP through updates. One iOS app supports both iPhone and iPad, landscape only.

There are two installation routes:

- **Windows or Linux, published preview IPA:** follow [Install V on iPhone or iPad](IOS-SIDELOADING.md). That preview is the existing **build 28** runtime repackaged as 28.1. It predates the build 31 launcher described here, so it does not have **V options** or **Play with friends**.
- **Mac source build (this guide):** compile V with the game code from your own copy and install it with your own signing team. Build 31 source includes the redesigned launcher, **V options**, **Play with friends** and the automatic touch-control handoff.

You supply the game: V contains no GTA V assets, and this repository publishes no game code. Two-device multiplayer and full-game play are not yet accepted; see [Build 31 status](V31-LAUNCHER-MULTIPLAYER.md#acceptance-status).

## 1. What you need

- An **Apple silicon Mac** with full Xcode and its iOS platform component, plus **Git, Python 3, XcodeGen** and **Rust 1.98.1** with the `aarch64-apple-ios` target.
- An **iPhone or iPad** running iOS/iPadOS 15.0 or newer, with Developer Mode enabled. The 15.0 minimum does not promise enough memory or acceptable performance on every device.
- **Your own Apple development team** and a bundle identifier you control. The normal build requests the Extended Virtual Addressing and Increased Memory Limit capabilities, which need a paid developer account; see the note at the top of the [README](../README.md).
- A **compatible GTA V web-port ZIP** that you have the right to use. The previously tested package is build `8b0b5899ed`. Read [Game files](GAME-FILES.md) first if you have a PC install, disc image or multipart download.
- **Storage.** The previously tested ZIP is about 21.2 GB; its imported data is about 15.7 GB. Plan for roughly **16 GB on the device for the game data**, plus the ZIP itself (about 21 GB more) while you import it on the device. See [Storage](#storage) for details.
- A compatible game controller is recommended. Touch controls appear automatically when no controller or hardware keyboard is connected.

## 2. Validate your game package

From the repository root:

~~~sh
V_ZIP="/absolute/path/to/game.zip"
python3 scripts/validate-game.py "$V_ZIP"
~~~

Stop if validation reports missing data, mismatched sizes, multiple builds or unsupported shaders. Use **the same ZIP** for the build in step 4 and the import in step 6. The app embeds that ZIP's compiled game code; importing a different package later does not replace it.

## 3. Set up the Mac tools

Clone the source and set up the repository-local Rust toolchain exactly as in [steps 2 and 3 of the Vision Pro guide](INSTALL-VISION-PRO.md#3-set-up-the-mac-tools), using the current repository, [zschwendi/avp-gta](https://github.com/zschwendi/avp-gta). Then add the iOS target to the same toolchain:

~~~sh
export CARGO_HOME="$PWD/.tools/cargo"
export RUSTUP_HOME="$PWD/.tools/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
rustup target add aarch64-apple-ios --toolchain 1.98.1
~~~

Keep these variables set in every Terminal session you use for this checkout. In Xcode, add your Apple account under **Settings › Accounts** and install the iOS platform component.

Pair the iPhone or iPad with the Mac (connect it by USB, unlock it and accept **Trust This Computer**), then enable Developer Mode on the device when asked. Find its identifier:

~~~sh
xcrun devicectl list devices
~~~

## 4. Build V with your game code

`make-ios.sh` extracts `b/<build>/game.wasm` from your ZIP, compiles it ahead of time for ARM64, embeds it in the app and signs the app. Nothing else from the ZIP goes into the app.

~~~sh
V_TEAM="YOUR_APPLE_TEAM_ID"
V_BUNDLE_ID="com.yourname.v"
V_DEVICE="YOUR_IPHONE_OR_IPAD_IDENTIFIER"

TEAM="$V_TEAM" BUNDLE_ID="$V_BUNDLE_ID" \
  ./make-ios.sh "$V_ZIP" --install "$V_DEVICE"
~~~

**Always set `BUNDLE_ID`, and keep using the same one.** If you leave it unset, the helper falls back to upstream Muguet's identifier, `ch.cclerc.muguet.ios`. A different bundle ID is a different app with an empty container: your imported data, settings and saves stay with the old ID.

The internal Xcode target is still **Muguet**; the installed app is named **V**. The app is written to `target/ios-xcode/Build/Products/Release-iphoneos/Muguet.app`. A first build takes substantial time and memory.

Helper options:

| Option | Effect |
| --- | --- |
| `--install [device]` | Signs with your team (`TEAM` required) using Xcode automatic provisioning, then installs with `devicectl`. Without a device identifier it picks the first available paired iPhone or iPad. |
| *(no mode)* | Builds and signs with your team but does not install. |
| `--ipa` | Builds a sideload package at `build/Muguet.ipa`: ad-hoc signed, Increased Memory Limit entitlement only, 5 GB game heap. `TEAM` is not needed. You must re-sign it with your own identity before it installs; see [the sideloading guide](IOS-SIDELOADING.md#install-from-windows-or-linux). |
| `FREE=1` | Signs without the memory entitlements and uses a 5 GB game heap, for teams that cannot sign those capabilities. It is not a promise that the game fits in memory. |
| `MUGUET_IOS_HEAP_GB` | Advanced override for the game heap size (8 GB normally, 5 GB with `FREE=1` or `--ipa`). |

Running the helper **without a ZIP** builds an app with no game code; its launcher reports **No game code**. Build 31 installation through the `--ipa` route has not been validated.

## 5. Install or update

`--install` installs over an existing V with the same bundle ID. **Do not delete V first.** Deleting the app deletes its container: the imported game data, `config.json` and saves.

An in-place update keeps everything in V's Documents folder:

~~~text
Documents/
├── config.json      # launcher settings (V options)
├── game/            # imported game data
│   ├── data/manifest.json …
│   └── b/<build>/shaders/…
└── saves/           # GTA saves and GTA's own settings.xml
~~~

Build 31 reads the values you already saved; it does not reset any of them. Reuse the existing game data only if the new app was built from the same game build.

## 6. Put the game files on the device

Choose one method. When the launcher shows **Ready to play**, the data is in place.

### A. Import the ZIP in V (recommended)

1. Copy the ZIP to the device:
   - **Finder file sharing:** on the Mac, select the device in Finder's sidebar, open **Files**, expand **V** and drag the ZIP onto it. It lands in V's own folder.
   - Or use **AirDrop** or **iCloud Drive**. Make sure the ZIP is downloaded to the device before importing; a large file still stored only in the cloud can fail partway.
2. Open V and tap **Import game data**. In Files, pick the ZIP (for Finder copies it is under **On My iPhone/iPad › V**).
3. Keep V open in the foreground until **Importing… %** finishes. The importer writes to a temporary folder and only replaces `Documents/game` after the whole copy succeeds, so an interrupted import leaves your previous data unchanged.
4. When the launcher shows **Ready to play**, you can delete the ZIP in Files to recover its space. To replace data later, use **Import other game data…**.

### B. Copy a prepared game folder through Files

The app's Documents folder is visible in Files and Finder. You can copy an already-prepared `game` folder instead of a ZIP; [Add your owned game data](IOS-SIDELOADING.md#add-your-owned-game-data) explains preparing it on Windows or Linux and the required layout. Wait for the copy to finish before opening V.

### C. Advanced: stage from the Mac with devicectl

This copies prepared data straight into the app container over the paired connection. It publishes `data/manifest.json` **last**: V treats the data as ready once `game/data/manifest.json` and `game/b/<build>/shaders/index.json` exist, so withholding the manifest keeps a partial copy from looking ready.

Prepare the data on the Mac into a new folder (the destination must not exist):

~~~sh
cargo build --locked --bin muguet -j 3
mkdir -p target/setup
V_GAME="$PWD/target/setup/ios-game"
target/debug/muguet --import "$V_ZIP" "$V_GAME"
~~~

Leave V closed and the device unlocked and connected, preferably by USB. Move the manifest aside, copy everything else, then publish the manifest:

~~~sh
mkdir "$V_GAME-manifest"
mv "$V_GAME/data/manifest.json" "$V_GAME-manifest/manifest.json"

xcrun devicectl device copy to --device "$V_DEVICE" \
  --source "$V_GAME" --destination Documents/game \
  --domain-type appDataContainer --domain-identifier "$V_BUNDLE_ID" \
  --remove-existing-content false --timeout 7200

xcrun devicectl device info files --device "$V_DEVICE" \
  --domain-type appDataContainer --domain-identifier "$V_BUNDLE_ID" \
  --subdirectory Documents/game --recurse

xcrun devicectl device copy to --device "$V_DEVICE" \
  --source "$V_GAME-manifest/manifest.json" \
  --destination Documents/game/data/manifest.json \
  --domain-type appDataContainer --domain-identifier "$V_BUNDLE_ID" \
  --remove-existing-content false

mv "$V_GAME-manifest/manifest.json" "$V_GAME/data/manifest.json"
~~~

Publish the manifest only after the large copy has exited successfully and the file list matches the Mac folder. If the connection drops, wait for the failed command to exit, then rerun the same non-removing copy. Never replace `Documents/saves` or delete the app to recover a transfer.

## 7. First launch

1. Open **V**. The status row along the bottom shows the state, the build number and the connected controller (or **Touch controls**).
2. Optional: tap the large **V** title (with its **Options** chip) to open **V options** before playing. See [V options](#v-options).
3. Tap **Play** and let the game load.
4. Save through GTA's own menus.

Options take effect when you press Play. To change them after the game has started, close V from the App Switcher, reopen it and change them in the launcher.

## V options

Tap the **V** title before Play. The sheet shows this device's saved values and saves when you tap **Done**, close the sheet or press **Play**.

| Option | Choices | Saved in `Documents/config.json` |
| --- | --- | --- |
| Mode | **Story Mode** (uses your story saves) or **Sandbox** | `start_mode`: 1 Story, 2 Sandbox |
| Resolution | 35 % (fastest), 50 %, 75 %, 100 % (sharpest) render scale | `scale`: 0.35, 0.5, 0.75 or 1.0 |
| Textures | Normal (less memory), High, Very High | `texture_quality`: 0–2, written only when you pick one; then applied to GTA's settings at every start |
| Frame limit | 30 fps (cooler) or 60 fps | `fps_limit` |
| Look sensitivity | Touch camera speed, ×0.25 to ×3 | `touch_sensitivity` |
| Mute audio | On or off | `mute` |
| Play with friends | Off, **Host** or **Join**, with an optional name | `multiplayer`, `host`, `server`, `player_name` |

Only the keys you change are written. Other keys, such as `extra_args` and anything V does not show, are kept. A saved value outside the menu choices is shown as **Current (…)** until you pick another. If `config.json` is missing, V creates it with Story Mode, 50 % scale and a 60 fps limit. A hand-added `resolution` key (for example `"1920x1080"`) overrides the Resolution scale.

Until you choose Textures, V shows GTA's own current texture setting and leaves it alone.

**Play with friends** uses V's own local-network session, not GTA Online. Hosting shows **Friends join with:** and this device's address; joining needs the host's IPv4 address or name, optionally with `:port`. An invalid address is reported in the sheet and Play waits until it is fixed. How to host and join, and the current limits, are in [Build 31: V options and Play with friends](V31-LAUNCHER-MULTIPLAYER.md#play-with-friends).

## Controls

- **Controller:** an extended gamepad works throughout the game. Use GTA's own button prompts.
- **Touch:** when neither an extended game controller nor a hardware keyboard is connected, touch controls appear during gameplay. They cover movement, camera, combat, vehicles, interaction, weapons and menu navigation.
- Connecting a controller or hardware keyboard hides the touch overlay and releases held touch inputs.

**Look sensitivity** in V options changes the touch camera speed.

## Storage

- **Imported data:** about 15.7 GB for the previously tested package.
- **On-device ZIP import:** the ZIP (about 21.2 GB) must stay on the device while it is imported, so allow roughly 37 GB free for the first import. Delete the ZIP afterwards.
- **Replacing data:** the old `game` folder is removed only after the new import completes, so a re-import temporarily needs room for both.
- Leave additional space for saves, caches and iOS itself.

## Troubleshooting

| What you see | What to check |
| --- | --- |
| **Game files required** | `Documents/game` has no complete data. Import the ZIP, or finish a Files/`devicectl` copy and make sure `game/data/manifest.json` and `game/b/<build>/shaders/index.json` exist with no extra `game/game` level. An interrupted Mac transfer that never published the manifest looks like this. |
| **No game code** / "build with ./make-ios.sh <game.zip>" | The app was built without a ZIP. Rebuild with your ZIP and install over it. |
| **Import failed: … no game data (data/manifest.json)** | The ZIP is not a web-port package, or `data/` is nested unusually. Run `scripts/validate-game.py`. |
| **Import failed: … no shaders (b/<build>/shaders)** | The package lacks its WGSL shaders. |
| **Import failed: not a zip file** / **reading … from the zip** | The file is damaged, incomplete or not fully downloaded. Copy it again and keep V in the foreground during import. |
| Import fails late, or the device reports low storage | Free space as described in [Storage](#storage), then import again. Your previous data remains until an import succeeds. |
| The game reports a module or shader error | The app's embedded code and the imported data come from different builds. Rebuild from the same ZIP you imported. |
| Signing or provisioning error | Use your own `TEAM` and `BUNDLE_ID`; check that your account can sign the memory capabilities, or try `FREE=1`. |
| Settings went back to defaults, or data and saves are missing after an update | The app was installed under a different `BUNDLE_ID` or deleted. Reinstall with the original bundle ID; do not delete the app. |
| **Play with friends** cannot connect | Allow **Local Network** for V (**Settings › Privacy & Security › Local Network**). Put every device on the same Wi-Fi/LAN, use the address shown on the host, and use the same Mode everywhere. Some guest or office networks block traffic between devices. |
| **Could not save settings** | V could not write `Documents/config.json`. Check free space and try again. |

For a bug report, include the device and OS, V build number, the bundle ID route (source build or IPA), V options, and the exact message. Do not attach game files or saves. Use [Issues](https://github.com/zschwendi/avp-gta/issues).

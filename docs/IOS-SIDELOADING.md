# Install V on iPhone or iPad

Download [SantosVision-iOS-28.1.ipa](https://github.com/zschwendi/SantosVision/releases/tag/ios-v28.1-preview).
This **one iOS IPA supports both iPhone and iPad** (native arm64, device families 1 and 2). Its minimum OS is **iOS/iPadOS 15.0**; that minimum does not guarantee enough RAM on every older device. Use the separate [Vision Pro guide](SIDELOADING.md) for visionOS.

**Paid developer signing is required for this package's Extended Virtual Addressing entitlement.** Increased Memory Limit is also preserved. Do not remove either capability to force an install. Apparition's [capability notes](https://github.com/illixion/Apparition#what-it-does) explain the paid-account requirement. These downloads are experimental; a fresh recipient's re-sign, install, import and gameplay have not been validated.

## Install from Windows or Linux

1. Download the desktop installer from [Apparition's official releases](https://github.com/illixion/Apparition/releases/latest). The [existing guide](SIDELOADING.md#windows-walkthrough) covers Windows startup and sign-in troubleshooting.
2. Connect the iPhone or iPad by **USB**, unlock it and accept **Trust This Computer** locally. Windows may need Apple's **Apple Devices** app/drivers; Linux needs **usbmuxd** running. Select the device in Apparition. There is no headset-style Remote Devices pairing code. See [Apparition's pairing instructions](https://github.com/illixion/Apparition).
3. Sign with **your own paid developer identity** and a profile covering this device and the app's required memory capabilities. Apparition supports Apple Account signing or importing your own certificate/profile. Never send your password, certificate or private key to the project maintainer. Certificate signing avoids the anisette sign-in dependency.
4. Install the downloaded **iOS IPA**. For managed refresh, first install Apparition on the phone/tablet, connect its companion, then use its Library's **Install from File**. A direct desktop installation must be renewed from the desktop unless separately imported into the managed library.
5. Follow any Developer Mode/trust prompts locally, then open the game normally. These iOS ports do not need **Launch with JIT**.

Keep the same signing identity and bundle identifier for updates; back up saves and refresh before the profile expires. Do not uninstall to solve a signing error.

## Add your owned game data

Use matching GTA V web-port build **8b0b5899ed**; ordinary retail PC folders and console images are not direct inputs. Follow [Prepare and copy your game files](SIDELOADING.md#prepare-and-copy-your-game-files) to validate/extract your own ZIP on Windows or Linux. On this platform, copy the resulting **game** folder into V's folder under **On My iPhone** or **On My iPad** in Files, instead of the headset location. Keep game/data/manifest.json and game/b/8b0b5899ed/shaders/index.json directly beneath the app's Documents folder, without an extra game/game level. Preserve saves/.

Allow roughly 16 GB for prepared data plus the app and working space. Open V and choose Play after copying finishes. A controller is recommended.

## What is in this preview

This is the existing iOS **Build 28** runtime repackaged as **28.1**, not the Vision Pro V30 renderer. All 33 non-signature Mach-O sections match the original device binary. Source inventory matches [native revision 039aaad](https://github.com/zschwendi/SantosVision/commit/039aaad40fef98e7aee75684f3e02b000394174c), except build-number metadata; its embedded AOT was prepared from [cfc03d7](https://github.com/zschwendi/SantosVision/commit/cfc03d7dd281e52a4bad95905445acf0a8850346). The runtime and embedded compiled game code remain; retail assets, saves, profiles and developer certificates are excluded.

The release contains a complete file manifest and SHA256SUMS. On Windows, compare Get-FileHash -Algorithm SHA256 on the downloaded IPA with that checksum; on Linux, use sha256sum -c SHA256SUMS. Maintainer recipe: [sideload-ios.json](../packaging/sideload-ios.json), consumed by tools/package_sideload.py with --recipe packaging/sideload-ios.json.

# Install V without Xcode

Download the [V30.1 visionOS preview](https://github.com/zschwendi/SantosVision/releases/tag/visionos-v30.1).
It contains the V30 compiled runtime, with Files access enabled for game-data
transfer. Supply your own matching GTA V web-port data. Ordinary retail PC
files, console images and the repository source ZIP cannot be imported directly.

Vision Pro on **visionOS 26 or later** and a gamepad are required. Windowed
play is the starting point; immersive image quality and performance remain
unfinished. This is a packaging update, not a new renderer build.

## Install the IPA with Windows, Linux or a Mac

You need a Vision Pro, an Apple account, a computer for the initial setup, and
both devices on the same Wi-Fi. Xcode is not needed for this route.

**Apple sign-in requirement (29 October 2026):** Apparition's
[0.4.2 release notes](https://github.com/illixion/Apparition/releases/tag/v0.4.2)
report that Apple rejects its Android-library anisette provider. A Windows
computer can install the app, but free-account signing needs a working
compatible external anisette service. The Linux/Docker provider alone is not
a working replacement. See Apparition's [current options](https://github.com/illixion/Apparition#anisette)
for third-party services and supported Mac/macOS-VM sources. Your own paid
developer certificate and device provisioning profiles can bypass Apple-ID
sign-in. This guide does not establish that any particular service works.

1. Download the desktop installer from [Apparition](https://apparition.illixion.com/).
   Use its current instructions for your operating system and configure a
   supported signing method before continuing. Each person uses their own
   account or certificate; the maintainer does not need anyone's credentials.
2. On Vision Pro, open **Settings → General → Remote Devices**. Select the
   headset in Apparition on your computer and enter the pairing code.
3. Follow Apparition's prompts to sign in with **your own Apple account** and
   install Apparition on the headset. Complete Apple's verification prompts
   yourself. Enable **Developer Mode** under **Privacy & Security** if prompted;
   the headset restarts. A developer-trust prompt may also appear.
4. Download this repo's **visionOS `.ipa`** from the release linked above. The
   automatically generated **Source code.zip** is not the app.
5. On the headset, open the IPA from Files with Apparition, or use
   **Library → Install from a File**. Let Apparition sign it for your account
   and install it. You can also use Apparition's desktop IPA installer.
6. Open the installed app from Home View, then supply your game files using
   the instructions below.

These are native visionOS builds. Do not use an iPhone/iPad IPA or a simulator
build. No JIT/debugger step is required for these ports.

## Keep the app working

With a free Apple account, apps normally need refreshing every **7 days** and
there is a **3-app limit**, including the sideloader. Use Apparition's refresh
feature before expiry. Its desktop companion or LocalDevVPN must be available
for refreshes; follow its guide for a Shortcuts automation. Paid developer
profiles have different limits and must support the app's capabilities.

Keep the same signing account and app identifier for updates. A new signer or
identifier can produce a separate app with separate saves. Back up first and
install updates over the existing app. **Do not uninstall to fix a refresh
problem.** These preview packages use local saves; the maintainer's iCloud
containers and development profiles are not included.

## If something goes wrong

- **It will not install:** check that you selected the visionOS IPA, enabled
  Developer Mode, completed trust prompts, and have a free signing slot.
- **It stopped opening after a week:** refresh its signature in Apparition.
- **There is no game / Play is unavailable:** installing the app does not
  supply your game files. Complete this repo's data steps below.
- **The sideloader rejects a capability:** do not blindly remove entitlements.
  Report the exact error and signing method, without account details.
- **A newer install has no saves:** check the signing account and identifier;
  preserve the old installation until its saves are backed up.

This route is an **experimental preview**. Package structure, code signatures
and contents were checked locally. A fresh Windows-to-headset installation,
free-account launch, game import, and save-preserving update have **not** been
validated end to end. The earlier development builds' gameplay results do not
establish those checks. Report reproducible problems through this repo's Issues.

## Prepare and copy your game files

1. Obtain your own compatible package as described in [Game files](GAME-FILES.md).
   This preview requires build **8b0b5899ed**. Its original `game.wasm` SHA-256 is
   `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`.
   The helper checks it against the IPA's expected input.
2. Download this repository's source ZIP and extract it, or clone it. Install
   Python **3.11 or newer** on your computer. From the repository folder, run
   this in PowerShell (use `python3` instead of `py` on Linux/macOS):

   ```powershell
   py scripts/prepare-sideload-data.py "C:\Games\game.zip" --output "C:\Games\V-data\game"
   ```

   Use a **new output folder**. This only reads your ZIP and creates prepared
   data; it does not compile the game, download assets, or overwrite an existing
   import. Allow about **16 GB** for the prepared data, in addition to your ZIP.
   It applies the same excluded-DLC cleanup as the app's original importer.
3. Make the prepared `game` folder accessible in Vision Pro's Files app, using
   your own file share or cloud storage. Apple's [Connect to Server guide](https://support.apple.com/guide/apple-vision-pro/connect-external-servers-tan7acdaa0e3/visionos)
   explains connecting Files to a computer. Keep private game files off public
   GitHub releases.
4. Open V once, then quit it. In Files, find V under **On My Apple Vision Pro**
   (the app folder may display its internal name, SantosVision). Copy the
   prepared **game** folder into that app folder. The final layout must be:

   ```text
   V/
     game/
       data/manifest.json
       b/8b0b5899ed/shaders/index.json
     saves/                         ← keep your existing saves
   ```

   Avoid an extra `game/game/` level. Wait until copying finishes; the headset
   needs roughly 16 GB plus the app and working space.
5. Reopen V and choose **Play**. Try the ordinary window first. Enter immersive
   view only after the world has loaded and GTA's pause menu is closed.

The visionOS launcher still has no ZIP picker. This preview exposes its
Documents folder through Files instead. Copying a ZIP there without preparing
and extracting it will not work. Do not replace the `saves` folder.

## Package details and source builds

The IPA is ad-hoc signed for transport and **must be re-signed by the recipient**.
It contains compiled game code, but no game asset folder, saves, user accounts,
Apple development certificate or provisioning profile. Source runtime revision:
[`7bd22fb`](https://github.com/zschwendi/SantosVision/commit/7bd22fbacd4f656d7ff1039c73ae293a9102ac9d).
The release includes `manifest.json` and `SHA256SUMS`; verify the download with
`Get-FileHash -Algorithm SHA256 <file.ipa>` in PowerShell if needed.

Developers can still use the [Mac source-build guide](INSTALL-VISION-PRO.md).
Maintainer packaging instructions are in [IPA-RELEASE.md](IPA-RELEASE.md).

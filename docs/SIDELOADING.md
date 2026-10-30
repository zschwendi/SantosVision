# Install V on Vision Pro from Windows or Linux

**iPhone/iPad users:** use the separate [iOS download and USB-pairing guide](IOS-SIDELOADING.md). [All platform downloads](DOWNLOADS.md).

Download the [V30.1 visionOS preview](https://github.com/zschwendi/SantosVision/releases/tag/visionos-v30.1).
It contains the V30 compiled runtime, with Files access enabled for game-data
transfer. Supply your own matching GTA V web-port data. Ordinary retail PC
files, console images and the repository source ZIP cannot be imported directly.

Vision Pro on **visionOS 26 or later** and a gamepad are required. Windowed
play is the starting point; immersive image quality and performance remain
unfinished. This is a packaging update, not a new renderer build.

## Windows walkthrough

This installs the published Vision Pro IPA using **Apparition**. You do not
need a Mac, Xcode, a Developer Strap, or a cable for this route. You still need
your own supported game files, the visionOS version listed above, a controller,
and enough headset storage for the app **and** imported data.

The steps below use Apparition **0.4.2**, checked **29 October 2026**. These are
experimental game packages: Windows pairing and the server-setting correction
have been exercised, but a fresh recipient's complete sign-in, game install,
import, gameplay and save-preserving refresh are not yet verified for this release.

### 1. Download and open Apparition on Windows

1. Open [Apparition's official releases](https://github.com/illixion/Apparition/releases/latest).
   Download the **Windows desktop ZIP**, not Apparition's iOS or visionOS IPA.
   The checked release is `Apparition-Installer-0.4.2-windows-x86_64.zip` for
   Intel/AMD Windows PCs. Windows 11 x64 was used for setup; Windows on ARM has
   not been checked here.
2. In File Explorer, right-click the ZIP, choose **Extract All**, and open
   `Apparition.exe` from the extracted folder. Keep its accompanying files
   together. If it reports a missing `WebView2Loader.dll`, use the
   [Windows startup fix](#windows-startup-fix) below before continuing.
3. Put the PC and Vision Pro on the same trusted home network. Keep the headset
   awake and unlocked. If Windows asks to allow Apparition through its firewall,
   allow the app on that trusted network; do not turn the firewall off.

### 2. Pair the Vision Pro

1. On the headset, open **Settings → General → Remote Devices** and leave it open.
2. On the PC, choose **Get started**, select your Vision Pro, then **Start pairing**.
3. A code appears **on the headset**. Enter it in Apparition on the PC.
4. Continue when Apparition confirms pairing and reaches **Signing** /
   **How should apps be signed?** Pairing lets Apparition communicate with this
   headset; it does not install the game or sign in to Apple by itself.

### 3. Set the anisette server, then sign in

Choose the **Apple ID** signing option and use **your own Apple Account**.
The project maintainer does not need your account, password, verification codes,
or signing certificate.

An *anisette server* supplies metadata Apple's sign-in process requires.
**Set this before pressing Sign in:**

1. If a source selector is shown, choose **From an anisette server**.
2. In **Anisette server**, replace the default address with:

   ```text
   https://new.ani.sidestore.io
   ```

   Use that base address, without `/v3/client_info` on the end. This service is
   listed as **SideStore (New)** in the [SideStore server list](https://github.com/SideStore/anisette-servers/blob/main/servers.json).
   It returned public client information during setup; that check alone does
   **not** prove Apple sign-in will succeed for every account. It is a
   third-party service and may change or become unavailable.
3. Confirm the field contains the new address. In 0.4.2 the header updates to
   **Sign-in anisette: custom server**. Enter your account details locally,
   choose **Sign in**, and complete Apple's verification prompts locally.
4. Wait for successful sign-in and the installation step. An error from an
   earlier attempt may remain visible until you try again; check the URL in
   the new error if the new attempt fails.

**Do not leave `http://localhost:6969` here unless you deliberately run a
compatible server on this PC.** It means “this computer,” not an Apple service.
Our Windows setup failed with connection-refused error **10061** because
nothing was running there. Changing this field fixed that configuration error.

Apparition's [0.4.2 release notes](https://github.com/illixion/Apparition/releases/tag/v0.4.2)
explain that Apple now rejects its older Android-library anisette provider.
Some text in the 0.4.2 interface still suggests Docker; installing that older
Linux/Docker provider does not solve the current sign-in requirement. If the
external service fails, follow the [current upstream anisette options](https://github.com/illixion/Apparition#anisette)
and record the exact error rather than repeatedly retrying accounts or servers.

**Already have paid developer signing?** You can instead select your own `.p12`
certificate and matching provisioning profiles that include this headset and
the app's required capabilities. That route avoids Apple-ID sign-in and
anisette. Keep the private key private; a maintainer's certificate is not a
public download for users. Follow [Apparition's certificate instructions](https://github.com/illixion/Apparition#your-own-developer-certificate).

### 4. Install Apparition on the headset

1. At **Install Apparition**, keep **Latest Apparition** selected and choose
   **Install**. The normal setup shares the signing and pairing with the
   headset's Apparition so it can refresh apps later. Wait for installation
   to finish; pairing or sign-in success alone is not an installation result.
2. If visionOS requests it, enable **Settings → Privacy & Security → Developer
   Mode**, restart, and confirm the setting. If the app reports an untrusted
   developer, follow the trust prompt under **Settings → General → VPN & Device
   Management** for your own signing identity.
3. Open **Apparition** from the headset's Home View. Keep its Windows desktop
   companion running and the PC awake for this initial setup and refreshes.
4. If the headset app needs a connection, open **Connection → Companion →
   Find Companion**, then **Use** for your PC. The companion carries the
   connection to the headset; it is separate from the sign-in metadata server.
   Under **Anisette Server**, use **Advanced: use another server** and enter
   `https://new.ani.sidestore.io` if it is not already configured. Do not select
   the Windows companion's older built-in anisette provider as a replacement.

If you reach a list of apps with **Launch with JIT**, those are apps Apparition
can inspect on the paired device. That list is not a download catalog for this
repository, and seeing a game there does not mean you installed this release.
**This port does not need Launch with JIT.** Continue with the IPA install below.
In particular, **Other Apps** can include apps installed through Xcode or a
different sideloader; Apparition does not automatically sign or refresh them.

### 5. Download and install this game's IPA

Open [this game's release](https://github.com/zschwendi/SantosVision/releases/tag/visionos-v30.1), expand **Assets**, and download:

```text
SantosVision-visionOS-30.1.ipa
```

Leave it as an `.ipa`; do not extract it or choose GitHub's **Source code.zip**.
The IPA contains the app runtime, not your retail game files. You can install
it in either of these ways:

**Recommended: from the headset, for managed refreshes.** Open the release link
in Safari, download the IPA,
then use **Apparition → Library → Install from a File** and select it from
Downloads in Files. You can also share/open the file with Apparition. Review
the app name and signing choice, then confirm installation. This gives the
headset's Apparition the IPA it needs for later refreshes.

**Alternative: from the Windows PC.** After installing Apparition, choose
**Install something else**. On the install page, expand **Advanced**, select
**An app file**, and choose the downloaded game IPA. Check that the heading
names the game rather than **Install Apparition**, along with the target
headset and signing identity, then choose **Install**. Keep the headset awake
until it finishes. In 0.4.2, this does **not** automatically add a game to the
headset's managed refresh library. Keep its IPA on the PC; renew it there by
re-signing/reinstalling with the same account, App ID and data-preserving choice.

If a signing or installation error appears, resolve it before moving on; the
presence of the downloaded IPA is not proof of installation. Do not reuse an
unrelated occupied App ID, revoke a certificate, or delete an existing app to
get past a prompt. Those actions can affect working apps and their saves.

### 6. Bring your game files into Files

Open **V** once from Home View, then follow [this game's preparation and
import steps](#prepare-and-copy-your-game-files) below. Installing its IPA alone is not enough to play.

To get your own prepared folder or archive from the PC into the headset's Files
app, use either of these routes:

- **Your private cloud storage:** copy it into a provider already available in
  Files, then download it fully on the headset before importing. Allow space
  for both the source and the game's imported copy.
- **A Windows network share:** on your trusted private network, enable network
  discovery and file/printer sharing as described in [Microsoft's sharing guide](https://support.microsoft.com/en-gb/windows/experience/connectivity-networking/file-sharing-over-a-network-in-windows).
  Right-click the game-data folder → **Show more options → Give access to →
  Specific people**, and share with your Windows user with read access.
  On Vision Pro, open **Files → Browse → More (…) → Connect to Server** and
  enter `smb://YOUR-PC-NAME` (or the PC's local IP address). Choose **Registered
  User** and enter the Windows account credentials for the share, not your
  Apple Account or Windows Hello PIN. Open the share and copy/download the
  required data locally. See [Apple's Files instructions](https://support.apple.com/guide/apple-vision-pro/connect-external-servers-tan7acdaa0e3/visionos).

Share only the folder you need. Keep game data, registration files and saves
out of public GitHub uploads. The game-specific steps below determine whether
you import a folder/archive or copy a prepared folder into the app's storage.

### 7. Confirm the first run and keep it working

After the game-specific import finishes:

1. Start in the ordinary game window with your controller. Check movement,
   sound and menus before trying the experimental immersive mode.
2. Make a save, fully close the app, reopen it and confirm the save is there.
3. For a game installed through the headset's Library, check its signing expiry
   there and **refresh before expiry**. For the desktop-only alternative,
   re-sign/reinstall the retained IPA from the PC before expiry. Free-account
   signatures normally last **7 days**. Keep the companion reachable for
   headset refreshes; the PC is not streaming the game during play.
4. Refresh/reinstall an update using the **same account and app identifier**,
   then confirm the save remains. Back up first; do not uninstall to fix expiry.

Free signing has app and App ID limits. The usual active-app allowance is
**three sideloaded apps total, including Apparition**, so plan for Apparition
plus two games, not all three previews at once. Check Apparition's actual
account/slot messages. Paid accounts have different limits and capability
requirements. Refresh automation is optional; use the [upstream Shortcuts guide](https://github.com/illixion/Apparition#shortcuts-automation)
once manual refresh works. Do not assume background refresh always fires.

These preview IPAs use local saves. The maintainer's iCloud containers and
development profiles are not included. Changing signers or identifiers can
create a separate app with separate saves.

## Linux users

Apparition also publishes Linux **x86_64** and **arm64** desktop builds on the
[same release page](https://github.com/illixion/Apparition/releases/latest).
Choose your PC's architecture. On Debian/Ubuntu-based distributions, install
the `.deb` with the package installer; alternatively, mark the `.AppImage`
executable and run it. Follow
[upstream Linux requirements](https://github.com/illixion/Apparition) if a
desktop dependency is missing.

Continue at [pairing](#2-pair-the-vision-pro). The headset, signing-server,
game-IPA, import and refresh steps are the same. Linux still needs a working
compatible external anisette service or your own eligible certificate; the
older Docker provider is not a working substitute. You can download the IPA
directly in headset Safari to avoid setting up a computer file share.

For the data-preparation command below, use `python3` and Linux paths instead
of `py` and `C:\…`. The helper accepts the same compatible input ZIP.

## Troubleshooting

| What you see | What to check |
| --- | --- |
| No headset in the device list | Unlock it, leave Remote Devices open, check both devices are on the same network, and allow Apparition through the firewall on that trusted network. Guest-network isolation can prevent discovery. |
| Connection refused / error `10061` at `localhost:6969` | Set the **Anisette server** field in step 3. No compatible server is listening at that local address. An error at a remote URL needs separate diagnosis. |
| A new Apple/anisette error after changing the URL | Check the URL in that new error, your connection, the current server list and upstream guidance. Public endpoint availability is not successful authentication. Share only a redacted error; never credentials or codes. |
| Certificate limit, replacement, or occupied App ID | Read which existing app/certificate is affected. Use Apparition's supported sharing of your own signing identity if applicable; do not blindly revoke or replace it. |
| Install fails or the app will not open | Confirm the exact visionOS IPA and minimum OS above, available storage/slots, Developer Mode, developer trust and the exact capability error. Do not remove entitlements at random. |
| Only a list with “Launch with JIT” | Use step 5 to install your downloaded IPA; this port does not need JIT. “Other Apps” entries are not automatically managed or refreshable. |
| App opens but there is no game / Play is unavailable | Finish the game-data preparation and import below. Renaming unsupported input does not make it compatible. |
| App stops opening after about a week | Refresh a managed Library app, or re-sign/reinstall a desktop-installed IPA from the PC with the same identity. Preserve the app and saves. |
| Files cannot open the PC share | Check the share permissions, PC address and Windows account password. A Windows Hello PIN is not the share password. A private cloud provider is an alternative. |

### Windows startup fix

On the checked Windows 0.4.2 ZIP, startup required an app-local Microsoft
`WebView2Loader.dll`. If a newer official package already includes it, skip
this workaround. For a **missing loader DLL** error:

1. Download [Microsoft's WebView2 1.0.4314.35 package](https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2/1.0.4314.35/microsoft.web.webview2.1.0.4314.35.nupkg).
2. Make a copy, change that copy's `.nupkg` extension to `.zip`, and extract it.
   Enable **View → Show → File name extensions** in File Explorer if needed.
3. Copy **`runtimes/win-x64/native/WebView2Loader.dll`** beside the extracted
   `Apparition.exe`, then open Apparition normally on the Windows desktop.
   Do not put it in a system folder or fetch DLLs from download-aggregator sites.

If the error instead says the **WebView2 Runtime** is missing, install the
[official Microsoft Evergreen WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
The runtime and the app-local loader DLL are different components; Microsoft's
[distribution guide](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)
explains both. The loader workaround was verified for x64 startup, not a full
game installation. Opening this GUI from an SSH/service session may fail;
open it on the logged-in Windows desktop.

If the 0.4.2 ZIP asks about duplicate `Apparition.exe`/`apparition.exe` names
while extracting, Windows treats those names as the same file. The checked
archive contained identical GUI executables. Keep one and use the GUI steps
above; do not assume the second name is a separate command-line installer.


## Prepare and copy your game files

1. Obtain your own compatible package as described in [Game files](GAME-FILES.md).
   This preview requires build **8b0b5899ed**. Its original `game.wasm` SHA-256 is
   `11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`.
   The helper checks it against the IPA's expected input.
2. Download this repository's source ZIP and extract it, or clone it. Install
   [Python **3.11 or newer**](https://www.python.org/downloads/windows/) on
   Windows. The source ZIP is only for the preparation helper; the IPA above
   is the app. Open the extracted repository folder in File Explorer,
   right-click its background and choose **Open in Terminal**, then run this
   in PowerShell (use `python3` and Linux paths on Linux):

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

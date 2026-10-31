# Build 31: V options and Play with friends

Build 31 applies to both **visionOS** and **iOS/iPadOS**. It changes the launchers and the settings read before the game starts. **It makes no renderer changes:** the V30 renderer, its normal game window and the unresolved immersive image quality carry over unchanged.

The app is still named **V**. Technical names stay as they were (target **SantosVision**, maintainer bundle ID `com.zephyrstudios.santosvision`), so an in-place update keeps the same app container. V is built on [Muguet](https://github.com/c22dev/muguet) by Constantin Clerc (c22dev); upstream attribution is retained (see [LICENSE](../LICENSE)). V is not branded as Muguet.

Setup guides: [iPhone and iPad](IPAD-IPHONE-SETUP.md) · [Vision Pro from a Mac](INSTALL-VISION-PRO.md) · [Vision Pro from an existing checkout](../AVP-INSTALL.md).

## What changed

**Both launchers**

- Tap the large **V** title (it shows an **Options** chip) before **Play** to open **V options**: Mode, Resolution, Textures, Frame limit, Look sensitivity, Mute audio and Play with friends.
- Each device shows its own currently saved values. **The update resets nothing.**
- A short summary on the launcher shows the choices the next game will use.
- **Play with friends** turns on Muguet's existing local-network UDP session. See [Play with friends](#play-with-friends).

**iPhone and iPad**

- A new V-style launcher replaces the upstream form, with separate iPad and iPhone landscape layouts and a status row showing the build number and controller.
- **Import game data** opens the Files picker for your game ZIP; the launcher explains how to copy the ZIP to the device first.

**Vision Pro**

- New **Textures** choice, stored in the new `native-textures.json` and applied after any Quality preset at each launch.
- **Mute audio** is separate from master volume, so unmuting restores your level. It is also in **Settings › Sound**.
- The old **Developer mode** toggle is now a **Story Mode / Sandbox** picker, in both V options and Settings. The stored value is unchanged.
- **More V settings…** (and the gear) still opens the full Settings: Quality preset, immersive display, iCloud saves, master volume, controller help, rendering optimizations and diagnostics.
- V now asks for **Local Network** access when Play with friends needs it.
- Every launcher file is validated before the engine starts and before GTA's `settings.xml` is changed. A problem blocks Play with a message instead of starting with half-applied settings.

## Using V options

| Option | iPhone / iPad | Vision Pro |
| --- | --- | --- |
| Mode | Story Mode or Sandbox | Story Mode or Sandbox |
| Resolution | Render scale 35 %, 50 %, 75 % or 100 % | 640×360 to 3840×2160, plus experimental 2560×2304 |
| Textures | Normal (less memory), High, Very High | Normal, High, Very High |
| Frame limit | 30 or 60 fps | 30 or 60 FPS |
| Look sensitivity | Touch camera, ×0.25 to ×3 | Controller right stick, 0–100 % |
| Mute audio | On or off | On or off; keeps master volume |
| Play with friends | Off, Host or Join, optional name | Off, Host or Join, optional name |

**When choices are saved:** on iPhone and iPad, when you tap **Done**, close the sheet or press **Play**. On Vision Pro, each change is saved immediately.

**When they apply:** at the next game start. On Vision Pro, V options are locked once the game is running; choose **Quit game**, reopen V and change them before Play. On iPhone and iPad, close V from the App Switcher and reopen it.

**Textures:** until you choose a value, V shows GTA's own current texture setting and leaves it alone. After you choose, V applies that value at every start.

## Settings files and what is preserved

Every value lives on the device in V's container. The rules are the same on both platforms: a missing value uses the default below, the launcher writes only what you change, and keys V does not own are left alone.

### iPhone and iPad: `Documents/config.json`

| Key | Meaning | Default |
| --- | --- | --- |
| `start_mode` | 1 Story Mode, 2 Sandbox | 1 |
| `scale` | Render scale: 0.35, 0.5, 0.75 or 1.0 | 0.5 in a new file |
| `texture_quality` | 0 Normal, 1 High, 2 Very High; written only when you pick one | absent (GTA's own value) |
| `fps_limit` | 30 or 60 | 60 |
| `touch_sensitivity` | Touch look multiplier, 0.25–3 | 1.0 |
| `mute` | Mute audio | false |
| `multiplayer`, `host` | Play with friends on; host (true) or join (false) | false |
| `server` | Host address when joining | empty |
| `player_name` | Name shown to friends, up to 24 characters | empty |
| `extra_args` | Extra engine arguments | `[]` |

V creates `config.json` with these defaults if it is missing. Unknown keys, such as upstream's `mouse_sensitivity`, `fullscreen` or a hand-added `resolution`, are preserved.

### Vision Pro: `Documents/saves/native-*.json`

| File | Keys | Default when absent |
| --- | --- | --- |
| `native-graphics.json` | exactly `width`, `height`, `fps_limit`, `graphics_preset` | 960×540, 60, `existing` |
| `native-gameplay.json` | `developer_mode` (true = Sandbox) | Story Mode |
| `native-audio.json` | `master_volume` (0–1), `mute` | 100 %, not muted |
| `native-controller.json` | `look_sensitivity_percent` (0–100) | 75 % |
| `native-textures.json` **(new)** | `texture_quality`: 0, 1, 2 or `null` | GTA's own value |
| `native-multiplayer.json` **(new)** | `multiplayer`, `host`, `server`, `player_name` | off |

- `native-graphics.json` must contain exactly its four keys. The other files keep any additional keys.
- `texture_quality` is applied after any Quality preset at each launch. `null` or a missing file keeps GTA's own value. V options has no "use GTA's value" entry; set the key to `null` to return to it.
- The two new files appear only after you change those options. Existing files and values are read as they are.
- An invalid value (for example a non-boolean `mute`, or `texture_quality` 3) shows **Launcher options need attention** or **Sound and controls need attention** and blocks Play until it is fixed.

For bug reports, `Documents/native-runtime-report.json` records the requested launcher options, and the runtime status records the session V applied.

## Play with friends

This is **Muguet's own LAN session**, not GTA Online. It does not use Rockstar services or accounts. Each player runs their own copy of the game, and the session exchanges player state and game events over UDP port **47520**, with up to **8 players** per session.

**Everyone needs:**

- V installed, with their **own** game files.
- The same Wi-Fi network or LAN. Guest and office networks may block traffic between devices.
- **Local Network** permission for V. Allow it when asked; you can change it later in **Settings › Privacy & Security › Local Network**.
- The **same Mode** (Story Mode or Sandbox) on every device. Using the same V build everywhere is also recommended.

**Host**

1. Open **V options** and turn on **Play with friends**.
2. Optionally enter **Your name**. Vision Pro suggests "Vision Pro".
3. Choose **Host**. The sheet shows **Friends join with:** followed by this device's IPv4 address. Share that address.
4. Press **Play**. The host device runs the session server inside V and also plays.

**Join**

1. Turn on **Play with friends**, optionally enter your name and choose **Join**.
2. Enter the host's address exactly as shown, for example `192.168.1.20`. A host name works too. Add `:port` only if the host is reachable on a different port; hosts listen on 47520.
3. Press **Play**.

Addresses are validated before launch. Spaces, IPv6 brackets, invalid IPv4 numbers, bad host names and ports outside 1–65535 are rejected with a message, and Play waits until the address is fixed. Names longer than 24 characters are shortened on iPhone/iPad and rejected on Vision Pro. A player who stops sending for about five seconds is dropped from the session.

## Acceptance status

Installed in place on October 31, 2026 from `430b768`: Vision Pro 30 → 31, iPad Pro 28 → 31 and iPhone Air 28 → 31, with saves and launcher settings kept. On the iPad the new launcher read the saved values (Story Mode, 50 %, 60 fps) and, after the game data import, the Story Mode prologue started. The Vision Pro options sheet has not yet been checked in the headset, and no two-device session has been played.

Verified so far:

- Host-side configuration tests: 27 native config tests pass, including the new mute, texture, mode and multiplayer files and address validation.
- A real UDP loopback session using V's actual multiplayer and server code: the host started the server on 47520 and was welcomed, a second player joined and was welcomed and named on both sides, and a reliable game event was relayed and acknowledged.
- The visionOS and iOS build 31 apps compile.

**Not yet done:** Play with friends between two physical devices inside the game has **not** been accepted. Mixed iPhone/iPad/Vision Pro sessions, sessions over real Wi-Fi, and longer play have not been tested. The loopback test shows the session protocol and server work; it does not show that players see each other correctly in the game.

The renderer is unchanged from V30. Immersive image quality remains an open issue; see the [README status](../README.md#source-and-installed-build-status).

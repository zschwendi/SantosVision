# Game package for SantosVision

SantosVision is a fork of Muguet, a native runtime for the **compiled GTA V web port**. It is not a console emulator. The input is a ZIP of that port's compiled package, containing matching game data, WebAssembly code, and WGSL shaders.

A PlayStation/Xbox ROM or disc ISO, an ordinary Steam/Epic/Rockstar PC installation, and a ZIP of this source repository are not direct inputs. This repository does not include game assets or a conversion procedure from a retail copy. Owning a retail copy alone does not produce the required package.

The ZIP must contain one package with this layout. A single enclosing folder is supported; `data/` and `b/` must share the same root.

```text
game.zip
└── optional-enclosing-folder/
    ├── data/
    │   ├── manifest.json
    │   └── ...game assets listed by the manifest...
    └── b/
        └── one-build-id/
            ├── game.wasm
            └── shaders/
                ├── index.json
                ├── <shader-hash>.wgsl
                └── ...additional shaders and optional .consts.json files...
```

`data/manifest.json` must contain a nonempty `files` array of `[relative_path, byte_size]` records. The runtime reads those assets relative to `data/`. `shaders/index.json` must be a nonempty JSON object; usable records have `"ok": true` and matching `<shader-hash>.wgsl` sources. Other shader formats such as `.spv` or `.glsl` do not replace the `.wgsl` files this fork loads.

Use one build per ZIP. The existing importer copies shader directories from all builds and the runtime selects the first directory with `shaders/index.json`; it does not provide dependable selection among multiple builds. `game.wasm` and the shaders must come from the same build. Mobile builds compile the WASM ahead of time and embed the compiled code in the app; importing assets later does not supply missing embedded code.

From the repository folder, check a package before building or importing it:

```sh
python3 scripts/validate-game.py /absolute/path/to/game.zip
python3 scripts/validate-game.py /absolute/path/to/game.zip --json
```

The validator opens the ZIP read-only and never extracts it. It reports the exact package prefix, build ID, WASM entry, and asset counts. It rejects ambiguous roots/builds, unsafe paths, duplicate or case-colliding entries, symlinks, special files, and encrypted entries. ZIP entries must use stored or deflate compression, matching the Rust importer's enabled codecs. It checks manifest paths and declared sizes against ZIP metadata, the WASM magic/version header, and the presence of nonempty WGSL files referenced by usable shader records. It accounts for the upstream importer's omission of `env_test/` and `verityradio/` assets.

A successful preflight confirms these structural requirements. It does not certify the full WASM module, shader compilation, every asset's CRC/content, authenticity, or game compatibility with the fork. Actual build and Vision Pro launch remain separate checks. Repackaging an unrelated ISO/ROM into this folder layout will not make it compatible.

## Assemble downloaded full-game-v1 parts

The [alihammoud21/playgta5 full-game-v1 release](https://github.com/alihammoud21/playgta5/releases/tag/full-game-v1) supplies 14 independent ZIP archives, plus `download-manifest.json` and `file-inventory.json`. Download all 14 `playgta5-part-XX-of-14.zip` files and both JSON files into one ordinary local folder. These archives must be read separately; concatenating them does not produce a game ZIP. The repository's **Code → Download ZIP** contains source and helpers, not this release payload.

From the SantosVision repository folder:

```sh
python3 scripts/assemble-game.py /Users/zach/gaming/game-assets/playgta5-full-game-v1 \
  --output /Users/zach/gaming/game-assets/muguet-game.zip
python3 scripts/validate-game.py /Users/zach/gaming/game-assets/muguet-game.zip
```

The assembler works entirely locally. It checks the two metadata files against pinned release SHA-256 values, checks all 14 archive hashes, and verifies every copied game's file size and SHA-256 against the inventory. It streams only `mirror/playgta5.com/b/` and `mirror/playgta5.com/data/` into a new stored ZIP64 archive with `b/` and `data/` at its root. Release launchers and Windows runtimes are excluded. The source archives remain unchanged; no release scripts are run or files extracted. Allow about 21.2 GB of additional free space for the assembled ZIP, on top of the downloaded parts.

This release has two stale asset-size declarations in `data/manifest.json`: `common/non_final/www/parser/psomismatch.html` and `schemas.html`. The assembler corrects only those two values after verifying the actual assets' expected bytes and hashes, then runs the strict package validator. Its adjacent `.receipt.json` records metadata and part hashes, output ZIP hash, and the manifest's before/after hashes and exact changes. Existing output and receipt files are never overwritten; failed assembly removes temporary files. Successful assembly confirms package integrity relative to this release inventory; gameplay, stereo rendering, head tracking, and Vision Pro compatibility still require runtime work.

## Local package verified — October 10, 2026

All 14 release archives were downloaded and verified against their published SHA-256 values. The assembled package is `/Users/zach/gaming/game-assets/muguet-game.zip`: **21,161,246,270 bytes**, SHA-256 `103ecf8d4b45ff5043e318bd71c0acb032a1c700f614cf7d144372d9f4c5eb35`. Its adjacent receipt records verification of all 12,479 copied source files and the two manifest repairs.

The package passed strict preflight and the fork's actual Rust importer in a temporary directory. The import produced 12,156 files, 5,537 manifest assets and 4,808 WGSL shaders. Every imported manifest byte count matched its file, including the importer-modified DLC list, and the retained `game.wasm` matched the published hash. Muguet's existing importer omits 277 manifest assets in its excluded directories; the assembled ZIP retains the full source game payload. The temporary extraction was removed after the check. Local evidence is `target/verification/actual-game-package.json` and `target/verification/actual-import-receipt.json`.

Actual game startup, shader execution, headset installation and full immersive gameplay remain separate acceptance checks.

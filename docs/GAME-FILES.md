# Game package for SantosVision

SantosVision is a fork of Muguet, a native runtime for the **compiled GTA V web port**. It is not a console emulator. The input is a ZIP of that port's compiled package, containing matching game data, WebAssembly code, and WGSL shaders.

A PlayStation/Xbox ROM or disc ISO, an ordinary Steam/Epic/Rockstar PC installation, and a ZIP of this source repository are not direct inputs. This repository does not provide the game assets, asset downloads, or a conversion procedure from a retail copy. Owning a retail copy alone does not produce the required package.

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

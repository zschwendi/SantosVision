# V on Apple Silicon Mac

Download the [native macOS preview](https://github.com/zschwendi/SantosVision/releases/tag/macos-v30.1-preview).
It is a ZIP containing **V.app**, for **Apple Silicon / arm64 and macOS 14.0 or newer**. It is separate from the iOS/iPadOS and Vision Pro IPAs; it does not support Intel Macs, Windows or Linux.

1. Compare the ZIP's SHA-256 with SHA256SUMS, then extract it.
2. Put the app in a location you choose without overwriting an existing copy you want to keep.
3. Open it. This preview is ad-hoc signed and is **not notarized**. If macOS blocks a known, verified download, use **System Settings → Privacy & Security → Open Anyway** for this app, following [Apple’s instructions](https://support.apple.com/en-us/102445). Do not disable Gatekeeper globally.
4. Supply your own matching game files as described below. No sideloader, Apple developer account or device pairing is needed to launch the Mac app.

This is an experimental native desktop build. Architecture, dependency linkage, signature, archive integrity and input checks are separate from full gameplay acceptance. This packaged Mac preview has not passed an end-to-end game session.

## Import and play

Open the launcher and choose your own compatible game ZIP using its import button. The ZIP needs the matching game.wasm, data manifest/assets and WGSL shader files described in [Game files](GAME-FILES.md); use the documented build 8b0b5899ed. A normal retail PC folder or console ISO is not directly importable.

The Mac build includes the existing Wasmtime compiler and imports game.wasm from your ZIP. It does not embed the mobile AOT game image. First launch compiles the imported module locally and can take longer; Apparition's Launch with JIT is not involved.

The existing desktop launcher offers settings and Play. Game data, configuration and saves remain under Library/Application Support/Muguet for compatibility with previous desktop builds. Back that folder up before changing versions. Allow around 16 GB for data plus source ZIP, compiled cache and working space. Optional upstream Easter-egg audio is omitted from this package.

The runtime was freshly built from the public source with Rust 1.98.1, locked dependencies and the default desktop features. The package includes license notices. Maintainers can use tools/package_macos.py with the fresh Cargo build directory; make-app.sh remains the upstream desktop build route.

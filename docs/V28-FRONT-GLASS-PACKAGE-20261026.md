# V28 front-glass package

The current local icon variant is `target/fidelity-v28-front-glass/SantosVision.app`: display name **V**, actual `CFBundleVersion` **28**, original technical identity `com.zephyrstudios.santosvision`. It is signed and uninstalled. The canonical build27 checkout and every prior signed candidate remain preserved. No headset, iPad or simulator action or query was used.

The icon keeps the emerald coastal sunset and palms, uses curved front glass with a fine silver edge, and removes the FIVE text and bulky headset hardware. Both platform asset catalogs use the new artwork. The original master and exact image-generation prompt are in `assets/branding/spatial-v-front-glass-master-v28.png` and `assets/branding/spatial-v-front-glass-v28.json`.

This package is an icon-only copy of the verified V28 candidate. The visionOS catalog was freshly compiled from hash-bound inputs at source `bb32c374bee69f6a504b0dd071a1cfd5754dadcc`. It does not incorporate the private native capture/replay work. Only `Assets.car`, the executable's embedded signature, and `_CodeSignature/CodeResources` changed. All 237,437,136 bytes before the executable signature are byte-identical, including the original 211,068,080-byte AOT and its read/execute protections. Info.plist, Metal shaders, entitlements, provisioning profile and signing certificate remain exact.

The packager passed 36 checks. Root independently rehashed every file in both complete packages, compared the actual Mach-O signature ranges and unsigned bytes, and reran strict signature verification on both copies. Evidence and the reproducible local recipe are in `target/fidelity-v28-front-glass/`:

- `candidate-receipt.json`: SHA256 `30af00b45c8286fbde0b1767e5d71e3b030b9b99d89f53de8f179cea62bef57b`.
- `root-verification.json`: independent package verification.
- `package_icon.py`: exclusive new-output packaging recipe, SHA256 `177d76892e9b9f34e7415b9893a1aaa7958572f684a5c1ee0dea80cc4fe01fe1`.
- New signed executable: SHA256 `dde735e5f31326b19a123707b069f67ad1402390f64b7c8c5d7407b701d0530b`.
- New compiled icon catalog: SHA256 `5fe10adb8705b7964e35cbffff9637b612e72ae3aa579200ba2cdb208a0b3060`.

The package retains the V28 color and source-publication corrections documented in `WORLD-PROVENANCE-FIDELITY-20261026.md`; this branding step adds no rendering or performance claim. The full window-quality objective remains open. After the user's hardware hold is lifted, the single acceptance test remains the same rejected scene at 1440 Clarity: window to ordinary third-person immersive, 15 seconds still, 15 seconds slow yaw, then 15 seconds still, with matched source/output captures. Check texture detail, dark shading, thin edges, HUD and temporal flicker with normal independent stereo.

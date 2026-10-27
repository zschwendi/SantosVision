# spatial-V title and front-glass icon

The local app title now matches the previously selected project name, **spatial-V**. The selected repository slug is `spatial-v`; a fresh GitHub metadata read on October 26 still returned `zschwendi/SantosVision`, and `zschwendi/spatial-v` was not found. No remote rename or push occurred during this local-only work.

Both platforms use the new text-free front-glass icon. It retains the original emerald palette, coastal sunset and palms, replaces the bulky headset silhouette with curved front glass and a fine silver edge, and removes the entire FIVE label. The built-in image-generation tool produced the artwork. Its original 1254-square master is retained at `assets/branding/spatial-v-front-glass-master-v28.png`; a size-only native `sips` resample supplies opaque, sRGB-tagged 1024-square catalog images. The exact prompt, original reference and hashes are in `assets/branding/spatial-v-front-glass-v28.json`. Earlier icon files remain preserved.

Both Info.plists and the current launcher, window/control titles, help and tracking explanations now say spatial-V. Bundle identifiers, executable/target names, signing, entitlements, saves, preferences and scene identifiers remain unchanged. Build values remain 28 on visionOS and 9 on iOS.

Local validation: both plists lint; all five changed Swift files parse; both asset catalogs compile for their device platforms (`xros` and `iphoneos`) with the new image. The compiler reports unassigned older PNGs retained in the catalogs, not an invalid new icon. The title verification and compiled asset receipts are in `target/spatial-v-title/`, `target/spatial-v-front-glass-icon/` and `target/spatial-v-front-glass-icon-ios/`.

These are source changes for the next package. The already signed V28 at `target/fidelity-v28/SantosVision.app` remains byte-identical and still has its original V title and icon. No headset, iPad or simulator action occurred. Branding makes no claim about immersive image-quality acceptance.

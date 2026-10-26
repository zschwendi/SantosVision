# V icon

The current catalog selects **santos-five-visor.png**: an original neon Vision
Pro visor with palm/sunset reflections and FIVE lettering. It was introduced
in commit `5baebf1` for build 12 and remains selected in signed build 22.
The current PNG was visually inspected during the October 24 request audit.

Current artwork: `Assets.xcassets/AppIcon.solidimagestack/Back.solidimagestacklayer/Content.imageset/santos-five-visor.png`.
SHA-256: `9f73e55f316442b4b9ac904bdc50fd609a17b401a88ff6f695f305ce08e1218c`.

The built-in image_gen edit used the project's own generated sunglasses icon,
without loading a game logo, artwork or font file. Its edit prompt was:

> Replace neon sunglasses with a recognizable Apple Vision Pro single curved smoked glass visor, silver rim and pale strap in the same bold painted comic style. Preserve FIVE lettering, neon green palette, palm tree sunset reflections, dark green background and safe outer icon margins. No Apple logo, new text, photorealism or game characters/artwork.

The opaque generated square was resized to 1024×1024 with sips. The earlier
sunglasses PNG is retained as santos-five-palms.png; it is not the selected
catalog image. The required second visionOS layer is a transparent PNG.
Receipt: `target/verification/device-session-20261024/playability12/icon-edit-receipt.json`.

The following prompts document the historical sunglasses source.

## Generation prompt

> Use case: logo-brand. Asset type: original 1024 x 1024 visionOS app icon, a single finished opaque square image. Design a cheeky premium cartoon emblem for an indie tropical open-world game launcher. Main subject: oversized ordinary neon-lime SUNGLASSES (two separate eyeglass lenses with a clear narrow bridge, absolutely not VR goggles, not a headset, not a visor), with two simple palm tree silhouettes reflected in their rich emerald lenses. The sunglasses are slightly tilted with personality, strongly outlined, centered and large. Integrate the exact word FIVE in original bold cream-and-lime chunky slab-serif lettering underneath or across the lower frame, completely custom lettering with subtle engraved/banknote hatching; keep it clean, readable and secondary to the sunglasses. A tiny warm coral sunset reflection can give tropical atmosphere. Background is a full-bleed opaque almost-black deep pine green with an understated green glow behind the emblem. Style is crisp cartoon poster graphics, thick confident shapes, a little dimensional bevel, very limited detail, high contrast; 80s neon coastal nightlife, mischievous and polished. Main emblem and all letters safely within central 72 percent width and 68 percent height so a circular app-icon mask will not cut them. No borders, no rounded-square container, no external drop shadow. No existing game logo, no official GTA V letterform or green Roman numeral V, no ribbon logo, no Rockstar symbol, no branded shape, no copied artwork, no guns, no characters, no cityscape, no watermark. Do not show devices/headsets/visors. Text must read exactly FIVE, all four letters visible and spelled correctly, no other text. Deliver only one square icon.

## Reframing prompt

> Edit only the composition/framing of this finished app-icon artwork. Keep the exact same sunglasses, palm-tree lens reflections, colors, crisp illustrated style, black outlines, and exact FIVE lettering. Scale the entire sunglasses-and-FIVE foreground artwork down uniformly to 80% of its current size, keep it centered, and fill the newly exposed area with the same deep dark green background and gentle green glow. The widest point of the emblem must fit within the middle 72% of the square canvas, with generous dark background on all sides for a circular visionOS app-icon mask. Do NOT crop or cut off the icon, do NOT draw an outer border or circular frame, no new objects, no headset, no new typography. Preserve opaque background. Output square.

## Packaging check

`project.yml` includes the catalog and selects `AppIcon`. Apple `actool` 27.0
compiles the catalog for `xros`, minimum deployment 26.0, device family `vision`,
with no errors or warnings. The emitted partial Info.plist names `AppIcon` as
`CFBundlePrimaryIcon`. The check runs on the host without starting a simulator:

```sh
mkdir -p target/verification/santos-original-icon-20261012/compiled
xcrun actool visionos/Assets.xcassets \
  --compile target/verification/santos-original-icon-20261012/compiled \
  --platform xros --minimum-deployment-target 26.0 --target-device vision \
  --app-icon AppIcon \
  --output-partial-info-plist target/verification/santos-original-icon-20261012/asset-info.plist \
  --output-format human-readable-text --warnings --notices
```

# Actual first-person capture — October 24, 2026

The explicitly rearmed installed build-24 session wrote a real capture. Root
copied only that own-app directory, validated all raw lengths/hashes, and
inspected colour and logarithmic depth previews. The earlier run with capture
disabled remains separate; it did not supply these files.

Evidence root:
`target/verification/device-session-20261024/sense-capture24-20261024T213856Z/rearmed-20261024T223043Z/`.
Files: `captured/manifest.json`, six registered `.raw` images,
`capture-validation.json`, `depth-point-checks.json`, `previews/`,
`runtime-05.json`, `capture-transfer.json` and the exact launch recipe.

## Recorded order

Present 2116, epoch 7, camera 1813, head 610, request 600, eye 0 of 2.
Trigger and published view match. All six images are 2560×1440; 2,963 events,
zero event omissions and no image omissions. Foreground merge is off.

1. Depth target 1221 clears, including lazy clear materialization before the
   first actual gun draw.
2. Fourteen opaque, depth-writing draws render seven gun/attachment and seven
   arm draws into target 1221, colour target 6329. No world record exists yet.
3. Qualified world geometry renders onto the same depth target, without an
   intervening destructive clear or replacement.
4. A late attributed gun decal has blending on and depth writes off.
5. After 1,675 world draws, target 1221 freezes for publication; the UI clear
   follows. This frozen depth is published with the same view identity.

Published raw depth visibly contains the gun/arm silhouettes aligned with
colour. Manually selected front-sight, receiver, sleeve and grip regions retain
identical depth values between the final recorded segment and publication.
These are visual checks and numeric samples, not inferred alpha, a complete
per-object coverage certificate or successful hand tracking.

## Consequences

The earlier general claim that merge-off necessarily loses the gun/arm depth
is contradicted by this arrangement. Actual foreground depth survives naturally
through later world drawing. **Do not enable the conditional merge as a fix.**
It currently rejects the initial pre-world draws with no qualified world record.

The capture's final after-foreground image follows the late decal and includes
intervening world geometry. Its difference from the initial image does not
isolate gun/arm coverage. It captures one eye/frame and does not prove movement,
both-eye agreement, reticle/bullet alignment or the headset's final composition.

The live report shows 954/954 corrections for each model family and native arm
requests, while cache-stage request cookies still do not match and drawable
skeleton observations remain null. The last reported arm-model placement agrees
with draw-world; that single last sample is not a gun-family certificate.

Next narrow diagnostic: observe the exact matching request immediately after
native 38548 completes its goal, then trace its actual solve/draw skeleton if
needed. Keep accepted third person and normal rendering defaults intact.
Visible Sense hands/arms/gun remain failed player acceptance.

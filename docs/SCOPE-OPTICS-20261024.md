# Qualified immersive scope optics

This host change preserves the native first-person scope's own zoom ratio. It
does not claim an absolute magnification relative to the ordinary third-person
or first-person gameplay camera. Flat rendering is unchanged.

The supported original is `game.wasm` SHA-256
`11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0`.
Existing embedded-AOT qualification and the original local player/weapon scope
predicates remain prerequisites. No Wasm import, AOT code or table slot changes.

At the actual primary `camera_post`, the host requires a fresh original scoped
aim observation for the current immersive epoch and requalifies its live local
camera and equipped weapon identity. Only the three owned camera metadata names
`ec22d348`, `8c40fb35`, `64e68b46` qualify, paired respectively with helpers
`26ea93d8`, `e2159f28`, `49f800ae`. Their minimum FOV values are 5, 7.5 and 10
degrees. Their common reference FOV is 45 degrees. The live camera metadata at
camera+568 must equal camera+16; metadata NameHash is at +8 and helper reference
at +28. Camera+576 identifies the helper, whose metadata is at +32 and whose
NameHash must match. The reference FOV is metadata+32 and the updated actual FOV
is camera+160. These runtime offsets are not serialized PSO field offsets.

Source recipe evidence is retained in
`target/verification/device-session-20261024/scope-optics-design/`: exact
original 26800/26273 constructors establish the metadata pointers, original
26415/26222 establish NameHash, original 26805 establishes the helper metadata
and native reference/updated FOV path, and `qualified-fov-reference.json` pins
the exact owned camera asset SHA and values. Original 26805 is retained in
`../sniper-scope17/source/26805-original.txt`. No raw global zoom scalar or
metadata+244 is treated as an ordinary camera FOV.

The current primary viewport FOV scalar must equal camera+160 bit for bit, and
the original `RegeneratePerspective` arithmetic must reproduce its matrix
exactly before XR crop ownership is acquired. Magnification is calculated in
f64 then cast to f32: `tan(reference/2) / tan(actual/2)`. Both clip X and Y scale
about the source guard projection's own forward-ray principal point. For its
canonical pinhole this multiplies p00 and p11 while preserving p8/p9 exactly;
clip Z/W are unchanged. The original typed camera and projection setters still
regenerate the engine's dependent matrices and frusta. All camera, metadata,
helper and FOV authority is rechecked around these guest calls. Failed setters
publish no qualified optical view and retain prospective crop ownership for
the existing current-primary callback restoration mechanism. Restoration keeps
the game's next fresh native lens rather than restoring a stale zoom value.

`WorldView` carries immutable optical state through GPU command provenance.
The production publisher copies it into the exact GPU-complete world lease.
The additive `sv_world_frame_optics_v1` accessor returns a 112-byte v1 sidecar;
the existing 288-byte request and world frame ABIs are unchanged. Tokens include
lease, provenance epoch, game/request/head/camera sequences, source eye/count,
presentation time and optical values. Its opaque scope sequence is stable
across zoom-slider changes and changes on scope entry, identity, epoch or stale
observation recovery. Retained state contains identity commitments, not guest
pointers. Unknown optical authority uses the exact identity path. Released or
stale-generation sidecars clear caller metadata before returning unavailable.

Host evidence is in
`target/verification/device-session-20261024/native-scope-optics18/`.
The actual Wasmtime setter fixture applies a magnified projection and exercises
next-native-update crop restoration. The actual Mac Metal test verifies leased
texture immutability, matching optical metadata, newer zoom-independent
publication, released leases and stale generations. Physical scope appearance,
absolute initial magnification, hand tracking, ballistics alignment and source
coverage at large optical scales still require the parent hardware test.

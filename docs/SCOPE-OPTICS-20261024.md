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

## Native scope graticule registration

The immersive native scope canvas is collimated. It uses the same selected
coverage-bearing HUD texture in both eyes, retaining each physical eye's
calibrated rotation and projection while removing eye-origin translation from
the canvas transform. Its original angular extent is unchanged. This prevents
the distant scope graticule from inheriting the ordinary HUD's 2 m reading-plane
convergence. The ordinary lowered HUD retains its exact existing finite-plane
transform, and window rendering is unaffected. Source leases and optical world
projection retain their existing independent per-eye provenance.

Run `scripts/test-scope-canvas.sh` for the captured physical-eye regression.
It checks 233 policy/projection assertions and executes the actual production
HUD Metal shaders in four offscreen 2048x2048 renders. With the captured 67.98 mm
IPD, the previous scope center introduced 1.98208 degrees of binocular
convergence; the collimated center differs by 0.00033 degrees after rasterization.
These are geometric and Mac Metal results. They do not establish that native
HUD isolation succeeds during the user's scoped frame, nor physical headset
acceptance of the complete scoped world and graticule.

## Scoped world registration against the collimated graticule (sniper22)

The graticule above is correct in isolation, but the magnified world it
overlays was not registered with it. Two layer defects remained after
collimation, in both first-person and third-person scopes:

- Baseline. Display magnification multiplies every binocular disparity by m,
  while the graticule has none. A target at distance D therefore showed
  m*b/D against it. With the captured 67.98mm IPD at m 9.487 (5 degree
  sniper lens) that is 1.82 degrees at 20m and 0.72 degrees at 50m: the same
  magnitude as the 1.98 degree reading-plane defect, at ordinary sniping
  distances.
- Magnification center. Each displayed eye was magnified about its own
  calibrated -Z. The captured eye frames differ by 0.041 degrees vertically, so
  magnification added (m - 1) times that: a constant 0.35 degree vertical
  misregistration at every distance, beyond normal vertical fusion tolerance.

Correction, active only for qualified scoped optics with m > 1:
`GameWorldScopeOptics.displayProjection` magnifies both eyes about the HEAD
forward axis, the direction on which the graticule is collimated. Z/W rows are
unchanged. `eyeTransforms` and the native `ScopedEyeBaseline` in xr_view
scale only the inter-ocular translation by 1/m about the calibrated eyes'
midpoint. The native source renders from that virtual eye and the display
reprojects from the same one. The magnified world then keeps the natural
disparity b/D of the true distance, as with a both-eyes-open sight collimated
at infinity. Rotations, the cyclopean point and two distinct eye renders
remain. No eye image is duplicated and no same-update stereo is claimed. Source
projections and their admission are unchanged, identity optics are byte-exact,
and the window path is untouched. The native and Swift baselines are
bit-identical.

First-person scoping very likely uses the same camera, not a separate path.
The owned weapons.meta gives both sniper rifles `DisableFPSAimForScope`.
Their scope components name `SNIPER_LOW_ZOOM_AIM_CAMERA`/`SNIPER_AIM_CAMERA`
(8c40fb35/ec22d348, already-qualified metadata), and these are also the
rifles' third-person default cameras. In first-person shooter mode, original
CTaskGun::GetOverriddenGameplayCameraSettings takes its camera hash from the
equipped scope component (+44 of the component object). This is a
source/data inference: the field mapping is not proven and no runtime
first-person scope observation has been recorded.

`scripts/test-scope-binocular.sh` extracts the native block verbatim, runs
its standalone tests and a bit fixture, then evaluates the production scoped
source, reprojection and display chain for both captured eyes against the
production collimated graticule: 388 assertions, m 1/3.146/6.320/9.487,
10-900m, on- and off-axis. The previous path exceeds natural disparity by up to
3.28 degrees horizontally and 0.37 degrees vertically. The corrected path stays
within 0.0041 degrees of natural, keeps the aim axis under the graticule at
every zoom, keeps m-times magnification and stays inside the 1.3 source guard
band. CPU geometry only.

Not corrected here: if a scoped drawable lacks the single HUD canvas, each eye
shows its own baked GTA HUD at its asymmetric source center. On the captured
calibration that is about 20 degrees outward per eye, so it can never fuse.
`hud_canvas.native_scope_canvas_unavailable` counts such drawables;
`immersive_aim.scope_sequences_allocated` counts qualified scope sequences ever
minted; `hud_canvas.scoped_world_virtual_baseline_drawables` counts corrected
world drawables. Natural disparity remains: fixating a target nearer than
about 20m still doubles the infinity graticule physiologically (0.39 degrees
at 10m), as in unmagnified viewing. The single-source stereo diagnostic's pose
check and opt-in geometry proof captures expect physical eye translations, so
they do not qualify scoped frames. Headset fusion, comfort, sniping accuracy
and HUD isolation during the user's scoped frames still need physical
acceptance.

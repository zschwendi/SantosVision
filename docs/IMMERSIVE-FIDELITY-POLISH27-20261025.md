# Immersive fidelity polish 27

Build26 is the accepted pointing baseline, preserved at `los-santos-point-aim-accepted26-20261025` (`eae9693`). The player confirms controller point-to-aim, binocular reticle tracking, shots landing where aimed, initial Wasted and first-person ADS entry. Right-hand roll, controller motion during ADS and enlarged Wasted stereo remain open. This pass preserves the accepted native firing and gun placement.

## Source and independent review

Actual external review ran as `claude-opus-5-5`, requested xhigh with Fast off, session `9ef35274-457f-49a9-8d39-201424bcd55c`; it completed successfully. The local report/receipt are in `target/verification/device-session-20261025/polish27/opus-quality/`. Opus reviewed source and one historical build25 snapshot, with no new build/device measurement. Its report is advisory; root and independent local review checked implementation tradeoffs.

The window copies the final game image at integer texels. Immersive uses the same already-lit color but reconstructs geometry from depth and bilinearly resamples it over a much wider field. No missing normals, lower immersive texture setting, extra tone mapper or newly proven gamma defect was found. Texture quality and AF were already high in the historical configuration. Source vertical density, resampling, depth-discontinuity gaps and alternating natural eye updates are separate limitations. No physical performance or flat-equivalent fidelity is claimed.

Opus suggested reducing the vertical guard to1.15. We rejected that as a safe default: the existing corner-cone guarantee couples both axes, and pure5-degree yaw alone needs vertical scale1.199 on the sampled frustum. Keeping the same cone/floor provides no rest gain and only modest moving gains. Head-turn coverage, physical eye transforms, source graphics and display allocation defaults remain unchanged.

## Changes and evidence

- `3924379` removes redundant stale private-mode history only at the two certified native arm callbacks. Their current original first-person predicate is authoritative there. All packet age/epoch/tracking and native owner/stack/request checks remain; other readers retain mode history. Fourteen aim and fourteen arm focused tests pass. This is an ADS-freeze correction candidate, not physical cause/fix acceptance.
- `4fcc311` adds a default-off smooth-continuity experiment after the unchanged affine4-ULP depth rule. Nine finite depths must meet metric first/second/mixed difference bounds tied to the source-ray footprint. Clear/invalid/sky/border and rejected neighborhoods keep old footprints. Runtime QA env `MUGUET_COMPOSITOR_SMOOTH_CONTINUITY=1` or argument `--sv-world-smooth-continuity` selects it; `--sv-world-disable-smooth-continuity` wins. Both eyes/captures/precompute share the captured policy and224-byte uniform ABI.
- Forty-two synthetic actual Mac Metal cases pass, including feature-off byte identity, both source depth conventions, steps/thin poles/sky, same-eye source color and bounded analytic curved interiors. Defined sphere/cylinder interiors lose2558 holes to0; this is a restricted synthetic mask, not gameplay improvement. Native-size paired M4 median deltas range from about-0.85 to+1.44ms, with no uniform speed gain.
- Actual historical third-person source replay is more limited: smooth changes interior owner0 count10449 to10423. Fill-only removes5556, leaving4893 under this source-domain mask; smooth+fill leaves the same4893. All1557 audited body gaps are filled. For each fixed geometry, fill preserves covered color/depth and unsupported borders exactly. The actual game body crops were visually inspected: fill removes the black outlines/specks; smooth adds little there. It stays off for the first physical quality comparison.

The replay's maximum depth delta includes clear0 becoming real coverage, not two disagreeing surfaces. Largest both-covered reverse-depth delta is0.000103928; top covered RGB changes are1LSB. The smooth classifier is a bounded heuristic, not a general occlusion or display-pixel error proof. Full reports, raw source/pose replay outputs and hashes are in `polish27/quality/continuity27/`.

Enlarged Wasted is not fixed by these quality changes. Its next diagnostic must distinguish missing final-Lens candidate, rejected HUD writer/resource lineage and admitted HUD before world-depth/GPU completion. Do not share an unqualified full eye image or infer coverage from color subtraction.

`8197cc1` plus `7343997` retain bounded diagnostic evidence only during the
certified original's live native death/fail effect states1/2. These states are
not a Wasted movie/pixel certificate. The observer keeps missing, rejected and
admitted candidates separately per eye, the exact first writer/resource hazard,
and a16-draw tail. Its own compact status cap is8KiB; the historical native
payload plus that cap fits the64KiB reader, which is not a permanent whole-status
bound. All guest-derived reads check the complete width and integer/range bounds
before shared reads. Six focused tests and independent review pass. Rendering
and admission decisions are unchanged. See `polish27/hud/` receipts and review.

Client reports now include actual bundle build/version and a process-lifetime
session UUID. A copied historical report can no longer be mistaken for a new
build merely because it was downloaded again. The launcher footer continues to
use the actual bundle build.

## Vertical color reconstruction experiment

Two separate opt-in Metal fragment entries retain the original bilinear entry
points and geometry ABI. The candidate samples four exact source row centers,
retains horizontal bilinear filtering, evaluates vertical Catmull-Rom and clamps
each channel to its central two samples. Finite local source support, source
bounds and the actual vertical derivative footprint qualify sampling; sky,
minification and unsafe support keep the exact original sample. The final
qualification uses a3×4 affine NDC-depth neighborhood including extra horizontal
context, rather than pretending that two columns can distinguish a depth step
from a slope. It is still a conservative image heuristic, not a surface proof.

Runtime flag `--sv-world-vertical-reconstruction` or
`MUGUET_COMPOSITOR_VERTICAL_RECONSTRUCTION=1` requests it; an explicit disable
argument wins. Baseline pipeline states remain available if optional pipeline
creation fails. Copy/probe/adaptive combinations retain baseline sampling.
Every drawable also needs complete qualified HUD isolation before selecting the
new sampler: otherwise an unextracted HUD could be sharpened as world color.
Actual per-pixel eligibility and subjective quality are not inferred from
pipeline availability.

The synthetic phase test improves sine RMSE0.02830→0.01205 and alternating-row
RMSE0.06040→0.00573, without central-range overshoot. Tested native horizontal
minification with vertical enlargement remains eligible. Actual registered
scene geometry/depth/ownership stay identical; eligible RGB changes are bounded
by the local clamp and fallback pixels remain exact.

**The cost is too high for a normal default.** The original metric-depth gate
adds about9.4–9.6ms on native-size synthetic M4 planes. The cheaper affine gate
cuts that to about5ms, but the actual scene gains more eligible pixels and still
adds about7.45ms. These are offscreen Mac results, not headset timing. Keep this
experiment off for the initial headset comparison. More expensive sharpening or
display resolution is not a substitute for a measured rendering budget.

The final48 sampling checks and6 geometry/production-encoder cases pass on Mac
Metal, including allowed/disabled/missing optional pipeline selection, both
source depth directions and actually selected precomputed vertices. The
reviewed source was committed at `e9f8044`; root owns the separate HUD-gated
Swift selection and report identity. Historical world crops were visually
compared: sharper edges are a modest gain at the measured extra cost. Retain
the experiment for controlled investigation, with normal play unchanged.

## Next rendering-budget work

The existing adaptive mesh has promising historical synthetic timings but
cannot be enabled as a shortcut: current adaptive selection disables the
finite-sky ownership path and therefore gap fill. Its old fragment pipeline
does not write the ownership MRT. The next meaningful benchmark must combine
production adaptive classification/fans with the same ownership, finite-sky
repair, gap fill, source, matrices and sampler, compare image/depth/ownership
first, then measure complete commands with classification overhead and class
counts. Do not promote an old isolated kernel timing into headset performance.
The bounded read-only review is AstraBoard work
`w-a915d1a786b147659e7f1862d51c477a`, topic `santosvision-immersive`.
No adaptive tolerance or normal-play default changed in this candidate.

## Next actual comparison

Use the next signed app, keep the accepted source settings/coverage and first run with owner0 fill enabled, smooth continuity off, normal display quality and no single-source stereo. Check a stationary scene and lateral head motion in first person; verify black silhouette improvement and absence of smeared foreground/background. Recheck Sense motion during ADS and preserve point-to-aim/reticle/shots. Obtain one fresh native death/fail transition report for enlarged Wasted. Right-wrist orientation needs the user's pending distinction between a rolled gun plus hand and an upright gun with a twisted wrist. Do not apply a global180-degree flip without that distinction.

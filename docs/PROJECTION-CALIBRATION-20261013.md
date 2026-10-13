# Physical compositor projection compatibility

Build 4 rejected both eye projections before native world rendering with `sv_xr_submit_views = -3`. The actual physical packet is retained locally in `target/verification/immersive-black-20261012/runtime-report-build4-black-20261013.json`, under `immersive_lifecycle.latest_rejected_view_request.request_bytes_hex`. Its header, rigid eye transforms and finite reverse depth passed. Calibration terms at matrix indices 3, 4 and 7 reached 3.62e-6, while the game's sparse scalar camera representation requires those terms below 1e-7.

`GameWorldProjection.nativePinhole` normalizes homogeneous scale and clears only bounded calibration residuals. It retains focal lengths, asymmetric stereo offsets and finite near/far depth. An analytic bound on `canonical * inverse(physical)` certifies ray and depth error across the entire clip box at at most 2e-5 NDC; a sampled check also runs. Unsupported skew, oblique, infinite and nonfinite projections remain rejected. Native validation is unchanged.

The retained eye record and native request use the same converted source projection. Final compositor encoding continues using Apple's complete physical projection. Its depth reprojection therefore accounts for the source conversion. Direct source-copy eligibility keeps its existing lattice and projection checks and can fall back to reprojection.

The standalone Swift regression consumes the exact 288-byte packet, verifies 18,491 double-precision points per eye, rejects malformed matrices and writes the actual converted native packet. Compile `visionos/Sources/GameWorldProjection.swift` with `visionos/Tests/ProjectionCalibrationValidation.swift`; run the result with input and output binary paths. Local evidence and production Rust contract checks are in `target/verification/projection-calibration-20261013`. The raw packet returned -3; the actual Swift output passed the unmodified Rust validator. Observed ray errors were 6.383832e-6 and 4.347413e-6 NDC, below 0.013 pixels at 3840 width.

Both Master Cheef and Project Reach pass full compositor matrices to Metal and do not impose GTA's scalar camera restrictions. The fix is specific to the GTA source camera boundary. Original game AOT and game assets remain unchanged. Build 5 is a hardware candidate; these numerical and build checks do not establish visible immersive rendering or stereo performance.

## User checkpoint

After build 5 installation, the user reported “The game appears around me” and requested this exact version be saved and committed. The implementation is commit `f32bbb1e91332673d95b16e96c46c2d5412dfcf1`; the signed build and installation receipts are retained at `target/verification/render-hotpath-20261011/los-santos-calibration-device-v5`. Local tag `los-santos-calibration-device-v5-20261013` pins the implementation.

The report copied at 2026-10-13T09:10:06Z shows window mode and no immersive lifecycle events or completed world-frame counters. It does not corroborate the user's immersive session; preserve that distinction. First visible success is user-reported. Image correctness, both-eye telemetry, performance and phone regression remain separate follow-up checks. Preserve this build before further changes.

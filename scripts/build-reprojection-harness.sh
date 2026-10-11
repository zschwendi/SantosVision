#!/bin/sh
# CPU only: extracts the production blocks verbatim, compiles the standalone harness and
# compiles both shader files offline for every SDK. Runs nothing on the GPU.
set -eu
cd "$(dirname "$0")/.."
D=target/verification/reprojection-implementation-v1
B=$D/build
mkdir -p "$B"
extract() { awk -v begin="$2" -v end="$3" '$0 ~ begin {on=1} on {print} $0 ~ end {on=0}' "$1" > "$4"; grep -q "$5" "$4"; }
extract visionos/Sources/GameWorldAdaptiveMesh.swift '^// BEGIN AdaptiveScratchPool' '^// END AdaptiveScratchPool' "$B/AdaptiveScratchPool.extracted.swift" "final class AdaptiveScratchPool"
extract visionos/Sources/GameWorldSourceCopy.swift '^// BEGIN PureSourceChecks' '^// END PureSourceChecks' "$B/PureSourceChecks.extracted.swift" "enum GameWorldPairValidation"
extract visionos/Sources/XRHeadTracking.swift '^// BEGIN XRRetainedEyeView' '^// END XRRetainedEyeView' "$B/XRRetainedEyeView.extracted.swift" "struct XRRetainedEyeView"
cat scripts/reprojection-harness.swift "$B/AdaptiveScratchPool.extracted.swift" "$B/PureSourceChecks.extracted.swift" "$B/XRRetainedEyeView.extracted.swift" > "$B/reprojection-harness-combined.swift"
xcrun swiftc -O -j 2 -import-objc-header visionos/WorldBridge.h "$B/reprojection-harness-combined.swift" -o "$B/reprojection-harness"
cp visionos/Sources/GameWorld.metal "$B/candidate-GameWorld.metal"
git show e74b9d2:visionos/Sources/GameWorld.metal > "$B/baseline-GameWorld.metal"
expected_baseline=62dc4b92fa41c7af2592b166b41175b12626a8b7e627400360c229c25f90c146
actual_baseline=$(shasum -a 256 "$B/baseline-GameWorld.metal" | cut -d " " -f 1)
[ "$actual_baseline" = "$expected_baseline" ] || { echo "baseline source digest mismatch" >&2; exit 1; }
for sdk in macosx xrsimulator xros; do
    xcrun -sdk "$sdk" metal -c visionos/Sources/GameWorld.metal -o "$B/candidate-$sdk.air"
    xcrun -sdk "$sdk" metal -c visionos/Sources/GameWorldProbe.metal -o "$B/probe-$sdk.air"
done
xcrun -sdk macosx metal -c "$B/baseline-GameWorld.metal" -o "$B/baseline-macosx.air"
shasum -a 256 "$B/reprojection-harness" "$B"/*.extracted.swift "$B/reprojection-harness-combined.swift" "$B/candidate-GameWorld.metal" "$B/baseline-GameWorld.metal"

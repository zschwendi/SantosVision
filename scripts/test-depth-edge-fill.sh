#!/bin/sh
# CPU-only checks for the opt-in depth-edge gap fill: Metal compile-only ABI
# asserts, then production Swift policy/uniforms plus a CPU mirror of the Metal
# walk on analytic scenes. An optional registered capture prefix replays the
# device's actual uncovered output pixels. No GPU execution.
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/depth-edge-fill}
capture=${2:-}
mkdir -p "$out/build"
cat > "$out/build/layout.metal" <<'METAL'
#include "GameWorld.metal"
static_assert(sizeof(GameWorldDepthEdgeFillUniforms) == 240, "fill uniform size");
static_assert(__builtin_offsetof(GameWorldDepthEdgeFillUniforms, viewportOrigin) == 192, "viewport");
static_assert(__builtin_offsetof(GameWorldDepthEdgeFillUniforms, sourceSize) == 208, "source size");
static_assert(__builtin_offsetof(GameWorldDepthEdgeFillUniforms, maximumInverseDistance) == 224, "distance");
static_assert(__builtin_offsetof(GameWorldDepthEdgeFillUniforms, maximumSteps) == 232, "steps");
static_assert(sizeof(GameWorldUniforms) == 224, "existing world uniforms");
METAL
for sdk in xros macosx; do
    xcrun -sdk "$sdk" metal -std=metal3.1 -Wall -Werror -I visionos/Sources -c "$out/build/layout.metal" -o "$out/build/layout-$sdk.air"
done
cp visionos/Tests/DepthEdgeFillValidation.swift "$out/build/main.swift"
xcrun swiftc -O visionos/Sources/GameWorldDepthEdgeFillPolicy.swift visionos/Tests/DepthEdgeFillMirror.swift "$out/build/main.swift" \
    -o "$out/build/depth-edge-fill-validation"
if [ -n "$capture" ]; then
    "$out/build/depth-edge-fill-validation" "$capture" "$out"
else
    "$out/build/depth-edge-fill-validation"
fi

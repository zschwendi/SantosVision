#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/depth-fill-preference}
mkdir -p "$out/build"
cp visionos/Tests/DepthEdgeFillPreferenceValidation.swift "$out/build/main.swift"
xcrun swiftc -O visionos/Sources/GameWorldDepthEdgeFillPolicy.swift "$out/build/main.swift" -o "$out/build/validate"
"$out/build/validate"

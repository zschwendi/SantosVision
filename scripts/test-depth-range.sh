#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261024/depth-range20}
packet=${2:-target/verification/device-session-20261024/scope-optics-design/physical-request.bin}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/GameWorldDepthRange.swift visionos/Sources/GameWorldProjection.swift \
    visionos/Sources/GameWorldSourceGuardBand.swift visionos/Sources/GameWorldScopeOptics.swift \
    visionos/Tests/DepthRangeValidation.swift -o "$out/swift-depth-validation"
"$out/swift-depth-validation" "$packet"

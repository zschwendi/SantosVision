#!/bin/sh
# CPU only. Production value capture/collector; no GPU, device or simulator.
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/completed-display-pair}
mkdir -p "$out/build"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    visionos/Sources/GameWorldDepthEdgeFillPolicy.swift \
    visionos/Sources/GameWorldDepthEdgeFill.swift \
    visionos/Tests/CompletedDisplayPairValidation.swift -framework Metal -o "$out/build/validation"
"$out/build/validation"

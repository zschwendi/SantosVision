#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/game-scene-transitions}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/GameSceneTransitions.swift visionos/Tests/GameSceneTransitionsValidation.swift \
    -o "$out/game-scene-transitions-validation"
"$out/game-scene-transitions-validation"

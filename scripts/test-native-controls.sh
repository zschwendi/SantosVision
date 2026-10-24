#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/native-controls}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/NativeControlsInput.swift visionos/Tests/NativeControlsValidation.swift \
    -o "$out/native-controls-validation"
"$out/native-controls-validation"

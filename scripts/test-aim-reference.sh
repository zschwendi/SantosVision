#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261024/barrel-reticle21}
packet=${2:-target/verification/device-session-20261024/scope-optics-design/physical-request.bin}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -import-objc-header visionos/RuntimeBridge.h \
    visionos/Sources/GameWorldReticle.swift visionos/Tests/AimReferenceValidation.swift -o "$out/validate-aim-reference"
"$out/validate-aim-reference" "$packet"

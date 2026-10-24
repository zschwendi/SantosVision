#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/xr-motion-pose}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/XRMotionPosePolicy.swift visionos/Tests/XRMotionPoseValidation.swift \
    -o "$out/motion-pose-validation"
"$out/motion-pose-validation"

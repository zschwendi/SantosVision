#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/left-wrist-calibration}
mkdir -p "$out"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/LeftWristCalibrationFlow.swift visionos/Tests/LeftWristCalibrationValidation.swift \
    -o "$out/left-wrist-calibration-validation"
"$out/left-wrist-calibration-validation"

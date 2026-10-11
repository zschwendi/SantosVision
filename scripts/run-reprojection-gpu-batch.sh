#!/bin/sh
# GPU. Run ONLY by the owner of the GPU slot (root), with nothing else using the GPU.
# Bounded: self-check, parity (small fixtures), then the paired timing batch. Fresh run id required.
# usage: run-gpu-batch.sh <run-id>      e.g. run-gpu-batch.sh root-batch-1
set -eu
cd "$(dirname "$0")/.."
D=target/verification/reprojection-implementation-v1
mkdir -p "$D/runs"
ID=${1:?run id}
case "$ID" in *[!a-zA-Z0-9_-]*|""|.) echo "invalid run id" >&2; exit 2;; esac
RUN="$D/runs/$ID"
mkdir "$RUN"
H="$D/build/reprojection-harness"
CANDIDATE="$D/build/candidate-GameWorld.metal"
BASELINE="$D/build/baseline-GameWorld.metal"
shasum -a 256 "$H" "$CANDIDATE" "$BASELINE" visionos/Sources/GameWorld.metal visionos/Sources/GameWorldRenderer.swift visionos/Sources/GameWorldAdaptiveMesh.swift visionos/Sources/GameWorldSourceCopy.swift > "$RUN/inputs.sha256"
cmp "$CANDIDATE" visionos/Sources/GameWorld.metal || { echo "stale candidate shader" >&2; exit 1; }
date -u +%FT%TZ > "$RUN/started-at.txt"
status=0
"$H" selfcheck > "$RUN/selfcheck.json" 2> "$RUN/selfcheck.stderr" || status=$?
echo "selfcheck exit $status" > "$RUN/exit-codes.txt"
if [ "$status" -eq 0 ]; then
    "$H" parity "$CANDIDATE" "$BASELINE" "$RUN" > "$RUN/parity.stdout" 2> "$RUN/parity.stderr" || status=$?
    echo "parity exit $status" >> "$RUN/exit-codes.txt"
fi
if [ "$status" -eq 0 ]; then
    timing=0
    "$H" timing "$CANDIDATE" "$BASELINE" "$RUN" > "$RUN/timing.stdout" 2> "$RUN/timing.stderr" || timing=$?
    echo "timing exit $timing" >> "$RUN/exit-codes.txt"
    status=$timing
else
    echo "timing skipped: earlier stage failed" >> "$RUN/exit-codes.txt"
fi
date -u +%FT%TZ > "$RUN/finished-at.txt"
shasum -a 256 "$RUN"/*.json > "$RUN/results.sha256" 2>/dev/null || true
cat "$RUN/exit-codes.txt"
exit "$status"

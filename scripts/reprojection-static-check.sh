#!/bin/sh
# CPU-only static checks for the experimental reprojection paths. No app build, no GPU, no launch.
# Root executes this sequentially; logs land under target/verification/reprojection-implementation-v1/static/.
#   1. Swift 6 strict-concurrency typecheck of every visionos source: simulator, device, and the non-runtime build.
#   2. Offline Metal compile of GameWorld.metal and GameWorldProbe.metal for macosx, xrsimulator, xros.
#   3. Harness build (extracts production blocks verbatim) and its CPU-only selfcheck.
# Every step writes its own log and the script exits with the FIRST non-zero compiler status (fail closed);
# nothing is piped through tee, so no exit status is masked.
set -u
cd "$(dirname "$0")/.."
OUT=target/verification/reprojection-implementation-v1/static
mkdir -p "$OUT"
date -u +%FT%TZ > "$OUT/started-at.txt"
: > "$OUT/exit-codes.txt"
failed=0
run() {
    # run <log-name> <command...>: captures stdout+stderr to the log, echoes it, records and propagates failure.
    name=$1; shift
    "$@" > "$OUT/$name.log" 2>&1
    status=$?
    echo "$name exit $status" >> "$OUT/exit-codes.txt"
    echo "== $name (exit $status)"
    cat "$OUT/$name.log"
    if [ "$status" -ne 0 ] && [ "$failed" -eq 0 ]; then failed=$status; fi
}
run typecheck-simulator xcrun -sdk xrsimulator swiftc -typecheck -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    -import-objc-header visionos/RuntimeBridge.h -target arm64-apple-xros26.0-simulator -j 2 visionos/Sources/*.swift
run typecheck-device xcrun -sdk xros swiftc -typecheck -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    -import-objc-header visionos/RuntimeBridge.h -target arm64-apple-xros26.0 -j 2 visionos/Sources/*.swift
run typecheck-noruntime xcrun -sdk xrsimulator swiftc -typecheck -swift-version 6 -strict-concurrency=complete \
    -import-objc-header visionos/RuntimeBridge.h -target arm64-apple-xros26.0-simulator -j 2 visionos/Sources/*.swift
for sdk in macosx xrsimulator xros; do
    run "metal-GameWorld-$sdk" xcrun -sdk "$sdk" metal -c visionos/Sources/GameWorld.metal -o "$OUT/GameWorld-$sdk.air"
    run "metal-GameWorldProbe-$sdk" xcrun -sdk "$sdk" metal -c visionos/Sources/GameWorldProbe.metal -o "$OUT/GameWorldProbe-$sdk.air"
done
run harness-build sh scripts/build-reprojection-harness.sh
if [ -x target/verification/reprojection-implementation-v1/build/reprojection-harness ]; then
    run harness-selfcheck target/verification/reprojection-implementation-v1/build/reprojection-harness selfcheck
else
    echo "harness-selfcheck exit 127 (binary missing)" >> "$OUT/exit-codes.txt"
    if [ "$failed" -eq 0 ]; then failed=127; fi
fi
shasum -a 256 visionos/Sources/*.swift visionos/Sources/*.metal visionos/WorldBridge.h > "$OUT/sources.sha256"
date -u +%FT%TZ > "$OUT/finished-at.txt"
cat "$OUT/exit-codes.txt"
if [ "$failed" -ne 0 ]; then echo "static checks FAILED (first failing status $failed)"; exit "$failed"; fi
echo "static checks complete"

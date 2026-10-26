#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ] || { [ "$#" -eq 2 ] && [ "$2" != build-only ]; }; then
    echo "Usage: $0 NEW_OUTPUT_DIR [build-only]" >&2
    exit 2
fi
out=$1
if [ -e "$out/blocked-writer.json" ] || [ -e "$out/metal-validation.json" ] || [ -e "$out/image-validation.json" ]; then
    echo "Use a new output directory; a completed GPU receipt already exists" >&2
    exit 2
fi
scripts/test-ordered-source-reuse.sh "$out" build-only
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    "$out/build/GameWorldUniforms.swift" visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Tests/OrderedPendingWriterRepro.swift -framework Metal -o "$out/build/pending-writer-repro"
if [ "${2:-}" = build-only ]; then exit 0; fi
"$out/build/pending-writer-repro" "$PWD" "$out/blocked-writer.json" > "$out/blocked-writer.txt"
"$out/build/metal-validation" "$PWD" "$out/metal-validation.json" > "$out/metal-validation.txt"
"$out/build/image-validation" "$PWD" "$out/image-validation.json" > "$out/image-validation.txt"
cat "$out/blocked-writer.txt" "$out/metal-validation.txt" "$out/image-validation.txt"

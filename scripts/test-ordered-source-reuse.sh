#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ] || { [ "$#" -eq 2 ] && [ "$2" != build-only ]; }; then
    echo "Usage: $0 OUTPUT_DIR [build-only]" >&2
    exit 2
fi
out=$1
if [ "${2:-}" != build-only ] && { [ -e "$out/metal-validation.json" ] || [ -e "$out/image-validation.json" ]; }; then
    echo "Use a new output directory; completed GPU result already exists" >&2
    exit 2
fi
mkdir -p "$out/build"
python3 - "$out/build/GameWorldUniforms.swift" <<'PY'
import pathlib, sys
source = pathlib.Path('visionos/Sources/GameWorldRenderer.swift').read_text()
start = source.index('struct GameWorldUniforms {')
end = source.index('\n}', start) + len('\n}')
pool_start = source.index('// BEGIN CornerScratchPool')
pool_end = source.index('// END CornerScratchPool', pool_start)
pathlib.Path(sys.argv[1]).write_text('import simd\nimport Foundation\nimport Metal\n'
    + source[start:end] + '\n' + source[pool_start:pool_end] + '\n')
PY
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Tests/OrderedSourceReusePolicyValidation.swift -o "$out/build/policy-validation"
"$out/build/policy-validation" > "$out/policy-validation.txt"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME -D SV_ORDERED_CORNERS_VALIDATION \
    "$out/build/GameWorldUniforms.swift" visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Tests/OrderedSourceReuseMetalValidation.swift -framework Metal -o "$out/build/metal-validation"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    "$out/build/GameWorldUniforms.swift" visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift \
    visionos/Sources/GameWorldDepthEdgeFillPolicy.swift visionos/Sources/GameWorldDepthEdgeFill.swift \
    visionos/Tests/OrderedSourceReuseImageValidation.swift -framework Metal -o "$out/build/image-validation"
cat "$out/policy-validation.txt"
if [ "${2:-}" = build-only ]; then exit 0; fi
"$out/build/metal-validation" "$PWD" "$out/metal-validation.json" > "$out/metal-validation.txt"
cat "$out/metal-validation.txt"
"$out/build/image-validation" "$PWD" "$out/image-validation.json" > "$out/image-validation.txt"
cat "$out/image-validation.txt"

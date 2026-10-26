#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "Usage: $0 OUTPUT_DIR [build-only]" >&2
    exit 2
fi
if [ "$#" -eq 2 ] && [ "$2" != build-only ]; then
    echo "The optional second argument must be build-only" >&2
    exit 2
fi
out=$1
if [ "${2:-}" != build-only ] && { [ -e "$out/metal-pool-validation.json" ] || [ -e "$out/integration-validation.json" ]; }; then
    echo "Use a new output directory; completed GPU result files already exist" >&2
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
xcrun swiftc -O visionos/Sources/GameWorldOrderedCorners.swift visionos/Tests/OrderedCornersBudgetValidation.swift \
    -o "$out/build/budget-validation"
"$out/build/budget-validation" > "$out/budget-validation.txt"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME -D SV_ORDERED_CORNERS_VALIDATION \
    "$out/build/GameWorldUniforms.swift" visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Tests/OrderedCornersMetalPoolValidation.swift -framework Metal -o "$out/build/metal-pool-validation"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME -D SV_ORDERED_CORNERS_VALIDATION \
    "$out/build/GameWorldUniforms.swift" visionos/Sources/GameWorldOrderedCorners.swift \
    visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift \
    visionos/Tests/OrderedCornersIntegrationValidation.swift -framework Metal -o "$out/build/integration-validation"
cat "$out/budget-validation.txt"
if [ "${2:-}" = build-only ]; then exit 0; fi
"$out/build/metal-pool-validation" "$PWD" "$out/metal-pool-validation.json" > "$out/metal-pool-validation.txt"
cat "$out/metal-pool-validation.txt"
"$out/build/integration-validation" "$PWD" "$out/integration-validation.json" > "$out/integration-validation.txt"
cat "$out/integration-validation.txt"

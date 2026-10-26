#!/bin/sh
# Root GPU step: actual offscreen Mac Metal parity of the production finite-sky
# ownership pass plus the production depth-edge fill, on the registered
# candidate7 capture. Run from the main checkout (it reads target/verification).
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/depth-edge-fill-metal}
mkdir -p "$out/build"
# Extract the actual production world uniform ABI instead of maintaining a copy.
python3 - "$out/build/GameWorldUniforms.swift" <<'PY'
import pathlib, sys
source = pathlib.Path('visionos/Sources/GameWorldRenderer.swift').read_text()
start = source.index('struct GameWorldUniforms {')
end = source.index('\n}', start) + 2
pool_start = source.index('// BEGIN CornerScratchPool')
pool_end = source.index('// END CornerScratchPool', pool_start)
pathlib.Path(sys.argv[1]).write_text('import simd\nimport Foundation\nimport Metal\n' + source[start:end] + '\n' + source[pool_start:pool_end] + '\n')
PY
cp visionos/Tests/DepthEdgeFillMetalValidation.swift "$out/build/main.swift"
xcrun swiftc -O -D SV_GAME_RUNTIME "$out/build/GameWorldUniforms.swift" \
    visionos/Sources/GameWorldOrderedCorners.swift visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift \
    visionos/Sources/GameWorldDepthEdgeFillPolicy.swift visionos/Sources/GameWorldDepthEdgeFill.swift \
    visionos/Tests/DepthEdgeFillMirror.swift "$out/build/main.swift" \
    -o "$out/build/depth-edge-fill-metal-validation"
if [ "${COMPILE_ONLY:-0}" = 1 ]; then exit 0; fi
"$out/build/depth-edge-fill-metal-validation" "$PWD" "$out"

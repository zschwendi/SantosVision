#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/finite-sky-support-20261020}
mkdir -p "$out/build"
# Extract the actual production ABI instead of maintaining a second struct.
python3 - "$out/build/GameWorldUniforms.swift" <<'PY'
import pathlib, sys
source = pathlib.Path('visionos/Sources/GameWorldRenderer.swift').read_text()
start = source.index('struct GameWorldUniforms {')
end = source.index('\n}', start) + 2
pathlib.Path(sys.argv[1]).write_text('import simd\n' + source[start:end] + '\n')
PY
cp visionos/Tests/FiniteSkySupportMetalValidation.swift "$out/build/main.swift"
xcrun swiftc -O -D SV_GAME_RUNTIME "$out/build/GameWorldUniforms.swift" \
    visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift "$out/build/main.swift" \
    -o "$out/build/finite-sky-support-validation"
"$out/build/finite-sky-support-validation" "$PWD" "$out"

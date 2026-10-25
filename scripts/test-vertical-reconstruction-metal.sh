#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261025/polish27/quality/vertical27}
baseline=${2:-$out/GameWorld.baseline.metal}
mkdir -p "$out/build"
if [ ! -f "$baseline" ]; then git show 3d1935a:visionos/Sources/GameWorld.metal > "$baseline"; fi
python3 - "$out/build/GameWorldUniforms.swift" <<'PY'
from pathlib import Path
import sys
s=Path('visionos/Sources/GameWorldRenderer.swift').read_text()
a=s.index('struct GameWorldUniforms {');b=s.index('\n}',a)+2
c=s.index('// BEGIN CornerScratchPool');d=s.index('// END CornerScratchPool',c)
Path(sys.argv[1]).write_text('import simd\nimport Foundation\nimport Metal\n'+s[a:b]+'\n'+s[c:d]+'\n')
PY
cp visionos/Tests/VerticalReconstructionMetalValidation.swift "$out/build/main.swift"
xcrun swiftc -O -D SV_GAME_RUNTIME "$out/build/GameWorldUniforms.swift" \
    visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift \
    "$out/build/main.swift" -o "$out/build/vertical-reconstruction-validation"
"$out/build/vertical-reconstruction-validation" "$PWD" "$out" "$baseline" "${3:-}"

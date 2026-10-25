#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261025/polish27/quality/continuity27/registered-replay}
baseline=${2:-target/verification/device-session-20261025/polish27/quality/continuity27/GameWorld.baseline.metal}
mkdir -p "$out/build"
if [ ! -f "$baseline" ]; then
    git show 3f9584b1661f840ef2c6acd3d49cb67a2f7c567d:visionos/Sources/GameWorld.metal > "$baseline"
fi
python3 - "$out/build/GameWorldUniforms.swift" <<'PY'
from pathlib import Path
import sys
s = Path('visionos/Sources/GameWorldRenderer.swift').read_text()
a = s.index('struct GameWorldUniforms {')
b = s.index('\n}', a) + 2
c = s.index('// BEGIN CornerScratchPool')
d = s.index('// END CornerScratchPool', c)
Path(sys.argv[1]).write_text('import simd\nimport Foundation\nimport Metal\n' + s[a:b] + '\n' + s[c:d] + '\n')
PY
cp visionos/Tests/ContinuityGapFillReplayMetalValidation.swift "$out/build/main.swift"
xcrun swiftc -O -D SV_GAME_RUNTIME "$out/build/GameWorldUniforms.swift" \
    visionos/Sources/GameWorldFiniteSkyPolicy.swift visionos/Sources/GameWorldFiniteSkySupport.swift \
    visionos/Sources/GameWorldDepthEdgeFillPolicy.swift visionos/Sources/GameWorldDepthEdgeFill.swift \
    "$out/build/main.swift" -o "$out/build/continuity-gap-fill-replay"
"$out/build/continuity-gap-fill-replay" "$PWD" "$out" "$baseline"

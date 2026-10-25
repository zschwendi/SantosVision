#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261025/polish27/quality/continuity27}
baseline=${2:-$out/GameWorld.baseline.metal}
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
Path(sys.argv[1]).write_text('import simd\n' + s[a:b] + '\n')
PY
cp visionos/Tests/CurvedSurfaceContinuityMetalValidation.swift "$out/build/main.swift"
xcrun swiftc -O "$out/build/GameWorldUniforms.swift" "$out/build/main.swift" -o "$out/build/curved-surface-validation"
"$out/build/curved-surface-validation" "$PWD" "$out" "$baseline" "${3:-}"

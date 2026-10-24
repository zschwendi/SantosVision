#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/sense-utilities}
mkdir -p "$out"
python3 - "$out" <<'PY'
from pathlib import Path
import sys
s=Path('visionos/Sources/GameWorldSenseUtilities.swift').read_text()
start=s.index('    private struct Uniforms {')
end=s.index('\n    }',start)+6
block=s[start:end].replace('    private struct Uniforms {','struct SenseUtilityTestUniforms {',1)
Path(sys.argv[1]+'/ActualUniforms.swift').write_text('import simd\n'+block+'\n')
PY
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/NativeControlsInput.swift visionos/Sources/GameWorldSenseUtilities.swift \
    "$out/ActualUniforms.swift" visionos/Tests/SenseUtilitiesValidation.swift \
    -o "$out/sense-utilities-validation"
"$out/sense-utilities-validation" visionos/Sources/GameWorld.metal

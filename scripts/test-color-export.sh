#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/color-export}
mkdir -p "$out/build"
python3 - "$out/build/GameWorldColorFile.swift" <<'PY'
from pathlib import Path
import sys
s=Path('visionos/Sources/GameWorldDebugCapture.swift').read_text()
a=s.index('// BEGIN GameWorldColorFile')
b=s.index('// END GameWorldColorFile',a)
Path(sys.argv[1]).write_text('import CoreGraphics\nimport Foundation\nimport ImageIO\nimport Metal\n'+s[a:b]+'\n')
PY
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    "$out/build/GameWorldColorFile.swift" visionos/Tests/ColorExportValidation.swift \
    -framework CoreGraphics -framework ImageIO -framework Metal -o "$out/build/color-export-validation"
"$out/build/color-export-validation" "$out/images"

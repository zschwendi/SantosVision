#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: $0 NEW_OUTPUT_DIR REGISTERED_OPTICAL_COMPARISON_DIR [build-only]" >&2
    exit 2
fi
out=$1
registry=$2
if [ -e "$out/report.json" ]; then
    echo "Use a fresh output directory; completed evidence already exists" >&2
    exit 2
fi
mkdir -p "$out/build"
# Freeze the exact historical shader, independently of the working tree.
git show 177acd56c88b5e41132d1d42ddc3dd53d58bc053:visionos/Sources/GameWorld.metal > "$out/build/GameWorld-before.metal"
python3 - "$out/build" <<'PY'
import hashlib, json, pathlib, shutil, sys
out = pathlib.Path(sys.argv[1])
s = pathlib.Path('visionos/Sources/GameWorldRenderer.swift').read_text()
a = s.index('struct GameWorldUniforms {')
b = s.index('\n}', a) + 2
c = s.index('// BEGIN CornerScratchPool')
d = s.index('// END CornerScratchPool', c)
hud = pathlib.Path('visionos/Sources/GameWorldHUD.swift').read_text().split('#if SV_GAME_RUNTIME')[0]
(out / 'Uniforms.swift').write_text('import Foundation\nimport Metal\nimport simd\n' + s[a:b] + '\n' + s[c:d] + '\n' + hud)
files = ['GameWorldColor.swift', 'GameWorldOrderedCorners.swift', 'GameWorldFiniteSkyPolicy.swift',
         'GameWorldFiniteSkySupport.swift', 'GameWorldDepthEdgeFillPolicy.swift', 'GameWorldDepthEdgeFill.swift',
         'GameWorldDepthRange.swift', 'GameWorld.metal', 'GameWorldHUD.swift', 'GameWorldRenderer.swift']
paths = [pathlib.Path('visionos/Sources') / f for f in files]
paths += [pathlib.Path('visionos/Tests/ColorPipelineMetalValidation.swift'), pathlib.Path('scripts/test-color-pipeline-metal.sh')]
manifest = {}
for path in paths:
    target = out / 'source' / path
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(path, target)
    manifest[str(path)] = hashlib.sha256(target.read_bytes()).hexdigest()
(out / 'source-manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
PY
src=$out/build/source/visionos
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    "$out/build/Uniforms.swift" "$src/Sources/GameWorldColor.swift" "$src/Sources/GameWorldOrderedCorners.swift" \
    "$src/Sources/GameWorldFiniteSkyPolicy.swift" "$src/Sources/GameWorldFiniteSkySupport.swift" \
    "$src/Sources/GameWorldDepthEdgeFillPolicy.swift" "$src/Sources/GameWorldDepthEdgeFill.swift" \
    "$src/Sources/GameWorldDepthRange.swift" "$src/Tests/ColorPipelineMetalValidation.swift" \
    -framework Metal -framework CoreGraphics -framework ImageIO -o "$out/build/color-pipeline-validation"
if [ "${3:-}" = build-only ]; then exit 0; fi
"$out/build/color-pipeline-validation" "$out/build/source" "$registry" "$out"

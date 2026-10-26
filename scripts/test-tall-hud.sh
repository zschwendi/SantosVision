#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -ne 4 ]; then
    echo "Usage: $0 OUTPUT_DIR PHYSICAL_REQUEST BASELINE_RGBA16F TALL_RGBA16F" >&2
    exit 2
fi
out=$1
packet=$2
baseline=$3
tall=$4
mkdir -p "$out/build"
# Use the actual private Swift uniform declaration. The production Metal shader
# is compiled from the repository without extraction or edits.
python3 - "$out/build/ScopeCanvasUniforms.swift" <<'PY'
import pathlib, sys
source = pathlib.Path('visionos/Sources/GameWorldHUD.swift').read_text()
start = source.index('    private struct Uniforms {')
end = source.index('\n    }', start) + len('\n    }')
actual = source[start:end].replace('private struct Uniforms', 'struct ScopeCanvasUniforms', 1)
pathlib.Path(sys.argv[1]).write_text('import simd\n' + actual + '\n')
PY
xcrun swiftc -O visionos/Sources/GameWorldHUD.swift visionos/Tests/HUDPolicyValidation.swift \
    -o "$out/build/hud-policy-validation"
"$out/build/hud-policy-validation" "$packet" > "$out/policy-validation.txt"
xcrun swiftc -O visionos/Sources/GameWorldHUD.swift "$out/build/ScopeCanvasUniforms.swift" \
    visionos/Tests/TallHUDMetalValidation.swift -framework Metal -o "$out/build/tall-hud-metal-validation"
"$out/build/tall-hud-metal-validation" "$PWD" "$packet" "$baseline" "$tall" "$out/metal-validation.json" \
    > "$out/metal-validation.txt"
cat "$out/policy-validation.txt" "$out/metal-validation.txt"

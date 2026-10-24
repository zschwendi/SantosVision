#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/device-session-20261024/binocular-scope19}
packet=${2:-target/verification/device-session-20261024/scope-optics-design/physical-request.bin}
mkdir -p "$out/build"
# This registered fixture contains calibrated eye transforms/projections only.
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
    visionos/Tests/ScopeCanvasMetalValidation.swift -framework Metal -o "$out/build/scope-canvas-metal-validation"
"$out/build/scope-canvas-metal-validation" "$PWD" "$packet" > "$out/metal-validation.txt"
cat "$out/policy-validation.txt" "$out/metal-validation.txt"

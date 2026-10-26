#!/bin/sh
# CPU only; extract the exact production relative transform, without Metal.
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/common-target-stage-qa}
mkdir -p "$out/build"
python3 - "$out/build/production-math.swift" <<'PY'
from pathlib import Path
import sys
s=Path('visionos/Sources/GameWorldRenderer.swift').read_text()
a=s.index('private func worldDoubleMatrix(');b=s.index('\nprivate func worldFloatMatrix(',a)
start=s.index('        let gameFromStage = worldDoubleMatrix(',s.index('    private func worldUniforms('));end=s.index('\n        var uniforms = GameWorldUniforms(',start)
Path(sys.argv[1]).write_text('import simd\n'+s[a:b].replace('private func','func',1)+'\nstruct Source { let gameFromStage: simd_float4x4; let gameFromEye: simd_float4x4 }\nfunc productionRelative(eye: Source, stageFromHead: simd_float4x4, headFromEye: simd_float4x4, targetGameFromStage: simd_float4x4? = nil) -> simd_double4x4 {\n'+s[start:end]+'\nreturn relativeEye\n}\n')
PY
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/GameWorldDepthEdgeFillPolicy.swift visionos/Sources/GameWorldCommonTargetStage.swift \
    "$out/build/production-math.swift" visionos/Tests/CommonTargetStageValidation.swift -o "$out/build/validation"
"$out/build/validation" "$out/cpu-results.json"

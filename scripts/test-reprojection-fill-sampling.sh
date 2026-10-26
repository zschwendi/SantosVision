#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: $0 NEW_OUTPUT_DIR REGISTERED_OPTICAL_COMPARISON_DIR [build-only]" >&2
    exit 2
fi
out=$1
registration=$2
if [ -e "$out/plane-validation.json" ] || [ -e "$out/registered/report.json" ]; then
    echo "Use a new output directory; completed evidence already exists" >&2
    exit 2
fi
mkdir -p "$out/build"
git show d53bea339e205a5ab38b919baf72e640048ef8fd:visionos/Sources/GameWorld.metal > "$out/build/GameWorld-before.metal"
python3 - "$out/build" <<'PY'
import hashlib,json,pathlib,shutil,sys
out=pathlib.Path(sys.argv[1])
files=['GameWorldColor.swift','GameWorldOrderedCorners.swift','GameWorldFiniteSkyPolicy.swift',
       'GameWorldFiniteSkySupport.swift','GameWorldDepthEdgeFillPolicy.swift','GameWorldDepthEdgeFill.swift',
       'GameWorldDepthRange.swift','GameWorld.metal','GameWorldRenderer.swift']
paths=[pathlib.Path('visionos/Sources')/f for f in files]
paths += [pathlib.Path('visionos/Tests')/f for f in ['ReprojectionFillSamplingProbe.metal',
          'ReprojectionFillSamplingValidation.swift','ReprojectionFillPlaneValidation.swift']]
paths += [pathlib.Path('scripts/test-reprojection-fill-sampling.sh')]
manifest={}
for path in paths:
    target=out/'source'/path
    target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(path,target)
    manifest[str(path)]=hashlib.sha256(target.read_bytes()).hexdigest()
s=(out/'source/visionos/Sources/GameWorldRenderer.swift').read_text()
a=s.index('struct GameWorldUniforms {');b=s.index('\n}',a)+2
c=s.index('// BEGIN CornerScratchPool');d=s.index('// END CornerScratchPool',c)
(out/'Uniforms.swift').write_text('import Foundation\nimport Metal\nimport simd\n'+s[a:b]+'\n'+s[c:d])
(out/'source-manifest.json').write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n')
PY
src=$out/build/source/visionos
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    "$out/build/Uniforms.swift" "$src/Sources/GameWorldColor.swift" "$src/Sources/GameWorldOrderedCorners.swift" \
    "$src/Sources/GameWorldFiniteSkyPolicy.swift" "$src/Sources/GameWorldFiniteSkySupport.swift" \
    "$src/Sources/GameWorldDepthEdgeFillPolicy.swift" "$src/Sources/GameWorldDepthEdgeFill.swift" \
    "$src/Sources/GameWorldDepthRange.swift" "$src/Tests/ReprojectionFillSamplingValidation.swift" \
    -framework Metal -framework CoreGraphics -framework ImageIO -o "$out/build/registered-validation"
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete -D SV_GAME_RUNTIME \
    "$src/Sources/GameWorldDepthEdgeFillPolicy.swift" "$src/Sources/GameWorldDepthEdgeFill.swift" \
    "$src/Tests/ReprojectionFillPlaneValidation.swift" -framework Metal -o "$out/build/plane-validation"
if [ "${3:-}" = build-only ]; then exit 0; fi
"$out/build/plane-validation" "$out/build/source" "$out/plane-validation.json" "$out/build/GameWorld-before.metal" > "$out/plane-validation.log"
"$out/build/registered-validation" "$out/build/source" "$registration" "$out/registered" "$out/build/GameWorld-before.metal" > "$out/registered-validation.log"
python3 - "$out" "$registration" <<'PY'
import hashlib,json,pathlib,sys
out=pathlib.Path(sys.argv[1]);registration=pathlib.Path(sys.argv[2])
registry=json.loads((registration/'registration.json').read_text())
inputs={}
capture=pathlib.Path(registry['profiles']['baseline1440'])
for eye in [0,1]:
    for suffix in ['report.json','hud-world.ppm','source-depth.f32']:
        path=capture/f'eye{eye}-{suffix}'
        digest=hashlib.sha256(path.read_bytes()).hexdigest()
        assert registry['inputs_sha256'][str(path)]==digest,str(path)
        inputs[str(path)]=digest
for path in [registration/'registration.json',pathlib.Path(registry['fixed_view_input'])]:
    inputs[str(path)]=hashlib.sha256(path.read_bytes()).hexdigest()
receipt={'schema':1,'baseline_commit':'d53bea339e205a5ab38b919baf72e640048ef8fd',
         'registered_input_hashes':inputs,'source_manifest':json.loads((out/'build/source-manifest.json').read_text()),
         'files_sha256':{str(f.relative_to(out)):hashlib.sha256(f.read_bytes()).hexdigest() for f in sorted(out.rglob('*')) if f.is_file()},
         'device_simulator_game_launch':False,'physical_quality_or_performance_acceptance':False}
(out/'receipt.json').write_text(json.dumps(receipt,indent=2,sort_keys=True)+'\n')
PY
cat "$out/plane-validation.log" "$out/registered-validation.log"

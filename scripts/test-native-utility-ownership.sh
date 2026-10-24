#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=${1:-target/verification/native-utility-ownership}
mkdir -p "$out"
python3 - "$out" <<'PY'
import hashlib,json,pathlib,sys
root=pathlib.Path.cwd(); out=pathlib.Path(sys.argv[1]).resolve()
source=root/'visionos/Sources/NativeRuntime.swift'; text=source.read_text()
a=text.index('// BEGIN NativeSenseUtilityOwnership'); b=text.index('// END NativeSenseUtilityOwnership',a)
(out/'NativeSenseUtilityOwnership.swift').write_text(text[a:b])
files=['visionos/Sources/NativeRuntime.swift','visionos/Sources/NativeControlsInput.swift','visionos/Tests/NativeUtilityOwnershipValidation.swift','scripts/test-native-utility-ownership.sh']
(out/'source-hashes.json').write_text(json.dumps({p:hashlib.sha256((root/p).read_bytes()).hexdigest() for p in files},indent=2)+'\n')
PY
xcrun swiftc -O -swift-version 6 -strict-concurrency=complete \
    visionos/Sources/NativeControlsInput.swift "$out/NativeSenseUtilityOwnership.swift" \
    visionos/Tests/NativeUtilityOwnershipValidation.swift -o "$out/native-utility-ownership"
"$out/native-utility-ownership"

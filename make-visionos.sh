#!/bin/bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: ./make-visionos.sh diagnostic [simulator|device]
       ./make-visionos.sh game [simulator|device]

diagnostic builds only the native stereo calibration and tracking checkpoint.
game fails: the game package and engine camera/compositor bridge are not implemented.
The device build is unsigned. This script never boots simulators or installs to a headset.
USAGE
}

if [[ $# -lt 1 || $# -gt 2 ]]; then usage >&2; exit 64; fi
mode="$1"
destination="${2:-simulator}"
case "$mode" in
    diagnostic) ;;
    game)
        echo 'Cannot build GTA immersive mode: no GTA runtime/camera bridge is linked.' >&2
        echo 'A compatible game ZIP with data/ and b/<build>/game.wasm is also required; retail PC/console discs and ISOs do not satisfy this runtime.' >&2
        exit 65 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 64 ;;
esac
case "$destination" in
    simulator) xcode_destination='generic/platform=visionOS Simulator' ;;
    device) xcode_destination='generic/platform=visionOS' ;;
    *) usage >&2; exit 64 ;;
esac
command -v xcodegen >/dev/null || { echo 'Missing XcodeGen. Install it before generating the visionOS project.' >&2; exit 69; }
xcrun --sdk xros --show-sdk-path >/dev/null || { echo 'A full Xcode installation with the visionOS SDK is required.' >&2; exit 69; }
repo="$(cd -- "$(dirname -- "$0")" && pwd)"
project_dir="$repo/target/visionos-xcode"
mkdir -p "$project_dir"
xcodegen generate --spec "$repo/visionos/project.yml" --project "$project_dir"
xcodebuild -project "$project_dir/SantosVision.xcodeproj" -scheme SantosVision \
    -configuration Debug -destination "$xcode_destination" \
    -derivedDataPath "$project_dir/DerivedData-$destination" \
    CODE_SIGNING_ALLOWED=NO build
echo "Built SantosVision diagnostic only ($destination). GTA immersive gameplay is unavailable."

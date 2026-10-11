#!/bin/bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: ./make-visionos.sh diagnostic [simulator|device]
       ./make-visionos.sh game [simulator|device]

diagnostic builds the native per-view calibration and tracking checkpoint.
game links the native Rust runtime, embedded AOT module, camera mailboxes, and immersive world renderer.
game builds the matching runtime using scripts/build-vision-runtime.sh.
Set MUGUET_GAME_WASM to override its source module, or MUGUET_RUNTIME_ARCHIVE to use an existing archive.
Game Swift is optimized; set MUGUET_SWIFT_OPTIMIZATION=-Onone when debugging Swift code.
Game data must be installed separately. World images require qualified native camera/color/depth snapshots.
The device build is unsigned. This script never boots simulators or installs to a headset.
USAGE
}

if [[ $# -lt 1 || $# -gt 2 ]]; then usage >&2; exit 64; fi
mode="$1"
destination="${2:-simulator}"
case "$mode" in
    diagnostic) ;;
    game) ;;
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
settings=(CODE_SIGNING_ALLOWED=NO)
if [[ "$mode" == game ]]; then
    if [[ -n "${MUGUET_RUNTIME_ARCHIVE:-}" ]]; then
        runtime_archive="$MUGUET_RUNTIME_ARCHIVE"
    elif [[ -n "${MUGUET_GAME_WASM:-}" ]]; then
        runtime_archive="$("$repo/scripts/build-vision-runtime.sh" "$destination" "$MUGUET_GAME_WASM")"
    else
        runtime_archive="$("$repo/scripts/build-vision-runtime.sh" "$destination")"
    fi
    [[ -f "$runtime_archive" ]] || {
        echo "Native runtime archive is missing: $runtime_archive" >&2
        echo 'Build the matching native Rust static library with its target-specific MUGUET_GAME_CODE first.' >&2
        exit 65
    }
    runtime_archive="$(cd -- "$(dirname -- "$runtime_archive")" && pwd)/$(basename -- "$runtime_archive")"
    # The embedded __GAME code must be executable in the final Swift binary.
    # Rust's binary linker flags cannot configure this separate Xcode link step.
    settings+=("MUGUET_RUNTIME_ARCHIVE=$runtime_archive"
        "SWIFT_OPTIMIZATION_LEVEL=${MUGUET_SWIFT_OPTIMIZATION:--O}"
        'SWIFT_ACTIVE_COMPILATION_CONDITIONS=$(inherited) SV_GAME_RUNTIME'
        "SWIFT_OBJC_BRIDGING_HEADER=$repo/visionos/RuntimeBridge.h"
        'OTHER_LDFLAGS=$(inherited) -Wl,-force_load,$(MUGUET_RUNTIME_ARCHIVE) -Wl,-segprot,__GAME,rx,rx -framework AudioToolbox -framework AVFAudio -framework CoreAudio -framework CoreFoundation -framework CoreGraphics -framework Foundation -framework GameController -framework Metal -framework QuartzCore -framework Security -framework SystemConfiguration -lc++ -liconv -lobjc -lSystem -lc -lm'
        ARCHS=arm64 ONLY_ACTIVE_ARCH=YES)
fi
xcodebuild -project "$project_dir/SantosVision.xcodeproj" -scheme SantosVision \
    -configuration Debug -destination "$xcode_destination" \
    -derivedDataPath "$project_dir/DerivedData-$mode-$destination" \
    "${settings[@]}" build
echo "Built SantosVision $mode ($destination)."

#!/bin/sh
# This is a private local Metal fixture. It never starts or links the game runtime.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
if [ "$#" -lt 2 ]; then
  echo "usage: run.sh CAPTURE_JSON OUTPUT_DIRECTORY [--cpu-only]" >&2
  exit 2
fi
export CARGO_HOME=${CARGO_HOME:-/Users/zach/gaming/SantosVision/.tools/cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-/Users/zach/gaming/SantosVision/.tools/rustup}
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_TARGET_DIR="$root/target/central-density-proof/build"
exec python3 "$root/scripts/central-density-proof/run.py" "$@"

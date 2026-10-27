#!/bin/sh
# Local immutable capture replay only; never links or starts the game.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
export CARGO_HOME=${CARGO_HOME:-/Users/zach/gaming/SantosVision/.tools/cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-/Users/zach/gaming/SantosVision/.tools/rustup}
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_TARGET_DIR="$root/target/central-density-proof/span-build"
exec python3 "$root/scripts/central-density-proof/replay-span.py" "$@"

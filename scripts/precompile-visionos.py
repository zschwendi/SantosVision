#!/usr/bin/env python3
"""Compile a validated web-port WASM for visionOS; this does not build an app."""

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("game_preflight", Path(__file__).with_name("validate-game.py"))
preflight = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preflight)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("game_zip", type=Path)
    parser.add_argument("--compiler", type=Path, default=ROOT / "target/debug/muguet")
    parser.add_argument("--output", type=Path, default=ROOT / "target/visionos-game/game.cwasm")
    args = parser.parse_args()
    report = preflight.validate(args.game_zip)
    if not report["ok"]:
        for error in report["errors"]:
            print(f"Game package: {error}", file=sys.stderr)
        return 1
    compiler = args.compiler.resolve()
    if not compiler.is_file() or not os.access(compiler, os.X_OK):
        print("Build the host compiler first: cargo build --locked --bin muguet", file=sys.stderr)
        return 1
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    # Keep the previous artifact intact if validation, extraction or compilation fails.
    with tempfile.TemporaryDirectory(prefix=".visionos-precompile-", dir=output.parent) as staging:
        wasm = Path(staging) / "game.wasm"
        compiled = Path(staging) / "game.cwasm"
        with zipfile.ZipFile(args.game_zip) as archive:
            with archive.open(report["wasm_entry"]) as source, wasm.open("wb") as dest:
                shutil.copyfileobj(source, dest, length=1024 * 1024)
        wasm_hash = sha256(wasm)
        command = [str(compiler), "--precompile-visionos", str(wasm), str(compiled)]
        process = subprocess.run(command, cwd=ROOT)
        if process.returncode != 0:
            return process.returncode if process.returncode > 0 else 1
        if not compiled.is_file() or compiled.stat().st_size == 0:
            print("Compiler did not produce a nonempty artifact.", file=sys.stderr)
            return 1
        os.replace(compiled, output)
        receipt = {
            "target": "aarch64-apple-visionos",
            "build": report["build"],
            "wasm_entry": report["wasm_entry"],
            "wasm_sha256": wasm_hash,
            "compiler": str(compiler),
            "compiler_sha256": sha256(compiler),
            "artifact": str(output),
            "artifact_sha256": sha256(output),
            "scope": "AOT game code only; no native runtime, camera bridge or headset playability verified",
        }
        output.with_name(output.name + ".json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Compiled visionOS game code: {output}")
    print("This is a compiler artifact, not a playable Vision Pro app.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, zipfile.BadZipFile, RuntimeError) as exc:
        print(f"Precompile failed: {exc}", file=sys.stderr)
        sys.exit(1)

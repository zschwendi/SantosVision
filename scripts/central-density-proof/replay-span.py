#!/usr/bin/env python3
"""Freeze and replay a real private native two-draw capture; no game/device APIs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] != "--cpu-only"):
        raise SystemExit("usage: replay-span.py MANIFEST NEW_OUTPUT_DIRECTORY [--cpu-only]")
    source = Path(__file__).resolve().parent
    root = source.parent.parent
    manifest = Path(sys.argv[1]).resolve(strict=True)
    metadata = json.loads(manifest.read_text())
    if metadata.get("schema") != 2 or metadata.get("kind") != "native_draw_span":
        raise SystemExit("Requires actual native_draw_span schema2; no geometry-only fallback")
    blobs = []
    total = 0
    for blob in metadata["blobs"]:
        relative = Path(blob["path"])
        if relative.is_absolute() or any(p in ("..", ".") for p in relative.parts):
            raise SystemExit("Invalid relative blob path")
        path = (manifest.parent / relative).resolve(strict=True)
        if not path.is_relative_to(manifest.parent):
            raise SystemExit("Blob escapes capture directory")
        total += blob["byte_length"]
        if total > 256 * 1024 * 1024 or path.stat().st_size != blob["byte_length"] or sha(path) != blob["sha256"]:
            raise SystemExit("Capture blob size/hash limit failed")
        blobs.append((path, relative))
    output = Path(sys.argv[2]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "capture"
    frozen.mkdir()
    shutil.copyfile(manifest, frozen / "manifest.json")
    for path, relative in blobs:
        destination = frozen / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    files = sorted(p for p in source.rglob("*") if p.is_file())
    before = {str(p.relative_to(root)): sha(p) for p in files}
    for path in files:
        destination = output / "source" / path.relative_to(source)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    receipt = {"schema": 1, "scope": "standalone real two-draw replay; no game/device/simulator/settings/save actions",
               "original_manifest": str(manifest), "original_manifest_sha256": sha(manifest),
               "source_sha256_before": before, "gpu_requested": len(sys.argv) == 3,
               "capture_blobs": {str(relative): sha(path) for path, relative in blobs}}
    started = time.time()
    command = ["cargo", "build", "--offline", "--locked", "--manifest-path", str(source / "Cargo.toml"), "--bin", "replay_span"]
    receipt["build_command"] = command
    with (output / "build.log").open("wb") as log:
        build = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=300)
    code = build.returncode
    receipt["build_returncode"] = code
    if code == 0:
        binary = Path(os.environ["CARGO_TARGET_DIR"]) / "debug/replay_span"
        receipt["binary_sha256"] = sha(binary)
        command = [str(binary), str(frozen / "manifest.json"), str(output / "replay")] + sys.argv[3:]
        receipt["run_command"] = command
        with (output / "run.log").open("wb") as log:
            run = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=120)
        code = run.returncode
        receipt["run_returncode"] = code
        print((output / "run.log").read_text()[-3000:])
    else:
        print((output / "build.log").read_text()[-3000:])
    receipt["elapsed_wall_seconds_not_gpu_time"] = time.time() - started
    receipt["source_sha256_after"] = {str(p.relative_to(root)): sha(p) for p in files}
    receipt["inputs_unchanged"] = (before == receipt["source_sha256_after"] and sha(manifest) == receipt["original_manifest_sha256"]
        and all(sha(path) == receipt["capture_blobs"][str(relative)] == sha(frozen / relative) for path, relative in blobs))
    receipt["artifact_sha256"] = {str(p.relative_to(output)): sha(p) for p in sorted(output.rglob("*")) if p.is_file()}
    receipt["passed"] = code == 0 and receipt["inputs_unchanged"]
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Receipt: {output / 'receipt.json'}")
    return code if code else (0 if receipt["inputs_unchanged"] else 1)


if __name__ == "__main__":
    sys.exit(main())

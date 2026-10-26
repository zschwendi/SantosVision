#!/usr/bin/env python3
"""Freeze inputs and log one private fixture invocation; no game or device tools."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] != "--cpu-only"):
        raise SystemExit("usage: run.py CAPTURE_JSON NEW_OUTPUT_DIRECTORY [--cpu-only]")
    directory = Path(__file__).resolve().parent
    root = directory.parent.parent
    capture = Path(sys.argv[1]).resolve(strict=True)
    output = Path(sys.argv[2]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    sources = sorted(path for path in directory.rglob("*") if path.is_file())
    vendor = root / "vendor/wgpu-core"
    sources += sorted(vendor.rglob("*.rs")) + [vendor / "Cargo.toml"]
    before = {str(path.relative_to(root)): digest(path) for path in sources}
    capture_before = digest(capture)
    target = Path(os.environ["CARGO_TARGET_DIR"])
    build = ["cargo", "build", "--offline", "--locked", "--manifest-path", str(directory / "Cargo.toml")]
    started = time.time()
    with (output / "build.log").open("wb") as log:
        built = subprocess.run(build, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=300)
    receipt = {"schema": 1, "scope": "standalone private fixture; no game/app/device/simulator/save/settings actions",
               "source_hashes_before": before, "private_capture_path": str(capture),
               "private_capture_sha256": capture_before, "build_command": build,
               "build_returncode": built.returncode, "gpu_requested": len(sys.argv) == 3}
    code = built.returncode
    binary = target / "debug/same-eye-central-density-proof"
    if code == 0:
        receipt["binary_sha256"] = digest(binary)
        command = [str(binary), str(capture), str(output)] + sys.argv[3:]
        receipt["run_command"] = command
        with (output / "run.log").open("wb") as log:
            run = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=120)
        code = run.returncode
        receipt["run_returncode"] = code
        print((output / "run.log").read_text()[-1500:])
    else:
        print((output / "build.log").read_text()[-3000:])
    receipt["elapsed_wall_seconds_not_gpu_time"] = time.time() - started
    receipt["source_hashes_after"] = {str(path.relative_to(root)): digest(path) for path in sources}
    receipt["inputs_unchanged"] = before == receipt["source_hashes_after"] and capture_before == digest(capture)
    receipt["output_sha256"] = {path.name: digest(path) for path in output.iterdir() if path.is_file()}
    receipt["passed"] = code == 0 and receipt["inputs_unchanged"]
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Receipt: {output / 'receipt.json'}")
    return code if code else (0 if receipt["inputs_unchanged"] else 1)


if __name__ == "__main__":
    sys.exit(main())

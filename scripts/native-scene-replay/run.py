#!/usr/bin/env python3
"""Freeze inputs/source and run the generic replay. GPU use requires an assigned slot."""
import hashlib
import json
import pathlib
import subprocess
import sys


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: run.py BINARY SCENE_JSON NEW_OUTPUT")
    binary, manifest, out = (pathlib.Path(x).resolve() for x in sys.argv[1:])
    if manifest.stat().st_size > 32 << 20:
        raise ValueError("manifest limit")
    source_root = pathlib.Path(__file__).resolve().parent
    if out == source_root or source_root in out.parents:
        raise ValueError("output must be outside the source directory")
    raw = manifest.read_bytes()
    if len(raw) > 32 << 20:
        raise ValueError("manifest changed beyond bound")
    metadata = json.loads(raw)
    root = manifest.parent
    out.mkdir()
    frozen = out / "input"
    frozen.mkdir()
    (frozen / "scene.json").write_bytes(raw)
    checked = [(manifest, digest(raw)), (frozen / "scene.json", digest(raw))]
    total = 0
    seen = set()
    for item in metadata["blobs"]:
        rel = pathlib.Path(item["path"])
        if rel.is_absolute() or any(p in (".", "..") for p in rel.parts) or str(rel) in seen:
            raise ValueError("unsafe or repeated blob path")
        seen.add(str(rel))
        source = (root / rel).resolve()
        source.relative_to(root)
        total += item["bytes"]
        if total > 256 << 20 or source.stat().st_size != item["bytes"]:
            raise ValueError("input budget/length")
        data = source.read_bytes()
        if len(data) != item["bytes"] or digest(data) != item["sha256"]:
            raise ValueError("input identity changed")
        dest = frozen / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(data)
        checked.extend(((source, item["sha256"]), (dest, item["sha256"])))
    source_root = pathlib.Path(__file__).resolve().parent
    records = []
    for source in sorted(source_root.rglob("*")):
        if not source.is_file() or "__pycache__" in source.parts:
            continue
        data = source.read_bytes()
        dest = out / "source" / source.relative_to(source_root)
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(data)
        records.append({"path": str(source), "sha256": digest(data)})
        checked.append((source, digest(data)))
    binary_sha = digest(binary.read_bytes())
    checked.append((binary, binary_sha))
    command = [str(binary), "run", str(frozen / "scene.json"), str(out / "replay")]
    with (out / "replay.log").open("wb") as log:
        process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=300)
    unchanged = all(p.is_file() and digest(p.read_bytes()) == expected for p, expected in checked)
    receipt = {"schema": 1, "command": command, "exit_code": process.returncode,
               "manifest_sha256": digest(raw), "binary_sha256": binary_sha,
               "sources": records, "original_and_frozen_inputs_and_sources_unchanged": unchanged,
               "passed": process.returncode == 0 and unchanged,
               "finished_frame_or_density_accepted": False}
    (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    raise SystemExit(0 if receipt["passed"] else 1)


if __name__ == "__main__":
    main()

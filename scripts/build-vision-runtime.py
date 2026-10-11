#!/usr/bin/env python3
"""Build a signed, no-JIT native archive with a matching local GTA AOT artifact.

All build logs go to stderr. The sole stdout line is the absolute archive path,
so make-visionos.sh can consume it without inferring an unverified build output.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "simulator": ("aarch64-apple-visionos-sim", "--precompile-visionos-sim", "game.visionos-sim.cwasm"),
    "device": ("aarch64-apple-visionos", "--precompile-visionos", "game.visionos.cwasm"),
}
CONFIG = {"memory_reservation": 5 << 30, "memory_guard_size": 64 << 10,
          "memory_may_move": False, "memory64": True, "shared_memory": True,
          "threads": True, "cpu_has_lse": True, "cranelift_opt_level": "Speed",
          "heap_access_spectre_mitigation": False, "table_access_spectre_mitigation": False}


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(8 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def atomic_json(path, data):
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, prefix=f".{path.name}.", delete=False) as out:
        temporary = Path(out.name)
        json.dump(data, out, indent=2)
        out.write("\n")
        out.flush()
        os.fsync(out.fileno())
    os.replace(temporary, path)


def command(args, env):
    print("[native-build] " + " ".join(map(str, args)), file=sys.stderr, flush=True)
    subprocess.run(args, cwd=ROOT, env=env, check=True, stdout=sys.stderr, stderr=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", choices=TARGETS)
    parser.add_argument("wasm", nargs="?", type=Path,
                        default=ROOT.parent / "game-assets/playgta5-full-game-v1/game.wasm")
    parser.add_argument("--jobs", type=int, default=3,
                        help="Cargo and AOT compiler worker limit (default: 3)")
    args = parser.parse_args()
    if not 1 <= args.jobs <= 32:
        parser.error("--jobs must be between 1 and 32")
    source = args.wasm.resolve(strict=True)
    with source.open("rb") as data:
        if data.read(8) != b"\0asm\x01\0\0\0":
            parser.error("input must be the compatible web-port game.wasm, not a Windows executable or console ISO")
    target, option, artifact_name = TARGETS[args.destination]
    cargo = ROOT / ".tools/cargo/bin/cargo"
    if not cargo.is_file():
        parser.error(f"isolated Rust toolchain is missing: {cargo}")
    env = os.environ.copy()
    env.update(CARGO_HOME=str(ROOT / ".tools/cargo"), RUSTUP_HOME=str(ROOT / ".tools/rustup"),
               PATH=str(cargo.parent) + os.pathsep + env.get("PATH", ""), XROS_DEPLOYMENT_TARGET="26.0",
               RAYON_NUM_THREADS=str(args.jobs))
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CARGO_BUILD_TARGET", None)
    env.pop("MUGUET_IOS_HEAP_GB", None)
    env.pop("MUGUET_GAME_CODE", None)
    command([cargo, "build", "--bin", "muguet", "--locked", "-j", str(args.jobs)], env)
    compiler = ROOT / "target/debug/muguet"
    artifact = source.parent / artifact_name
    receipt_path = artifact.with_name(artifact.name + ".json")
    patch_version = int(re.search(r"^pub const VERSION: u32 = (\d+);", (ROOT / "src/patch.rs").read_text(), re.MULTILINE).group(1))
    key = {"source_sha256": sha(source), "compiler_sha256": sha(compiler), "target": target,
           "configuration": CONFIG, "wasmtime": "49.0.2", "patch_sha256": sha(ROOT / "src/patch.rs"),
           "patch_version": patch_version, "commands_source_sha256": sha(ROOT / "src/mp.rs")}
    cached = False
    try:
        prior = json.loads(receipt_path.read_text())
        cached = (prior.get("build_key") == key and artifact.is_file()
                  and prior.get("artifact_sha256") == sha(artifact))
    except (OSError, ValueError):
        pass
    if not cached:
        with tempfile.NamedTemporaryFile(dir=artifact.parent, prefix=f".{artifact_name}.", suffix=".tmp", delete=False) as out:
            temporary = Path(out.name)
        try:
            command([compiler, option, source, temporary], env)
            if temporary.stat().st_size == 0:
                raise RuntimeError("AOT compiler returned an empty artifact")
            os.replace(temporary, artifact)
            atomic_json(receipt_path, {"schema": 1, "build_key": key, "source_path": str(source),
                        "artifact_path": str(artifact), "artifact_bytes": artifact.stat().st_size,
                        "artifact_sha256": sha(artifact), "scope": "AOT compilation; no game execution is implied"})
        finally:
            temporary.unlink(missing_ok=True)
    else:
        print(f"[native-build] verified matching AOT artifact: {artifact}", file=sys.stderr)
    env["MUGUET_GAME_CODE"] = str(artifact)
    command([cargo, "rustc", "--lib", "--locked", "--target", target, "--no-default-features",
             "--release", "-j", str(args.jobs), "--", "--print=native-static-libs"], env)
    archive = ROOT / "target" / target / "release/libmuguet.a"
    if not archive.is_file():
        raise RuntimeError(f"build returned no native archive: {archive}")
    atomic_json(archive.with_name(archive.name + ".json"), {"schema": 1, "target": target,
                "archive_path": str(archive), "archive_bytes": archive.stat().st_size,
                "archive_sha256": sha(archive), "artifact_path": str(artifact), "artifact_sha256": sha(artifact),
                "aot_receipt": str(receipt_path), "jit": False,
                "patch_version": patch_version, "build_jobs": args.jobs,
                "final_link_requirement": "-Wl,-segprot,__GAME,rx,rx"})
    print(archive)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"[native-build] failed: {error}", file=sys.stderr)
        sys.exit(1)

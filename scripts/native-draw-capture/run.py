#!/usr/bin/env python3
"""Bounded Mac native capture runner. Default is read-only admission; --execute runs once.

No desktop launcher, player profile, save replacement or scene manipulation.
The separate replay admission tool must qualify semantic completeness afterward.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

WASM_SHA = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0"
AOT_SHA = "dc4806cba7b44366fd039dc5212a20fbfcc96139d3208047b5e52e41db2fdfff"
INPUTS = {
    "b/8b0b5899ed/game.wasm": WASM_SHA,
    "data/manifest.json": "2dea2a8a9b38951d9c67cdce88d587425e14f9ab6520e1259a2a5b3bfd6b6d4e",
    "b/8b0b5899ed/shaders/index.json": "11c331895051b30126980ef666bd8935314ab400348b17c56ef8fe8983f50ee0",
}
GRAPHICS = {
    "native-graphics.json": "580215049b6cd19e7f307097003dec08ea67342a35c83c17039675f2d8e6a0c4",
    "native-graphics-applied.json": "efce6e9588a619c1e81e22b3b2e90e72a70ca8297e73edbf9a34d56ada47274c",
    "settings.xml": "3acd159e5195c30b4347ce12919170d2441e76a9016cfa9131f37bdc862a80ea",
}
MAX_BLOBS = 256 << 20
MAX_JSON = 16 << 20
MAX_LOG = 128 << 20
FLAGS = {
    "MUGUET_MUTE": "1", "MUGUET_MASTER_VOLUME": "0",
    "MUGUET_XR_MOTION_AIM": "0", "MUGUET_XR_MOTION_ARMS": "0",
    "MUGUET_XR_FP_VIEWMODEL": "0", "MUGUET_XR_PAIRED_RENDER": "0",
    "MUGUET_GEOMETRY_STEREO_CAPTURE": "0", "MUGUET_XR_GUEST_PHASE_TRACE": "0",
    "MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT": "1",
    "MUGUET_XR_SOURCE_GUARD_BAND": "1.3", "RAYON_NUM_THREADS": "2",
    **{k: "1" for k in ["MUGUET_RENDER_PREPARE_OPT", "MUGUET_UNIFORM_STAGE_PACK_OPT",
        "MUGUET_BIND_GROUP_PREPARE_OPT", "MUGUET_UNIFORM_SNAPSHOT_CACHE",
        "MUGUET_XR_EMISSION_FASTPATH", "MUGUET_METAL_SKIP_EMPTY_TRANSIT",
        "MUGUET_METAL_SKIP_EMPTY_PREPASS", "MUGUET_TRANSFER_FROZEN_DEPTH"]},
}


def need(condition, message):
    if not condition:
        raise ValueError(message)


def observation_seconds(text):
    try:
        value = int(text)
    except ValueError:
        raise argparse.ArgumentTypeError("observation must be an integer from 0 to 30")
    if not 0 <= value <= 30:
        raise argparse.ArgumentTypeError("observation must be an integer from 0 to 30")
    return value


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def exact_path(path, exists=True):
    path = Path(path)
    need(path.is_absolute() and path.resolve(strict=exists) == path,
         f"absolute path without symlinks/traversal required: {path}")
    return path


def load_json(path):
    exact_path(path)
    need(path.is_file() and 0 < path.stat().st_size <= MAX_JSON, f"bounded JSON file required: {path}")
    return json.loads(path.read_text())


def write_new(path, value):
    with path.open("x") as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())


def source_inventory(checkout):
    files = list((checkout / "src").rglob("*.rs"))
    files += [p for p in (checkout / "vendor").rglob("*") if p.is_file()
              and (p.suffix == ".rs" or p.name in ("Cargo.toml", "Cargo.lock", "build.rs"))]
    files += [checkout / n for n in ("Cargo.toml", "Cargo.lock", "build.rs", ".cargo/config.toml")
              if (checkout / n).is_file()]
    return {str(p.relative_to(checkout)): digest(p) for p in sorted(files)}


def bound_binary(receipt, inventory, binary, runner):
    need(receipt.get("success") is True and receipt.get("source_unchanged") is True,
         "successful unchanged-source build receipt required")
    need(receipt.get("source_sha256") == inventory, "build receipt does not bind exact full source inventory")
    need(receipt.get("binary_sha256") == digest(binary)
         and receipt.get("binary_bytes") == binary.stat().st_size, "build receipt does not bind executable")
    need(receipt.get("runner_sha256") == digest(runner), "build receipt does not bind this runner")


def admission(args, require_new_run=True):
    checkout, binary, build, game, graphics, aot = [exact_path(getattr(args, n))
        for n in ("checkout", "binary", "build_receipt", "game_root", "graphics_seed", "aot")]
    need(binary.name == "native-span-capture" and binary.is_file() and os.access(binary, os.X_OK),
         "dedicated executable required")
    need((checkout / "Cargo.toml").is_file() and (checkout / "src/bin/native-span-capture.rs").is_file(),
         "capture source checkout required")
    root = exact_path(args.run_root, exists=False)
    need(root.is_relative_to(checkout / "target/native-draw-capture")
         and root != checkout / "target/native-draw-capture", "run must be a new child of checkout/target/native-draw-capture")
    if require_new_run:
        need(not root.exists() and not root.is_symlink(), "existing run must be preserved")
    need(1 <= args.deadline <= 180, "deadline must be 1..180 seconds")
    need(0 <= args.observe_after_capture <= 30, "post-capture observation must be 0..30 seconds")
    inventory = source_inventory(checkout)
    bound_binary(load_json(build), inventory, binary, Path(__file__).resolve())
    records = {}
    for path, wanted in [(game / n, h) for n, h in INPUTS.items()] + [(graphics / n, h) for n, h in GRAPHICS.items()] + [(aot, AOT_SHA)]:
        exact_path(path)
        need(path.is_file() and digest(path) == wanted, f"pinned input differs: {path}")
        records[str(path)] = {"sha256": wanted, "bytes": path.stat().st_size}
    need(load_json(graphics / "native-graphics.json") ==
         {"width": 2560, "height": 1440, "fps_limit": 60, "graphics_preset": "clarity"}, "Clarity seed differs")
    return {"schema": 1, "source_sha256": inventory, "binary_sha256": digest(binary),
            "binary_bytes": binary.stat().st_size, "build_receipt_sha256": digest(build),
            "runner_sha256": digest(Path(__file__).resolve()), "inputs": records}


def child_env(root):
    # Host HOME/TMP/cache are disposable as well. No arbitrary MUGUET, loader,
    # graphics or Python environment variables leak from the parent shell.
    env = {k: os.environ[k] for k in ("PATH", "LANG", "LC_ALL", "LC_CTYPE", "USER", "LOGNAME") if k in os.environ}
    env.update(FLAGS)
    env.update({"HOME": str(root / "host-home"), "TMPDIR": str(root / "tmp"),
                "XDG_CACHE_HOME": str(root / "host-cache"),
                "MUGUET_NATIVE_DRAW_CAPTURE": str(root / "capture")})
    return env


def xml_values(path):
    def normalized(node):
        return (node.tag, tuple(sorted(node.attrib.items())), (node.text or "").strip(),
                tuple(normalized(child) for child in node))
    return normalized(ET.parse(path).getroot())


def validate_capture(directory):
    need(not (directory / "rejected.json").exists(), "capture module rejected the span")
    capture = load_json(directory / "capture.json")
    need((capture.get("schema"), capture.get("kind"), capture.get("status")) ==
         (2, "native_draw_span", "captured"), "unsupported or incomplete capture")
    closure = capture.get("closure", {})
    for name in ("ordered_two_draws", "initial_contents_captured", "original_final_captured", "natural_submission_completed"):
        need(closure.get(name) is True, f"missing capture closure: {name}")
    need(closure.get("unmodelled_commands") == 0 and closure.get("resource_mutation_rejections") == 0,
         "span has unmodelled commands or resource mutation")
    need(capture.get("normal_frame_publication_changed") is False, "frame publication changed")
    need(isinstance(capture.get("draws"), list) and len(capture["draws"]) == 2, "exact two-draw span required")
    blobs = capture.get("blobs")
    need(isinstance(blobs, list) and 0 < len(blobs) <= 4096, "bounded nonempty blob list required")
    names = set(); total = 0
    for blob in blobs:
        name = blob.get("path")
        need(isinstance(name, str) and name not in ("", ".", "..", "capture.json", "rejected.json")
             and "/" not in name and "\\" not in name and name not in names, "unsafe/duplicate blob name")
        names.add(name); path = exact_path(directory / name)
        size = blob.get("byte_length")
        need(type(size) is int and 0 < size <= MAX_BLOBS, "invalid blob size")
        total += size; need(total <= MAX_BLOBS, "payload budget exceeded")
        need(path.is_file() and path.stat().st_size == size and digest(path) == blob.get("sha256"),
             f"payload absent or differs: {name}")
    need({p.name for p in directory.iterdir()} == names | {"capture.json"}, "unmanifested capture contents")
    return {"capture_sha256": digest(directory / "capture.json"), "blob_count": len(blobs),
            "payload_bytes": total, "byte_admission_passed": True,
            "semantic_replay_admission_required": True}


def run_child(command, checkout, root, deadline):
    start = time.monotonic(); proc = None
    record = {"schema": 1, "timed_out": False, "termination_reason": None}
    try:
        with (root / "stdout.log").open("xb") as out, (root / "stderr.log").open("xb") as err:
            proc = subprocess.Popen(command, cwd=checkout, env=child_env(root), stdout=out, stderr=err, start_new_session=True)
            record["pid"] = proc.pid
            write_new(root / "child-pid.json", {"pid": proc.pid, "process_group": proc.pid, "owned_by_runner": True})
            while proc.poll() is None:
                elapsed = time.monotonic() - start
                logs = sum((root / n).stat().st_size for n in ("stdout.log", "stderr.log"))
                captured = sum(p.stat().st_size for p in (root / "capture").iterdir() if p.is_file()) if (root / "capture").is_dir() else 0
                if elapsed > deadline + 20 or logs > MAX_LOG or captured > MAX_BLOBS + MAX_JSON:
                    record["termination_reason"] = "outer deadline" if elapsed > deadline + 20 else "output bound exceeded"
                    record["timed_out"] = elapsed > deadline + 20
                    break
                time.sleep(0.2)
    finally:
        if proc is not None:
            if proc.poll() is None:
                try: os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError: pass
                try: proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try: os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError: pass
                    proc.wait(timeout=5)
            record["return_code"] = proc.wait()
        record["elapsed_seconds"] = time.monotonic() - start
        write_new(root / "child-lifecycle.json", record)
    return record


def execute(args, before):
    root = Path(args.run_root); root.mkdir(parents=True, exist_ok=False)
    try:
        execute_owned(args, before, root)
    except (Exception, KeyboardInterrupt) as error:
        # Retain failed runs and their original logs. A missing driver result
        # (for example, a terminated child) is never a successful capture.
        failure = {"schema": 1, "status": "failed", "error_type": type(error).__name__,
                   "error": str(error), "captured_bytes_accepted": False,
                   "finished_world_accepted": False, "hardware_accepted": False,
                   "original_inputs_unchanged": None,
                   "preserved_logs": [str(root / n) for n in ("stdout.log", "stderr.log") if (root / n).exists()]}
        if (root / "child-lifecycle.json").is_file():
            failure["child_lifecycle"] = load_json(root / "child-lifecycle.json")
        if (root / "source-after.json").is_file():
            failure["original_inputs_unchanged"] = load_json(root / "source-after.json") == before
        if not (root / "receipt.json").exists():
            write_new(root / "receipt.json", failure)
        raise


def execute_owned(args, before, root):
    for name in ("host-home", "tmp", "host-cache"): (root / name).mkdir()
    write_new(root / "source-before.json", before)
    command = [args.binary, args.game_root, str(root / "fresh-save"), str(root / "capture"),
               args.graphics_seed, args.aot, str(args.deadline), str(args.observe_after_capture)]
    write_new(root / "invocation.json", {"argv": command, "cwd": args.checkout,
        "environment": child_env(root), "fixture": "fresh developer-mode natural game with synthetic static two-eye input",
        "source_guard_band": 1.3, "source_guard_policy": "fixed diagnostic fixture FOV; not production coverage tuning",
        "observe_after_capture_seconds": args.observe_after_capture,
        "scene_steered": False, "finished_world_accepted": False, "hardware_accepted": False})
    lifecycle = run_child(command, Path(args.checkout), root, args.deadline)
    after = admission(args, require_new_run=False); write_new(root / "source-after.json", after)
    need(before == after, "source, executable, build receipt, runner or original seeds changed during run")
    need(lifecycle.get("return_code") == 0 and lifecycle["termination_reason"] is None,
         f"child stopped or failed before accepted capture: {lifecycle}")
    final = load_json(root / "driver-result.json")
    need(final.get("outcome") == "captured", "driver did not finish a bounded capture")
    need(xml_values(Path(args.graphics_seed) / "settings.xml") ==
         xml_values(root / "fresh-save/userdata/Documents/Rockstar Games/GTA V/settings.xml"), "disposable graphics XML semantics changed")
    need(digest(root / "fresh-save/native-graphics.json") == GRAPHICS["native-graphics.json"], "disposable Clarity config changed")
    result = validate_capture(root / "capture")
    need(final.get("capture") == load_json(root / "capture/capture.json"), "driver/capture manifest mismatch")
    observation = final.get("observation", {})
    need(observation.get("requested_seconds") == args.observe_after_capture
         and observation.get("whole_run_gpu_health_accepted") is False
         and observation.get("status") in ("not_requested", "completed", "deadline_reached", "runtime_failed"),
         "driver observation scope/result missing")
    write_new(root / "receipt.json", {"schema": 1, "status": "captured_bytes_verified", **result,
        "observation": observation, "whole_run_gpu_health_accepted": False,
        "original_inputs_unchanged": True, "source_unchanged": True, "graphics_semantics_unchanged": True,
        "finished_world_accepted": False, "hardware_accepted": False})
    print(json.dumps({"run_root": str(root), **result}))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ("checkout", "binary", "build-receipt", "game-root", "graphics-seed", "aot", "run-root"):
        p.add_argument("--" + name, required=True)
    p.add_argument("--deadline", type=int, default=180)
    p.add_argument("--observe-after-capture", type=observation_seconds, default=0,
                   help="observe normal runtime for 0..30 seconds after capture within the original deadline; no whole-run health claim")
    p.add_argument("--execute", action="store_true", help="run one reviewed native game process; otherwise read-only admission")
    args = p.parse_args()
    before = admission(args)
    if args.execute: execute(args, before)
    else: print(json.dumps({"status": "inputs_admitted_no_execution", "binary_sha256": before["binary_sha256"], "source_files": len(before["source_sha256"])}))


if __name__ == "__main__":
    try: main()
    except (Exception, KeyboardInterrupt) as error:
        print(f"native capture runner failed: {type(error).__name__}: {error}", file=sys.stderr)
        raise SystemExit(1)

#!/usr/bin/env python3
"""Freeze and run one bounded Mac actual-game frozen-depth probe; never build."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
EXPECTED_GPU = "948f58229780c511f47f35a505422ae594c88d27290302e4d2579e1da7403d46"
EXPECTED_WORLD = "b202105591f283000929218c7a165131af1e7f9159f2b4d9ade970854e930eb0"
EXPECTED_WASM = "11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0"
EXPECTED_WASM_BYTES = 63_201_802
EXPECTED_SHADER_INDEX = "11c331895051b30126980ef666bd8935314ab400348b17c56ef8fe8983f50ee0"
VERSION5_SEED_NAME = "game-63201802-1791666949-51705d56.cwasm"
VERSION5_SEED_SHA = "fd9260a36f5aacfdd8dc505e7021a23c14c05d50771a2598f7e69ce0156af0d0"
VERSION5_SEED_BYTES = 212_443_920
PARENT_BUILD_INPUTS = Path("/Users/zach/gaming/SantosVision/target/verification/reprojection-implementation-v1/host-capture-runs/root-real-draw-2/build-inputs.json")
PARENT_BUILD_INPUTS_SHA = "1ce8cfc6fbcf64c6b40239c537f20842388c32ba46649354549e663e46c5be53"
PARENT_REVIEWED_HOST_CHANGES = {"src/bin/geometry-capture.rs", "src/gpu.rs",
                                "src/native.rs", "src/world_frame.rs", "src/xr_view.rs"}
ENGINE_INPUTS = {
    "src/patch.rs": "4301ec987afb506c3330b7a50e6ae4df47884942cae31976e8dd2a9029419281",
    "src/lib.rs": "740df2fff200779f86cfbd2df079f8f2f052ac19943f5b9d8e0fffd285984546",
    "src/mp.rs": "46b7d912f3a5d22a761dd44fdb37e976891422ad1455e4475344ad2f54dc08b1",
    "Cargo.toml": "af846e02616e7e86f4ce9c0d3eff5f18b78dfe43873a348ba90a868d997a7aa0",
    "Cargo.lock": "61d349c6ea690ad75a597bccf8d3b09e9d1c208b583551a5e6f8fbb9cc8aea9e",
    "build.rs": "a0fbaef34538dcb274f75dc64eee6fea3e270c34d80bb6a30238342bc3040ee1",
}


def require(ok: bool, message: str) -> None:
    if not ok:
        raise RuntimeError(message)


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            sha.update(chunk)
    return sha.hexdigest()


def write_new(path: Path, value: dict) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write("\n")


def files(checkout: Path) -> dict[str, str]:
    source = list((checkout / "src").rglob("*.rs"))
    vendor = checkout / "vendor"
    if vendor.is_dir():
        source += [path for path in vendor.rglob("*") if path.is_file()
                   and (path.suffix == ".rs" or path.name in ("Cargo.toml", "Cargo.lock", "build.rs"))]
    source += [checkout / name for name in ("Cargo.toml", "Cargo.lock", "build.rs", ".cargo/config.toml")
               if (checkout / name).is_file()]
    return {str(path.relative_to(checkout)): digest(path) for path in sorted(set(source))}


def receipt(checkout: Path, binary: Path, game: Path, seed: Path | None,
            cargo: Path, rustc: Path, build_log: Path) -> dict:
    checkout = checkout.resolve(strict=True)
    binary = binary.resolve(strict=True)
    game = game.resolve(strict=True)
    cargo = cargo.resolve(strict=True)
    rustc = rustc.resolve(strict=True)
    build_log = build_log.resolve(strict=True)
    require(binary.is_file() and os.access(binary, os.X_OK) and binary.name == "frozen-depth-live",
            "dedicated frozen-depth-live binary missing")
    require(build_log.is_file() and build_log.stat().st_size > 0, "raw build log missing")
    require(re.search(r"pub const VERSION:\s*u32\s*=\s*5\s*;",
                      (checkout / "src/patch.rs").read_text()) is not None,
            "VERSION5 game patch required")
    source = files(checkout)
    require(source.get("src/gpu.rs") == EXPECTED_GPU and source.get("src/world_frame.rs") == EXPECTED_WORLD,
            "root-tested transfer source hashes changed")
    require(all(source.get(name) == sha for name, sha in ENGINE_INPUTS.items()),
            "VERSION5 JIT engine/compiler inputs changed")
    wasm = sorted(path for path in (game / "b").glob("*/game.wasm")
                  if (path.parent / "shaders/index.json").is_file())
    require(len(wasm) == 1 and (game / "data/manifest.json").is_file(),
            "game needs exactly one WASM/shader build and manifest")
    wasm = wasm[0]
    require(wasm.stat().st_size == EXPECTED_WASM_BYTES and digest(wasm) == EXPECTED_WASM,
            "original game WASM changed")
    require(digest(wasm.parent / "shaders/index.json") == EXPECTED_SHADER_INDEX,
            "qualified shader index changed")
    shader_files = {str(path.relative_to(wasm.parent)): digest(path)
                    for path in sorted((wasm.parent / "shaders").rglob("*")) if path.is_file()}
    seed_data = None
    if seed is not None:
        require(digest(PARENT_BUILD_INPUTS) == PARENT_BUILD_INPUTS_SHA,
                "qualified VERSION5 parent build receipt changed")
        parent = json.loads(PARENT_BUILD_INPUTS.read_text())
        require(parent["command"][0] == str(cargo) and parent["env"]["RUSTC"] == str(rustc),
                "VERSION5 parent toolchain paths differ")
        deviations = {name for name, sha in parent["sha256"].items()
                      if not (checkout / name).is_file() or digest(checkout / name) != sha}
        require(deviations == PARENT_REVIEWED_HOST_CHANGES,
                f"VERSION5 parent input deviations differ: {sorted(deviations)}")
        require(not seed.is_symlink(), "cache seed may not be a symlink")
        seed = seed.resolve(strict=True)
        metadata = seed.lstat()
        require(seed.is_file() and seed.name == VERSION5_SEED_NAME
                and metadata.st_size == VERSION5_SEED_BYTES and digest(seed) == VERSION5_SEED_SHA,
                "explicit qualified VERSION5 Mac JIT seed differs")
        require(int(wasm.stat().st_mtime) == 1_791_666_949,
                "game WASM mtime differs from VERSION5 cache key")
        seed_data = {"path": str(seed), "sha256": VERSION5_SEED_SHA,
                     "bytes": VERSION5_SEED_BYTES, "name": VERSION5_SEED_NAME,
                     "parent_build_inputs_path": str(PARENT_BUILD_INPUTS),
                     "parent_build_inputs_sha256": PARENT_BUILD_INPUTS_SHA,
                     "parent_reviewed_host_changes": sorted(deviations),
                     "provenance": "root-real-draw-2 actual-game Mac VERSION5 cache; all other 117 parent inputs identical; runtime deserialization and post-run hash remain checked"}
    for tool in (cargo, rustc):
        require(tool.is_file() and os.access(tool, os.X_OK), f"tool unavailable: {tool}")
    status = subprocess.run(["git", "-C", str(checkout), "status", "--short", "--untracked-files=all"],
                            capture_output=True, text=True, timeout=15)
    head = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"],
                          capture_output=True, text=True, timeout=15)
    require(status.returncode == 0 and head.returncode == 0, "checkout Git status unavailable")
    return {"schema": 1, "runner_sha256": digest(Path(__file__)),
            "source": {"checkout": str(checkout), "head": head.stdout.strip(),
                       "status_short": status.stdout, "files": source},
            "binary": {"path": str(binary), "sha256": digest(binary), "bytes": binary.stat().st_size},
            "game": {"root": str(game), "wasm": str(wasm), "wasm_sha256": EXPECTED_WASM,
                     "wasm_bytes": EXPECTED_WASM_BYTES, "wasm_mtime_ns": wasm.stat().st_mtime_ns,
                     "manifest_sha256": digest(game / "data/manifest.json"),
                     "shader_index_sha256": EXPECTED_SHADER_INDEX,
                     "shader_files": shader_files},
            "tools": {"cargo": {"path": str(cargo), "sha256": digest(cargo)},
                      "rustc": {"path": str(rustc), "sha256": digest(rustc)}},
            "build_log": {"path": str(build_log), "sha256": digest(build_log),
                          "scope": "raw log hash only; root verifies actual successful build command and exit code"},
            "seed": seed_data}


def verify_frozen(frozen: dict) -> None:
    checkout = Path(frozen["source"]["checkout"])
    require(frozen["runner_sha256"] == digest(Path(__file__)), "runner changed after freeze")
    require(files(checkout) == frozen["source"]["files"], "Rust source inventory changed")
    binary = Path(frozen["binary"]["path"])
    require(binary.is_file() and binary.stat().st_size == frozen["binary"]["bytes"]
            and digest(binary) == frozen["binary"]["sha256"], "binary changed after freeze")
    game = frozen["game"]
    wasm = Path(game["wasm"])
    require(wasm.is_file() and wasm.stat().st_size == game["wasm_bytes"]
            and wasm.stat().st_mtime_ns == game["wasm_mtime_ns"]
            and digest(wasm) == game["wasm_sha256"], "game WASM changed after freeze")
    require(digest(Path(game["root"]) / "data/manifest.json") == game["manifest_sha256"]
            and digest(wasm.parent / "shaders/index.json") == game["shader_index_sha256"],
            "game manifest or shader index changed")
    require({str(path.relative_to(wasm.parent)): digest(path)
             for path in sorted((wasm.parent / "shaders").rglob("*")) if path.is_file()}
            == game["shader_files"], "game shader files changed")
    for value in frozen["tools"].values():
        require(digest(Path(value["path"])) == value["sha256"], "tool changed after freeze")
    require(digest(Path(frozen["build_log"]["path"])) == frozen["build_log"]["sha256"],
            "raw build log changed after freeze")
    if frozen["seed"]:
        seed = frozen["seed"]
        require(digest(Path(seed["path"])) == seed["sha256"]
                and digest(Path(seed["parent_build_inputs_path"])) == seed["parent_build_inputs_sha256"],
                "VERSION5 seed or parent receipt changed")
        parent = json.loads(Path(seed["parent_build_inputs_path"]).read_text())
        deviations = {name for name, sha in parent["sha256"].items()
                      if not (checkout / name).is_file() or digest(checkout / name) != sha}
        require(sorted(deviations) == seed["parent_reviewed_host_changes"],
                "VERSION5 parent source inputs changed during run")


def prepare(args: argparse.Namespace) -> None:
    out = args.out.resolve()
    require(not out.exists(), "output already exists; choose a new run directory")
    frozen = receipt(args.checkout, args.binary, args.game, args.seed,
                     args.cargo, args.rustc, args.build_log)
    out.mkdir(parents=True)
    write_new(out / "freeze.json", frozen)
    print(out / "freeze.json")


def static_eye_bytes(x: float) -> bytes:
    def f32(value: float) -> float:
        return struct.unpack("<f", struct.pack("<f", value))[0]
    near, far = f32(0.05), f32(1000.0)
    denominator = f32(far - near)
    head = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, 0., 0., 1.]
    projection = [1., 0., 0., 0., 0., f32(16. / 9.), 0., 0., 0., 0.,
                  f32(near / denominator), -1., 0., 0., f32(f32(near * far) / denominator), 0.]
    return struct.pack("<32f", *(head + projection))


def check_result(result: dict, frozen: dict, out: Path) -> dict:
    require(result.get("schema") == 1 and result.get("status") == "complete",
            "driver did not complete")
    source = result.get("leased_source") or {}
    capture = result.get("capture") or {}
    require(result.get("capture_game_frame") == source.get("game_frame")
            and isinstance(source.get("request_frame"), int) and source["request_frame"] > 0
            and isinstance(source.get("head_frame"), int) and source["head_frame"] > 0,
            "actual capture and GPU-complete lease identity differ")
    require((result.get("input") or {}).get("head_pose_frame") == source["head_frame"],
            "reported synthetic pose frame differs from leased source")
    require(source.get("view_count") == 2 and source.get("view_index") in (0, 1)
            and source.get("width") == 640 and source.get("height") == 360
            and source.get("color_texture", 0) > 0 and source.get("depth_texture", 0) > 0,
            "lease is not one eye of the requested Low two-view source")
    receipt = result.get("consumed_request_receipt") or {}
    require(receipt.get("schema") == 1 and receipt.get("source") == "consumed_world_frame_receipt"
            and receipt.get("headset_applied") is True
            and isinstance(receipt.get("provenance_epoch"), int) and receipt["provenance_epoch"] > 0
            and isinstance(receipt.get("camera_sequence"), int) and receipt["camera_sequence"] > 0,
            "exact consumed-request receipt missing")
    for field, receipt_field in (("abi_version", "frame_abi_version"),
                                 ("byte_size", "frame_byte_size"), ("lease", "frame_lease"),
                                 ("depth_convention", "frame_depth_convention"),
                                 ("game_frame", "game_frame"), ("request_frame", "request_frame"),
                                 ("head_frame", "head_frame"), ("presentation_time", "presentation_time"),
                                 ("view_index", "view_index"), ("view_count", "view_count"),
                                 ("projection_from_eye", "projection_from_eye"),
                                 ("game_from_eye", "game_from_eye"),
                                 ("game_from_stage", "game_from_stage")):
        require(source.get(field) == receipt.get(receipt_field),
                f"consumed receipt differs from leased {field}")
    require(source["abi_version"] == 1 and source["byte_size"] == 288
            and source["lease"] > 0 and source["depth_convention"] == 0,
            "leased ABI/depth convention invalid")
    try:
        raw_request = bytes.fromhex(receipt["request_bytes_hex"])
    except (KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"consumed request bytes invalid: {error}") from error
    require(len(raw_request) == 288, "consumed request byte count differs from ABI")
    version, size, request_frame, request_time, views, reserved = struct.unpack("<IIQdII", raw_request[:32])
    require((version, size, request_frame, views, reserved) ==
            (1, 288, source["request_frame"], 2, 0)
            and request_time == 1.0
            and request_time == source["presentation_time"],
            "consumed request header differs from leased source")
    for index, x in enumerate((-0.032, 0.032)):
        require(raw_request[32 + index * 128:160 + index * 128] == static_eye_bytes(x),
                f"consumed eye {index} differs from exact static 64 mm Low request")
    require(result.get("frozen_depth_handoffs", 0) > 0 and result.get("published", 0) > 2
            and result.get("max_slots", 7) <= 6 and result.get("max_leases", 13) <= 12,
            "handoff publication or resource bound absent")
    status = result.get("final_native_status") or {}
    world = status.get("world_frames") or {}
    require(status.get("gpu_started") is True and status.get("presents", 0) > 0
            and not status.get("last_error") and not world.get("error")
            and world.get("outstanding_leases") == 0 and world.get("slots", 7) <= 6
            and world.get("pairs_published") == 0 and world.get("pairs_acquired") == 0,
            "native GPU, lease, or false pair state failed")
    ppm = Path(capture.get("path", ""))
    require(ppm == out / "save" / "actual-game-source.ppm" and ppm.is_file()
            and ppm.stat().st_size == capture.get("bytes")
            and capture.get("varied") is True and capture.get("nonzero") is True,
            "actual game color PPM absent or blank")
    return {"capture_ppm_sha256": digest(ppm), "capture_ppm_bytes": ppm.stat().st_size,
            "capture_game_frame": result["capture_game_frame"],
            "leased_source": source, "consumed_request_receipt": receipt,
            "frozen_depth_handoffs": result["frozen_depth_handoffs"],
            "copied_depth_snapshots": result.get("copied_depth_snapshots"),
            "max_slots": result["max_slots"], "max_leases": result["max_leases"],
            "scope": "Mac actual-game color/source identity and aggregate handoff; no leased-depth live pixel readback or coherent stereo"}


def run(args: argparse.Namespace) -> None:
    out = args.out.resolve(strict=True)
    frozen = json.loads((out / "freeze.json").read_text())
    verify_frozen(frozen)
    require(1 <= args.deadline <= 600, "deadline must be 1..600 seconds")
    require(not (out / "invocation.json").exists() and not (out / "save").exists(),
            "this frozen run has already started")
    env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR", "LANG", "LC_ALL",
                                          "LC_CTYPE", "USER", "LOGNAME") if key in os.environ}
    env.update({"MUGUET_TRANSFER_FROZEN_DEPTH": "1", "MUGUET_XR_PAIRED_RENDER": "0",
                "MUGUET_GEOMETRY_STEREO_CAPTURE": "0", "MUGUET_MASTER_VOLUME": "0",
                "MUGUET_MUTE": "1"})
    command = [frozen["binary"]["path"], frozen["game"]["root"],
               str(out / "save"), str(args.deadline)]
    if frozen["seed"]:
        command += ["--cache-seed", frozen["seed"]["path"]]
    write_new(out / "invocation.json", {"command": command, "environment": env,
                                        "freeze_sha256": digest(out / "freeze.json"),
                                        "started_unix": time.time()})
    wait_error = None
    cleanup_error = None
    return_code = -126
    child = None
    with (out / "stdout.log").open("xb") as stdout, (out / "stderr.log").open("xb") as stderr:
        try:
            child = subprocess.Popen(command, cwd=frozen["source"]["checkout"], env=env,
                                     stdout=stdout, stderr=stderr, start_new_session=True)
            write_new(out / "child.json", {"pid": child.pid, "process_group": child.pid,
                                           "start_new_session": True})
            return_code = child.wait(timeout=args.deadline + 20)
        except BaseException as error:
            wait_error = f"{type(error).__name__}: {error}"
            return_code = -124 if isinstance(error, subprocess.TimeoutExpired) else -125
        finally:
            if child is not None and child.poll() is None:
                try:
                    try:
                        os.killpg(child.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        try:
                            os.killpg(child.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        child.wait(timeout=5)
                except BaseException as error:
                    cleanup_error = f"{type(error).__name__}: {error}"
    post_error = None
    try:
        verify_frozen(frozen)
    except Exception as error:
        post_error = str(error)
    raw = (out / "stderr.log").read_text(errors="replace")
    events = []
    parse_error = None
    for line in raw.splitlines():
        marker = "[frozen-depth-driver] {"
        if marker in line:
            try:
                events.append(json.loads(line.split("[frozen-depth-driver] ", 1)[1]))
            except (json.JSONDecodeError, ValueError) as error:
                parse_error = f"malformed driver result JSON: {error}"
    summary = {"schema": 1, "return_code": return_code,
               "child_pid": child.pid if child else None,
               "child_final_poll": child.poll() if child else None,
               "wait_error": wait_error, "cleanup_error": cleanup_error,
               "result_parse_error": parse_error,
               "stdout_sha256": digest(out / "stdout.log"), "stderr_sha256": digest(out / "stderr.log"),
               "driver_results": len(events), "freeze_sha256": digest(out / "freeze.json"),
               "post_source_check_error": post_error, "passed": False}
    try:
        require(post_error is None, f"source changed during run: {post_error}")
        require(wait_error is None, f"child wait interrupted: {wait_error}")
        require(cleanup_error is None, f"child process-group cleanup failed: {cleanup_error}")
        require(parse_error is None, f"driver result parse failed: {parse_error}")
        require(return_code == 0 and len(events) == 1,
                f"driver return code {return_code}, result count {len(events)}")
        result = events[0]
        saved = out / "save" / "frozen-depth-result.json"
        require(saved.is_file() and json.loads(saved.read_text()) == result,
                "saved and stderr driver results differ")
        summary.update(check_result(result, frozen, out))
        if frozen["seed"]:
            cache = list((out / "save" / "cache").glob("*.cwasm"))
            require(len(cache) == 1 and cache[0].name == frozen["seed"]["name"]
                    and digest(cache[0]) == frozen["seed"]["sha256"],
                    "VERSION5 seed was replaced or wrong cache compiled")
        else:
            cache = list((out / "save" / "cache").glob("*.cwasm"))
            require(len(cache) == 1 and cache[0].stat().st_mtime >=
                    json.loads((out / "invocation.json").read_text())["started_unix"],
                    "fresh run did not produce exactly one new Mac JIT cache")
        summary["passed"] = True
    except Exception as error:
        summary["error"] = str(error)
    write_new(out / "result.json", summary)
    require(summary["passed"], summary.get("error", "live driver failed"))
    print(json.dumps(summary, sort_keys=True))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    prep = sub.add_parser("prepare", help="freeze exact source, binary, game, tools, build log and optional VERSION5 seed")
    prep.add_argument("--checkout", required=True, type=Path)
    prep.add_argument("--binary", required=True, type=Path)
    prep.add_argument("--game", required=True, type=Path)
    prep.add_argument("--cargo", required=True, type=Path)
    prep.add_argument("--rustc", required=True, type=Path)
    prep.add_argument("--build-log", required=True, type=Path)
    prep.add_argument("--seed", type=Path)
    prep.add_argument("--out", required=True, type=Path)
    execute = sub.add_parser("run", help="run once from a prepared immutable freeze")
    execute.add_argument("--out", required=True, type=Path)
    execute.add_argument("--deadline", type=int, default=300)
    args = parser.parse_args()
    try:
        (prepare if args.mode == "prepare" else run)(args)
    except Exception as error:
        print(f"[frozen-depth-runner] {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()

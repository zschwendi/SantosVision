#!/usr/bin/env python3
"""Read-only preflight for the compiled game ZIP consumed by Muguet/SantosVision."""

import argparse
import json
import re
import stat
import sys
import zipfile
import zlib
from pathlib import Path


MAX_JSON_BYTES = 64 * 1024 * 1024
SKIPPED_DIRS = {"env_test", "verityradio"}
WASM_HEADER = b"\x00asm\x01\x00\x00\x00"


def path_problem(name):
    """Require an unambiguous, relative ZIP path without normalizing it."""
    if not name or "\x00" in name:
        return "empty name or NUL byte"
    if "\\" in name:
        return "backslash separator; use forward-slash ZIP paths"
    if name.startswith("/") or re.match(r"^[A-Za-z]:", name):
        return "absolute path or drive prefix"
    parts = name.removesuffix("/").split("/")
    if any(part in {"", ".", ".."} for part in parts):
        return "empty, '.' or '..' path component"
    return None


def skipped(name):
    return any(part.lower() in SKIPPED_DIRS for part in name.split("/"))


def validate(archive_path):
    result = {
        "ok": False,
        "archive": str(archive_path),
        "prefix": None,
        "build": None,
        "wasm_entry": None,
        "manifest_entry": None,
        "shader_index_entry": None,
        "data_files": 0,
        "wgsl_files": 0,
        "uncompressed_import_bytes": 0,
        "errors": [],
        "warnings": [],
    }
    errors = result["errors"]
    warnings = result["warnings"]

    try:
        archive = zipfile.ZipFile(archive_path, "r")
    except (OSError, zipfile.BadZipFile) as exc:
        errors.append(f"Cannot open a game ZIP: {exc}")
        return result

    with archive:
        entries = {}
        case_names = {}
        for info in archive.infolist():
            name = info.orig_filename
            problem = path_problem(name)
            if problem:
                errors.append(f"Unsafe ZIP entry {name!r}: {problem}.")
                continue
            mode = info.external_attr >> 16
            if stat.S_ISLNK(mode):
                errors.append(f"Symlink ZIP entry is unsupported: {name!r}.")
                continue
            kind = stat.S_IFMT(mode)
            if kind not in {0, stat.S_IFREG, stat.S_IFDIR}:
                errors.append(f"Special-file ZIP entry is unsupported: {name!r}.")
                continue
            if info.flag_bits & 1:
                errors.append(f"Encrypted ZIP entry is unsupported: {name!r}.")
                continue
            if info.compress_type not in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}:
                errors.append(f"Unsupported ZIP compression for {name!r}; use stored or deflate entries.")
                continue
            if name in entries:
                errors.append(f"Duplicate ZIP entry: {name!r}.")
                continue
            folded = name.rstrip("/").casefold()
            if folded in case_names and case_names[folded] != name:
                errors.append(
                    f"ZIP paths collide on a case-insensitive filesystem: "
                    f"{case_names[folded]!r} and {name!r}."
                )
                continue
            entries[name] = info
            case_names[folded] = name

        for name in entries:
            parts = name.rstrip("/").split("/")
            for length in range(1, len(parts)):
                parent = "/".join(parts[:length]).casefold()
                ancestor = case_names.get(parent)
                if ancestor is not None and not entries[ancestor].is_dir():
                    errors.append(f"ZIP file {ancestor!r} is also a parent directory of {name!r}.")
                    break

        # Do not use unsafe/excluded entries to discover a plausible package.
        manifests = sorted(
            name for name, info in entries.items()
            if not info.is_dir()
            and (name == "data/manifest.json" or name.endswith("/data/manifest.json"))
        )
        if not manifests:
            errors.append(
                "Missing data/manifest.json. This requires the compiled GTA V web-port "
                "package, not a console ROM, disc ISO, or ordinary PC installation."
            )
            return result
        if len(manifests) != 1:
            errors.append(
                "Ambiguous package roots: found " + ", ".join(repr(n) for n in manifests)
                + ". Supply a ZIP containing exactly one data/ + b/ package."
            )
            return result

        manifest_name = manifests[0]
        prefix = manifest_name[:-len("data/manifest.json")]
        result["prefix"] = prefix
        result["manifest_entry"] = manifest_name
        relative = {
            name[len(prefix):]: info for name, info in entries.items()
            if name.startswith(prefix) and not info.is_dir()
        }
        data_entries = {
            name: info for name, info in relative.items()
            if name.startswith("data/") and not skipped(name)
        }
        result["data_files"] = len(data_entries)
        if not any(name != "data/manifest.json" and info.file_size > 0
                   for name, info in data_entries.items()):
            errors.append("data/ has no nonempty game assets beyond manifest.json.")

        def read_json(name):
            info = entries.get(name)
            if info is None:
                errors.append(f"Missing {name!r}.")
                return None
            if not 0 < info.file_size <= MAX_JSON_BYTES:
                errors.append(f"{name!r} must be nonempty JSON at most 64 MiB.")
                return None
            try:
                with archive.open(info) as stream:
                    payload = stream.read(MAX_JSON_BYTES + 1)
                if len(payload) > MAX_JSON_BYTES:
                    errors.append(f"{name!r} exceeds the 64 MiB JSON limit.")
                    return None
                return json.loads(payload)
            except (OSError, RuntimeError, NotImplementedError, zipfile.BadZipFile,
                    UnicodeError, ValueError, EOFError, zlib.error) as exc:
                errors.append(f"Cannot read JSON {name!r}: {exc}")
                return None

        manifest = read_json(manifest_name)
        files = manifest.get("files") if isinstance(manifest, dict) else None
        if not isinstance(files, list) or not files:
            errors.append("data/manifest.json must contain a nonempty 'files' array.")
        else:
            omitted = 0
            for index, row in enumerate(files):
                if (not isinstance(row, list) or len(row) < 2
                        or not isinstance(row[0], str)
                        or not isinstance(row[1], int) or isinstance(row[1], bool)
                        or row[1] < 0):
                    errors.append(f"Manifest files[{index}] must be [relative_path, byte_size].")
                    continue
                name, size = row[:2]
                problem = path_problem(name)
                if problem or name.endswith("/"):
                    errors.append(f"Unsafe manifest asset path {name!r}: {problem or 'directory path'}.")
                    continue
                if skipped(name):
                    omitted += 1
                    continue
                info = data_entries.get("data/" + name)
                if info is None:
                    errors.append(f"Manifest asset is missing from data/: {name!r}.")
                elif info.file_size != size:
                    errors.append(
                        f"Manifest asset size differs for {name!r}: "
                        f"manifest says {size}, ZIP says {info.file_size} bytes."
                    )
            if omitted:
                warnings.append(
                    f"Importer omits {omitted} manifest assets in env_test/ or verityradio/."
                )

        builds = sorted({
            parts[1] for name in relative
            if name.startswith("b/") and not skipped(name)
            and len(parts := name.split("/")) >= 3
            and (parts[2:] == ["game.wasm"] or parts[2] == "shaders")
        })
        if not builds:
            errors.append("Missing b/<build>/game.wasm and b/<build>/shaders/.")
            return result
        if len(builds) != 1:
            errors.append(
                "Ambiguous builds: " + ", ".join(repr(b) for b in builds)
                + ". Repackage one matching game.wasm + shaders/ build; "
                "the importer/runtime does not reliably select among multiple builds."
            )
            return result

        build = builds[0]
        result["build"] = build
        wasm_name = prefix + f"b/{build}/game.wasm"
        shader_root = f"b/{build}/shaders/"
        index_name = prefix + shader_root + "index.json"
        result["wasm_entry"] = wasm_name
        result["shader_index_entry"] = index_name
        wasm = entries.get(wasm_name)
        if wasm is None:
            errors.append(f"Missing {wasm_name!r}; mobile builds need it to compile embedded game code.")
        elif wasm.file_size < len(WASM_HEADER):
            errors.append(f"{wasm_name!r} is too small to contain a WebAssembly module.")
        else:
            try:
                with archive.open(wasm) as stream:
                    header = stream.read(len(WASM_HEADER))
                if header != WASM_HEADER:
                    errors.append(f"{wasm_name!r} lacks the WebAssembly magic/version-1 header.")
            except (OSError, RuntimeError, NotImplementedError, zipfile.BadZipFile,
                    EOFError, zlib.error) as exc:
                errors.append(f"Cannot read WebAssembly header {wasm_name!r}: {exc}")

        wgsl = {
            name[len(shader_root):]: info for name, info in relative.items()
            if name.startswith(shader_root) and name.endswith(".wgsl") and not skipped(name)
        }
        result["wgsl_files"] = len(wgsl)
        if not wgsl:
            errors.append(f"{prefix + shader_root!r} has no .wgsl shader sources; .spv/.glsl are not substitutes.")
        for name, info in wgsl.items():
            if info.file_size == 0:
                errors.append(f"Empty WGSL shader source: {prefix + shader_root + name!r}.")

        shader_index = read_json(index_name)
        if not isinstance(shader_index, dict) or not shader_index:
            errors.append(f"{index_name!r} must be a nonempty JSON object.")
        else:
            usable = 0
            for shader_hash, record in shader_index.items():
                if not isinstance(record, dict) or record.get("ok") is not True:
                    continue
                usable += 1
                if path_problem(shader_hash) or "/" in shader_hash:
                    errors.append(f"Unsafe shader index key: {shader_hash!r}.")
                    continue
                shader_name = shader_hash + ".wgsl"
                if shader_name not in wgsl:
                    errors.append(f"Shader index marks {shader_hash!r} usable, but {shader_name!r} is missing.")
            if not usable:
                errors.append(f"{index_name!r} has no shader records with 'ok': true.")

        result["uncompressed_import_bytes"] = sum(
            info.file_size for name, info in relative.items()
            if not skipped(name)
            and (name.startswith("data/") or name == f"b/{build}/game.wasm"
                 or name.startswith(shader_root))
        )
        result["ok"] = not errors
        return result


def main():
    parser = argparse.ArgumentParser(
        description="Check a compiled GTA V web-port ZIP without extracting or changing it."
    )
    parser.add_argument("archive", type=Path, help="ZIP containing data/ and one b/<build>/")
    parser.add_argument("--json", action="store_true", help="emit a machine-readable result")
    args = parser.parse_args()
    result = validate(args.archive)
    if args.json:
        print(json.dumps(result, indent=2))
    else:
        if result["ok"]:
            print(f"Package preflight passed: {args.archive}")
            print(f"Root: {result['prefix'] or '(ZIP root)'}; build: {result['build']}")
            print(f"WASM: {result['wasm_entry']}")
            print(f"Assets: {result['data_files']} data files, {result['wgsl_files']} WGSL shaders")
            print("Structure/header check only; game compatibility and device launch still need verification.")
        else:
            print(f"Package preflight failed: {args.archive}")
        for kind in ("errors", "warnings"):
            for message in result[kind][:20]:
                print(f"{kind[:-1].upper()}: {message}")
            if len(result[kind]) > 20:
                print(f"{len(result[kind]) - 20} more {kind}; use --json for the complete list.")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())

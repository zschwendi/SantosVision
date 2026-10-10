#!/usr/bin/env python3
"""Build a Muguet ZIP from verified, already downloaded full-game-v1 parts.

No downloads, extraction, or execution of release contents are performed.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import re
import stat
import sys
import tempfile
import time
import zipfile
import zlib
from pathlib import Path


REPOSITORY = "alihammoud21/playgta5"
TAG = "full-game-v1"
SITE_PREFIX = "mirror/playgta5.com/"
MANIFEST_PATH = SITE_PREFIX + "data/manifest.json"
CHUNK_BYTES = 1024 * 1024
METADATA_HASHES = {
    "download-manifest.json": "83a2865640038e07352e554f7bebf78ce3fbdb49235714e5d69af06e7c4082ae",
    "file-inventory.json": "6def50e4232646763c1ad4c7e317a5afbd884b81c428e46f975ca7f58ee1d835",
}
# These two source-manifest declarations are stale in this specific release.
# The replacement is permitted only after the corresponding source bytes match.
REPAIRS = {
    "common/non_final/www/parser/psomismatch.html": {
        "before_bytes": 5716, "after_bytes": 5718,
        "sha256": "dbd62ce0ffeed781a634edde1f6de516369afb83c0a286384dac7509722f5e36",
    },
    "common/non_final/www/parser/schemas.html": {
        "before_bytes": 3073, "after_bytes": 3075,
        "sha256": "03018d16c1e5aefb98084aaef47e69140cfdad5021ec58f6f3270f1dadf8fc0e",
    },
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def safe_path(name):
    require(isinstance(name, str) and name and "\x00" not in name,
            f"Invalid path: {name!r}")
    require("\\" not in name and not name.startswith("/")
            and not re.match(r"^[A-Za-z]:", name), f"Unsafe path: {name!r}")
    require(all(part not in {"", ".", ".."} for part in name.rstrip("/").split("/")),
            f"Unsafe path component: {name!r}")


def valid_size(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def valid_hash(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def selected(path):
    return path.startswith((SITE_PREFIX + "b/", SITE_PREFIX + "data/"))


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(CHUNK_BYTES):
            digest.update(chunk)
    return digest.hexdigest()


def unique_json(payload):
    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f"Duplicate JSON key: {key!r}")
            result[key] = value
        return result
    return json.loads(payload, object_pairs_hook=object_pairs)


def load_metadata(directory, expected_hashes=METADATA_HASHES):
    documents = {}
    hashes = {}
    for name, expected_hash in expected_hashes.items():
        path = directory / name
        require(path.is_file() and not path.is_symlink(), f"Missing regular metadata file: {path}")
        require(path.stat().st_size <= 16 * 1024 * 1024, f"Metadata is too large: {path}")
        payload = path.read_bytes()
        digest = hashlib.sha256(payload).hexdigest()
        require(digest == expected_hash, f"Metadata SHA-256 mismatch: {name}")
        hashes[name] = digest
        documents[name] = unique_json(payload)
    download = documents["download-manifest.json"]
    inventory = documents["file-inventory.json"]
    require(download.get("version") == 1 and download.get("repository") == REPOSITORY
            and download.get("tag") == TAG, "Unexpected release download manifest")
    require(inventory.get("version") == 1, "Unexpected file inventory version")
    part_rows = download.get("parts")
    require(isinstance(part_rows, list) and len(part_rows) == 14, "Expected exactly 14 release parts")
    parts = {}
    for row in part_rows:
        require(isinstance(row, dict), "Malformed release part record")
        name = row.get("name")
        safe_path(name)
        require("/" not in name and re.fullmatch(r"playgta5-part-\d{2}-of-14\.zip", name),
                f"Unexpected part name: {name!r}")
        require(name not in parts and valid_size(row.get("bytes")) and valid_hash(row.get("sha256")),
                f"Invalid or duplicate part record: {name!r}")
        parts[name] = row
    require(set(parts) == {f"playgta5-part-{n:02d}-of-14.zip" for n in range(1, 15)},
            "Release part numbering is incomplete")
    rows = inventory.get("files")
    require(isinstance(rows, list) and rows, "Empty release file inventory")
    files = {}
    folded = {}
    for row in rows:
        require(isinstance(row, dict), "Malformed inventory record")
        name = row.get("path")
        safe_path(name)
        require(not name.endswith("/") and name not in files and name.casefold() not in folded,
                f"Duplicate/case-colliding inventory path: {name!r}")
        require(row.get("part") in parts and valid_size(row.get("bytes"))
                and valid_hash(row.get("sha256")), f"Invalid inventory record: {name!r}")
        files[name] = row
        folded[name.casefold()] = name
    for name in files:
        components = name.split("/")
        require(not any("/".join(components[:i]).casefold() in folded
                        for i in range(1, len(components))), f"File/directory collision: {name!r}")
    mirror = [row for name, row in files.items() if name.startswith(SITE_PREFIX)]
    require(inventory.get("mirror_files") == len(mirror)
            and inventory.get("mirror_bytes") == sum(row["bytes"] for row in mirror),
            "Mirror inventory totals disagree with its records")
    require(MANIFEST_PATH in files and any(selected(name) for name in files),
            "Inventory has no game package")
    return parts, files, hashes


def inspect_archives(directory, parts, files, progress):
    source_archives = []
    for name, row in sorted(parts.items()):
        path = directory / name
        require(path.is_file() and not path.is_symlink(), f"Missing regular release part: {path}")
        require(path.stat().st_size == row["bytes"], f"Release part size mismatch: {name}")
        progress(f"Verifying archive SHA-256: {name}")
        digest = file_hash(path)
        require(digest == row["sha256"], f"Release part SHA-256 mismatch: {name}")
        source_archives.append({"name": name, "bytes": row["bytes"], "sha256": digest})

    seen = set()
    folded = {}
    for name in sorted(parts):
        expected = {path for path, row in files.items() if row["part"] == name}
        found = set()
        with zipfile.ZipFile(directory / name) as archive:
            for info in archive.infolist():
                path = info.orig_filename
                safe_path(path)
                require(path not in seen and path.rstrip("/").casefold() not in folded,
                        f"Duplicate/case-colliding ZIP entry: {path!r}")
                seen.add(path)
                folded[path.rstrip("/").casefold()] = path
                mode = info.external_attr >> 16
                kind = stat.S_IFMT(mode)
                require(kind in {0, stat.S_IFREG, stat.S_IFDIR}, f"Unsupported ZIP entry mode: {path!r}")
                require(not info.flag_bits & 1, f"Encrypted ZIP entry: {path!r}")
                require(info.compress_type in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED},
                        f"Unsupported compression: {path!r}")
                if info.is_dir():
                    require(kind in {0, stat.S_IFDIR} and info.file_size == 0
                            and any(p.startswith(path) for p in expected),
                            f"Unknown or invalid ZIP directory: {path!r}")
                    continue
                require(kind != stat.S_IFDIR, f"Directory mode on ZIP file: {path!r}")
                row = files.get(path)
                require(row is not None and row["part"] == name,
                        f"Unknown or misplaced release payload: {path!r}")
                require(info.file_size == row["bytes"], f"Inventory size mismatch: {path!r}")
                found.add(path)
        require(found == expected, f"Missing inventory files in {name}: {sorted(expected - found)[:5]}")
    # A directory entry can collide with a file ancestor even when metadata does not.
    for path in seen:
        components = path.rstrip("/").split("/")
        for i in range(1, len(components)):
            ancestor = folded.get("/".join(components[:i]).casefold())
            require(ancestor is None or ancestor.endswith("/"), f"ZIP file/directory collision: {path!r}")
    return source_archives


class HashingWriter:
    """Non-seekable output keeps the digest aligned with final ZIP bytes."""
    def __init__(self, raw):
        self.raw = raw
        self.digest = hashlib.sha256()
        self.position = 0

    def write(self, data):
        count = self.raw.write(data)
        require(count == len(data), "Short output write")
        self.digest.update(data)
        self.position += count
        return count

    def tell(self):
        return self.position

    def flush(self):
        self.raw.flush()


def repair_manifest(original, verified, repairs=REPAIRS):
    manifest = unique_json(original)
    rows = manifest.get("files") if isinstance(manifest, dict) else None
    require(isinstance(rows, list) and rows, "Invalid game data manifest")
    lookup = {}
    for row in rows:
        require(isinstance(row, list) and len(row) >= 2 and isinstance(row[0], str)
                and valid_size(row[1]), "Malformed game manifest file record")
        safe_path(row[0])
        require(row[0] not in lookup and not row[0].endswith("/"), "Duplicate/directory manifest path")
        lookup[row[0]] = row
    changes = []
    for path, repair in repairs.items():
        actual = verified.get(SITE_PREFIX + "data/" + path)
        row = lookup.get(path)
        require(actual is not None and actual["bytes"] == repair["after_bytes"]
                and actual["sha256"] == repair["sha256"], f"Unverified manifest repair asset: {path}")
        require(row is not None and row[1] == repair["before_bytes"],
                f"Unexpected original manifest size for known repair: {path}")
        row[1] = repair["after_bytes"]
        changes.append({"path": path, **repair})
    repaired = json.dumps(manifest, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return repaired, {
        "path": "data/manifest.json",
        "before_bytes": len(original), "after_bytes": len(repaired),
        "before_sha256": hashlib.sha256(original).hexdigest(),
        "after_sha256": hashlib.sha256(repaired).hexdigest(), "changes": changes,
    }


def validate_output(path):
    spec = importlib.util.spec_from_file_location("muguet_game_preflight", Path(__file__).with_name("validate-game.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    result = module.validate(path)
    require(result["ok"], "Assembled game preflight failed: " + "; ".join(result["errors"][:10]))
    return result


def assemble(directory, metadata_directory, output, *, progress=print,
             metadata_hashes=METADATA_HASHES, repairs=REPAIRS):
    """Parameter injection supports small local tests; CLI always uses release pins."""
    directory, metadata_directory, output = map(Path, (directory, metadata_directory, output))
    receipt_path = output.with_name(output.name + ".receipt.json")
    require(not output.exists() and not output.is_symlink(), f"Output already exists: {output}")
    require(not receipt_path.exists() and not receipt_path.is_symlink(), f"Receipt already exists: {receipt_path}")
    require(output.parent.is_dir(), f"Output directory does not exist: {output.parent}")
    started = time.monotonic()
    parts, files, hashes = load_metadata(metadata_directory, metadata_hashes)
    sources = inspect_archives(directory, parts, files, progress)
    selected_files = {name: row for name, row in files.items() if selected(name)}
    verified = {}
    original_manifest = None
    last_report = time.monotonic()
    copied_bytes = 0
    with tempfile.TemporaryDirectory(prefix=".assemble-game-", dir=output.parent) as temporary:
        temporary = Path(temporary)
        zip_path = temporary / "game.zip"
        temporary_receipt = temporary / "receipt.json"
        with zip_path.open("wb") as raw:
            writer = HashingWriter(raw)
            with zipfile.ZipFile(writer, "w", compression=zipfile.ZIP_STORED, allowZip64=True) as target:
                for part in sorted(parts):
                    progress(f"Copying verified game files: {part}")
                    with zipfile.ZipFile(directory / part) as source:
                        for path, row in sorted(selected_files.items()):
                            if row["part"] != part:
                                continue
                            digest = hashlib.sha256()
                            count = 0
                            info = zipfile.ZipInfo(path[len(SITE_PREFIX):])
                            info.compress_type = zipfile.ZIP_STORED
                            info.external_attr = (stat.S_IFREG | 0o644) << 16
                            # The data manifest is emitted after all repair assets verify.
                            content = bytearray() if path == MANIFEST_PATH else None
                            if content is not None:
                                require(row["bytes"] <= 16 * 1024 * 1024, "Game manifest is too large")
                            with source.open(path) as reader:
                                if content is None:
                                    destination = target.open(info, "w", force_zip64=True)
                                else:
                                    destination = None
                                try:
                                    while chunk := reader.read(CHUNK_BYTES):
                                        count += len(chunk)
                                        require(count <= row["bytes"], f"Oversized inventory payload: {path}")
                                        digest.update(chunk)
                                        if destination is None:
                                            content.extend(chunk)
                                        else:
                                            destination.write(chunk)
                                        copied_bytes += len(chunk)
                                        if time.monotonic() - last_report >= 10:
                                            progress(f"Verified/copied {copied_bytes:,} source bytes")
                                            last_report = time.monotonic()
                                finally:
                                    if destination is not None:
                                        destination.close()
                            sha256 = digest.hexdigest()
                            require(count == row["bytes"] and sha256 == row["sha256"],
                                    f"Inventory payload SHA-256/size mismatch: {path}")
                            verified[path] = {"bytes": count, "sha256": sha256}
                            if content is not None:
                                original_manifest = bytes(content)
                require(set(verified) == set(selected_files) and original_manifest is not None,
                        "Incomplete selected game payload")
                repaired, manifest_receipt = repair_manifest(original_manifest, verified, repairs)
                info = zipfile.ZipInfo("data/manifest.json")
                info.external_attr = (stat.S_IFREG | 0o644) << 16
                with target.open(info, "w", force_zip64=True) as destination:
                    destination.write(repaired)
            raw.flush()
            os.fsync(raw.fileno())
            output_sha256 = writer.digest.hexdigest()
            output_bytes = writer.position
        preflight = validate_output(zip_path)
        receipt = {
            "version": 1, "repository": REPOSITORY, "release_tag": TAG,
            "metadata_sha256": hashes, "source_archives": sources,
            "selected_source_files": len(verified), "selected_source_bytes": copied_bytes,
            "manifest_repair": manifest_receipt,
            "output": {"name": output.name, "bytes": output_bytes, "sha256": output_sha256,
                       "compression": "stored ZIP64", "prefix": "", "build": preflight["build"]},
            "seconds": round(time.monotonic() - started, 1),
            "verification": "All source archive hashes and selected file hashes verified; strict game preflight passed. Gameplay, shader compilation and device compatibility remain untested.",
        }
        temporary_receipt.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
        # Hard links publish without overwriting an existing file, even after a race.
        # Receipt first: if publication fails, remove only our own newly linked receipt.
        os.link(temporary_receipt, receipt_path)
        try:
            os.link(zip_path, output)
        except BaseException:
            if receipt_path.stat().st_ino == temporary_receipt.stat().st_ino:
                receipt_path.unlink()
            raise
    progress(f"Assembled {output} ({output_bytes:,} bytes); receipt: {receipt_path}")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("parts_directory", type=Path, help="local directory with all 14 release ZIPs")
    parser.add_argument("--metadata", type=Path, help="directory containing the pinned release JSON files; defaults to parts_directory")
    parser.add_argument("--output", type=Path, required=True, help="new Muguet ZIP path; existing files are never overwritten")
    args = parser.parse_args()
    try:
        assemble(args.parts_directory, args.metadata or args.parts_directory, args.output,
                 progress=lambda message: print(message, flush=True))
    except (OSError, ValueError, RuntimeError, EOFError, zipfile.BadZipFile, zipfile.LargeZipFile, zlib.error) as exc:
        print(f"Assembly failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

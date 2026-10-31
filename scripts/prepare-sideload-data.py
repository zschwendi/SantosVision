#!/usr/bin/env python3
"""Prepare an owned compatible game ZIP on Windows, Linux or macOS."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import zipfile

spec = importlib.util.spec_from_file_location('validate_game', Path(__file__).with_name('validate-game.py'))
validator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validator)


GAME_BUILD = '8b0b5899ed'
GAME_WASM_SHA256 = '11ca8d2c04c5e843d18ff4aea4899d72c86973c6b031df334e67c446b2ae83e0'


def prepare(archive, output, expected_build=GAME_BUILD, expected_wasm=GAME_WASM_SHA256):
    if output.exists() or output.is_symlink():
        raise ValueError('Output must be a new folder; existing game files are never replaced')
    check = validator.validate(archive)
    if not check['ok']:
        raise ValueError('Package preflight failed: ' + '; '.join(check['errors'][:5]))
    if check['build'] != expected_build:
        raise ValueError('This IPA requires game build ' + expected_build)
    with zipfile.ZipFile(archive) as zipped, zipped.open(check['wasm_entry']) as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != expected_wasm:
            raise ValueError('Game code does not match this IPA; use its matching package')
    output.parent.mkdir(parents=True, exist_ok=True)
    required = check['uncompressed_import_bytes'] + 1024**3
    if shutil.disk_usage(output.parent).free < required:
        raise ValueError('Not enough space for the prepared files plus 1 GiB reserve')
    prefix = check['prefix']
    with tempfile.TemporaryDirectory(prefix='.v-import-', dir=output.parent) as temporary:
        staging = Path(temporary) / 'game'
        staging.mkdir()
        with zipfile.ZipFile(archive) as zipped:
            for entry in zipped.infolist():
                if entry.is_dir() or not entry.filename.startswith(prefix):
                    continue
                rel = entry.filename[len(prefix):]
                if validator.skipped(rel):
                    continue
                # Match src/import.rs with keep_code=false. Executable game
                # code is already in the IPA; copy only data and shaders.
                if not (rel.startswith('data/') or rel.startswith('b/' + check['build'] + '/shaders/')):
                    continue
                destination = staging.joinpath(*rel.split('/'))
                destination.parent.mkdir(parents=True, exist_ok=True)
                with zipped.open(entry) as source, destination.open('xb') as target:
                    shutil.copyfileobj(source, target, 1024 * 1024)
        data = staging / 'data'
        dlc = data / 'common/data/dlcList.xml'
        if dlc.is_file():
            text = dlc.read_bytes().decode('utf-8')
            text = '\n'.join(line for line in text.splitlines()
                             if not any(skip in line.lower() for skip in validator.SKIPPED_DIRS)) + '\n'
            dlc.write_bytes(text.encode('utf-8'))
        manifest_file = data / 'manifest.json'
        manifest = json.loads(manifest_file.read_bytes())
        manifest['files'] = [row for row in manifest['files'] if (data / row[0]).is_file()]
        for row in manifest['files']:
            actual = (data / row[0]).stat().st_size
            if row[0] == 'common/data/dlcList.xml':
                row[1] = actual
            elif row[1] != actual:
                raise ValueError('Asset size mismatch after extraction: ' + row[0])
        manifest_file.write_text(json.dumps(manifest, separators=(',', ':')), encoding='utf-8')
        # rename() would replace an empty directory on Unix: recheck first.
        if output.exists() or output.is_symlink():
            raise ValueError('Output appeared during import; nothing was replaced')
        staging.rename(output)
    return {'build': check['build'], 'dataFiles': len(manifest['files']), 'output': str(output)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(prepare(args.archive, args.output.absolute()), indent=2))
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        parser.exit(2, 'Preparation failed: ' + str(error) + '\n')


if __name__ == '__main__':
    main()

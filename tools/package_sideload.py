#!/usr/bin/env python3
"""Package a reviewed visionOS app for re-signing, without modifying the input."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def package(app, output, recipe):
    app = app.absolute()
    if app.is_symlink() or not app.is_dir() or app.suffix != '.app':
        raise ValueError('Input must be an ordinary .app directory')
    if output.resolve().is_relative_to(app.resolve()):
        raise ValueError('Output must be outside the input app')
    if output.exists():
        raise ValueError('Output already exists; choose a new directory')
    info = plistlib.loads((app / 'Info.plist').read_bytes())
    executable = info['CFBundleExecutable']
    if (info.get('CFBundleSupportedPlatforms') != ['XROS']
            or executable != recipe['executable']
            or info.get('CFBundleVersion') != recipe['inputBuild']):
        raise ValueError('App does not match the reviewed visionOS build')
    include = recipe['include']
    if set(include) != set(recipe['inputSHA256']):
        raise ValueError('Every included file must have a reviewed input hash')
    names = {p.name for p in app.iterdir()}
    unexpected = names - set(include) - set(recipe['exclude'])
    if unexpected:
        raise ValueError('Unreviewed app entries: ' + ', '.join(sorted(unexpected)))
    for name in include:
        p = app / name
        if '/' in name or name in {'.', '..'} or p.is_symlink() or not p.is_file():
            raise ValueError('Only reviewed regular top-level files are supported: ' + name)
        if sha256(p) != recipe['inputSHA256'][name]:
            raise ValueError('Input changed since review: ' + name)
    build = subprocess.check_output(['xcrun', 'vtool', '-show-build', str(app / executable)], text=True)
    if not any(('platform ' + name) in build for name in ['VISIONOS', 'XROS']) or 'SIMULATOR' in build:
        raise ValueError('Executable is not a physical visionOS build')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='ipa-stage-', dir=output.parent) as temporary:
        stage = Path(temporary)
        copy = stage / 'Payload' / app.name
        copy.mkdir(parents=True)
        for name in include:
            shutil.copyfile(app / name, copy / name)
            (copy / name).chmod(0o755 if name == executable else 0o644)
        info.update(recipe['plistOverrides'])
        (copy / 'Info.plist').write_bytes(plistlib.dumps(info))
        shutil.copyfile(ROOT / 'LICENSE', copy / 'PortLicense.txt')
        # Replace the development signature and its embedded certificate with
        # an ad-hoc signature. A sideloader must sign this again for its user.
        subprocess.run(['codesign', '--remove-signature', str(copy / executable)], check=True)
        if recipe.get('entitlements'):
            entitlements = stage / 'entitlements.plist'
            entitlements.write_bytes(plistlib.dumps(recipe['entitlements']))
            args = ['--entitlements', str(entitlements)]
        else:
            args = []
        subprocess.run(['codesign', '--force', '--sign', '-', '--timestamp=none', *args, str(copy)], check=True)
        subprocess.run(['codesign', '--verify', '--strict', str(copy)], check=True)
        metadata = subprocess.check_output(['codesign', '-dvv', str(copy)], stderr=subprocess.STDOUT, text=True)
        if 'Signature=adhoc' not in metadata or 'Authority=' in metadata:
            raise ValueError('Unexpected signing identity remains')
        output.mkdir()
        ipa = output / recipe['assetName']
        entries = []
        with zipfile.ZipFile(ipa, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as zipped:
            for p in sorted(copy.rglob('*')):
                if p.is_symlink():
                    raise ValueError('Unexpected output symlink')
                if p.is_file():
                    rel = p.relative_to(stage).as_posix()
                    zipped.write(p, rel)
                    entries.append({'path': rel, 'bytes': p.stat().st_size, 'sha256': sha256(p)})
        with zipfile.ZipFile(ipa) as zipped:
            if zipped.testzip() is not None:
                raise ValueError('IPA failed ZIP integrity check')
        manifest = {
            'schemaVersion': 1, 'repository': recipe['repository'],
            'sourceRevision': recipe['sourceRevision'], 'inputBuild': recipe['inputBuild'],
            'packagedBuild': info['CFBundleVersion'], 'version': info['CFBundleShortVersionString'],
            'minimumOSVersion': info['MinimumOSVersion'], 'platform': 'visionOS',
            'bundleIdentifier': info['CFBundleIdentifier'], 'signing': 'ad-hoc; user must re-sign',
            'entitlements': recipe.get('entitlements', {}), 'changes': recipe['changes'],
            'asset': {'name': ipa.name, 'bytes': ipa.stat().st_size, 'sha256': sha256(ipa)},
            'files': entries, 'freshSideloadTested': False, 'gameplayAfterResigningTested': False,
        }
        (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        (output / 'SHA256SUMS').write_text(sha256(ipa) + '  ' + ipa.name + '\n')
    # Prove that packaging did not mutate the source application.
    for name, digest in recipe['inputSHA256'].items():
        if sha256(app / name) != digest:
            raise ValueError('Original app changed during packaging: ' + name)
    print(json.dumps(manifest['asset']))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--recipe', type=Path, default=ROOT / 'packaging/sideload.json')
    args = parser.parse_args()
    try:
        package(args.app, args.output.absolute(), json.loads(args.recipe.read_text()))
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(2, 'Packaging failed: ' + str(error) + '\n')


if __name__ == '__main__':
    main()

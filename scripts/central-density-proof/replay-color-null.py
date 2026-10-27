#!/usr/bin/env python3
"""Freeze and replay one actual color-null draw. Never starts a game or device."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time
import zlib


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def png(path, width, height, pixels, channels, *, linear=False):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    if len(pixels) != width * height * channels:
        raise ValueError('PNG dimensions do not match captured bytes')
    raw = b''.join(b'\0' + pixels[y * width * channels:(y + 1) * width * channels] for y in range(height))
    color = chunk(b'gAMA', struct.pack('>I', 100000)) if linear else chunk(b'sRGB', b'\0')
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2 if channels == 3 else 0, 0, 0, 0)) + color + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))


def exports(output, manifest):
    result = json.loads((output / 'replay/result.json').read_text())
    w, h = result['width'], result['height']
    capture = output / 'capture'
    names = {'initial': capture / manifest['attachments'][0]['initial_blob'],
             'original-corrected': capture / manifest['attachments'][0]['final_blob'],
             'replayed-corrected': output / 'replay/corrected-aspect0.bin',
             'historical-rejected': output / 'replay/historical-single-aspect0.bin'}
    images = output / 'images'
    images.mkdir()
    values = {}
    for name, path in names.items():
        bgra = path.read_bytes()
        if len(bgra) != w*h*4:
            raise ValueError('Color extent mismatch')
        rgb = bytearray(w*h*3)
        rgb[0::3], rgb[1::3], rgb[2::3] = bgra[2::4], bgra[1::4], bgra[0::4]
        alpha = bgra[3::4]
        png(images / f'{name}-rgb.png', w, h, bytes(rgb), 3)
        png(images / f'{name}-alpha.png', w, h, alpha, 1, linear=True)
        values[name] = (rgb, alpha)
    before, after = values['historical-rejected'], values['original-corrected']
    changed = []
    rgb_abs, alpha_abs = bytearray(w*h*3), bytearray(w*h)
    for i in range(w*h):
        for c in range(3):
            rgb_abs[i*3+c] = abs(before[0][i*3+c]-after[0][i*3+c])
        alpha_abs[i] = abs(before[1][i]-after[1][i])
        if alpha_abs[i] or any(rgb_abs[i*3:i*3+3]):
            changed.append(i)
    png(images / 'absolute-rgb-difference.png', w, h, bytes(rgb_abs), 3, linear=True)
    png(images / 'absolute-alpha-difference.png', w, h, bytes(alpha_abs), 1, linear=True)
    bounds = None
    if changed:
        bounds = [min(i % w for i in changed), min(i // w for i in changed), max(i % w for i in changed)+1, max(i // w for i in changed)+1]
        # Exact one-to-one crop centered on changed pixels; no sharpening/upscale.
        x0, y0, x1, y1 = bounds
        cx, cy = (x0+x1)//2, (y0+y1)//2
        cw, ch = min(512, w), min(512, h)
        left, top = min(max(cx-cw//2, 0), w-cw), min(max(cy-ch//2, 0), h-ch)
        for name, (rgb, alpha) in values.items():
            for suffix, src, channels in [('rgb', rgb, 3), ('alpha', alpha, 1)]:
                crop = b''.join(src[((top+y)*w+left)*channels:((top+y)*w+left+cw)*channels] for y in range(ch))
                png(images / f'{name}-{suffix}-crop.png', cw, ch, crop, channels, linear=suffix=='alpha')
        bounds_crop = [left, top, left+cw, top+ch]
    else:
        bounds_crop = None
    note = {'scope': 'Actual single-draw target RGB and numerical alpha; these are not finished-world or compositor screenshots.',
            'rgb_encoding': 'Raw BGRA8 bytes reordered to RGB and interpreted as sRGB for preview; this intermediate target encoding is not independently established. No gamma conversion; raw bins are authoritative.',
            'alpha_encoding': 'Captured numeric alpha displayed as grayscale with gamma 1.0; independent of RGB.',
            'difference_encoding': 'Absolute byte differences, gamma 1.0, unscaled.',
            'changed_bounds_exclusive': bounds, 'crop_bounds_exclusive': bounds_crop,
            'changed_channels': result['original_color_channel_changes'],
            'effective_pipeline': result['effective_pipeline']}
    (images / 'metadata.json').write_text(json.dumps(note, indent=2)+'\n')


def main():
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] != '--cpu-only'):
        raise SystemExit('usage: replay-color-null.py MANIFEST NEW_OUTPUT_DIRECTORY [--cpu-only]')
    source = Path(__file__).resolve().parent
    root = source.parent.parent
    manifest = Path(sys.argv[1]).resolve(strict=True)
    manifest_raw = manifest.read_bytes()
    manifest_sha256 = hashlib.sha256(manifest_raw).hexdigest()
    metadata = json.loads(manifest_raw)
    if metadata.get('schema') != 3 or metadata.get('kind') != 'native_color_null_draw':
        raise SystemExit('Requires actual native_color_null_draw schema3')
    blobs, total = [], 0
    for blob in metadata['blobs']:
        relative = Path(blob['path'])
        if relative.is_absolute() or not relative.parts or any(p in ('..', '.') for p in relative.parts):
            raise SystemExit('Invalid blob path')
        path = (manifest.parent / relative).resolve(strict=True)
        if not path.is_relative_to(manifest.parent):
            raise SystemExit('Blob escapes capture')
        total += blob['byte_length']
        if total > 256*1024*1024 or path.stat().st_size != blob['byte_length'] or sha(path) != blob['sha256']:
            raise SystemExit('Capture blob size/hash failed')
        blobs.append((path, relative))
    output = Path(sys.argv[2]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / 'capture'
    frozen.mkdir()
    (frozen / 'manifest.json').write_bytes(manifest_raw)
    for path, relative in blobs:
        dest = frozen / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, dest)
    files = [source / p for p in ['Cargo.toml', 'Cargo.lock', 'src/bin/replay_color_null.rs', 'replay-color-null.py', 'replay-color-null.sh', 'COLOR-NULL-REPLAY.md']]
    files += sorted(p for p in (root/'vendor/wgpu-core').rglob('*') if p.is_file() and (p.suffix == '.rs' or p.name == 'Cargo.toml'))
    before = {str(p.relative_to(root)): sha(p) for p in files}
    for path in files:
        dest = output / 'source' / path.relative_to(root)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, dest)
    receipt = {'schema': 1, 'scope': 'Exact original single-draw replay and historical alias control; no game/device/simulator/save actions',
               'original_manifest': str(manifest), 'original_manifest_sha256': manifest_sha256, 'source_sha256_before': before,
               'gpu_requested': len(sys.argv) == 3, 'capture_blobs': {str(r): sha(p) for p, r in blobs}}
    started = time.time()
    command = ['cargo', 'build', '--offline', '--locked', '--manifest-path', str(source/'Cargo.toml'), '--bin', 'replay_color_null']
    receipt['build_command'] = command
    with (output/'build.log').open('wb') as log:
        build = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=300)
    code = build.returncode
    receipt['build_returncode'] = code
    if code == 0:
        binary = Path(os.environ['CARGO_TARGET_DIR'])/'debug/replay_color_null'
        receipt['binary_sha256'] = sha(binary)
        command = [str(binary), str(frozen/'manifest.json'), str(output/'replay')] + sys.argv[3:]
        receipt['run_command'] = command
        with (output/'run.log').open('wb') as log:
            run = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=120)
        code = run.returncode
        receipt['run_returncode'] = code
        print((output/'run.log').read_text()[-3000:])
        if (output/'replay/result.json').exists():
            exports(output, metadata)
    else:
        print((output/'build.log').read_text()[-3000:])
    receipt['elapsed_wall_seconds_not_gpu_time'] = time.time()-started
    receipt['source_sha256_after'] = {str(p.relative_to(root)): sha(p) for p in files}
    receipt['inputs_unchanged'] = (before == receipt['source_sha256_after'] and sha(manifest) == sha(frozen/'manifest.json') == manifest_sha256
        and all(sha(p) == receipt['capture_blobs'][str(r)] == sha(frozen/r) for p, r in blobs))
    receipt['artifact_sha256'] = {str(p.relative_to(output)): sha(p) for p in sorted(output.rglob('*')) if p.is_file()}
    receipt['passed'] = code == 0 and receipt['inputs_unchanged']
    (output/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
    print(f'Receipt: {output / "receipt.json"}')
    return code if code else (0 if receipt['inputs_unchanged'] else 1)


if __name__ == '__main__':
    sys.exit(main())

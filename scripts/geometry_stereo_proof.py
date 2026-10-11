#!/usr/bin/env python3
"""CPU-only, fail-closed geometry stereo qualification for one frozen game draw.

No game launch, GPU work, pair publication, or claim about full-scene rendering.
"""
import argparse
import hashlib
import json
import math
import struct
import sys
import zipfile
from pathlib import Path

VS_HASH = '6a6a0898a51cf403'  # Active Low-scene default::VS_TransformD.
OLD_VS_HASH = '2e9584811e870546'  # Frozen synthetic regression fixture.
PS_HASH = '75cf2d53bf9497c9'
VS_SHA256 = 'fc914d159a51b8bd8e6d35633dd385a6cef50fb1ed740d5ad3d86893a03acb61'
OLD_VS_SHA256 = '696bab4312d89127bcbec89d20e5158ee45e1af6f5d726892e02867f62e6572d'
PS_SHA256 = '1e3d02aa02c1219e721c351c2fbcebc45154b2f9b1a8f2bdc8f1f6a96ff0daed'
DEFAULT_ZIP = Path('/Users/zach/gaming/game-assets/playgta5-full-game-v1/playgta5-part-01-of-14.zip')
CLIP_PATTERN = ('x0[0u].x = dot(r0, cb0_0.tint_symbol_1[0u]);',
                'o5[0u] = x0[0u].x;', 'o5[1u] = x0[0u].y;',
                'o5[2u] = x0[0u].z;', 'o5[3u] = x0[0u].w;')
POSITION_PREFIX = ('r0 = (v0.yyyy * cb1_1.tint_symbol[9u]);',
                   'r0 = fma(v0.xxxx, cb1_1.tint_symbol[8u], r0);',
                   'r0 = fma(v0.zzzz, cb1_1.tint_symbol[10u], r0);')
OLD_POSITION_PATTERN = POSITION_PREFIX + ('r0 = (r0 + cb1_1.tint_symbol[11u]);', 'o4 = r0;',
                                          '@builtin(position)\n  o4 : vec4<f32>,')
D_POSITION_PATTERN = POSITION_PREFIX + ('o0 = (r0 + cb1_1.tint_symbol[11u]);',
                                        '@builtin(position)\n  o0 : vec4<f32>,')
VS_VARIANTS = {
    OLD_VS_HASH: (OLD_VS_SHA256, OLD_POSITION_PATTERN + CLIP_PATTERN, True),
    VS_HASH: (VS_SHA256, D_POSITION_PATTERN, False),
}


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def shader_evidence(archive, vs_hash):
    require(vs_hash in VS_VARIANTS, 'unsupported vertex shader variant')
    vs_digest, position_pattern, has_user_clip = VS_VARIANTS[vs_hash]
    with zipfile.ZipFile(archive) as z:
        names = z.namelist()
        def read(hash_id, digest):
            name = next((n for n in names if n.endswith('/shaders/' + hash_id + '.wgsl')), None)
            require(name is not None, 'packaged shader missing: ' + hash_id)
            data = z.read(name)
            require(hashlib.sha256(data).hexdigest() == digest, 'packaged shader digest changed: ' + hash_id)
            return data.decode('utf-8')
        vs = read(vs_hash, vs_digest)
        ps = read(PS_HASH, PS_SHA256)
    source_vs = vs.replace('\r\n', '\n')
    require(all(token in source_vs for token in position_pattern), 'source VS position formula changed')
    require(('@builtin(clip_distances)' in source_vs) == has_user_clip,
            'source VS user clip-distance contract changed')
    require('discard;' not in ps and '@builtin(frag_depth)' not in ps, 'source PS alpha/depth behavior changed')
    return {'vertex_shader_sha256': vs_digest, 'pixel_shader_sha256': PS_SHA256}, position_pattern, has_user_clip


def mat_col_major(values):
    require(len(values) == 16 and all(math.isfinite(x) for x in values), 'invalid 4x4 matrix')
    return [[float(values[c * 4 + r]) for c in range(4)] for r in range(4)]


def mul(a, b):
    return [[sum(a[r][k] * b[k][c] for k in range(4)) for c in range(4)] for r in range(4)]


def vec(a, v):
    return [sum(a[r][c] * v[c] for c in range(4)) for r in range(4)]


def inverse(a):
    aug = [row[:] + [float(r == c) for c in range(4)] for r, row in enumerate(a)]
    for c in range(4):
        pivot = max(range(c, 4), key=lambda r: abs(aug[r][c]))
        require(abs(aug[pivot][c]) > 1e-9, 'singular matrix')
        aug[c], aug[pivot] = aug[pivot], aug[c]
        d = aug[c][c]
        aug[c] = [x / d for x in aug[c]]
        for r in range(4):
            if r != c:
                d = aug[r][c]
                aug[r] = [x - d * y for x, y in zip(aug[r], aug[c])]
    return [row[4:] for row in aug]


def from_columns(cols):
    return [[float(cols[c][r]) for c in range(4)] for r in range(4)]


def rigid(values):
    m = mat_col_major(values)
    if max(abs(m[3][i] - (1.0 if i == 3 else 0.0)) for i in range(4)) > 1e-4:
        return False
    basis = [[m[r][c] for r in range(3)] for c in range(3)]
    for a in range(3):
        for b in range(3):
            dot = sum(basis[a][r] * basis[b][r] for r in range(3))
            if abs(dot - (1.0 if a == b else 0.0)) > .03:
                return False
    det = (basis[0][0] * (basis[1][1] * basis[2][2] - basis[1][2] * basis[2][1])
           - basis[1][0] * (basis[0][1] * basis[2][2] - basis[0][2] * basis[2][1])
           + basis[2][0] * (basis[0][1] * basis[1][2] - basis[0][2] * basis[1][1]))
    return .97 <= det <= 1.03


def request_eyes(record, source_view, embedded=False):
    require(record.get('schema') == 1 and record.get('source') == 'sv_xr_submit_views',
            'requires raw submitted SVXRViewRequest evidence')
    require(len(record['request_bytes_hex']) == 576, 'SVXRViewRequest hex length invalid')
    payload = bytes.fromhex(record['request_bytes_hex'])
    require(len(payload) == 288, 'SVXRViewRequest must be 288 bytes')
    digest = hashlib.sha256(payload).hexdigest()
    require(record.get('request_sha256', digest if embedded else None) == digest,
            'request payload SHA256 missing or mismatched')
    version, size, frame, time, count, reserved = struct.unpack_from('<IIQdII', payload)
    require((version, size, count, reserved) == (1, 288, 2, 0) and frame > 0
            and math.isfinite(time) and time >= 0, 'invalid stereo request header')
    require(frame == source_view['request_frame'] and record['provenance_epoch'] == source_view['provenance_epoch']
            and record['head_pose_frame'] == source_view['head_pose_frame'] != 0,
            'request does not match source command epoch/frame/pose identity')
    require(source_view['view_count'] == 2 and source_view['eye_index'] in (0, 1),
            'source command was not a stereo-request eye')
    require(abs(time - source_view['presentation_time_seconds']) <= .05,
            'request and consumed pose presentation times disagree')
    raw_eyes = []
    for index in range(2):
        vals = struct.unpack_from('<32f', payload, 32 + 128 * index)
        head, projection = vals[:16], vals[16:]
        require(rigid(head), f'request eye {index} head transform invalid')
        require(all(math.isfinite(v) for v in projection) and projection[0] > 0
                and projection[5] > 0 and abs(projection[11] + 1) < 1e-4
                and projection[10] >= 0 and projection[14] > 0 and abs(projection[15]) < 1e-5,
                f'request eye {index} projection invalid')
        raw_eyes.append((head, projection))
    source_index = source_view['eye_index']
    source_projection = source_view['projection_from_eye']
    require(max(abs(a - b) for a, b in zip(source_projection, raw_eyes[source_index][1])) < 1e-4,
            'submitted request does not reproduce source-eye projection')
    source_camera = mat_col_major(source_view['game_from_eye'])
    game_from_head = mul(source_camera, inverse(mat_col_major(raw_eyes[source_index][0])))
    eyes = []
    for head, projection in raw_eyes:
        game_from_eye = mul(game_from_head, mat_col_major(head))
        camera = [game_from_eye[r][c] for c in range(4) for r in range(4)]
        require(rigid(camera), 'derived game eye transform invalid')
        eyes.append({'game_from_eye': camera, 'projection_from_eye': projection})
    source_derived = eyes[source_index]['game_from_eye']
    require(max(abs(a - b) for a, b in zip(source_derived, source_view['game_from_eye'])) < 1e-4,
            'submitted request does not reproduce source-eye camera')
    separation = math.sqrt(sum((eyes[0]['game_from_eye'][i] - eyes[1]['game_from_eye'][i]) ** 2
                               for i in (12, 13, 14)))
    require(.001 <= separation <= .25, f'eye separation {separation:.6g} m is implausible')
    return eyes, digest, separation


def check_unclipped(clips, label):
    for i, (x, y, z, w) in enumerate(clips):
        require(all(math.isfinite(v) for v in (x, y, z, w)) and w > 1e-4,
                f'{label} vertex {i} reaches eye/near clipping')
        require(-w + 1e-4 < x < w - 1e-4 and -w + 1e-4 < y < w - 1e-4
                and 1e-4 < z < w - 1e-4,
                f'{label} vertex {i} reaches side/near/far clipping')


def unpack_buffer(item):
    data = bytes.fromhex(item['bytes_hex'])
    require(len(data) == item['readback_size'], 'readback length mismatch')
    require(len(data) <= 1024 * 1024, 'buffer over capture limit')
    return data


def decode_positions(capture):
    signature = capture['vertex_signature']
    zero = next((x for x in signature if x['register'] == 0), None)
    require(zero is not None, 'VS location 0 absent')
    el = next((x for x in capture['layout'] if x['semantic'] == zero['semantic']
               and x['semantic_index'] == zero['semantic_index']), None)
    require(el is not None and el['format'] == 6 and el['class'] == 0,
            'VS location 0 must be per-vertex DXGI R32G32B32_FLOAT')
    vb = next((x for x in capture['vertex_buffers'] if x['slot'] == el['slot']), None)
    require(vb is not None and vb['stride'] >= el['offset'] + 12, 'position VB stride/slot invalid')
    raw = unpack_buffer(vb)
    ib = capture['index_buffer']
    indices = unpack_buffer(ib)
    fmt = {57: '<H', 42: '<I'}.get(ib['format'])
    require(fmt is not None, 'unsupported index format')
    draw = capture['draw']
    count, start, base = draw['count'], draw['start_index'], draw['base_vertex']
    require(3 <= count <= 65536 and count % 3 == 0, 'draw count is not bounded triangles')
    unit = struct.calcsize(fmt)
    require(ib['offset'] % unit == 0 and ib['offset'] + (start + count) * unit <= len(indices),
            'index range invalid')
    out = []
    for i in range(start, start + count):
        index = struct.unpack_from(fmt, indices, ib['offset'] + i * unit)[0] + base
        off = vb['offset'] + index * vb['stride'] + el['offset']
        require(index >= 0 and 0 <= off <= len(raw) - 12, 'position index outside VB')
        p = struct.unpack_from('<3f', raw, off)
        require(all(math.isfinite(x) for x in p), 'nonfinite game vertex')
        out.append((*p, 1.0))
    return out


def cb_vec(capture, binding, index):
    cb = next((c for c in capture['constant_buffers'] if c['stage'] == 0 and c['binding'] == binding), None)
    require(cb is not None, 'required VS constant buffer missing')
    data = bytes.fromhex(cb['bytes_hex'])
    require(len(data) >= (index + 1) * 16, 'required VS vector CB truncated')
    return struct.unpack_from('<4f', data, index * 16)


def cb_columns(capture, binding, start):
    return [cb_vec(capture, binding, start + i) for i in range(4)]


def raster(clips, width, height):
    depth = [-math.inf] * (width * height)  # reverse Z, greater wins
    coverage = bytearray(width * height)
    valid = 0
    for q in range(0, len(clips), 3):
        tri = clips[q:q + 3]
        require(not any(v[3] <= 1e-6 or not all(math.isfinite(n) for n in v) for v in tri),
                'raster received a vertex requiring eye/near clipping')
        pts = [((v[0] / v[3] * .5 + .5) * width,
                (1 - (v[1] / v[3] * .5 + .5)) * height, v[2] / v[3]) for v in tri]
        (ax, ay, az), (bx, by, bz), (cx, cy, cz) = pts
        area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
        if abs(area) < 1e-8:
            continue
        valid += 1
        for y in range(max(0, int(math.floor(min(ay, by, cy)))), min(height, int(math.ceil(max(ay, by, cy))))):
            for x in range(max(0, int(math.floor(min(ax, bx, cx)))), min(width, int(math.ceil(max(ax, bx, cx))))):
                px, py = x + .5, y + .5
                u = ((bx - px) * (cy - py) - (by - py) * (cx - px)) / area
                v = ((cx - px) * (ay - py) - (cy - py) * (ax - px)) / area
                w = 1 - u - v
                if min(u, v, w) < -1e-7:
                    continue
                z = u * az + v * bz + w * cz
                i = y * width + x
                if z > depth[i]:
                    depth[i], coverage[i] = z, 1
    return coverage, depth, valid


def write_ppm(path, coverage, width, height, tint):
    with path.open('xb') as f:
        f.write(f'P6\n{width} {height}\n255\n'.encode())
        for covered in coverage:
            f.write(bytes(tint if covered else (0, 0, 0)))


def scratch_size(capture):
    viewport = capture.get('viewport')
    require(isinstance(viewport, list) and len(viewport) == 6,
            'captured full viewport missing')
    require(all(isinstance(value, (int, float)) and not isinstance(value, bool)
                and math.isfinite(value) for value in viewport),
            'viewport contains nonfinite or nonnumeric values')
    x, y, width, height, near, far = viewport
    require(x == 0 and y == 0 and near == 0 and far == 1,
            'viewport origin or depth range is not full target')
    require(width == int(width) and height == int(height) and width > 0 and height > 0,
            'viewport extent is not positive integer pixels')
    width, height = int(width), int(height)
    require(width <= 1024 and height <= 1024 and width * height <= 1_000_000,
            'captured viewport exceeds bounded scratch resolution')
    return width, height


def qualify(capture, request_record, archive, output, allow_synthetic=False):
    synthetic = capture.get('synthetic', False)
    require(not synthetic or allow_synthetic, 'synthetic fixture cannot qualify a real game draw')
    require(capture.get('schema') == 1 and capture.get('status') == 'captured'
            and capture.get('source') == 'Gpu::do_draw', 'requires one actual host draw capture')
    vs_hash = capture['shader']['vertex_hash']
    require(vs_hash in VS_VARIANTS and capture['shader']['pixel_hash'] == PS_HASH,
            'draw shader pair is not the frozen opaque fixture')
    shader, position_pattern, has_user_clip = shader_evidence(archive, vs_hash)
    for stage, expected in [('vertex', shader['vertex_shader_sha256']),
                            ('pixel', PS_SHA256)]:
        source = capture['shader'][stage + '_source_wgsl'].encode('utf-8')
        translated = capture['shader'][stage + '_translated_wgsl'].encode('utf-8')
        require(hashlib.sha256(source).hexdigest() == expected, stage + ' captured source differs from package')
        require(0 < len(translated) <= 256 * 1024, stage + ' translated shader missing/oversize')
        shader[stage + '_translated_sha256'] = hashlib.sha256(translated).hexdigest()
    translated_vs = capture['shader']['vertex_translated_wgsl'].replace('cbpack_v.', '').replace('\r\n', '\n')
    require(all(token in translated_vs for token in position_pattern),
            'translated VS position path changed')
    require(('@builtin(clip_distances)' in translated_vs) == has_user_clip,
            'translated VS user clip-distance contract changed')
    source_view = capture['world_view']
    require(source_view['headset_applied'] and source_view['depth_reverse_z'], 'source view unqualified')
    require(capture['draw']['kind'] == 1 and capture['draw']['instances'] == 1
            and capture['draw']['start_instance'] == 0 and capture['draw']['topology'] == 4,
            'draw topology/instance unqualified')
    embedded = request_record is None
    captured_receipt = capture.get('request_record')
    if embedded:
        request_record = captured_receipt
    require(request_record is not None, 'exact consumed request receipt missing')
    if captured_receipt is not None:
        require(request_record['request_bytes_hex'] == captured_receipt['request_bytes_hex']
                and request_record['provenance_epoch'] == captured_receipt['provenance_epoch']
                and request_record['head_pose_frame'] == captured_receipt['head_pose_frame'],
                'separate request record differs from consumed host receipt')
    eyes, request_digest, eye_separation = request_eyes(request_record, source_view, embedded=embedded)
    if has_user_clip:
        # The old VS emits four clip distances. Its first is dot(clip, CB0[0]);
        # the other three are zero. We only model the identically disabled plane.
        clip_plane = cb_vec(capture, 0, 0)
        require(all(abs(value) <= 1e-7 for value in clip_plane),
                'game VS clip distance plane is active or ambiguous')
    positions = decode_positions(capture)
    object_to_game = from_columns(cb_columns(capture, 1, 0))
    source_local_to_clip = from_columns(cb_columns(capture, 1, 8))
    source_camera = mat_col_major(source_view['game_from_eye'])
    source_projection = mat_col_major(source_view['projection_from_eye'])
    expected_source = mul(mul(source_projection, inverse(source_camera)), object_to_game)
    mismatch = max(abs(source_local_to_clip[r][c] - expected_source[r][c])
                   for r in range(4) for c in range(4))
    require(mismatch < 0.02, f'game shader CB clip matrix does not match command camera ({mismatch:.6g})')
    inv_source = inverse(source_local_to_clip)
    check_unclipped([vec(source_local_to_clip, p) for p in positions], 'source')
    width, height = scratch_size(capture)
    results = []
    screens = []
    for index, eye in enumerate(eyes):
        eye_local = mul(mul(mat_col_major(eye['projection_from_eye']),
                            inverse(mat_col_major(eye['game_from_eye']))), object_to_game)
        correction = mul(eye_local, inv_source)
        direct, corrected = [], []
        for p in positions:
            src = vec(source_local_to_clip, p)
            direct.append(vec(eye_local, p))
            corrected.append(vec(correction, src))
        check_unclipped(direct, f'eye {index}')
        check_unclipped(corrected, f'corrected eye {index}')
        error = max(abs(a - b) for x, y in zip(direct, corrected) for a, b in zip(x, y))
        require(error < 0.002, f'eye {index} correction differs from independent projection: {error}')
        cov, dep, valid = raster(corrected, width, height)
        ref_cov, ref_dep, _ = raster(direct, width, height)
        require(cov == ref_cov, f'eye {index} coverage differs from direct reference')
        depth_error = max((abs(a - b) for a, b, hit in zip(dep, ref_dep, cov) if hit), default=0)
        require(depth_error < 0.002, f'eye {index} depth differs from direct reference')
        require(sum(cov) >= 4 and valid > 0, f'eye {index} has insufficient visible triangles')
        screens.append(cov)
        results.append({'covered_pixels': sum(cov), 'valid_triangles': valid,
                        'max_clip_error': error, 'max_depth_error': depth_error})
    distinct_pixels = sum(a != b for a, b in zip(*screens))
    require(distinct_pixels >= 2, 'eyes have no geometric disparity in scratch targets')
    report = {'status': 'synthetic-plumbing-pass' if synthetic else 'single-draw-cpu-geometry-verified',
              'synthetic': synthetic, 'not_full_scene_pair': True,
              'draw_shader': {key: capture['shader'][key] for key in ('vertex_id', 'vertex_hash', 'pixel_id', 'pixel_hash')
                              if key in capture['shader']}, **shader, 'vertices': len(positions),
              'request_sha256': request_digest, 'eye_separation_m': eye_separation,
              'scratch_size': {'width': width, 'height': height, 'source': 'captured_full_viewport'},
              'source_matrix_max_error': mismatch, 'eyes': results, 'distinct_coverage_pixels': distinct_pixels,
              'limits': ['CPU position/depth only; game PS not evaluated',
                         'any user clip plane or side/eye/near/far crossing rejected',
                         'one draw only; no union-culling or deferred/post graph proof']}
    output.mkdir(parents=False, exist_ok=False)
    for index, cov in enumerate(screens):
        write_ppm(output / f'eye-{index}.ppm', cov, width, height,
                  (64, 180, 255) if index == 0 else (255, 170, 64))
    with (output / 'report.json').open('x') as f:
        f.write(json.dumps(report, indent=2) + '\n')
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True)
    parser.add_argument('--request-record', type=Path,
                        help='JSON record containing exact 288-byte submitted SVXRViewRequest')
    parser.add_argument('--shader-zip', type=Path, default=DEFAULT_ZIP)
    parser.add_argument('--output', type=Path, required=True)
    a = parser.parse_args()
    try:
        request_record = json.loads(a.request_record.read_text()) if a.request_record else None
        report = qualify(json.loads(a.capture.read_text()), request_record, a.shader_zip, a.output)
    except (ValueError, KeyError, IndexError, OSError, zipfile.BadZipFile) as error:
        print('NOT QUALIFIED:', error, file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())

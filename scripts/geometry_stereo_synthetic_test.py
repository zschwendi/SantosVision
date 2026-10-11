#!/usr/bin/env python3
"""Synthetic plumbing test only; it never certifies a real game draw."""
import json
import hashlib
import struct
import tempfile
import zipfile
from pathlib import Path
from geometry_stereo_proof import (DEFAULT_ZIP, VS_HASH, OLD_VS_HASH, PS_HASH, inverse, mul,
                                   qualify)


def flat(matrix):
    return [matrix[r][c] for c in range(4) for r in range(4)]


def columns(matrix):
    return [[matrix[r][c] for r in range(4)] for c in range(4)]


def camera(x):
    return [[1, 0, 0, x], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]


def fixture(vs_hash=OLD_VS_HASH):
    projection = [[1.5, 0, 0, 0], [0, 1.5, 0, 0],
                  [0, 0, .001, .1001], [0, 0, -1, 0]]
    obj = [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]
    source_cam = camera(-.032)
    local_to_clip = mul(mul(projection, inverse(source_cam)), obj)
    cb = [0.0] * 64
    for first, matrix in ((0, obj), (8, local_to_clip)):
        for i, col in enumerate(columns(matrix)):
            cb[(first + i) * 4:(first + i + 1) * 4] = col
    vertices = [(-.20, -.18, -.6), (.20, -.18, -.6),
                (.20, .18, -.6), (-.20, .18, -.6)]
    vb = b''.join(struct.pack('<3f', *v) for v in vertices)
    ib = struct.pack('<6H', 0, 1, 2, 0, 2, 3)
    with zipfile.ZipFile(DEFAULT_ZIP) as z:
        vs = z.read(next(n for n in z.namelist() if n.endswith(vs_hash + '.wgsl'))).decode()
        ps = z.read(next(n for n in z.namelist() if n.endswith(PS_HASH + '.wgsl'))).decode()
    view = {'request_frame': 17, 'head_pose_frame': 17, 'provenance_epoch': 3,
            'presentation_time_seconds': 1.0, 'eye_index': 0, 'view_count': 2,
            'headset_applied': True, 'depth_reverse_z': True,
            'game_from_eye': flat(source_cam), 'projection_from_eye': flat(projection)}
    payload = struct.pack('<IIQdII', 1, 288, 17, 1.0, 2, 0)
    for x in (-.032, .032):
        payload += struct.pack('<32f', *(flat(camera(x)) + flat(projection)))
    request = {'schema': 1, 'source': 'sv_xr_submit_views',
               'request_bytes_hex': payload.hex(), 'request_sha256': hashlib.sha256(payload).hexdigest(),
               'provenance_epoch': 3, 'head_pose_frame': 17}
    capture = {'schema': 1, 'status': 'captured', 'source': 'Gpu::do_draw', 'synthetic': True,
               'shader': {'vertex_hash': vs_hash, 'pixel_hash': PS_HASH,
                          'vertex_source_wgsl': vs, 'pixel_source_wgsl': ps,
                          'vertex_translated_wgsl': vs, 'pixel_translated_wgsl': ps},
               'world_view': view,
               'viewport': [0, 0, 128, 128, 0, 1],
               'draw': {'kind': 1, 'instances': 1, 'start_instance': 0,
                        'topology': 4, 'count': 6, 'start_index': 0, 'base_vertex': 0},
               'vertex_signature': [{'semantic': 0, 'semantic_index': 0, 'register': 0}],
               'layout': [{'semantic': 0, 'semantic_index': 0, 'format': 6,
                           'slot': 0, 'offset': 0, 'class': 0}],
               'vertex_buffers': [{'slot': 0, 'buffer_id': 1, 'stride': 12, 'offset': 0,
                                   'readback_size': len(vb), 'bytes_hex': vb.hex()}],
               'index_buffer': {'format': 57, 'offset': 0,
                                'readback_size': len(ib), 'bytes_hex': ib.hex()},
               'constant_buffers': ([{'stage': 0, 'binding': 0, 'bytes_hex': '00' * 16}]
                                    if vs_hash == OLD_VS_HASH else [])
                                   + [{'stage': 0, 'binding': 1,
                                       'bytes_hex': struct.pack('<64f', *cb).hex()}]}
    return capture, request


def must_reject(capture, request, reason, expected=None):
    with tempfile.TemporaryDirectory() as tmp:
        try:
            qualify(capture, request, DEFAULT_ZIP, Path(tmp) / 'run', allow_synthetic=True)
        except ValueError as error:
            if expected is not None:
                assert expected in str(error), f'{reason}: unexpected rejection: {error}'
            return
    raise AssertionError('expected rejection: ' + reason)


def main():
    capture, request = fixture()
    capture['request_record'] = {key: value for key, value in request.items() if key != 'request_sha256'}
    with tempfile.TemporaryDirectory() as tmp:
        report = qualify(capture, None, DEFAULT_ZIP, Path(tmp) / 'run', allow_synthetic=True)
        assert report['status'] == 'synthetic-plumbing-pass'
        assert report['scratch_size'] == {'width': 128, 'height': 128,
                                          'source': 'captured_full_viewport'}
        assert report['distinct_coverage_pixels'] >= 2
        assert len(list((Path(tmp) / 'run').glob('eye-*.ppm'))) == 2
        report_path = Path(tmp) / 'run' / 'report.json'
        original = report_path.read_bytes()
        try:
            qualify(capture, request, DEFAULT_ZIP, Path(tmp) / 'run', allow_synthetic=True)
        except FileExistsError:
            pass
        else:
            raise AssertionError('existing output directory was reused')
        assert report_path.read_bytes() == original
    bad = json.loads(json.dumps(capture))
    bad['constant_buffers'][1]['bytes_hex'] = '00' * 256
    must_reject(bad, request, 'stale source camera matrix')
    for viewport, reason, expected in [
            ([0, 0, 128.5, 128, 0, 1], 'fractional extent', 'positive integer pixels'),
            ([1, 0, 128, 128, 0, 1], 'offset origin', 'origin or depth range'),
            ([0, 0, 128, 128, 0.1, 1], 'partial depth range', 'origin or depth range'),
            ([0, 0, float('nan'), 128, 0, 1], 'nonfinite extent', 'nonfinite'),
            ([0, 0, 1025, 128, 0, 1], 'oversize width', 'bounded scratch resolution'),
            ([0, 0, 1024, 1024, 0, 1], 'oversize area', 'bounded scratch resolution')]:
        bad = json.loads(json.dumps(capture))
        bad['viewport'] = viewport
        must_reject(bad, request, reason, expected)
    duplicate = json.loads(json.dumps(request))
    payload = bytearray.fromhex(duplicate['request_bytes_hex'])
    payload[160:288] = payload[32:160]
    duplicate['request_bytes_hex'] = payload.hex()
    duplicate['request_sha256'] = hashlib.sha256(payload).hexdigest()
    must_reject(capture, duplicate, 'same-eye duplicate geometry')
    bad = json.loads(json.dumps(capture))
    bad['shader']['vertex_source_wgsl'] += '\n'
    must_reject(bad, request, 'shader source drift')
    bad = json.loads(json.dumps(capture))
    bad['constant_buffers'][0]['bytes_hex'] = struct.pack('<4f', 1, 0, 0, 0).hex()
    must_reject(bad, request, 'active clip plane')
    for vertex, reason in [((2.0, -.18, -.6), 'side clip'),
                           ((-.2, -.18, .1), 'eye/near clip'),
                           ((-.2, -.18, -200.0), 'far clip')]:
        bad = json.loads(json.dumps(capture))
        raw = bytearray.fromhex(bad['vertex_buffers'][0]['bytes_hex'])
        struct.pack_into('<3f', raw, 0, *vertex)
        bad['vertex_buffers'][0]['bytes_hex'] = raw.hex()
        must_reject(bad, request, reason)
    stale = json.loads(json.dumps(request))
    stale['provenance_epoch'] = 4
    must_reject(capture, stale, 'stale provenance epoch')
    digest_drift = json.loads(json.dumps(request))
    digest_drift['request_sha256'] = '0' * 64
    must_reject(capture, digest_drift, 'raw request digest')
    missing_receipt = json.loads(json.dumps(capture))
    del missing_receipt['request_record']
    must_reject(missing_receipt, None, 'missing consumed request receipt')
    active, active_request = fixture(VS_HASH)
    active['request_record'] = {key: value for key, value in active_request.items()
                                if key != 'request_sha256'}
    assert not any(cb['binding'] == 0 for cb in active['constant_buffers'])
    with tempfile.TemporaryDirectory() as tmp:
        report = qualify(active, None, DEFAULT_ZIP, Path(tmp) / 'active-d', allow_synthetic=True)
        assert report['status'] == 'synthetic-plumbing-pass'
        assert report['draw_shader']['vertex_hash'] == VS_HASH
        assert report['distinct_coverage_pixels'] >= 2
    bad = json.loads(json.dumps(active))
    bad['shader']['vertex_translated_wgsl'] += '\n@builtin(clip_distances)\n'
    must_reject(bad, active_request, 'D variant gained unsupported user clip output')
    bad = json.loads(json.dumps(active))
    bad['shader']['vertex_source_wgsl'] += '\n'
    must_reject(bad, active_request, 'D variant shader source drift')
    bad = json.loads(json.dumps(active))
    bad['shader']['vertex_hash'] = '0' * 16
    must_reject(bad, active_request, 'unsupported VS hash')
    print('PASS: synthetic CPU plumbing for old and active D variants, embedded consumed request, independent eye projection, disparity, clipping, shader contracts, and no-overwrite output; no game draw qualified')


if __name__ == '__main__':
    main()

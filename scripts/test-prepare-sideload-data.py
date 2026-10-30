#!/usr/bin/env python3
"""Synthetic import regressions; no retail files or headset needed."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('prepare', Path(__file__).with_name('prepare-sideload-data.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
WASM = b'\0asm\1\0\0\0'
DLC = b'<Paths>\r\n<Item>env_test</Item>\r\n<Item>main</Item>\r\n</Paths>\r\n'


def fixture(path, extra=None):
    rows = {'data/regular.bin': b'original', 'data/common/data/dlcList.xml': DLC,
            'data/env_test/skipped.bin': b'skip', 'b/test/game.wasm': WASM,
            'b/test/shaders/index.json': b'{"abc":{"ok":true}}',
            'b/test/shaders/abc.wgsl': b'// authored fixture', 'private-extra.txt': b'not game data'}
    rows['data/manifest.json'] = json.dumps({'custom': 'keep', 'files': [
        ['regular.bin', 8, {'keep': True}], ['common/data/dlcList.xml', len(DLC)], ['env_test/skipped.bin', 4]
    ]}).encode()
    rows.update(extra or {})
    with zipfile.ZipFile(path, 'w') as zipped:
        for name, data in rows.items():
            zipped.writestr('package/' + name, data)


class ImportTests(unittest.TestCase):
    def test_data_only_and_dlc_size(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); archive = root / 'game.zip'; output = root / 'game'
            fixture(archive)
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            module.prepare(archive, output, 'test', hashlib.sha256(WASM).hexdigest())
            self.assertFalse((output / 'b/test/game.wasm').exists())
            self.assertFalse((output / 'private-extra.txt').exists())
            self.assertFalse((output / 'data/env_test').exists())
            self.assertEqual((output / 'data/common/data/dlcList.xml').read_bytes(), b'<Paths>\n<Item>main</Item>\n</Paths>\n')
            data = json.loads((output / 'data/manifest.json').read_text())
            self.assertEqual(data['custom'], 'keep')
            self.assertEqual(len(data['files']), 2)
            self.assertEqual(data['files'][0][2], {'keep': True})
            self.assertEqual(data['files'][1][1], (output / 'data/common/data/dlcList.xml').stat().st_size)
            self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)
            with self.assertRaises(ValueError): module.prepare(archive, output)

    def test_unsafe_archive_and_wrong_code(self):
        for extra, build, digest in [({'../escape': b'bad'}, 'test', hashlib.sha256(WASM).hexdigest()),
                                     ({}, 'test', '0' * 64), ({}, 'wrong-build', hashlib.sha256(WASM).hexdigest())]:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp); archive = root / 'game.zip'; output = root / 'game'
                fixture(archive, extra)
                with self.assertRaises(ValueError): module.prepare(archive, output, build, digest)
                self.assertFalse(output.exists())
                self.assertFalse((root / 'escape').exists())


if __name__ == '__main__': unittest.main()

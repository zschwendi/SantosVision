#!/usr/bin/env python3
"""CPU-only wrapper admission tests; the executor subprocess is explicitly mocked."""
import importlib.util
import json
import pathlib
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

PATH = pathlib.Path(__file__).with_name('run.py')
spec = importlib.util.spec_from_file_location('scene_runner_under_test', PATH)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class RunnerIdentity(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='scene-wrapper-cpu-')
        self.root = pathlib.Path(self.temp.name)
        self.binary = self.root / 'mocked-not-executed-binary'
        self.binary.write_bytes(b'CPU test: subprocess is never executed')
        self.native = self.root / 'native-capture.json'
        self.native.write_bytes(b'{"schema":8,"kind":"native_executed_finished_graph"}')
        self.scene = self.root / 'scene.json'
        self.scene.write_text(json.dumps({'provenance': {'kind': 'native_executed_finished_graph', 'source_sha256': runner.digest(self.native.read_bytes())}, 'blobs': []}))
        self.receipt = self.root / 'native-import.json'
        self.receipt.write_text(json.dumps({'capture_sha256': runner.digest(self.native.read_bytes()), 'scene_sha256': runner.digest(self.scene.read_bytes())}))

    def tearDown(self):
        self.temp.cleanup()

    def invoke(self):
        with patch.object(sys, 'argv', ['run.py', str(self.binary), str(self.scene), str(self.root / 'run')]), patch.object(runner.subprocess, 'run', return_value=types.SimpleNamespace(returncode=0)) as call:
            try:
                runner.main()
            finally:
                self.calls = call.call_count

    def test_executed_contract_copies_same_raw_identity(self):
        with self.assertRaises(SystemExit) as result:
            self.invoke()
        self.assertEqual(result.exception.code, 0)
        self.assertEqual(self.calls, 1)  # mocked; no GPU executable ran
        for path in (self.scene, self.native, self.receipt):
            self.assertEqual(path.read_bytes(), (self.root / 'run/input' / path.name).read_bytes())
        receipt = json.loads((self.root / 'run/receipt.json').read_bytes())
        self.assertTrue(receipt['original_and_frozen_inputs_and_sources_unchanged'])
        self.assertFalse(receipt['finished_frame_or_density_accepted'])

    def test_native_identity_mismatch_fails_before_execution(self):
        self.native.write_bytes(b'{"schema":7}')
        with self.assertRaisesRegex(ValueError, 'identity mismatch'):
            self.invoke()
        self.assertEqual(self.calls, 0)

    def test_native_receipt_bound_stays_enforced(self):
        self.receipt.write_bytes(b' ' * ((64 << 10) + 1))
        with self.assertRaisesRegex(ValueError, 'native provenance bound'):
            self.invoke()
        self.assertEqual(self.calls, 0)

    def test_caps_are_finished_specific(self):
        for kind in ('native_finished_graph', 'native_executed_finished_graph'):
            self.assertEqual(runner.payload_bound(kind), 1 << 30)
        for kind in ('native_graph', 'native', 'authored', 'legacy_captured', 'unrecognized'):
            self.assertEqual(runner.payload_bound(kind), 256 << 20)


if __name__ == '__main__':
    unittest.main()

"""CPU-only boundary tests; fake payloads/child processes, never game assets or saves."""
import copy
import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("native_capture_runner", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class RunnerBoundaries(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()

    def tearDown(self):
        self.tmp.cleanup()

    def capture(self):
        p = self.root / "capture"; p.mkdir()
        (p / "payload.bin").write_bytes(b"test material bytes")
        value = {"schema": 2, "kind": "native_draw_span", "status": "captured",
                 "normal_frame_publication_changed": False, "draws": [{}, {}],
                 "closure": {"ordered_two_draws": True, "initial_contents_captured": True,
                     "original_final_captured": True, "natural_submission_completed": True,
                     "unmodelled_commands": 0, "resource_mutation_rejections": 0},
                 "blobs": [{"path": "payload.bin", "byte_length": (p / "payload.bin").stat().st_size,
                            "sha256": runner.digest(p / "payload.bin")}]}
        (p / "capture.json").write_text(json.dumps(value))
        return p, value

    def color_null_capture(self):
        p, value = self.capture()
        value.update(schema=3, kind="native_color_null_draw", world_view=None, draw={"kind": 0, "count": 6})
        del value["draws"]
        value["closure"] = {"single_original_draw": True, "initial_contents_captured": True,
            "original_final_captured": True, "natural_submission_completed": True,
            "queries_inactive": True, "unmodelled_commands": 0, "gpu_errors": 0}
        (p / "capture.json").write_text(json.dumps(value))
        return p, value

    def test_complete_bytes_require_separate_semantic_admission(self):
        p, _ = self.capture()
        result = runner.validate_capture(p)
        self.assertTrue(result["byte_admission_passed"])
        self.assertTrue(result["semantic_replay_admission_required"])

    def test_color_null_capture_allows_null_world_view_but_requires_explicit_kind(self):
        p, _ = self.color_null_capture()
        with self.assertRaisesRegex(ValueError, "selected kind"):
            runner.validate_capture(p)  # Default remains the original two-draw kind.
        result = runner.validate_capture(p, "color-null")
        self.assertEqual(result["capture_kind"], "color-null")
        self.assertTrue(result["byte_admission_passed"])
        self.assertTrue(result["semantic_replay_admission_required"])

    def test_color_null_wrong_schema_kind_or_closure_is_rejected(self):
        p, original = self.color_null_capture()
        invalid = []
        for key, replacement in [("schema", 2), ("kind", "native_draw_span"), ("draw", None), ("draws", [{}]),
                                  ("normal_frame_publication_changed", True)]:
            value = copy.deepcopy(original); value[key] = replacement; invalid.append(value)
        for key in ("single_original_draw", "initial_contents_captured", "original_final_captured",
                    "natural_submission_completed", "queries_inactive"):
            value = copy.deepcopy(original); value["closure"][key] = False; invalid.append(value)
        for key in ("unmodelled_commands", "gpu_errors"):
            for count in (1, False, 0.0, None):
                value = copy.deepcopy(original); value["closure"][key] = count; invalid.append(value)
        for index, value in enumerate(invalid):
            (p / "capture.json").write_text(json.dumps(value))
            with self.subTest(case=index), self.assertRaises(ValueError):
                runner.validate_capture(p, "color-null")

    def test_two_draw_cannot_be_admitted_as_color_null(self):
        p, _ = self.capture()
        with self.assertRaisesRegex(ValueError, "selected kind"):
            runner.validate_capture(p, "color-null")

    def test_color_null_retains_general_payload_byte_admission(self):
        p, _ = self.color_null_capture()
        (p / "payload.bin").write_bytes(b"wrong")
        with self.assertRaisesRegex(ValueError, "payload absent or differs"):
            runner.validate_capture(p, "color-null")

    def test_original_manifest_identity_survives_driver_float_roundtrip(self):
        p = self.root / "capture.json"
        p.write_bytes(b'{"matrix":[-0.014437142759561539,-0.9598827958106995]}')
        driver = {"capture": {"matrix": [-0.01443714275956154, -0.9598827958106996]},
                  "capture_identity": {"sha256": runner.digest(p), "byte_length": p.stat().st_size}}
        self.assertNotEqual(driver["capture"], runner.load_json(p))  # Prior gate falsely rejected this.
        runner.validate_capture_identity(driver, p)

    def test_raw_manifest_identity_rejects_tampering_even_with_equal_json(self):
        p = self.root / "capture.json"; p.write_bytes(b'{"matrix":[1.0]}')
        driver = {"capture": runner.load_json(p),
                  "capture_identity": {"sha256": runner.digest(p), "byte_length": p.stat().st_size}}
        for replacement in (b'{"matrix":[1e0]}', b'{"matrix":[1.0]} '):
            p.write_bytes(replacement)
            self.assertEqual(driver["capture"], runner.load_json(p))
            with self.subTest(raw=replacement), self.assertRaisesRegex(ValueError, "raw manifest identity mismatch"):
                runner.validate_capture_identity(driver, p)

    def test_corrupt_or_truncated_material_rejected(self):
        p, _ = self.capture()
        (p / "payload.bin").write_bytes(b"wrong")
        with self.assertRaisesRegex(ValueError, "payload absent or differs"):
            runner.validate_capture(p)

    def test_duplicate_and_traversing_blob_paths_rejected(self):
        p, original = self.capture()
        for invalid in ["../escape", "/absolute", "a/b", "a\\b", "capture.json", ".."]:
            value = copy.deepcopy(original); value["blobs"][0]["path"] = invalid
            (p / "capture.json").write_text(json.dumps(value))
            with self.subTest(path=invalid), self.assertRaisesRegex(ValueError, "unsafe/duplicate"):
                runner.validate_capture(p)
        original["blobs"] *= 2; (p / "capture.json").write_text(json.dumps(original))
        with self.assertRaisesRegex(ValueError, "unsafe/duplicate"):
            runner.validate_capture(p)

    def test_symlink_payload_rejected(self):
        p, value = self.capture()
        (p / "payload.bin").rename(self.root / "outside.bin")
        (p / "payload.bin").symlink_to(self.root / "outside.bin")
        with self.assertRaisesRegex(ValueError, "without symlinks"):
            runner.validate_capture(p)

    def test_missing_closure_and_extra_output_rejected(self):
        p, value = self.capture()
        for key in ("ordered_two_draws", "initial_contents_captured", "original_final_captured", "natural_submission_completed"):
            changed = copy.deepcopy(value); changed["closure"][key] = False
            (p / "capture.json").write_text(json.dumps(changed))
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "missing capture closure"):
                runner.validate_capture(p)
        (p / "capture.json").write_text(json.dumps(value)); (p / "unexpected.bin").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError, "unmanifested"):
            runner.validate_capture(p)

    def test_rejection_file_overrides_success_manifest(self):
        p, _ = self.capture(); (p / "rejected.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "rejected"):
            runner.validate_capture(p)

    def test_budget_checked_before_hash_or_read(self):
        p, value = self.capture(); value["blobs"][0]["byte_length"] = runner.MAX_BLOBS + 1
        (p / "capture.json").write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "invalid blob size"):
            runner.validate_capture(p)

    def test_new_source_or_changed_binary_invalidates_build(self):
        binary = self.root / "binary"; binary.write_bytes(b"fake executable")
        script = self.root / "runner"; script.write_bytes(b"fake runner")
        source = {"src/main.rs": "original"}
        receipt = {"success": True, "source_unchanged": True, "source_sha256": source,
            "binary_sha256": runner.digest(binary), "binary_bytes": binary.stat().st_size,
            "runner_sha256": runner.digest(script)}
        runner.bound_binary(receipt, source, binary, script)
        with self.assertRaisesRegex(ValueError, "exact full source inventory"):
            runner.bound_binary(receipt, {**source, "src/new.rs": "new"}, binary, script)
        binary.write_bytes(b"different executable")
        with self.assertRaisesRegex(ValueError, "bind executable"):
            runner.bound_binary(receipt, source, binary, script)

    def test_environment_rejects_loader_or_graphics_override(self):
        with patch.dict(os.environ, {"DYLD_INSERT_LIBRARIES": "unwanted", "MUGUET_MUTE": "0", "MUGUET_FSROOT": "/old-save",
                                    "MUGUET_NATIVE_DRAW_CAPTURE_KIND": "color-null",
                                    "MUGUET_NATIVE_TEXTURE_LEDGER": "1", "MUGUET_NATIVE_SHADER_SOURCES": "1",
                                    "MUGUET_XR_SAME_RAY_ALIAS": "1", "MUGUET_XR_EXACT_VIEWPORT_MATCH": "1"}):
            env = runner.child_env(self.root)
        self.assertNotIn("DYLD_INSERT_LIBRARIES", env)
        self.assertNotIn("MUGUET_FSROOT", env)
        self.assertEqual(env["MUGUET_MUTE"], "1")
        self.assertEqual(env["MUGUET_TRANSFER_FROZEN_DEPTH_RECEIPT"], "1")
        self.assertEqual(env["MUGUET_GEOMETRY_STEREO_CAPTURE"], "0")
        self.assertEqual(env["MUGUET_NATIVE_DRAW_CAPTURE_KIND"], "two-draw")
        self.assertEqual(env["MUGUET_NATIVE_TEXTURE_LEDGER"], "0")
        self.assertEqual(env["MUGUET_NATIVE_SHADER_SOURCES"], "0")
        self.assertEqual(env["MUGUET_XR_SAME_RAY_ALIAS"], "0")
        self.assertEqual(env["MUGUET_XR_EXACT_VIEWPORT_MATCH"], "0")
        self.assertEqual(env["HOME"], str(self.root / "host-home"))

    def test_capture_kind_environment_is_an_explicit_bounded_choice(self):
        self.assertEqual(runner.child_env(self.root, "color-null")["MUGUET_NATIVE_DRAW_CAPTURE_KIND"], "color-null")
        for invalid in ("", "unknown", "native_color_null_draw"):
            with self.subTest(kind=invalid), self.assertRaisesRegex(ValueError, "unsupported capture kind"):
                runner.child_env(self.root, invalid)

    def test_scene_provenance_requires_explicit_choice(self):
        env = runner.child_env(self.root, scene_provenance=True)
        self.assertEqual(env["MUGUET_NATIVE_TEXTURE_LEDGER"], "1")
        self.assertEqual(env["MUGUET_NATIVE_SHADER_SOURCES"], "1")
        for invalid in (1, "1", None):
            with self.subTest(value=invalid), self.assertRaisesRegex(ValueError, "explicit boolean"):
                runner.child_env(self.root, scene_provenance=invalid)

    def test_same_ray_alias_requires_explicit_choice(self):
        self.assertEqual(runner.child_env(self.root, same_ray_alias=True)["MUGUET_XR_SAME_RAY_ALIAS"], "1")
        for invalid in (1, "1", None):
            with self.subTest(value=invalid), self.assertRaisesRegex(ValueError, "explicit boolean"):
                runner.child_env(self.root, same_ray_alias=invalid)

    def test_exact_viewport_match_requires_explicit_choice(self):
        self.assertEqual(runner.child_env(self.root, exact_viewport_match=True)["MUGUET_XR_EXACT_VIEWPORT_MATCH"], "1")
        for invalid in (1, "1", None):
            with self.subTest(value=invalid), self.assertRaisesRegex(ValueError, "explicit boolean"):
                runner.child_env(self.root, exact_viewport_match=invalid)

    def test_failed_child_records_failure_and_preserves_logs_without_driver_result(self):
        root = self.root / "failed-run"
        args = argparse.Namespace(run_root=str(root), binary="unused", game_root="unused",
                                  graphics_seed="unused", aot="unused", deadline=1, observe_after_capture=20,
                                  capture_kind="color-null", checkout=str(self.root))
        before = {"schema": 1, "test": "unchanged inputs"}
        lifecycle = {"return_code": -15, "termination_reason": None, "timed_out": False}
        def stopped(command, *unused):
            self.assertEqual(command[-2:], ["20", "color-null"])
            self.assertEqual(unused[-4:], ("color-null", False, False, False))
            runner.write_new(root / "child-lifecycle.json", lifecycle)
            (root / "stderr.log").write_bytes(b"preserve failure evidence")
            return lifecycle
        with patch.object(runner, "run_child", side_effect=stopped), patch.object(runner, "admission", return_value=before):
            with self.assertRaisesRegex(ValueError, "child stopped or failed"):
                runner.execute(args, before)
        receipt = json.loads((root / "receipt.json").read_text())
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual(receipt["capture_kind"], "color-null")
        self.assertFalse(receipt["captured_bytes_accepted"])
        self.assertEqual(receipt["child_lifecycle"]["return_code"], -15)
        self.assertTrue(receipt["original_inputs_unchanged"])
        self.assertEqual((root / "stderr.log").read_bytes(), b"preserve failure evidence")
        self.assertFalse((root / "driver-result.json").exists())

    def test_observation_argument_bounds(self):
        for value in (0, 20, 30):
            self.assertEqual(runner.observation_seconds(str(value)), value)
        for value in ("-1", "31", "0.5", "invalid"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                runner.observation_seconds(value)

    def test_exclusive_receipts_never_replace_existing_file(self):
        p = self.root / "receipt.json"; runner.write_new(p, {"original": True})
        with self.assertRaises(FileExistsError): runner.write_new(p, {"replacement": True})
        self.assertEqual(json.loads(p.read_text()), {"original": True})

    def test_xml_comparison_preserves_text_and_duplicate_structure(self):
        a = self.root / "a.xml"; b = self.root / "b.xml"
        a.write_text('<Settings><source>A</source><graphics><FXAA value="true" /></graphics></Settings>')
        b.write_text('<Settings>\n <source>A</source>\n <graphics>\n <FXAA value="true"/>\n </graphics>\n</Settings>')
        self.assertEqual(runner.xml_values(a), runner.xml_values(b))
        b.write_text(a.read_text().replace('<source>A', '<source>B'))
        self.assertNotEqual(runner.xml_values(a), runner.xml_values(b))
        b.write_text(a.read_text().replace('</graphics>', '<FXAA value="true" /></graphics>'))
        self.assertNotEqual(runner.xml_values(a), runner.xml_values(b))

    def test_owned_cpu_child_reaped_on_outer_deadline(self):
        # Negative allowance makes the outer deadline immediate in this isolated
        # function test. Public CLI admission rejects any such deadline.
        result = runner.run_child([sys.executable, "-c", "import time; time.sleep(30)"], self.root, self.root, -20)
        self.assertTrue(result["timed_out"])
        self.assertIsNotNone(result["return_code"])
        with self.assertRaises(ChildProcessError): os.waitpid(result["pid"], os.WNOHANG)


if __name__ == "__main__":
    unittest.main()

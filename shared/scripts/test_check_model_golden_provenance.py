#!/usr/bin/env python3
import json
from pathlib import Path
import tempfile
import unittest

from check_model_golden_provenance import check


class ModelGoldenProvenanceTests(unittest.TestCase):
    def inspect(self, goldens):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, body in goldens.items():
                (root / name).write_text(body if isinstance(body, str) else json.dumps(body))
            return check(root)

    def test_reference_generator_passes(self):
        self.assertEqual(self.inspect({"a.json": {"generator": "onnxruntime 1.22.0 cpu"}}), [])

    def test_tract_generated_golden_fails(self):
        self.assertTrue(self.inspect({"a.json": {"generator": "tract-onnx 0.23.10 cpu (rebased)"}}))

    def test_missing_or_non_string_generator_fails(self):
        self.assertTrue(self.inspect({"a.json": {}}))
        self.assertTrue(self.inspect({"a.json": {"generator": 7}}))

    def test_malformed_json_fails(self):
        self.assertTrue(self.inspect({"a.json": "{"}))
        self.assertTrue(self.inspect({"a.json": "[]"}))

    def test_one_bad_golden_among_good_ones_fails(self):
        failures = self.inspect({"a.json": {"generator": "onnxruntime 1.22.0 cpu"}, "b.json": {"generator": "tract"}})
        self.assertEqual(len(failures), 1)
        self.assertIn("b.json", failures[0])

    def test_empty_directory_fails(self):
        self.assertTrue(self.inspect({}))

    def test_repository_goldens_pass(self):
        self.assertEqual(check(Path(__file__).resolve().parents[1] / "test-corpus" / "model-golden"), [])


if __name__ == "__main__":
    unittest.main()

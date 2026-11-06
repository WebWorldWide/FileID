#!/usr/bin/env python3
import copy
import json
import unittest

import gen_model_manifest as gen


class GenModelManifestTests(unittest.TestCase):
    def manifest(self) -> dict:
        return json.loads(gen.MANIFEST_PATH.read_text(encoding="utf-8"))

    def test_committed_swift_table_is_current(self) -> None:
        self.assertEqual(gen.main(["--check"]), 0)

    def test_macos_rows_carry_derived_urls_hashes_and_licenses(self) -> None:
        manifest = self.manifest()
        text = gen.generate(manifest)
        for artifact in manifest["artifacts"]:
            url = gen.artifact_url(artifact)
            self.assertEqual(f'url: "{url}"' in text, "macos" in artifact["platforms"], url)
            if "macos" in artifact["platforms"]:
                self.assertIn(f'sha256: "{artifact["sha256"]}"', text)
        for repo in manifest["vlmRepos"]:
            self.assertIn(f'revision: "{repo["revision"]}"', text)
        self.assertIn('"Gemma": ModelLicensePolicy(', text)

    def test_rejects_values_that_would_escape_swift_literals(self) -> None:
        for field, value in [("path", 'model".onnx'), ("path", "model\\(x).onnx"), ("license", "MIT\n")]:
            with self.subTest(field=field, value=value):
                manifest = self.manifest()
                artifact = next(a for a in manifest["artifacts"] if "macos" in a["platforms"])
                artifact[field] = value
                with self.assertRaises(ValueError):
                    gen.generate(manifest)

    def test_rejects_non_positive_or_non_integer_sizes(self) -> None:
        for value in [0, -1, 1.5, True, "10"]:
            with self.subTest(value=value):
                manifest = copy.deepcopy(self.manifest())
                manifest["vlmRepos"][0]["approxBytes"] = value
                with self.assertRaises((ValueError, TypeError)):
                    gen.generate(manifest)

    def test_number_groups_digits(self) -> None:
        self.assertEqual(gen.number(925_600_000), "925_600_000")
        self.assertEqual(gen.number(47_000), "47_000")
        self.assertEqual(gen.number(7), "7")


if __name__ == "__main__":
    unittest.main()

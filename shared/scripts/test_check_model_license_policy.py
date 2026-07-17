#!/usr/bin/env python3
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class ModelLicensePolicyTests(unittest.TestCase):
    def run_checker(self, reviewed_at: str) -> subprocess.CompletedProcess[str]:
        repository = Path(__file__).resolve().parents[2]
        manifest = json.loads(
            (repository / "shared/models/manifest.json").read_text(encoding="utf-8")
        )
        for policy in manifest["licensePolicies"].values():
            policy["reviewedAt"] = reviewed_at
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_text(json.dumps(manifest), encoding="utf-8")
            return subprocess.run(
                [
                    sys.executable,
                    str(repository / "shared/scripts/check_model_license_policy.py"),
                    str(path),
                ],
                capture_output=True,
                text=True,
                check=False,
                cwd=repository,
            )

    def test_valid_calendar_date_passes(self) -> None:
        result = self.run_checker("2026-07-16")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_impossible_month_and_day_fail(self) -> None:
        for invalid in ["2026-99-16", "2026-02-30"]:
            with self.subTest(invalid=invalid):
                result = self.run_checker(invalid)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("valid YYYY-MM-DD calendar date", result.stdout)

    def test_non_date_text_fails(self) -> None:
        result = self.run_checker("not-a-date")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("valid YYYY-MM-DD calendar date", result.stdout)


if __name__ == "__main__":
    unittest.main()

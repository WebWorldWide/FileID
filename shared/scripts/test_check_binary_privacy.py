import tempfile
import unittest
from pathlib import Path

from check_binary_privacy import scan


class BinaryPrivacyTests(unittest.TestCase):
    def test_scans_dll_payloads_for_forbidden_markers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            dll = Path(directory) / "runtime.dll"
            dll.write_bytes(b"prefix HTTPS://INGEST.SENTRY.IO suffix")
            failures = scan([dll])
            self.assertEqual(len(failures), 1)
            self.assertIn("sentry.io", failures[0])

    def test_reports_missing_and_accepts_clean_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            clean = root / "clean.exe"
            clean.write_bytes(b"native payload without network SDK markers")
            self.assertEqual(scan([clean]), [])
            self.assertIn("missing binary", scan([root / "missing.dll"])[0])


if __name__ == "__main__":
    unittest.main()

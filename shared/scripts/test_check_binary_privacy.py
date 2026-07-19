import codecs
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_binary_privacy import FORBIDDEN, scan


class BinaryPrivacyTests(unittest.TestCase):
    def test_scans_every_marker_in_ascii_utf16le_and_utf16be(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for marker in FORBIDDEN:
                text = marker.decode("ascii").upper()
                for encoding in ("ascii", "utf-16le", "utf-16be"):
                    with self.subTest(marker=text, encoding=encoding):
                        binary = root / "runtime.dll"
                        binary.write_bytes(b"\x7f" + text.encode(encoding) + b"\xff")
                        failures = scan([binary])
                        self.assertTrue(any(marker.decode("ascii") in failure for failure in failures))

    def test_utf16_matching_is_bom_and_alignment_independent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = "SENTRY.IO"
            fixtures = (
                b"\x01" + marker.encode("utf-16le"),
                b"\x01" + marker.encode("utf-16be"),
                codecs.BOM_UTF16_LE + marker.encode("utf-16le"),
                codecs.BOM_UTF16_BE + marker.encode("utf-16be"),
            )
            for index, payload in enumerate(fixtures):
                with self.subTest(index=index):
                    binary = root / f"fixture-{index}.exe"
                    binary.write_bytes(payload)
                    self.assertEqual(len(scan([binary])), 1)

    def test_one_marker_reports_once_across_encodings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "runtime.dll"
            marker = "SENTRY.IO"
            binary.write_bytes(
                marker.encode("ascii")
                + marker.encode("utf-16le")
                + marker.encode("utf-16be")
            )
            failures = scan([binary])
            self.assertEqual(failures, [f"{binary}: sentry.io"])

    def test_requires_a_complete_marker(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            clean = root / "clean.exe"
            clean.write_bytes(
                b"native payload without network SDK markers"
                + "sentry.i".encode("utf-16le")
                + "sentry.i".encode("utf-16be")
            )
            self.assertEqual(scan([clean]), [])

    def test_recurses_windows_publish_directories_and_reports_empty_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nested = root / "publish/runtimes/x64/native"
            nested.mkdir(parents=True)
            dll = nested / "runtime.dll"
            dll.write_bytes("POSTHOG.COM".encode("utf-16be"))
            (nested / "notes.txt").write_bytes(b"sentry.io")
            failures = scan([root / "publish"])
            self.assertEqual(failures, [f"{dll}: posthog.com"])
            empty = root / "empty"
            empty.mkdir()
            self.assertIn("no EXE/DLL binaries found", scan([empty])[0])

    def test_reports_missing_and_accepts_clean_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            clean = root / "clean.exe"
            clean.write_bytes(b"native payload without network SDK markers")
            self.assertEqual(scan([clean]), [])
            self.assertIn("missing binary", scan([root / "missing.dll"])[0])

    def test_cli_rejects_utf16le_and_utf16be(self) -> None:
        scanner = Path(__file__).with_name("check_binary_privacy.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for encoding in ("utf-16le", "utf-16be"):
                with self.subTest(encoding=encoding):
                    binary = root / f"telemetry-{encoding}.dll"
                    binary.write_bytes("APPLICATIONINSIGHTS".encode(encoding))
                    result = subprocess.run(
                        [sys.executable, str(scanner), str(binary)],
                        check=False,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("applicationinsights", result.stdout)

    def test_shipping_consumers_use_the_shared_checker(self) -> None:
        root = Path(__file__).resolve().parents[2]
        workflows = (
            ".github/workflows/windows-engine.yml",
            ".github/workflows/windows-app.yml",
            ".github/workflows/macos.yml",
            ".github/workflows/linux.yml",
            ".github/workflows/packaging.yml",
        )
        for relative in workflows:
            with self.subTest(workflow=relative):
                text = (root / relative).read_text(encoding="utf-8")
                self.assertIn("check_binary_privacy.py", text)
                self.assertEqual(text.count("      - 'shared/scripts/check_binary_privacy.py'\n"), 2)
                self.assertEqual(text.count("      - 'shared/scripts/test_check_binary_privacy.py'\n"), 2)
                for stale in ("$forbidden = @(", "forbidden=(", "$ForbiddenTelemetryStrings", "strings -a"):
                    self.assertNotIn(stale, text)

        consumers = (
            ".github/workflows/release.yml",
            ".github/workflows/tools.yml",
            "platforms/apple/scripts/build_dmg.sh",
            "platforms/apple/scripts/release.sh",
            "platforms/linux/build/build.sh",
            "platforms/windows/build/publish-bundle.ps1",
            "platforms/windows/build/iterate.ps1",
            "scripts/package-tools.py",
        )
        for relative in consumers:
            with self.subTest(consumer=relative):
                self.assertIn(
                    "check_binary_privacy.py",
                    (root / relative).read_text(encoding="utf-8"),
                )


if __name__ == "__main__":
    unittest.main()

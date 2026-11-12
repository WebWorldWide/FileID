import codecs
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_binary_privacy import FORBIDDEN, check_ort_runtime, scan, scan_binary_paths

TELEMETRY_GROUP = bytes.fromhex("1a73504fcf898247b3e0dce8c90476ba")


def tracelogging_provider(name: str, group: bytes) -> bytes:
    encoded = name.encode("ascii") + b"\x00"
    traits = b"\x13\x00\x01" + group
    return (2 + len(encoded) + len(traits)).to_bytes(2, "little") + encoded + traits


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
            self.assertIn("no EXE/DLL/dylib/so binaries found", scan([empty])[0])

    def test_rejects_onnxruntime_1ds_uploader_but_not_its_generic_strings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            clean = root / "libonnxruntime.dylib"
            clean.write_bytes(b"Unknown error during telemetry flush\x00 DeviceId: \x00ORT_DISABLE_TELEMETRY")
            self.assertEqual(scan([clean]), [])
            uploader = root / "libonnxruntime.1.30.0.dylib"
            uploader.write_bytes(
                b"https://mobile.events.data.microsoft.com/OneCollector/1.0\x00Microsoft/DeveloperTools/.onnxruntime"
            )
            self.assertEqual(
                scan([uploader]),
                [
                    f"{uploader}: mobile.events.data.microsoft.com",
                    f"{uploader}: onecollector",
                    f"{uploader}: microsoft/developertools/.onnxruntime",
                ],
            )

    def test_rejects_onnxruntime_in_the_windows_telemetry_group(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            official = root / "official" / "onnxruntime.dll"
            official.parent.mkdir()
            official.write_bytes(b"MZ\x00" + tracelogging_provider("Microsoft.ML.ONNXRuntime", TELEMETRY_GROUP))
            failures = scan([official])
            self.assertEqual(len(failures), 1)
            self.assertIn("ONNX Runtime ETW provider joins Microsoft's telemetry group", failures[0])

            source_build = root / "onnxruntime.dll"
            source_build.write_bytes(b"MZ\x00" + tracelogging_provider("Microsoft.ML.ONNXRuntime", bytes(16)))
            self.assertEqual(scan([source_build]), [])

    def test_lists_other_telemetry_group_providers_as_notes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            directml = Path(directory) / "DirectML.dll"
            directml.write_bytes(
                b"MZ\x00" + tracelogging_provider("Microsoft.Windows.AI.MachineLearning.Dml", TELEMETRY_GROUP)
            )
            notes: list[str] = []
            self.assertEqual(scan_binary_paths([directml], notes), [])
            self.assertEqual(
                notes,
                [f"{directml}: TraceLogging provider Microsoft.Windows.AI.MachineLearning.Dml joins Microsoft's telemetry group"],
            )

    def test_group_match_needs_consistent_provider_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "onnxruntime.dll"
            metadata = tracelogging_provider("Microsoft.ML.ONNXRuntime", TELEMETRY_GROUP)
            binary.write_bytes(b"\x00\x00" + metadata[2:])
            self.assertEqual(scan([binary]), [])

    def test_discovers_shared_libraries_in_folders(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("libonnxruntime.dylib", "libonnxruntime.so.1.22.0"):
                (root / name).write_bytes(b"OneCollector")
            self.assertEqual(len(scan([root])), 2)

    def test_ort_runtime_check_reports_unloadable_libraries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertIn("missing ONNX Runtime library", check_ort_runtime(root / "absent.dylib")[0])
            fake = root / "libonnxruntime.dylib"
            fake.write_bytes(b"not a library")
            self.assertIn("could not create an ONNX Runtime environment", check_ort_runtime(fake)[0])

    def test_macos_ci_scans_the_linked_onnxruntime_framework(self) -> None:
        root = Path(__file__).resolve().parents[2]
        text = (root / ".github/workflows/macos.yml").read_text(encoding="utf-8")
        self.assertIn("onnxruntime.xcframework/macos-arm64_x86_64/onnxruntime.framework/Versions/A/onnxruntime", text)
        self.assertIn('.build/release/FileIDEngine "$ort"', text)

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
                text = (root / relative).read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertIn("check_binary_privacy.py", text)
                self.assertEqual(text.count("      - 'shared/scripts/check_binary_privacy.py'\n"), 2)
                self.assertEqual(text.count("      - 'shared/scripts/test_check_binary_privacy.py'\n"), 2)
                for stale in ("$forbidden = @(", "forbidden=(", "$ForbiddenTelemetryStrings", "strings -a"):
                    self.assertNotIn(stale, text)

        consumers = (
            ".github/workflows/release.yml",
            ".github/workflows/tools-verify.yml",
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
                    (root / relative).read_text(encoding="utf-8").replace("\r\n", "\n"),
                )


if __name__ == "__main__":
    unittest.main()

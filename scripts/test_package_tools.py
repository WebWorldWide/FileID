import importlib.util
import shutil
import tempfile
import unittest
import zipfile
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("package-tools.py")
SPEC = importlib.util.spec_from_file_location("package_tools", MODULE_PATH)
assert SPEC and SPEC.loader
package_tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_tools)


class PackageToolsTests(unittest.TestCase):
    def test_staged_dll_privacy_failure_blocks_packaging(self) -> None:
        fixtures = (
            b"embedded https://ingest.sentry.io endpoint",
            "embedded HTTPS://INGEST.SENTRY.IO endpoint".encode("utf-16le"),
            "embedded HTTPS://INGEST.SENTRY.IO endpoint".encode("utf-16be"),
        )
        for index, fixture in enumerate(fixtures):
            with self.subTest(index=index), tempfile.TemporaryDirectory() as directory:
                payload = Path(directory) / "runtime.dll"
                payload.write_bytes(fixture)
                with self.assertRaises(SystemExit):
                    package_tools.scan_staged_payloads(MODULE_PATH.parents[1], [payload])

    def test_archive_payload_must_exactly_match_stage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "FileID-tools-test"
            stage.mkdir()
            (stage / "fileid").write_bytes(b"binary")
            archive = Path(shutil.make_archive(str(root / stage.name), "zip", root, stage.name))
            package_tools.verify_archive_payload(archive, stage)
            with zipfile.ZipFile(archive, "a") as bundle:
                bundle.writestr(f"{stage.name}/unexpected.dll", b"extra")
            with self.assertRaises(SystemExit):
                package_tools.verify_archive_payload(archive, stage)


if __name__ == "__main__":
    unittest.main()

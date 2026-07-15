import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("generate-cargo-sources.py")
SPEC = importlib.util.spec_from_file_location("flatpak_cargo_sources", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class CargoSourceGenerationTests(unittest.TestCase):
    def write_lock(self, text: str) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "Cargo.lock"
        path.write_text(text, encoding="utf-8")
        return path

    def test_registry_package_is_pinned_and_configured_offline(self) -> None:
        lock = self.write_lock(
            '''version = 4
[[package]]
name = "demo"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
'''
        )
        sources = MODULE.generate(lock)
        self.assertEqual(3, len(sources))
        self.assertEqual(
            "https://static.crates.io/crates/demo/demo-1.2.3.crate",
            sources[0]["url"],
        )
        self.assertEqual("a" * 64, sources[0]["sha256"])
        self.assertEqual(
            {"package": "a" * 64, "files": {}},
            json.loads(sources[1]["contents"]),
        )
        self.assertIn('replace-with = "vendored-sources"', sources[2]["contents"])

    def test_unknown_source_fails_closed(self) -> None:
        lock = self.write_lock(
            '''version = 4
[[package]]
name = "demo"
version = "1.2.3"
source = "git+https://example.invalid/demo#0123456789"
'''
        )
        with self.assertRaisesRegex(ValueError, "unsupported Cargo source"):
            MODULE.generate(lock)


if __name__ == "__main__":
    unittest.main()

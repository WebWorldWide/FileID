#!/usr/bin/env python3
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from fingerprint_readonly_tree import fingerprint, write_output_atomically


class FingerprintReadonlyTreeTests(unittest.TestCase):
    def test_stable_and_change_sensitive_without_writing_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "corpus"
            root.mkdir()
            (root / "a.txt").write_text("alpha", encoding="utf-8")
            (root / "nested").mkdir()
            (root / "nested" / "b.bin").write_bytes(b"beta")
            before_names = sorted(str(path.relative_to(root)) for path in root.rglob("*"))
            os.chmod(root, 0o555)
            try:
                first = fingerprint(root, sample_count=2, max_sample_bytes=1024)
                second = fingerprint(root, sample_count=2, max_sample_bytes=1024)
            finally:
                os.chmod(root, 0o755)
            after_names = sorted(str(path.relative_to(root)) for path in root.rglob("*"))
            self.assertEqual(before_names, after_names)
            for key in ["counts", "totalFileBytes", "metadataSha256", "contentSamples", "errors"]:
                self.assertEqual(first[key], second[key])

            (root / "a.txt").write_text("changed", encoding="utf-8")
            changed = fingerprint(root, sample_count=2, max_sample_bytes=1024)
            self.assertNotEqual(first["metadataSha256"], changed["metadataSha256"])
            self.assertNotEqual(first["contentSamples"], changed["contentSamples"])

    def test_root_metadata_changes_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "corpus"
            root.mkdir(mode=0o755)
            before = fingerprint(root, sample_count=0, max_sample_bytes=0)
            os.chmod(root, 0o555)
            try:
                after = fingerprint(root, sample_count=0, max_sample_bytes=0)
            finally:
                os.chmod(root, 0o755)
            self.assertNotEqual(before["metadataSha256"], after["metadataSha256"])

    def test_atomic_output_replaces_symlink_without_following_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / "corpus"
            root.mkdir()
            protected = root / "protected.json"
            protected.write_text("do not overwrite", encoding="utf-8")
            output_dir = base / "output"
            output_dir.mkdir()
            output = output_dir / "fingerprint.json"
            output.symlink_to(protected)

            write_output_atomically(output, root.resolve(strict=True), "safe output\n")

            self.assertEqual(protected.read_text(encoding="utf-8"), "do not overwrite")
            self.assertFalse(output.is_symlink())
            self.assertEqual(output.read_text(encoding="utf-8"), "safe output\n")

    def test_cli_rejects_output_inside_corpus(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "corpus"
            root.mkdir()
            protected = root / "existing.txt"
            protected.write_text("do not overwrite", encoding="utf-8")
            script = Path(__file__).with_name("fingerprint_readonly_tree.py")
            result = subprocess.run(
                [
                    sys.executable,
                    str(script),
                    str(root),
                    "--output",
                    str(protected),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("outside the fingerprinted root", result.stderr)
            self.assertEqual(protected.read_text(encoding="utf-8"), "do not overwrite")


if __name__ == "__main__":
    unittest.main()

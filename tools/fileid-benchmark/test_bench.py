import importlib.util
import os
import tempfile
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("fileid_bench", Path(__file__).with_name("bench.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class VolumeSafetyTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "Windows volume root")
    def test_adlon_is_never_writable_even_without_sampling(self):
        with mock.patch.object(bench, "volume_label", return_value="Adlon"):
            with self.assertRaisesRegex(ValueError, "prohibited"):
                bench.require_safe("H:\\temp")

    @unittest.skipUnless(os.name == "nt", "Windows volume root")
    def test_sample_reads_metadata_only_and_respects_bounds(self):
        first = mock.Mock(path="H:\\file.png")
        first.is_file.return_value = True
        first.is_dir.return_value = False
        second = mock.Mock(path="H:\\other.png")
        second.is_file.return_value = True
        second.is_dir.return_value = False
        iterator = mock.MagicMock()
        iterator.__enter__.return_value = iter((first, second))
        with (mock.patch.object(bench, "volume_label", return_value="Adlon"),
              mock.patch.object(bench.Path, "is_dir", return_value=True),
              mock.patch.object(bench.os, "scandir", return_value=iterator),
              mock.patch("builtins.open", side_effect=AssertionError("content read"))):
            sampled = bench.sample_drive("H:\\", max_entries=1, max_dirs=1)
        self.assertEqual(sampled["volume_label"], "Adlon")
        self.assertEqual(sampled["files"], 1)
        self.assertEqual(sampled["entries"], 1)
        self.assertTrue(sampled["bounded"])
        self.assertFalse(sampled["engine_invoked"])
        first.stat.assert_called_once_with(follow_symlinks=False)
        second.stat.assert_not_called()
        self.assertNotIn("file.png", str(sampled))

    @unittest.skipUnless(os.name == "nt", "Windows reparse attributes")
    def test_model_junction_is_rejected_before_engine_launch(self):
        link = mock.Mock()
        link.is_symlink.return_value = False
        link.stat.return_value.st_file_attributes = 0x400
        listing = mock.MagicMock()
        listing.__enter__.return_value = iter((link,))
        with (mock.patch.object(bench.Path, "exists", return_value=True),
              mock.patch.object(bench.os, "scandir", return_value=listing)):
            with self.assertRaisesRegex(ValueError, "junction"):
                bench.require_plain_models(Path("C:\\models"))
        link.stat.assert_called_once_with(follow_symlinks=False)

    @unittest.skipUnless(os.name == "nt", "Windows volume root")
    def test_missing_drive_fails_explicitly(self):
        with mock.patch.object(bench.Path, "is_dir", return_value=False):
            with self.assertRaisesRegex(ValueError, "unavailable"):
                bench.sample_drive("H:\\", max_entries=1, max_dirs=1)


    @unittest.skipUnless(os.name == "posix", "Linux mount identity")
    def test_linux_models_support_plain_files_and_reject_symlinks(self):
        with tempfile.TemporaryDirectory(prefix="fileid-model-safety-") as temp:
            root = Path(temp)
            (root / "plain.onnx").write_bytes(b"fixture")
            bench.require_safe(root)
            bench.require_plain_models(root)
            (root / "redirected").symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                bench.require_plain_models(root)

    @unittest.skipUnless(os.name == "posix", "Linux mount identity")
    def test_linux_rejects_same_mount_and_unknown_external_mount(self):
        with tempfile.TemporaryDirectory(prefix="fileid-mount-safety-") as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ValueError, "different volume"):
                bench.require_safe(root / "library", forbidden=root)
            with mock.patch.object(bench, "linux_mount", return_value=(Path("/unknown"), "exfat")):
                with mock.patch.object(bench, "volume_label", return_value=None):
                    with self.assertRaisesRegex(ValueError, "Unknown external mount"):
                        bench.require_safe(root / "library")

if __name__ == "__main__":
    unittest.main()

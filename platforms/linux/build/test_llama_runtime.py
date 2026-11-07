#!/usr/bin/env python3
"""Pins for the bundled llama.cpp runtime agree everywhere, and the probe's checksum logic holds."""

from __future__ import annotations

import hashlib
import importlib.util
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
BUILDER = ROOT / "platforms/linux/build/build-llama-runtime.sh"
PROBE = ROOT / "platforms/linux/build/probe_llama_runtime.py"
FLATPAK = ROOT / "packaging/flatpak/io.github.fileid.FileID.yaml"
APPIMAGE = ROOT / "packaging/appimage/build-appimage.sh"


def constant(text: str, name: str) -> str:
    match = re.search(rf"^readonly {name}=(\S+)$", text, re.M)
    assert match, name
    return match.group(1)


def load_probe():
    spec = importlib.util.spec_from_file_location("probe_llama_runtime", PROBE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Pins(unittest.TestCase):
    def setUp(self) -> None:
        self.builder = BUILDER.read_text(encoding="utf-8")
        self.commit = constant(self.builder, "LLAMA_COMMIT")
        self.sha = constant(self.builder, "LLAMA_ARCHIVE_SHA256")
        self.build = constant(self.builder, "LLAMA_BUILD")
        self.url = f"https://codeload.github.com/ggml-org/llama.cpp/tar.gz/{self.commit}"

    def test_pins_are_full_length(self) -> None:
        self.assertRegex(self.commit, r"^[0-9a-f]{40}$")
        self.assertRegex(self.sha, r"^[0-9a-f]{64}$")
        self.assertRegex(self.build, r"^[0-9]+$")

    def test_flatpak_source_matches_builder(self) -> None:
        text = FLATPAK.read_text(encoding="utf-8")
        match = re.search(r"url: (https://codeload\.github\.com/ggml-org/llama\.cpp/\S+)\n\s+sha256: ([0-9a-f]{64})", text)
        self.assertIsNotNone(match)
        self.assertEqual(match.group(1), self.url)
        self.assertEqual(match.group(2), self.sha)
        self.assertIn(f"llama.cpp-b{self.build}.tar.gz", text)
        self.assertIn("--output /app/lib/fileid/llama.cpp", text)
        module = text.split("- name: llama-runtime", 1)[1].split("- name: fileid", 1)[0]
        self.assertIn("no-debuginfo: true", module)

    def test_appimage_source_matches_builder(self) -> None:
        text = APPIMAGE.read_text(encoding="utf-8")
        self.assertIn(f'"{self.url}"', text)
        self.assertIn(f'"{self.sha}"', text)
        self.assertIn("usr/bin/RuntimeBundles/llama.cpp", text)

    def test_probe_expects_builder_version(self) -> None:
        probe = load_probe()
        self.assertEqual(probe.BUILD, self.build)
        self.assertEqual(probe.COMMIT, self.commit)

    def test_patch_touches_only_pinned_files(self) -> None:
        patch = ROOT / f"platforms/linux/build/patches/llama-b{self.build}-no-outbound.patch"
        touched = set(re.findall(r"^\+\+\+ b/(\S+)$", patch.read_text(encoding="utf-8"), re.M))
        patched_block = self.builder.split("no-outbound patch did not apply cleanly", 1)[1].split("EOF", 2)[1]
        pinned = set(re.findall(r"^[0-9a-f]{64}  (\S+)$", patched_block, re.M))
        self.assertEqual(touched, pinned)
        self.assertEqual(touched, {"common/http.h", "tools/server/server-models.cpp", "vendor/cpp-httplib/httplib.cpp"})

    def test_builder_never_downloads(self) -> None:
        self.assertNotRegex(self.builder, r"\b(curl|wget|git clone|FetchContent_Populate)\b")
        for option in ("-DLLAMA_OPENSSL=OFF", "-DLLAMA_BUILD_UI=OFF", "-DLLAMA_USE_PREBUILT_UI=OFF",
                       "-DGGML_RPC=OFF", "-DFETCHCONTENT_FULLY_DISCONNECTED=ON", "-DLLAMA_LLGUIDANCE=OFF"):
            self.assertIn(option, self.builder)


class ProbeChecksums(unittest.TestCase):
    def setUp(self) -> None:
        self.probe = load_probe()
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        (self.root / "llama-server").write_bytes(b"server")
        (self.root / "libllama.so.0.0.1").write_bytes(b"library")
        os.symlink("libllama.so.0.0.1", self.root / "libllama.so.0")
        self.write_sums()

    def tearDown(self) -> None:
        self.dir.cleanup()

    def write_sums(self) -> None:
        lines = [
            f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}"
            for p in sorted(self.root.iterdir())
            if p.is_file() and not p.is_symlink() and p.name != "SHA256SUMS"
        ]
        (self.root / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")

    def test_matching_runtime_passes(self) -> None:
        self.assertEqual(self.probe.check_sums(self.root), [])

    def test_modified_file_fails(self) -> None:
        (self.root / "llama-server").write_bytes(b"other")
        self.assertTrue(self.probe.check_sums(self.root))

    def test_unlisted_file_fails(self) -> None:
        (self.root / "extra.so").write_bytes(b"x")
        self.assertTrue(self.probe.check_sums(self.root))

    def test_link_leaving_runtime_fails(self) -> None:
        os.symlink("/etc/hostname", self.root / "libggml.so")
        self.assertTrue(self.probe.check_sums(self.root))


if __name__ == "__main__":
    sys.exit(unittest.main())

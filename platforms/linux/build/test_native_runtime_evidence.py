#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BUILD = Path(__file__).resolve().parent
REPO = BUILD.parents[2]
spec = importlib.util.spec_from_file_location('evidence', BUILD / 'native-runtime-evidence.py')
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


class NativeRuntimeEvidenceTests(unittest.TestCase):
    def test_source_license_and_patch_identities_are_exact(self):
        pins = json.loads((REPO / 'shared/models/native-runtime-sources.json').read_text())
        self.assertEqual(pins['llama']['commit'], 'e947228222147356bc7e64154d3439e142481632')
        self.assertEqual(pins['whisper']['commit'], '86c40c3bd6fc86f1187fb751d111b49e0fc18e84')
        for name in ('llama', 'whisper'):
            self.assertEqual(pins[name]['license'], 'MIT')
            self.assertEqual(len(pins[name]['archiveSha256']), 64)
        evidence.verify(REPO / pins['llama']['patch'], pins['llama']['patchSha256'])
        self.assertEqual(set(pins['llama']['patchedFiles']), set(pins['llama']['upstreamFiles']))

    def test_missing_cached_archive_never_acquires_or_extracts(self):
        with tempfile.TemporaryDirectory(prefix='fileid-native-missing-source-') as scratch:
            result = subprocess.run([sys.executable, str(BUILD / 'native-runtime-evidence.py'), 'prepare', str(REPO), scratch], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('acquisition is disabled', result.stderr)
            self.assertEqual(list(Path(scratch).iterdir()), [])

    def test_headless_graph_rejects_ui_fallback_and_install_rules(self):
        pins = json.loads((REPO / 'shared/models/native-runtime-sources.json').read_text())
        with tempfile.TemporaryDirectory(prefix='fileid-native-graph-') as scratch:
            root = Path(scratch)
            common = {'GGML_CUDA': 'OFF', 'GGML_VULKAN': 'OFF', 'GGML_NATIVE': 'OFF', 'BUILD_SHARED_LIBS': 'OFF'}
            for name in ('llama', 'whisper'):
                build = root / (name + '-build')
                (build / 'CMakeFiles').mkdir(parents=True)
                flags = dict(common)
                if name == 'llama':
                    flags.update(LLAMA_OPENSSL='OFF', LLAMA_BUILD_NUMBER='9254', LLAMA_BUILD_COMMIT=pins[name]['commit'], LLAMA_BUILD_UI='OFF', LLAMA_USE_PREBUILT_UI='OFF', LLAMA_LLGUIDANCE='OFF', LLAMA_BUILD_BORINGSSL='OFF', LLAMA_BUILD_LIBRESSL='OFF', FETCHCONTENT_FULLY_DISCONNECTED='ON')
                else:
                    flags.update(WHISPER_CURL='OFF', WHISPER_BUILD_SERVER='OFF')
                (build / 'CMakeCache.txt').write_text(''.join(f'{key}:STRING={value}\n' for key, value in flags.items()))
                (build / 'build.ninja').write_text('build native: phony\n')
                (build / 'CMakeFiles/rules.ninja').write_text('rule CXX_COMPILER\n')
            self.assertFalse(evidence.check_build_graph(root, pins)['uiProvisioningTargets'])
            graph = root / 'llama-build/build.ninja'
            graph.write_text('COMMAND = npm install\n')
            with self.assertRaisesRegex(RuntimeError, 'provisioning'):
                evidence.check_build_graph(root, pins)
            graph.write_text('build native: phony\n')
            cache = root / 'llama-build/CMakeCache.txt'
            cache.write_text(cache.read_text().replace('LLAMA_USE_PREBUILT_UI:STRING=OFF', 'LLAMA_USE_PREBUILT_UI:STRING=ON'))
            with self.assertRaisesRegex(RuntimeError, 'configuration'):
                evidence.check_build_graph(root, pins)

    def test_unexpected_source_archive_fails_before_fetch_or_extract(self):
        with tempfile.TemporaryDirectory(prefix='fileid-native-bad-source-') as scratch:
            root = Path(scratch)
            (root / 'llama.tar.gz').write_bytes(b'not the pinned official source')
            result = subprocess.run([sys.executable, str(BUILD / 'native-runtime-evidence.py'), 'prepare', str(REPO), scratch], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Pinned SHA256 mismatch', result.stderr)
            self.assertFalse((root / 'llama-source').exists())
            self.assertFalse((root / 'whisper.tar.gz').exists())


if __name__ == '__main__':
    unittest.main()

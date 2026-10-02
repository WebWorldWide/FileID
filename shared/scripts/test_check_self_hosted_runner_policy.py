#!/usr/bin/env python3
from pathlib import Path
import tempfile
import unittest

from check_self_hosted_runner_policy import ROUTES, check


class RunnerPolicyTests(unittest.TestCase):
    def inspect(self, name, source):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / name).write_text(source)
            return check(root)

    def source(self, name):
        return "on:\n  pull_request:\njobs:\n  build:\n    runs-on: ${{ " + ROUTES[name] + " }}\n"

    def test_reviewed_routes(self):
        for name in ROUTES:
            with self.subTest(name=name):
                self.assertEqual(self.inspect(name, self.source(name)), [])

    def test_missing_main_guard(self):
        source = self.source("linux.yml").replace("github.ref == 'refs/heads/main' && ", "")
        self.assertTrue(self.inspect("linux.yml", source))

    def test_missing_pull_request_guard(self):
        source = self.source("linux.yml").replace(" && github.event_name != 'pull_request'", "")
        self.assertTrue(self.inspect("linux.yml", source))

    def test_unguarded_literal(self):
        self.assertTrue(self.inspect("linux.yml", "jobs:\n  build:\n    runs-on: [self-hosted, Linux, fileid-adlon]\n"))

    def test_privileged_fork_event(self):
        source = self.source("linux.yml").replace("  pull_request:", "  pull_request_target:")
        self.assertTrue(self.inspect("linux.yml", source))

    def test_multiline_literal(self):
        self.assertTrue(self.inspect("linux.yml", "jobs:\n  build:\n    runs-on:\n      - self-hosted\n      - Linux\n"))

    def test_unreviewed_workflow(self):
        self.assertTrue(self.inspect("other.yml", self.source("linux.yml")))

    def test_native_arm_stays_hosted(self):
        source = self.source("windows-engine.yml").replace("matrix.label != 'arm64-native' && ", "")
        self.assertTrue(self.inspect("windows-engine.yml", source))

    def test_hosted_workflow(self):
        self.assertEqual(self.inspect("other.yml", "jobs:\n  build:\n    runs-on: ubuntu-latest\n"), [])


if __name__ == "__main__":
    unittest.main()

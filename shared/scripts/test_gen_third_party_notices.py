#!/usr/bin/env python3
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_third_party_notices as notices  # noqa: E402


class ElectionTests(unittest.TestCase):
    def test_prefers_mit_from_dual_licence(self):
        self.assertEqual(notices.elect("Apache-2.0 OR MIT"), ["MIT"])
        self.assertEqual(notices.elect("MIT/Apache-2.0"), ["MIT"])

    def test_and_expression_needs_every_licence(self):
        self.assertEqual(notices.elect("Apache-2.0 AND ISC"), ["Apache-2.0", "ISC"])
        self.assertEqual(notices.elect("(MIT OR Apache-2.0) AND Unicode-3.0"), ["MIT", "Unicode-3.0"])

    def test_copyleft_without_permissive_alternative_is_rejected(self):
        self.assertIsNone(notices.elect("GPL-3.0-only"))
        self.assertIsNone(notices.elect("LGPL-2.1-or-later"))
        self.assertIsNone(notices.elect("MIT AND AGPL-3.0-only"))
        self.assertIsNone(notices.elect("EPL-2.0 OR CDDL-1.0"))

    def test_copyleft_alternative_is_avoided_when_permissive_exists(self):
        self.assertEqual(notices.elect("GPL-2.0-only OR MIT"), ["MIT"])
        self.assertEqual(notices.elect("MIT OR MPL-2.0"), ["MIT"])

    def test_mpl_alone_is_allowed_with_its_note(self):
        self.assertEqual(notices.elect("MPL-2.0"), ["MPL-2.0"])
        self.assertIn("MPL-2.0", notices.COPYLEFT_NOTES)

    def test_with_exception_stays_one_licence(self):
        self.assertEqual(notices.elect("Apache-2.0 WITH LLVM-exception"), ["Apache-2.0 WITH LLVM-exception"])


class RepositoryTests(unittest.TestCase):
    def test_committed_notices_pass_the_gate(self):
        self.assertEqual(notices.check(), [])

    def test_unlisted_lock_crate_fails(self):
        real = notices.lock_packages
        with mock.patch.object(notices, "lock_packages", lambda directory: real(directory) | {("unlisted-crate", "9.9.9")}):
            failures = notices.check()
        self.assertTrue(any("unlisted-crate" in failure or "not in rust-crates.json" in failure for failure in failures))

    def test_unlisted_swift_package_fails(self):
        real = notices.resolved_swift
        with mock.patch.object(notices, "resolved_swift", lambda: {**real(), "new-package": "1.0.0"}):
            self.assertTrue(any("new-package" in failure for failure in notices.check()))

    def test_unlisted_nuget_package_fails(self):
        real = notices.declared_nuget
        with mock.patch.object(notices, "declared_nuget", lambda: {**real(), "Some.New.Package": "1.0.0"}):
            self.assertTrue(any("Some.New.Package" in failure for failure in notices.check()))

    def test_disallowed_licence_fails(self):
        crates = notices.load_crates()
        crates[0] = {**crates[0], "license": "GPL-3.0-only", "elected": []}
        with mock.patch.object(notices, "load_crates", lambda: crates):
            self.assertTrue(any("not on the allowlist" in failure for failure in notices.check()))

    def test_stale_notices_file_fails(self):
        with mock.patch.object(notices, "generate", lambda: "different"):
            self.assertTrue(any("out of date" in failure for failure in notices.check()))

    def test_notices_include_model_terms_and_copyleft_note(self):
        text = notices.NOTICES.read_text(encoding="utf-8")
        self.assertIn("https://ai.google.dev/gemma/terms", text)
        self.assertIn("Note on MPL-2.0", text)
        self.assertIn("Apache License", text)


if __name__ == "__main__":
    unittest.main()

import tempfile
import unittest
from pathlib import Path

from check_workflow_permissions import violations


class WorkflowPermissionTests(unittest.TestCase):
    def test_rejects_top_level_and_unapproved_job_write(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bad.yml").write_text(
                "permissions:\n"
                "  contents: write\n"
                "jobs:\n"
                "  build:\n"
                "    permissions:\n"
                "      packages: write\n"
                "    steps: []\n",
                encoding="utf-8",
            )
            failures = violations(root)
            self.assertTrue(any("top-level" in failure for failure in failures))
            self.assertTrue(any("unapproved" in failure for failure in failures))

    def test_rejects_scalar_write_all(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bad.yml").write_text(
                "permissions:\n"
                "  contents: read\n"
                "jobs:\n"
                "  build:\n"
                "    permissions: write-all\n"
                "    steps: []\n",
                encoding="utf-8",
            )
            self.assertTrue(any("scalar/flow permissions" in failure for failure in violations(root)))

    def test_rejects_noncanonical_job_declarations(self) -> None:
        fixtures = [
            "  build: {runs-on: ubuntu-latest, permissions: write-all, steps: []}\n",
            "  attacker: # valid YAML, forbidden canonical form\n"
            "    permissions:\n"
            "      contents: write\n"
            "    steps: []\n",
            "  'quoted-job':\n"
            "    permissions:\n"
            "      contents: write\n"
            "    steps: []\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "bad.yml").write_text(
                    "permissions:\n  contents: read\njobs:\n" + fixture,
                    encoding="utf-8",
                )
                self.assertTrue(any("job declarations" in failure for failure in violations(root)))

    def test_accepts_only_isolated_pages_and_release_publishers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "pages.yml").write_text(
                "permissions:\n"
                "  contents: read\n"
                "jobs:\n"
                "  build:\n"
                "    steps: []\n"
                "  deploy:\n"
                "    needs: build\n"
                "    permissions:\n"
                "      pages: write\n"
                "      id-token: write\n"
                "    steps: []\n",
                encoding="utf-8",
            )
            (root / "release.yml").write_text(
                "permissions:\n"
                "  contents: read\n"
                "jobs:\n"
                "  windows-release:\n"
                "    steps: []\n"
                "  publish-release:\n"
                "    needs: windows-release\n"
                "    if: needs.windows-release.outputs.publish == 'true'\n"
                "    permissions:\n"
                "      contents: write\n"
                "    steps: []\n",
                encoding="utf-8",
            )
            self.assertEqual(violations(root), [])

    def test_release_write_requires_real_exact_job_gate(self) -> None:
        spoofed = [
            "    # if: needs.windows-release.outputs.publish == 'true'\n",
            "    env:\n"
            "      SPOOF: \"if: needs.windows-release.outputs.publish == 'true'\"\n",
            "    if: always()\n",
        ]
        for spoof in spoofed:
            with self.subTest(spoof=spoof), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "release.yml").write_text(
                    "permissions:\n"
                    "  contents: read\n"
                    "jobs:\n"
                    "  publish-release:\n"
                    + spoof
                    + "    permissions:\n"
                    "      contents: write\n"
                    "    steps: []\n",
                    encoding="utf-8",
                )
                self.assertTrue(any("exact job gate" in failure for failure in violations(root)))


if __name__ == "__main__":
    unittest.main()

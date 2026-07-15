import tempfile
import unittest
from pathlib import Path

from check_workflow_action_pins import violations


class WorkflowActionPinTests(unittest.TestCase):
    def test_rejects_mutable_external_ref(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bad.yml").write_text(
                "jobs:\n  test:\n    steps:\n      - uses: actions/checkout@v4\n",
                encoding="utf-8",
            )
            self.assertEqual(len(violations(root)), 1)

    def test_accepts_commit_pins_local_actions_and_docker_images(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "good.yml").write_text(
                "jobs:\n"
                "  test:\n"
                "    steps:\n"
                "      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5\n"
                "      - uses: ./.github/actions/local\n"
                "      - uses: docker://alpine:3.22\n"
                "      - uses: owner/repo/.github/workflows/reuse.yml@0123456789abcdef0123456789abcdef01234567\n",
                encoding="utf-8",
            )
            self.assertEqual(violations(root), [])


if __name__ == "__main__":
    unittest.main()

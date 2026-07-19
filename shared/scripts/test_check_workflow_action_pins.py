import tempfile
import unittest
from pathlib import Path

from check_workflow_action_pins import violations


COMMIT = "0123456789abcdef0123456789abcdef01234567"
DIGEST = "a" * 64


class WorkflowActionPinTests(unittest.TestCase):
    def check(self, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "workflow.yml").write_text(text, encoding="utf-8")
            return violations(root)

    def workflow(self, entries: str) -> str:
        return f"permissions:\n  contents: read\njobs:\n  test:\n    steps:\n{entries}"

    def test_accepts_immutable_remote_local_docker_and_reusable_references(self) -> None:
        text = (
            "permissions:\n  contents: read\njobs:\n"
            f"  reusable:\n    uses: owner/repo/.github/workflows/reuse.yml@{COMMIT}\n"
            "  local-reusable:\n    uses: ./.github/workflows/local.yaml\n"
            "  test:\n    steps:\n"
            f"      - uses: actions/checkout@{COMMIT} # v4\n"
            f"      - uses: owner/repo/sub/action@{COMMIT}\n"
            "      - uses: ./.github/actions/local\n"
            f"      - uses: docker://registry.example:5000/team/image:3.22@sha256:{DIGEST}\n"
            "      - name: Looks like policy text\n"
            "        run: |\n"
            "          echo 'uses: actions/checkout@v4'\n"
            "          echo '{ uses: owner/repo@main }'\n"
            '      - name: "\\\\\\\\" # { uses: actions/checkout@main }\n'
            "      - name: ordinary\n"
            "        with:\n"
            "          uses: display-only\n"
            "          container: display-only\n"
            "      - run: \"echo { uses: actions/checkout@main }\"\n"
            "      - run: |2-\n"
            "          uses: actions/checkout@main\n"
            "    strategy:\n"
            "      matrix:\n"
            "        steps:\n"
            "          - uses: display-only\n"
        )
        self.assertEqual(self.check(text), [])

    def test_rejects_mutable_and_malformed_remote_references(self) -> None:
        bad = [
            "actions/checkout@v4",
            "actions/checkout@main",
            f"actions/checkout@{COMMIT[:-1]}",
            f"actions/checkout@{COMMIT}0",
            f"actions/checkout@{COMMIT.upper()}",
            f"actions//checkout@{COMMIT}",
            f"actions/../checkout@{COMMIT}",
            "${{ matrix.action }}",
            "https://github.com/actions/checkout",
            f"evil/action@{COMMIT}#branch",
            f"evil/action@{COMMIT}\u00a0#mutable",
            f"evil/action@{COMMIT}\u2003#mutable",
            f"evil/action@{COMMIT}\u00a0",
            f"evil/action@{COMMIT}\u00a0 # comment",
        ]
        for reference in bad:
            with self.subTest(reference=reference):
                failures = self.check(self.workflow(f"      - uses: {reference}\n"))
                self.assertTrue(failures)

    def test_rejects_quoted_spaced_flow_explicit_and_indirect_uses_keys(self) -> None:
        bad_lines = [
            "      - 'uses': actions/checkout@v4\n",
            '      - "uses": actions/checkout@v4\n',
            '      - "u\\u0073es": actions/checkout@v4\n',
            "      - uses : actions/checkout@v4\n",
            "      -  uses: actions/checkout@main\n",
            "      - name : ordinary\n        uses: actions/checkout@main\n",
            "      - uses:actions/checkout@v4\n",
            "      - ? uses: actions/checkout@v4\n",
            "      - ? uses\n        : actions/checkout@v4\n",
            "      - !!str uses: actions/checkout@v4\n",
            "      - ! uses: actions/checkout@main\n",
            "      - !<tag:yaml.org,2002:map> uses: actions/checkout@main\n",
            "      - !\n        uses: actions/checkout@main\n",
            "      - { uses: actions/checkout@v4 }\n",
            "      -  { uses: actions/checkout@main }\n",
            "    steps: [{ uses: actions/checkout@v4 }]\n",
            "      - &shared\n        uses: actions/checkout@v4\n",
            "      - name: ordinary\n        &u uses: actions/checkout@main\n",
            "      - *shared\n",
            "      - <<: *shared\n",
            "      - << : *shared\n",
            "      - name: |\n          harmless name\n        uses: actions/checkout@main\n",
            "      - uses: >-\n          actions/checkout@main\n",
            "      - {\n        uses: actions/checkout@main\n        }\n",
            "      - name: ordinary\n      [ { ? uses: actions/checkout@main } ]\n",
            "      - &shared { &u uses: actions/checkout@main }\n        &m <<: *shared\n",
            "  attack: { runs-on: ubuntu-latest, steps: [ { uses: actions/checkout@main } ] }\n",
            "  attack: { uses: owner/repo/.github/workflows/reuse.yml@main }\n",
            "--- {jobs: {attack: {steps: [{uses: actions/checkout@main}]}}}\n",
            "  test:\n    services:\n      db: { image: postgres:latest }\n    steps:\n      - uses: ./action\n",
        ]
        for line in bad_lines:
            with self.subTest(line=line):
                self.assertTrue(self.check(self.workflow(line)))

    def test_rejects_mutable_or_invalid_docker_references(self) -> None:
        bad = [
            "docker://alpine",
            "docker://alpine:3.22",
            "docker://alpine:latest",
            f"docker://alpine@sha512:{DIGEST}",
            f"docker://alpine@sha256:{DIGEST[:-1]}",
            f"docker://alpine@sha256:{DIGEST.upper()}",
            f"docker://alpine@@sha256:{DIGEST}",
        ]
        for reference in bad:
            with self.subTest(reference=reference):
                self.assertTrue(self.check(self.workflow(f"      - uses: {reference}\n")))

    def test_rejects_wrong_reusable_workflow_context_and_refs(self) -> None:
        bad = [
            "  test:\n    uses: owner/repo/.github/workflows/reuse.yml@main\n",
            f"  test:\n    uses: owner/repo/action@{COMMIT}\n",
            "  test:\n    uses: ./outside/reuse.yml\n",
            f"  test:\n    steps:\n      - uses: owner/repo/.github/workflows/reuse.yml@{COMMIT}\n",
            f"  test:\n    uses: docker://alpine@sha256:{DIGEST}\n",
            f"  test:\n    uses: ../repo/.github/workflows/reuse.yml@{COMMIT}\n",
            f"  test:\n    uses: owner/../.github/workflows/reuse.yml@{COMMIT}\n",
        ]
        for jobs in bad:
            with self.subTest(jobs=jobs):
                self.assertTrue(self.check(f"permissions:\n  contents: read\njobs:\n{jobs}"))

    def test_container_and_service_images_require_digests(self) -> None:
        good = (
            "permissions:\n  contents: read\nenv:\n  image: display-only\njobs:\n  test:\n"
            f"    container: ubuntu:24.04@sha256:{DIGEST}\n"
            f"    services:\n      db:\n        image: postgres:17@sha256:{DIGEST}\n"
            "        env:\n          image: display-only\n"
            "    steps:\n      - uses: ./action\n"
            "        with:\n          image: display-only\n"
            "      - run: echo foo{bar,baz}\n"
            "  mapped:\n"
            f"    container:\n      image: ubuntu:24.04@sha256:{DIGEST}\n"
            "    steps:\n      - uses: ./action\n"
            "        with:\n          services:\n            image: display-only\n"
        )
        self.assertEqual(self.check(good), [])
        for image in ("ubuntu:latest", "postgres:17", f"ubuntu@sha512:{DIGEST}", "${{ matrix.image }}"):
            with self.subTest(image=image):
                bad = (
                    "permissions:\n  contents: read\njobs:\n  test:\n"
                    f"    container: {image}\n"
                    "    steps:\n      - uses: ./action\n"
                )
                self.assertTrue(self.check(bad))
        block_images = (
            "permissions:\n  contents: read\njobs:\n  test:\n"
            "    container:\n      image: >-\n        ubuntu:latest\n"
            "    steps:\n      - uses: ./action\n"
        )
        self.assertTrue(self.check(block_images))
        continued_container = (
            "permissions:\n  contents: read\njobs:\n  test:\n"
            "    container:\n      ubuntu:latest\n"
            "    steps:\n      - uses: ./action\n"
        )
        self.assertTrue(self.check(continued_container))

    def test_rejects_noncanonical_ancestor_keys_and_utf8_bom(self) -> None:
        bad = [
            f"permissions:\n  contents: read\njobs :\n  test:\n    steps:\n      - uses: actions/checkout@main\n",
            f"permissions:\n  contents: read\njobs:\n  test :\n    uses: owner/repo/.github/workflows/reuse.yml@main\n",
            f"permissions:\n  contents: read\njobs:\n  test:\n    services :\n      db:\n        image: postgres:latest\n    steps:\n      - uses: ./action\n",
        ]
        for text in bad:
            with self.subTest(text=text):
                self.assertTrue(self.check(text))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bom.yml").write_bytes(
                ("\ufeffjobs:\n  test:\n    steps:\n      - uses: actions/checkout@main\n").encode("utf-8")
            )
            self.assertTrue(violations(root))

    def test_rejects_invalid_local_paths_tabs_and_missing_workflows(self) -> None:
        for reference in (
            "../action",
            "./../action",
            "./a//b",
            ".\\action",
            "./",
            "./safe#/../../evil",
        ):
            with self.subTest(reference=reference):
                self.assertTrue(self.check(self.workflow(f"      - uses: {reference}\n")))
        self.assertTrue(self.check(self.workflow("\t- uses: ./action\n")))
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(violations(Path(directory)))

    def test_current_repository_workflows_pass(self) -> None:
        workflows = Path(__file__).resolve().parents[2] / ".github" / "workflows"
        self.assertEqual(violations(workflows), [])


if __name__ == "__main__":
    unittest.main()

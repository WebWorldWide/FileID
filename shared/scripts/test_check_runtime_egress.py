import tempfile
import unittest
from pathlib import Path

from check_runtime_egress import (
    EXPECTED_INITIAL_GUARD,
    EXPECTED_INITIAL_PREDICATE,
    EXPECTED_REDIRECT_POLICY,
    known_blocker_violations,
    release_wiring_violations,
    violations,
)


class RuntimeEgressTests(unittest.TestCase):
    def files(
        self, directory: str, urls: list[str], hosts: list[str],
        *, dynamic: str = "", comment_decoy: str = "", limited_redirect: bool = False,
        ineffective_guards: bool = False, misplaced_guards: bool = False,
        pre_guard_request: bool = False,
    ) -> tuple[Path, Path]:
        root = Path(directory)
        registry = root / "registry.rs"
        registry.write_text(
            "\n".join(f'FileEntry {{ url: "{url}".to_string() }}' for url in urls)
            + (f"\nFileEntry {{ url: {dynamic} }}" if dynamic else ""),
            encoding="utf-8",
        )
        downloader = root / "downloader.rs"
        predicate = EXPECTED_INITIAL_PREDICATE
        redirect = EXPECTED_REDIRECT_POLICY
        if ineffective_guards:
            predicate = (
                "fn download_url_allowed(url: &str) -> bool {\n"
                "    let _ = (url, ALLOWED_DOWNLOAD_HOSTS);\n"
                "    true\n"
                "}\n"
            )
            redirect = (
                "let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {\n"
                "    let _ = ALLOWED_DOWNLOAD_HOSTS;\n"
                "    attempt.follow()\n"
                "});\n"
            )
        elif limited_redirect:
            redirect = "let redirect_policy = reqwest::redirect::Policy::limited(10);\n"
        entry_points = (
            "pub async fn download_simple(request: Request) {\n"
            + EXPECTED_INITIAL_GUARD
            + "    client.get(&request.url);\n}\n"
            "pub async fn download_parallel(request: Request) {\n"
            + EXPECTED_INITIAL_GUARD
            + "    client.head(&request.url);\n}\n"
        )
        if pre_guard_request:
            entry_points = (
                "pub async fn download_simple(request: Request) {\n"
                "    client.post(&request.url).send();\n"
                + EXPECTED_INITIAL_GUARD
                + "    client.get(&request.url);\n}\n"
                "pub async fn download_parallel(request: Request) {\n"
                "    client.request(Method::GET, &request.url).send();\n"
                + EXPECTED_INITIAL_GUARD
                + "    client.head(&request.url);\n}\n"
            )
        elif misplaced_guards:
            entry_points = (
                "pub async fn download_simple(request: Request) { client.get(&request.url); }\n"
                "pub async fn download_parallel(request: Request) { client.head(&request.url); }\n"
                "fn dead_one(request: Request) { if !download_url_allowed(&request.url) {} }\n"
                "fn dead_two(request: Request) { if !download_url_allowed(&request.url) {} }\n"
            )
        downloader.write_text(
            comment_decoy
            + "const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[\n"
            + "\n".join(f'    "{host}",' for host in hosts)
            + "\n];\n"
            + predicate
            + "fn client() {\n"
            + redirect
            + "}\n"
            + entry_points,
            encoding="utf-8",
        )
        return registry, downloader

    def test_accepts_only_hugging_face_and_subdomains(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                [
                    "https://huggingface.co/org/model/resolve/main/file.bin",
                    "https://cdn-lfs.hf.co/file.bin",
                ],
                ["huggingface.co", "hf.co"],
            )
            self.assertEqual(violations(registry, downloader), [])

    def test_rejects_off_policy_url_and_allowlist_host(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://github.com/org/repo/releases/download/v1/runtime.zip"],
                ["huggingface.co", "hf.co", "github.com"],
            )
            failures = violations(registry, downloader)
            self.assertTrue(any("non-Hugging-Face" in failure for failure in failures))
            self.assertTrue(any("allowlist must be exactly" in failure for failure in failures))

    def test_rejects_dynamic_url_comment_decoy_and_redirect_bypass(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co", "evil.example"],
                dynamic='format!("https://{}.example/file", "evil")',
                comment_decoy=(
                    '// const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &["huggingface.co", "hf.co"];\n'
                ),
                limited_redirect=True,
            )
            failures = violations(registry, downloader)
            self.assertTrue(any("exactly one literal" in failure for failure in failures))
            self.assertTrue(any("limited redirect" in failure for failure in failures))
            self.assertTrue(any("allowlist must be exactly" in failure for failure in failures))

    def test_rejects_shorthand_url_and_ineffective_guards(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co"],
                dynamic="url",
                ineffective_guards=True,
            )
            failures = violations(registry, downloader)
            self.assertTrue(any("exactly one literal" in failure for failure in failures))
            self.assertTrue(any("predicate differs" in failure for failure in failures))
            self.assertTrue(any("redirect policy differs" in failure for failure in failures))

    def test_rejects_model_file_alias_and_dead_guard_calls(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co"],
                dynamic="url",
                misplaced_guards=True,
            )
            text = registry.read_text(encoding="utf-8").replace("FileEntry { url }", "ModelFile { url }")
            registry.write_text(text, encoding="utf-8")
            failures = violations(registry, downloader)
            self.assertTrue(any("exactly one literal" in failure for failure in failures))
            self.assertTrue(any("download_simple must begin" in failure for failure in failures))
            self.assertTrue(any("download_parallel must begin" in failure for failure in failures))

    def test_rejects_alternate_request_before_canonical_guard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co"],
                pre_guard_request=True,
            )
            failures = violations(registry, downloader)
            self.assertTrue(any("download_simple must begin" in failure for failure in failures))
            self.assertTrue(any("download_parallel must begin" in failure for failure in failures))

    def test_repository_release_workflow_runs_gate_before_staging(self) -> None:
        release = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "release.yml"
        self.assertEqual(release_wiring_violations(release), [])

    def test_repository_known_blocker_baseline_has_no_unreviewed_additions(self) -> None:
        root = Path(__file__).resolve().parents[2]
        registry = root / "platforms/windows/src/engine/src/models/registry.rs"
        downloader = root / "platforms/windows/src/engine/src/downloader.rs"
        self.assertEqual(known_blocker_violations(registry, downloader), [])

    def test_rejects_missing_release_wiring(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            release = Path(directory) / "release.yml"
            release.write_text("jobs: {}\n", encoding="utf-8")
            self.assertTrue(release_wiring_violations(release))

    def test_rejects_plain_http_and_missing_contracts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            registry = root / "registry.rs"
            downloader = root / "downloader.rs"
            registry.write_text('FileEntry { url: "http://huggingface.co/file" }\n', encoding="utf-8")
            downloader.write_text("fn main() {}\n", encoding="utf-8")
            failures = violations(registry, downloader)
            self.assertTrue(any("non-Hugging-Face" in failure for failure in failures))
            self.assertTrue(any("expected exactly one" in failure for failure in failures))


if __name__ == "__main__":
    unittest.main()

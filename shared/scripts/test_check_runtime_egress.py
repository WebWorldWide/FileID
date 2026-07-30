import tempfile
import unittest
from pathlib import Path

from check_runtime_egress import (
    EXPECTED_INITIAL_GUARD,
    EXPECTED_INITIAL_PREDICATE,
    EXPECTED_REDIRECT_POLICY,
    RAW_NETWORK_FILES,
    REVIEWED_NETWORK_SOURCE_SHA256,
    SAFE_NETWORK_CALLER_FILES,
    known_blocker_violations,
    policy_source_wiring_violations,
    release_wiring_violations,
    source_boundary_violations,
    violations,
)


class RuntimeEgressTests(unittest.TestCase):
    def source_tree(self, directory: str) -> Path:
        root = Path(directory)
        repository = Path(__file__).resolve().parents[2]
        roots = [
            "platforms/windows/src/engine/src",
            "platforms/cli/src",
            "platforms/tui/src",
            "platforms/linux/src/app/src",
            "platforms/apple/app/Sources",
            "platforms/apple/engine/Sources",
            "platforms/apple/shared/Sources",
            "platforms/windows/src/FileID.App",
            "platforms/windows/src/FileID.IpcSchema",
        ]
        for relative in roots:
            (root / relative).mkdir(parents=True, exist_ok=True)
        for relative in sorted(REVIEWED_NETWORK_SOURCE_SHA256):
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text((repository / relative).read_text(encoding="utf-8"), encoding="utf-8")
        return root

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

    def test_reviewed_role_inventories_are_exact(self) -> None:
        self.assertEqual(RAW_NETWORK_FILES, {
            "platforms/windows/src/engine/src/downloader.rs",
            "platforms/windows/src/engine/src/models/vlm_server.rs",
            "platforms/windows/src/engine/src/commands/prewarm.rs",
            "platforms/windows/src/engine/src/commands/bulk.rs",
            "platforms/windows/src/engine/src/main.rs",
            "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
            "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
            "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift",
            "platforms/apple/shared/Sources/FileIDShared/CLIPTokenizer.swift",
            "platforms/apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Models/WordPieceTokenizer.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Models/RamPlusService.swift",
            "platforms/apple/app/Sources/FileID/EngineClient.swift",
            "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Pipeline/DocText.swift",
            "platforms/cli/src/runtime.rs",
            "platforms/cli/src/scan_models.rs",
            "platforms/linux/src/app/src/engine_client.rs",
            "platforms/linux/src/app/src/tabs/settings.rs",
            "platforms/tui/src/models.rs",
            "platforms/tui/src/scan.rs",
            "platforms/windows/src/engine/src/commands/trash.rs",
            # Reviewed 2026-07-29: both match the deliberately-broad raw-network
            # patterns on non-network constructs — restructure.rs imports the
            # Win32 FILE_SHARE_READ *filesystem* constant, and
            # EngineClient.Commands.cs declares a local `Process?` handle for the
            # engine child. Neither performs network I/O.
            "platforms/windows/src/engine/src/commands/restructure.rs",
            "platforms/windows/src/FileID.App/ViewModels/EngineClient.Commands.cs",
            "platforms/windows/src/engine/src/models/vlm.rs",
            "platforms/windows/src/engine/src/models/whisper.rs",
            "platforms/windows/src/engine/src/platform.rs",
            "platforms/windows/src/engine/src/shell/mod.rs",
            "platforms/windows/src/FileID.App/Program.cs",
            "platforms/windows/src/FileID.App/Services/SafeOpen.cs",
            "platforms/windows/src/FileID.App/ViewModels/EngineClient.cs",
            "platforms/windows/src/FileID.App/Views/Settings/SettingsView.xaml.cs",
            "platforms/windows/src/FileID.App/Views/Sidebar/SidebarProcessingControl.xaml.cs",
            "platforms/windows/src/FileID.App/App.xaml.cs",
            "platforms/windows/src/FileID.App/MainWindow.xaml.cs",
            "platforms/windows/src/FileID.App/Services/FolderPickerService.cs",
            "platforms/windows/src/FileID.App/Services/WinVerifyTrustChecker.cs",
            "platforms/windows/src/engine/src/models/runtime.rs",
            "platforms/windows/src/engine/src/pipeline/deep_analyze.rs",
            "platforms/windows/src/engine/src/pipeline/doc_extract.rs",
            "platforms/windows/src/engine/src/pipeline/restructure_apply.rs",
            "platforms/windows/src/engine/src/shell/heic.rs",
            "platforms/windows/src/engine/src/shell/ocr.rs",
            "platforms/windows/src/engine/src/shell/reveal.rs",
            "platforms/windows/src/engine/src/shell/tags.rs",
            "platforms/windows/src/engine/src/shell/thumbnail.rs",
            "platforms/windows/src/engine/src/shell/trash.rs",
            "platforms/windows/src/engine/src/shell/video.rs",
            "platforms/windows/src/engine/src/util/content_hash.rs",
            "platforms/windows/src/engine/src/util/path_safety.rs",
        })
        self.assertEqual(set(REVIEWED_NETWORK_SOURCE_SHA256), RAW_NETWORK_FILES)
        self.assertTrue(all(len(digest) == 64 for digest in REVIEWED_NETWORK_SOURCE_SHA256.values()))
        self.assertEqual(SAFE_NETWORK_CALLER_FILES, {
            "platforms/windows/src/engine/src/downloader.rs",
            "platforms/windows/src/engine/src/commands/prewarm.rs",
            "platforms/cli/src/runtime.rs",
            "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
            "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
            "platforms/apple/app/Sources/FileID/Services/ArcFaceModelInstaller.swift",
            "platforms/apple/app/Sources/FileID/Services/BGEModelInstaller.swift",
            "platforms/apple/app/Sources/FileID/Services/CLIPModelInstaller.swift",
            "platforms/apple/app/Sources/FileID/Services/RamPlusModelInstaller.swift",
        })

    def test_string_decoys_cannot_replace_live_downloader_contracts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co"],
            )
            decoy = EXPECTED_INITIAL_PREDICATE + EXPECTED_REDIRECT_POLICY + (
                "pub async fn download_simple(request: Request) {\n"
                + EXPECTED_INITIAL_GUARD + "}\n"
                "pub async fn download_parallel(request: Request) {\n"
                + EXPECTED_INITIAL_GUARD + "}\n"
            )
            downloader.write_text(
                'const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &["huggingface.co", "hf.co"];\n'
                'fn download_url_allowed(_: &str) -> bool { true }\n'
                'fn client() { let redirect_policy = reqwest::redirect::Policy::custom(|a| { a.follow() }); }\n'
                'pub async fn download_simple(request: Request) { client.get(&request.url); }\n'
                'pub async fn download_parallel(request: Request) { client.head(&request.url); }\n'
                + 'const DECOY: &str = r###"' + decoy + '"###;\n',
                encoding="utf-8",
            )
            failures = violations(registry, downloader)
            self.assertTrue(any("predicate differs" in failure for failure in failures))
            self.assertTrue(any("redirect policy differs" in failure for failure in failures))

    def test_registry_file_entry_string_decoy_is_not_production(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            registry, downloader = self.files(
                directory,
                ["https://huggingface.co/org/model/file"],
                ["huggingface.co", "hf.co"],
            )
            registry.write_text(
                'const DECOY: &str = r#"FileEntry { url: \\"https://huggingface.co/x\\" }"#;\n',
                encoding="utf-8",
            )
            self.assertTrue(any("no production FileEntry" in failure
                                for failure in violations(registry, downloader)))

    def test_source_boundary_rejects_new_cross_platform_clients_and_sinks(self) -> None:
        fixtures = [
            (
                "platforms/windows/src/engine/src/beacon.rs",
                'fn send(host: &str) { reqwest::Client::new().get(format!("https://{host}/x")); }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/Telemetry.swift",
                'func send(_ host: String) { URLSession.shared.dataTask(with: URL(string: "https://\\(host)/x")!) }',
                "raw network API",
            ),
            (
                "platforms/windows/src/FileID.App/UpdateClient.cs",
                'class UpdateClient { void Go(string u) { new HttpClient().GetAsync(u); } }',
                "raw network API",
            ),
            (
                "platforms/linux/src/app/src/download.rs",
                'fn go(url: &str) { download_parallel(url); }',
                "network download sink",
            ),
            (
                "platforms/windows/src/engine/src/alias.rs",
                'use reqwest as web; fn go() { web::Client::new(); }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteRead.swift",
                'func go(_ remote: URL) { _ = try? Data(contentsOf: remote) }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteInit.swift",
                'func go(_ remote: URL) { _ = try? Data.init(contentsOf: remote) }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteAlias.swift",
                'typealias Blob = Data; func go(_ remote: URL) { _ = try? Blob(contentsOf: remote) }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteString.swift",
                'func go(_ remote: URL) { _ = try? String(contentsOf: remote, encoding: .utf8) }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteAliasChain.swift",
                'typealias Blob = Foundation.Data; typealias Payload = Blob; func go(_ remote: URL) { _ = try? Payload.init(contentsOf: remote) }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/ExtendedDecoy.swift",
                'let decoy = #"abc " xyz"#; func go() { URLSession.shared.dataTask(with: URL(string: "https://example.invalid")!) }',
                "raw network API",
            ),
            (
                "platforms/windows/src/engine/src/raw_decoy.rs",
                'const D: &str = r#################"abc " xyz"#################; fn go() { reqwest::Client::new(); }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeHTTP.swift",
                "func go() { _ = CFReadStreamCreateForHTTPRequest(nil, CFHTTPMessageCreateEmpty(nil, false)) }",
                "raw network API",
            ),
            (
                "platforms/windows/src/FileID.App/NativeSocket.cs",
                "class N { void Go() { _ = new Windows.Networking.Sockets.StreamSocket(); } }",
                "raw network API",
            ),
            (
                "platforms/windows/src/engine/src/process_egress.rs",
                'fn go() { std::process::Command::new("curl").arg("https://evil.invalid"); }',
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeBrowse.swift",
                "func go() { _ = NWBrowser(for: .bonjour(type: \"_http._tcp\", domain: nil), using: .tcp) }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeResolve.swift",
                "func go() { _ = CFHostStartInfoResolution(host, .addresses, nil) }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeSpawn.swift",
                "func go() { posix_spawn(nil, \"/usr/bin/curl\", nil, nil, nil, nil) }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeSpawnP.swift",
                "func go() { posix_spawnp(nil, \"curl\", nil, nil, nil, nil) }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/LegacyTask.swift",
                "func go() { let task = NSTask(); task.launchPath = \"/usr/bin/curl\" }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeSystem.swift",
                "func go() { Darwin.system(\"curl https://evil.invalid\") }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/NativeLinuxSystem.swift",
                "func go() { Glibc.system(\"curl https://evil.invalid\") }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteDictionary.swift",
                "func go(_ remote: URL) { _ = NSDictionary(contentsOf: remote) }",
                "raw network API",
            ),
            (
                "platforms/apple/app/Sources/RemoteArray.swift",
                "func go(_ remote: URL) { _ = NSArray(contentsOf: remote) }",
                "raw network API",
            ),
            (
                "platforms/windows/src/FileID.App/NativeWinHttp.cs",
                'class W { [DllImport("winhttp.dll")] static extern nint WinHttpOpen(); }',
                "raw network API",
            ),
            (
                "platforms/windows/src/engine/src/native_winhttp.rs",
                "fn go() { unsafe { windows::Win32::Networking::WinHttp::WinHttpOpen(None, 0, None, None, 0); } }",
                "raw network API",
            ),
            (
                "platforms/windows/src/engine/src/bin/beacon.rs",
                'fn go() { reqwest::Client::new(); }',
                "raw network API",
            ),
            (
                "platforms/windows/src/FileID.App/QuoteClient.cs",
                'class Q { char q = \'"\'; void Go() { new HttpClient(); } }',
                "raw network API",
            ),
        ]
        for relative, source, expected in fixtures:
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as directory:
                root = self.source_tree(directory)
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source, encoding="utf-8")
                failures = source_boundary_violations(root)
                self.assertTrue(any(relative in failure and expected in failure for failure in failures))

    def test_approved_transport_files_cannot_add_raw_request_sites(self) -> None:
        mutations = [
            (
                "platforms/windows/src/engine/src/downloader.rs",
                "\nfn beacon(client: reqwest::Client) { reqwest::Client::execute(client, todo!()); }\n",
                "raw client/request inventory",
            ),
            (
                "platforms/windows/src/engine/src/models/vlm_server.rs",
                "\nfn beacon(rogue: reqwest::Client) { rogue.execute(todo!()); }\n",
                "loopback raw client/request inventory",
            ),
            (
                "platforms/windows/src/engine/src/main.rs",
                "\nasync fn beacon(http_client: reqwest::Client) { reqwest::Client::execute(http_client, todo!()).await; }\n",
                "HTTP client plumbing",
            ),
            (
                "platforms/windows/src/engine/src/commands/prewarm.rs",
                "\nasync fn beacon(http_client: reqwest::Client) { let transport = http_client.clone(); transport.execute(todo!()).await; }\n",
                "HTTP client plumbing",
            ),
            (
                "platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift",
                "\nfunc beacon(session: URLSession) { session.streamTask(withHostName: \"evil.invalid\", port: 443).resume() }\n",
                "raw session/request inventory",
            ),
            (
                "platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift",
                "\nfunc beacon(session: URLSession, request: URLRequest) { session.dataTask(with: request) }\n",
                "must not construct sessions or requests",
            ),
            (
                "platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift",
                "\nfunc beacon(session: URLSession, request: URLRequest) { session.dataTask(with: request) }\n",
                "raw tree-listing request inventory",
            ),
            (
                "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift",
                "\nfunc beacon() { URLSession.shared.dataTask(with: URL(string: \"https://example.invalid\")!) }\n",
                "local Foundation URL-loader inventory",
            ),
        ]
        for relative, addition, expected in mutations:
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as directory:
                root = self.source_tree(directory)
                path = root / relative
                path.write_text(path.read_text(encoding="utf-8") + addition, encoding="utf-8")
                self.assertTrue(any(expected in failure for failure in source_boundary_violations(root)))

    def test_reviewed_local_loader_content_is_digest_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.source_tree(directory)
            relative = "platforms/apple/app/Sources/FileID/Database/ThumbnailService.swift"
            path = root / relative
            source = path.read_text(encoding="utf-8").replace(
                "Data(contentsOf: url)",
                'Data(contentsOf: URL(string: "https://evil.invalid/pixel")!)',
                1,
            )
            path.write_text(source, encoding="utf-8")
            self.assertTrue(any(
                relative in failure and "source digest changed" in failure
                for failure in source_boundary_violations(root)
            ))

    def test_source_discovery_covers_rust_path_modules_outside_src(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.source_tree(directory)
            main = root / "platforms/windows/src/engine/src/main.rs"
            main.write_text(
                main.read_text(encoding="utf-8") + '\n#[path = "../rogue.rs"] mod rogue;\n',
                encoding="utf-8",
            )
            rogue = root / "platforms/windows/src/engine/rogue.rs"
            rogue.write_text("fn go() { reqwest::Client::new(); }\n", encoding="utf-8")
            self.assertTrue(any(
                "platforms/windows/src/engine/rogue.rs" in failure and "raw network API" in failure
                for failure in source_boundary_violations(root)
            ))

    def test_source_boundary_ignores_comments_strings_and_build_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = self.source_tree(directory)
            safe = root / "platforms/windows/src/engine/src/decoys.rs"
            safe.write_text(
                '// reqwest::Client::new()\nconst NOTE: &str = "URLSession HttpClient TcpStream";\n'
                '#[cfg(test)] mod tests { fn decoy() { reqwest::Client::new(); } }\n',
                encoding="utf-8",
            )
            excluded = root / "platforms/windows/src/engine/src/target/network.rs"
            excluded.parent.mkdir(parents=True)
            excluded.write_text("reqwest::Client::new();", encoding="utf-8")
            self.assertEqual(source_boundary_violations(root), [])

    def test_repository_production_network_boundary_is_closed(self) -> None:
        root = Path(__file__).resolve().parents[2]
        self.assertEqual(source_boundary_violations(root), [])

    def test_repository_policy_runs_for_every_change(self) -> None:
        policy = Path(__file__).resolve().parents[2] / ".github/workflows/policy.yml"
        self.assertEqual(policy_source_wiring_violations(policy), [])

    def test_repository_release_workflow_runs_gate_before_staging(self) -> None:
        release = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "release.yml"
        self.assertEqual(release_wiring_violations(release), [])

    def test_registry_cfg_test_raw_string_cannot_hide_added_url(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            registry = root / "registry.rs"
            downloader = root / "downloader.rs"
            registry_text = (
                repository / "platforms/windows/src/engine/src/models/registry.rs"
            ).read_text(encoding="utf-8")
            insertion = (
                'const POLICY_DECOY: &str = r##"#[cfg(test)]"##;\n'
                'const ROGUE: FileEntry = FileEntry { url: "https://evil.invalid/model" };\n'
            )
            registry.write_text(registry_text.replace("#[cfg(test)]", insertion + "#[cfg(test)]", 1), encoding="utf-8")
            downloader.write_text(
                (repository / "platforms/windows/src/engine/src/downloader.rs").read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            self.assertTrue(any("off-policy runtime URL baseline changed" in failure
                                for failure in known_blocker_violations(registry, downloader)))

    def test_repository_known_blocker_baseline_has_no_unreviewed_additions(self) -> None:
        root = Path(__file__).resolve().parents[2]
        registry = root / "platforms/windows/src/engine/src/models/registry.rs"
        downloader = root / "platforms/windows/src/engine/src/downloader.rs"
        self.assertEqual(known_blocker_violations(registry, downloader), [])

    def test_rejects_conditional_or_filtered_policy_workflow(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        original = (repository / ".github/workflows/policy.yml").read_text(encoding="utf-8")
        mutations = [
            original.replace("  pull_request:\n", "  pull_request:\n    branches-ignore: ['**']\n", 1),
            original.replace("    runs-on: ubuntu-latest\n", "    if: false\n    runs-on: ubuntu-latest\n", 1),
            original.replace("    runs-on: ubuntu-latest\n", "    continue-on-error: true\n    runs-on: ubuntu-latest\n", 1),
            original.replace("jobs:\n", "defaults:\n  run:\n    shell: bash {0}\n\njobs:\n", 1),
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation[:80]), tempfile.TemporaryDirectory() as directory:
                policy = Path(directory) / "policy.yml"
                policy.write_text(mutation, encoding="utf-8")
                self.assertTrue(policy_source_wiring_violations(policy))

    def test_release_egress_gate_cannot_continue_after_failure(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        original = (repository / ".github/workflows/release.yml").read_text(encoding="utf-8")
        mutated = original.replace(
            "        run: python ../../shared/scripts/check_runtime_egress.py\n",
            "        run: python ../../shared/scripts/check_runtime_egress.py\n        continue-on-error: true\n",
            1,
        )
        with tempfile.TemporaryDirectory() as directory:
            release = Path(directory) / "release.yml"
            release.write_text(mutated, encoding="utf-8")
            self.assertTrue(release_wiring_violations(release))

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

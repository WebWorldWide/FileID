# FileID — Security Notes

What we've audited, what's enforced today, and the hardening that gates the v1.0 release. This is a security posture doc, not a marketing claim — every statement below is meant to be verifiable against the source. For the privacy guarantees (no telemetry, network surface), see `PRIVACY.md`.

## Threat model

FileID is two local binaries — the engine (Rust on Windows, Swift on macOS) and the app (WinUI 3 / .NET 8 on Windows, SwiftUI on macOS) — that talk newline-delimited JSON over stdio. The engine owns a single-writer SQLite WAL database and runs all ML inference on-device. The only network egress is user-initiated model and runtime downloads (see `PRIVACY.md` for the host list). Three attacker classes are in scope:

- **Local untrusted process / privileged local adversary.** Can write to the user's filesystem or to FileID's install + data directories. Goals: swap the engine binary the app spawns, replace a model file on disk, or escalate via the engine's stdio IPC.
- **Network MITM.** Intercepting a model or runtime download to substitute a malicious bundle.
- **Malicious file content.** A crafted image / PDF / archive that the scan pipeline processes — decoder, OCR, face detector, VLM, hash — that tries to exploit a parser.

## Findings

### Enforced today

- **Engine binary integrity before spawn (Windows).** `EngineClient.Start()` refuses to launch the engine unless it passes `WinVerifyTrustChecker.Verify(enginePath, expectedThumbprintHex)`:
  - `NotFound` / `Untrusted` → refuse and surface a crash reason.
  - `Unsigned` → refused when an EV thumbprint is pinned (release builds, via `FILEID_EV_THUMBPRINT`); allowed with a warning in dev builds (no thumbprint pinned, so Visual Studio rebuilds aren't blocked).
  - `Trusted` → proceed. When a thumbprint is pinned, the app SHA256-hashes the binary after the verdict and re-hashes immediately before `Process.Start`; a mismatch aborts the spawn (TOCTOU mitigation against a binary swapped between verify and launch).
  - On macOS the equivalent guard lives in `EngineClient.start()`: the engine path must resolve inside the app bundle's `Contents/MacOS/`, and the engine's signing Team ID (`kSecCodeInfoTeamIdentifier`) must match the app's (or both unsigned/ad-hoc for dev).
- **Single-writer database.** The engine holds the only writer connection; all writes serialize through it. Reads fan out via fresh read-only connections. No cross-process write race on the DB.
- **Parameterized SQL.** Every `rusqlite` call in the engine uses bound parameters (`params![...]`, `query_row(sql, [args], …)`) — values are never string-interpolated into SQL. The migration runner (`db/migrations.rs`) binds the migration identifier as `?1`. Migrations v1–v12 are append-only and byte-faithful with the macOS GRDB schema (same `grdb_migrations` table + identifier strings) so a DB written by either platform opens on the other. On macOS, GRDB's typed API enforces the same discipline.
- **Restructure path containment.** Restructure proposals are built by joining sanitized components onto the user-picked library root (`pipeline/restructure.rs::classify`); `sanitize_path_component` strips the Windows-reserved set `< > : " / \ | ? *` (which also defuses `..`-style traversal via separators), and only the original file's own `file_name()` is reused as the leaf. VLM-proposed names from Deep Analyze pass through `sanitize_proposed_name` (`pipeline/deep_analyze.rs`), which strips quotes/punctuation, lowercases, hyphen-joins, caps length at a word boundary, and falls back to `untitled` on empty input.
- **Download transport hardening.** The engine's downloader (`downloader.rs`) is the only network code in the engine. It uses HTTPS only, a generic non-fingerprinting User-Agent, a bounded retry/back-off (1 s / 4 s / 16 s, `Retry-After` honored, capped at 60 s), a global in-flight request cap, and writes to `.part` files renamed atomically into place only on success.
- **Path redaction in logs.** Logs are local-only (`%LOCALAPPDATA%\FileID\logs\` / `~/Library/Application Support/FileID/logs/`). User file paths are redacted before they reach a log call site: `redact_path_for_log` (Rust, `platform.rs`), `PathRedactor.Redact` (C#), and `redactPathForLog(_:)` (Swift) all strip the user's home/username, keeping only enough tail to stay useful. App-structural FileID paths pass through unredacted.
- **No shell command construction.** The engine shells out only to the bundled `llama-server` (Deep Analyze) and uses argument-array process spawning, never a shell string. The llama server binds an ephemeral port on `127.0.0.1`; that's loopback IPC, not egress (the CI URL allowlist exempts loopback explicitly).

### Confirmed clean (no fix needed)

- **SQL injection.** See "Parameterized SQL" above — no dynamic value interpolation anywhere in the engine's SQLite access.
- **Telemetry / analytics.** No analytics SDK, crash reporter, update ping, or beacon ships in either binary. Enforced, not just asserted — see the CI gates below and `PRIVACY.md`.
- **Engine respawn race (macOS).** `EngineClient` keeps a single process reference and uses bounded backoff; no multi-instance race.

### SHA256 download verification — wired but currently inert

The downloader fully supports per-file SHA256 verification: when a `DownloadRequest` carries an `expected_sha256`, the stream is hashed (or the file re-hashed from disk after a resume), and a mismatch deletes the `.part` file and aborts with a clear error. The prewarm call sites pass each model file's `sha256` straight through.

**However, every entry in the model registry (`models/registry.rs`) currently sets `sha256: None`** — including the llama.cpp runtime zips pulled from GitHub releases. So in the shipped build the integrity check is plumbed end-to-end but does nothing, because the manifest supplies no digests. Until the registry is populated with the pinned hashes from `MODELS.md`, a network MITM or a CDN compromise could substitute a download undetected. Populating these digests is the highest-value open hardening item (below) and is the accurate state to cite — do not describe model downloads as "SHA256-pinned" in user-facing copy until the registry carries the hashes.

## Open hardening — gates the v1.0 release (NOT yet shipped)

These items must land before the v1.0 tag. They are not required for day-to-day development builds.

- **Populate the model-download SHA256 manifest.** Fill `models/registry.rs` `sha256` fields (and the macOS equivalent) from the canonical `MODELS.md` hashes so the already-wired download verifier actually fires. Covers the HuggingFace model weights and the GitHub-hosted llama.cpp runtime zips. This is the single change that turns the dormant integrity check on.
- **Per-model verification on load.** Even once downloads are verified, a model file on disk can be replaced afterward by a local adversary with write access to the Models directory. Record the SHA256 at download time and re-verify on every load (Windows engine + macOS, including the MLX VLMs).
- **Certificate pinning for downloads.** No TLS cert pinning today; an active MITM with a trusted CA could swap a download. Pin `huggingface.co` (and the GitHub release host) in the downloader. Best landed together with the SHA256 manifest above — the two together close the MITM gap.
- **EV signing + thumbprint pin in release builds.** The engine-integrity guard is only as strong as a real Authenticode/EV signature and a pinned `FILEID_EV_THUMBPRINT`; today's builds run unsigned-with-warning. Ship signed, with the thumbprint pinned, so the strict path (refuse unsigned + tamper-mismatched binaries) is live. Tracked with WiX MSI packaging in `SHIP.md`.

## CI enforcement

Three workflows enforce the security/privacy posture on every PR and push; a failure is a release blocker.

- **`windows-engine.yml`** (x64 + arm64-native + arm64-cross): `cargo fmt --check`, `clippy --all-targets -D warnings`, `cargo deny` (license + advisory + duplicate-version + source ban gate), a **source-URL allowlist** scan (every `https?://` in Rust/C#/XAML must hit an allowlisted host; loopback exempt), build, test, engine-startup + `verifyCudaPack` smokes, and a **telemetry-string scan** of the shipped `FileIDEngine.exe` (ASCII + UTF-16, ~22 forbidden SDK/endpoint strings).
- **`windows-app.yml`** (x64 + arm64): msbuild Debug/Release, self-contained publish, xUnit tests, `dotnet format --verify-no-changes`, a **vulnerable-package** scan (`dotnet list package --vulnerable --include-transitive`), the same **telemetry-string scan** over every shipped EXE + DLL, and an app-startup smoke.
- **`macos.yml`**: `swift build`/`swift test`, engine-startup smoke, the same **source-URL allowlist** and **telemetry-string scans**.

The forbidden-strings list and source-URL allowlist must stay in sync across all three workflows and `platforms/windows/build/publish-bundle.ps1`. Adding a host or string requires a matching rationale entry in `DECISIONS.md`; reviewers reject PRs that touch either list without it.

## Reporting

Security issues: open a private security advisory on the GitHub repository, or email the project owner. Do not file public issues for vulnerabilities.

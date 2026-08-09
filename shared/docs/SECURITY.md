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
- **Parameterized SQL.** Every `rusqlite` call in the engine uses bound parameters (`params![...]`, `query_row(sql, [args], …)`) — values are never string-interpolated into SQL. The migration runner (`db/migrations.rs`) binds the migration identifier as `?1`. Migrations v1–v16 are append-only and byte-faithful with the macOS GRDB schema (same `grdb_migrations` table + identifier strings, chain pinned by tests on both platforms) so a DB written by either platform opens on the other; a DB migrated beyond the local registry is refused with `db_newer_than_engine` instead of silently written. On macOS, GRDB's typed API enforces the same discipline.
- **Restructure path containment.** Restructure proposals are built by joining sanitized components onto the user-picked library root (`pipeline/restructure.rs::classify`); `sanitize_path_component` strips the Windows-reserved set `< > : " / \ | ? *` (which also defuses `..`-style traversal via separators), and only the original file's own `file_name()` is reused as the leaf. VLM-proposed names from Deep Analyze pass through `sanitize_proposed_name` (`pipeline/deep_analyze.rs`), which strips quotes/punctuation, lowercases, hyphen-joins, caps length at a word boundary, and falls back to `untitled` on empty input.
- **Download transport hardening.** The engine's downloader (`downloader.rs`) is the only network code in the engine. It uses HTTPS only, a generic non-fingerprinting User-Agent, a bounded retry/back-off (1 s / 4 s / 16 s, `Retry-After` honored, capped at 60 s), a global in-flight request cap, and writes to `.part` files renamed atomically into place only on success.
- **Path redaction in logs.** Logs are local-only (`%LOCALAPPDATA%\FileID\logs\` / `~/Library/Application Support/FileID/logs/`). User file paths are redacted before they reach a log call site: `redact_path_for_log` (Rust, `platform.rs`), `PathRedactor.Redact` (C#), and `redactPathForLog(_:)` (Swift) all strip the user's home/username, keeping only enough tail to stay useful. App-structural FileID paths pass through unredacted.
- **No shell command construction.** The engine shells out only to the bundled `llama-server` (Deep Analyze) and uses argument-array process spawning, never a shell string. The llama server binds an ephemeral port on `127.0.0.1`; that's loopback IPC, not egress (the CI URL allowlist exempts loopback explicitly).

### Confirmed clean (no fix needed)

- **SQL injection.** See "Parameterized SQL" above — no dynamic value interpolation anywhere in the engine's SQLite access.
- **Telemetry / analytics.** No analytics SDK, crash reporter, update ping, or beacon ships in either binary. Enforced, not just asserted — see the CI gates below and `PRIVACY.md`.
- **Engine respawn race (macOS).** `EngineClient` keeps a single process reference and uses bounded backoff; no multi-instance race.

### SHA256 download verification — live on both platforms (2026-06-10)

`shared/models/manifest.json` is the single source of truth: 29 static artifacts with pinned
SHA256 + the MLX VLM repos pinned by immutable HuggingFace revision (per-file LFS `oid`
verification). The Windows registry (`models/registry.rs`) is locked to the manifest by a cargo
test (`manifest_consistency.rs`, bidirectional); macOS parses the same manifest as a SwiftPM
resource (`ModelManifest.swift`) and `StreamingDownload` hashes incrementally, deleting the
artifact and failing with a distinct checksum error *before* the atomic move. Small embedders
(CLIP/ArcFace) re-verify on load at preWarm; multi-GB VLMs are install-time-verified with a
`.fileid-verified-<revision>` sentinel (re-hashing 5–7 GB on every load is unacceptable — the
residual risk and rationale are in `DECISIONS.md`).

### TLS pinning — live on both platforms (2026-06-10)

CA-allowlist pinning (root SPKI set + backups, NOT leaf pins) for the model-download hosts.
Pins live in `shared/security/tls-pins.json` + `pinned-roots/*.pem` (11 roots covering
huggingface.co, its CDNs, and GitHub releases); `shared/scripts/check_tls_pins.sh` asserts the
two representations match in CI. macOS: `TLSPinning.swift` challenge handler (system trust
first, then chain∩pin-set). Windows: `reqwest` built with `.tls_built_in_root_certs(false)` +
only the embedded roots; the fail-closed fallback client has no roots at all. Escape hatch
`FILEID_DISABLE_TLS_PINNING=1` logs loudly and changes no egress. SHA256 above remains the
primary integrity control; pinning is defense-in-depth.

**Rotation runbook:** if a download host rotates to a CA outside the pin set, downloads fail
with the distinct pin error (`download_tls_pin_failed` / `pinningFailed`). Recovery: re-capture
the live chains (`openssl s_client -showcerts` against each host in `tls-pins.json`, including
the `cdn-lfs*` and `objects.githubusercontent.com` redirect targets), add the new root PEM to
`shared/security/pinned-roots/`, regenerate the SPKI entry in `tls-pins.json`, run
`check_tls_pins.sh`, ship a point release. Users mid-outage can set the escape hatch.

### Tokenizer input bounds — live on both platforms (2026-06-10)

Both CLIP tokenizers (Swift `CLIPTokenizer` in FileIDShared, Rust `clip_tokenizer.rs`) cap
input at 1 024 chars pre-regex (lossless under the 77-token context), bound piece counts and
vocab/merge sizes at construction, and truncate char-boundary-safely. The BGE wordpiece
tokenizer (attacker-controlled doc/OCR text) stops at `max_len` words with input pre-slicing.
Pathological-input tests (1 MB single word, combining-char floods, 4-byte emoji runs) run on
both platforms.

## Open hardening — gates the v1.0 release (NOT yet shipped)

- **EV signing + thumbprint pin in release builds (Windows).** The engine-integrity guard is only as strong as a real Authenticode/EV signature and a pinned `FILEID_EV_THUMBPRINT`; today's builds run unsigned-with-warning. Ship signed, with the thumbprint pinned, so the strict path (refuse unsigned + tamper-mismatched binaries) is live. Tracked with WiX MSI packaging in `SHIP.md`.
- **macOS Developer ID signing + notarization (user-gated).** The full pipeline exists and dry-runs green (`platforms/apple/scripts/release.sh --skip-notarize` produces a hardened-runtime, ad-hoc-signed DMG). The real signing pass needs the Developer ID certificate + `notarytool store-credentials fileid-notary` on the owner's machine — steps documented in the script header.

## CI enforcement

Three workflows enforce the security/privacy posture on every PR and push; a failure is a release blocker.

- **`windows-engine.yml`** (x64 + arm64-native + arm64-cross): `cargo fmt --check`, `clippy --all-targets -D warnings`, `cargo deny` (license + advisory + duplicate-version + source ban gate), a **source-URL allowlist** scan (every `https?://` in Rust/C#/XAML must hit an allowlisted host; loopback exempt), build, test, engine-startup + `verifyCudaPack` smokes, and a **telemetry-string scan** of the shipped `FileIDEngine.exe` (ASCII + UTF-16, ~22 forbidden SDK/endpoint strings).
- **`windows-app.yml`** (x64 + arm64): msbuild Debug/Release, self-contained publish, xUnit tests, `dotnet format --verify-no-changes`, a **vulnerable-package** scan (`dotnet list package --vulnerable --include-transitive`), the same **telemetry-string scan** over every shipped EXE + DLL, and an app-startup smoke.
- **`macos.yml`**: `swift build`/`swift test`, engine-startup smoke, the same **source-URL allowlist** and **telemetry-string scans**.

The forbidden-strings list and source-URL allowlist must stay in sync across all three workflows and `platforms/windows/build/publish-bundle.ps1`. Adding a host or string requires a matching rationale entry in `DECISIONS.md`; reviewers reject PRs that touch either list without it.

## Reporting

Security issues: open a private security advisory on the GitHub repository, or email the project owner. Do not file public issues for vulnerabilities.

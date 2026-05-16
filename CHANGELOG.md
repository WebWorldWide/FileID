# Changelog

All notable changes to FileID are tracked here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per `shared/docs/PRIVACY.md` and `CLAUDE.md`: this project ships no telemetry, no analytics, no crash-reporter SDKs. The CI privacy gate scans every release binary against a 22-string deny-list before publication.

## [Unreleased]

### Added
- **Windows Rust engine** decomposed into `commands/` (10 IPC-domain submodules), `util/` (HMAC, path safety, zip), `logging.rs`, and `ipc/bounded_read.rs`. `main.rs` is now 678 LOC (was 3,463).
- **Windows .NET `EngineClient`** split via `partial class` into `EngineClient.cs` (lifecycle, event router, observable surface) + `EngineClient.Commands.cs` (command facade, AutoPilot).
- **Windows .NET `ModelSlot`** extracted from `ModelInstallerService.cs` into its own file.
- **macOS Swift `SankeyLayout.swift`** — nested types from `SankeyFlowView.swift` lifted into a sibling extension.
- **Rust property tests** via `proptest` (dev-dep): 9 tests across `util/path_safety`, `util/zip`, `pipeline/face_clustering`. Caught two real bugs the example tests missed.
- **.NET test classes**: `PathRedactorTests`, `UndoStackTests`, `SafeOpenTests`, `AppSettingsTests` (36 cases in `FileID.App.Tests`).
- **Test infrastructure**: `FileID.App.Tests` xUnit project; `tools/git-hooks/pre-commit` (privacy scan + format + clippy in < 15 s); `shared/docs/{COVERAGE,TESTING,CONTRIBUTING}.md`.
- **`cargo-deny` config** at `platforms/windows/src/engine/deny.toml` (license + advisory + duplicate-version + source allowlist).
- **PGO release profile** in `Cargo.toml` (`[profile.release-pgo]`).

### Changed
- **Image-decode fast path** in `pipeline/tagging.rs` now uses `memmap2::Mmap` for a single open + two reads, eliminating the ~100 µs-per-file double-open that was visible at scan scale.
- **SQLite `PRAGMA cache_spill = 0`** added to engine + reader connections — prevents mid-transaction temp-file spills (worst-case batch fits in the 64 MB cache, so spill never helps).
- **`commands/bulk::handle_apply_tags`** hoists per-tag INSERT to `prepare_cached` for prepared-statement reuse across the inner loop.
- **`identity_clustering::cluster`** now iterates `root_members` in sorted-key order so cluster IDs are deterministic across re-scans. (Previously HashMap iteration order leaked into cluster numbering — re-scans of the same library could renumber People-tab clusters.)
- **`is_safe_filename`** rejects any input containing `/` or `\` before the path-component walk. `Path::components()` silently strips trailing separators, which previously let inputs like `"A\\"` slip past. Security-relevant: this function is the path-traversal guard for `renameFiles`.
- **CI clippy gate** tightened from a narrow lint-group filter to `-D warnings` on all targets, paired with documented `[lints.clippy]` allows for style-only pedantic rules.
- **CI .NET workflow** now runs `dotnet format --verify-no-changes`, `dotnet list package --vulnerable` (hard gate), and `dotnet test FileID.sln` on every project (was IpcSchema-only with `continue-on-error`).
- **CI Rust toolchain** bumped to 1.90 (matches `rust-toolchain.toml`).

### Fixed
- **`is_safe_filename("A\\")`** previously accepted because `Path::components()` strips trailing separators. Fixed by an explicit slash check. Caught by `proptest`.
- **Non-deterministic cluster IDs** in `identity_clustering`. Fixed by sorting HashMap iteration. Caught by `proptest`.
- **`stable_path_hash`** was duplicated between `main.rs` and `dbwriter.rs`. Consolidated into `util/path_safety.rs`.

### Removed
- **`fast_image_resize`** unused dep dropped from `Cargo.toml`. It was declared but never imported.
- **22 inline command handlers** removed from `main.rs` (moved to `commands/*` submodules).
- **2 .NET file-bloat blocks**: `EngineClient.cs` command facade extracted to `EngineClient.Commands.cs`; `ModelSlot` class extracted to `Services/ModelSlot.cs`.

### Security
- **SEC: `is_safe_filename` defense-in-depth.** See Fixed above; the `renameFiles` destination check still applied, but the function's documented "single Normal path component" guarantee was leaky.
- **Telemetry-string scan** posture preserved: 22 deny-listed substrings + outbound traffic restricted to 5 allowed hosts (HuggingFace, GitHub, nvidia.com, plus two help links). CI binary scan enforces.

---

## Earlier versions

Versions V11–V15.2.1 predate this CHANGELOG. Their release notes live in commit messages and `shared/docs/STATE.md` (top-of-file entries, latest-first). Anyone wanting the history can `git log --oneline` or read STATE.md from the bottom up. Future releases (V15.3+) populate this file at tag time.

[Unreleased]: ./compare/V15.2.1...HEAD

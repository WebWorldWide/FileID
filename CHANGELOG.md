# Changelog

All notable changes to FileID are tracked here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per `shared/docs/PRIVACY.md` and `CLAUDE.md`: this project ships no telemetry, no analytics, no crash-reporter SDKs. The CI privacy gate scans every release binary against a 22-string deny-list before publication.

## [Unreleased]

### Added
- **Windows Rust engine** decomposed into `commands/` (10 IPC-domain submodules), `util/` (HMAC, path safety, zip), `logging.rs`, and `ipc/bounded_read.rs`. `main.rs` is now 678 LOC (was 3,463).
- **Windows .NET `EngineClient`** split via `partial class` into `EngineClient.cs` (lifecycle, event router, observable surface) + `EngineClient.Commands.cs` (command facade, AutoPilot).
- **Windows .NET `ModelSlot`** extracted from `ModelInstallerService.cs` into its own file.
- **macOS Swift `SankeyLayout.swift`** — nested types from `SankeyFlowView.swift` lifted into a sibling extension.
- **Rust property tests** via `proptest` (dev-dep): 12 tests across `util/path_safety`, `util/zip`, `pipeline/face_clustering`, `pipeline/dbwriter`, and `ipc/mod.rs`. Caught two real bugs the example tests missed.
- **Rust IPC round-trip test** — `every_command_variant_round_trips` exercises all 26 `CommandPayload` variants and asserts the discriminant survives encode/decode. Catches serde rename drift between Rust + Swift schema.
- **Rust criterion benches** — engine crate now lib+bin so `benches/*.rs` can `use fileid_engine::*`. Two bench targets shipped: `tagging_hashes.rs` (compute_dhash + resize_rgb_nearest at multiple sizes) and `face_clustering_5k.rs` (cluster() on 5K synthetic 512-d embeddings).
- **.NET test classes**: `PathRedactorTests`, `UndoStackTests`, `SafeOpenTests`, `AppSettingsTests` (36 cases in `FileID.App.Tests`).
- **Test infrastructure**: `FileID.App.Tests` xUnit project; `tools/git-hooks/pre-commit` (privacy scan + format + clippy in < 15 s); `shared/docs/{COVERAGE,TESTING,CONTRIBUTING}.md`.
- **`cargo-deny` config** at `platforms/windows/src/engine/deny.toml` (license + advisory + duplicate-version + source allowlist).
- **PGO release profile** in `Cargo.toml` (`[profile.release-pgo]`).
- **CI source URL allowlist scan** (both Windows + macOS workflows). Scans every `*.{rs,cs,xaml,xaml.cs,swift}` for any `https?://` URL and fails if the host isn't on the 6-entry allowlist (`huggingface.co`, `github.com`, `developer.download.nvidia.com`, `developer.nvidia.com`, plus 2 XAML namespace identifiers). Flips the privacy posture from deny-list to allow-list.
- **CI advisory-DB cache** (`actions/cache@v4` on `~/.cargo/advisory-db`, keyed weekly). Stabilizes `cargo audit` results across CI runs so the gate isn't tripped by transient advisory churn.

### Changed
- **Image-decode fast path** in `pipeline/tagging.rs` now uses `memmap2::Mmap` for a single open + two reads, eliminating the ~100 µs-per-file double-open that was visible at scan scale.
- **SQLite `PRAGMA cache_spill = 0`** added to engine + reader connections — prevents mid-transaction temp-file spills (worst-case batch fits in the 64 MB cache, so spill never helps).
- **`commands/bulk::handle_apply_tags`** hoists per-tag INSERT to `prepare_cached` for prepared-statement reuse across the inner loop.
- **`identity_clustering::cluster`** now iterates `root_members` in sorted-key order so cluster IDs are deterministic across re-scans. (Previously HashMap iteration order leaked into cluster numbering — re-scans of the same library could renumber People-tab clusters.)
- **`is_safe_filename`** rejects any input containing `/` or `\` before the path-component walk. `Path::components()` silently strips trailing separators, which previously let inputs like `"A\\"` slip past. Security-relevant: this function is the path-traversal guard for `renameFiles`.
- **CI macOS smoke** no longer asserts `"executionProvider"` is present in the engine's ready event — that field is Windows-only (ORT execution-provider picker output). Also fixed to grep `engine.stderr` (not stdout) because macOS `IPCSink` writes events via `FileHandle.standardError`. Windows engine writes to stdout — that asymmetry is documented in both workflows. Pre-existing failure since V15.2.
- **CI clippy gate** tightened from a narrow lint-group filter to `-D warnings` on all targets, paired with documented `[lints.clippy]` allows for style-only pedantic rules.
- **CI .NET workflow** now runs `dotnet format --verify-no-changes`, `dotnet list package --vulnerable` (hard gate), and `dotnet test FileID.sln` on every project (was IpcSchema-only with `continue-on-error`).
- **CI `cargo audit` re-tightened** to a hard gate (`--deny warnings`). Was softened temporarily when the advisory DB on CI drifted from the local one; paired now with the advisory-DB cache (above) so the corpus stays stable.
- **CI Rust toolchain** bumped to 1.90 (matches `rust-toolchain.toml`).
- **Engine crate restructured to lib+bin** so `benches/*.rs` and integration tests can reach internals. Dev compile cost +30%; runtime cost zero (shipped bin still gets release LTO independently).
- **STATE.md / NEXT.md consolidated** — older release entries collapsed to one-line bullets (STATE.md 2371→183 LOC, 92% reduction; NEXT.md 473→97 LOC, 80% reduction). Detail history in git log.

### Fixed
- **`is_safe_filename("A\\")`** previously accepted because `Path::components()` strips trailing separators. Fixed by an explicit slash check. Caught by `proptest`.
- **Non-deterministic cluster IDs** in `identity_clustering`. Fixed by sorting HashMap iteration. Caught by `proptest`.
- **`stable_path_hash`** was duplicated between `main.rs` and `dbwriter.rs`. Consolidated into `util/path_safety.rs`.
- **macOS engine smoke** now reliably detects engine startup. The grep targets `engine.stderr` (where macOS `IPCSink` writes) instead of `engine.stdout` (where it doesn't).

### Removed
- **`fast_image_resize`** unused dep dropped from `Cargo.toml`. It was declared but never imported.
- **22 inline command handlers** removed from `main.rs` (moved to `commands/*` submodules).
- **2 .NET file-bloat blocks**: `EngineClient.cs` command facade extracted to `EngineClient.Commands.cs`; `ModelSlot` class extracted to `Services/ModelSlot.cs`.

### Security
- **SEC: `is_safe_filename` defense-in-depth.** See Fixed above; the `renameFiles` destination check still applied, but the function's documented "single Normal path component" guarantee was leaky.
- **Telemetry-string scan** posture preserved: 22 deny-listed substrings + outbound traffic restricted to 6 allowed hosts (HuggingFace, GitHub, nvidia.com download, nvidia.com developer, plus 2 XAML namespace tokens). CI binary scan + new source-URL scan both enforce.
- **CI source URL allowlist** is the new defense layer: catches a contributor who adds a brand-new URL not on the deny-list. Flips posture from "anything except these 22 strings" to "only these 6 documented hosts".

---

## Earlier versions

Versions V11–V15.2.1 predate this CHANGELOG. Their release notes live in commit messages and `shared/docs/STATE.md` (top-of-file entries, latest-first). Anyone wanting the history can `git log --oneline` or read STATE.md from the bottom up. Future releases (V15.3+) populate this file at tag time.

[Unreleased]: ./compare/V15.2.1...HEAD

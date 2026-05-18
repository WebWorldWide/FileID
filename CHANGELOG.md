# Changelog

All notable changes to FileID are tracked here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per `shared/docs/PRIVACY.md` and `CLAUDE.md`: this project ships no telemetry, no analytics, no crash-reporter SDKs. The CI privacy gate scans every release binary against a 22-string deny-list before publication.

## [Unreleased]

### Added

#### 2026-05-17 — follow-up parity session

- **Comment surgery across `platforms/windows/src/`**: removed every `V14.x` / `V15.x` version-history prefix and every `Mirror of macOS X.swift:Y` reference (387 + 54 hits). Phase-N references inside still-relevant sentences are kept only where the reference is part of the explanation. Engine + app + tests build clean; clippy `-D warnings` passes.
- **`pdf-analyze` Cargo feature.** Adds `pdfium-render = "0.8"` as an optional dep; `analyze_file()` rasterizes PDF page 0 at 1024 px before handing off to the VLM caption flow. Default build path is unchanged; Deep Analyze on PDFs now works under `cargo build --features pdf-analyze`. New unit tests cover both the feature-enabled and feature-disabled call sites.
- **C# ViewModel binding tests** (`ViewModelBindingTests.cs`, 26 new test cases): `ModelSlot.Apply` progress→Installed transitions, `PersonCluster.BuildCropPath` (newly-extracted static helper), `ScanProgress`/`DeepAnalyzeProgress`/`HardwareInfo` DTO field shape, `WelcomeSheetModelSizeTests` (theory across 7 model_kinds via new `ModelDisplaySize.GetDisplaySizeMB`). All 62 C# tests pass (was 36 prior).
- **`ModelDisplaySize` helper** (`Services/ModelDisplaySize.cs`): single C#-side source of truth for Welcome-sheet size labels, keyed to engine `registry.rs` `approx_bytes` sums.
- **`PersonCluster.BuildCropPath` static helper** (`ViewModels/PeopleViewModel.cs`): pure-function face-crop-path builder so the binding logic is unit-testable without standing up XAML.
- **VRAM measurement on RTX 2060**: scanned `%USERPROFILE%\Pictures` under `nvidia-smi` sampling; engine attribution peaked at ~940 MB above the 1.65 GB idle baseline. `VRAM_PER_POOL_INSTANCE_MB` stays at 1500 (preserves ~560 MB safety margin against DirectML allocator fragmentation); comment updated with the measurement and method.
- **WiX 4 wixproj fixes** for `publish-bundle.ps1`:
  - `FileID.Msi.wixproj` + `FileID.Bundle.wixproj` override `<DebugType>full</DebugType>` (the platforms-wide `portable` default trips `wix.exe`).
  - `FileID.Bundle.wixproj` switched `DefineConstants` from WiX 3 ItemGroup form to WiX 4 PropertyGroup form.
  - `Bundle.wxs` moved `<bal:Condition>` expressions from element bodies to `Condition` attributes (WiX 4 syntax); dropped removed `DisplayInternalUI`.
  - Removed explicit `<Compile Include="Bundle.wxs" />` (auto-discovery means explicit include trips "Multiple entry sections").
- **Privacy gate verified** on 513 published x64 binaries (engine + app + .NET self-contained payload): 0 telemetry-pattern hits.
- **SCRFD `decode_scrfd_stride` / `decode_scrfd_single_anchor` pure functions** extracted from `detect()`, so the anchor-decode math is unit-testable without standing up an ORT session. 3 regression tests + 1 proptest (`scrfd_decoded_bbox_within_image_bounds`) verify threshold filtering, single-anchor decode, full-stride decode, and bbox bounds across randomized inputs.
- **CI-installable pwsh 7.6.1** via `winget install Microsoft.PowerShell` for sessions where the runner lacks pwsh. `publish-bundle.ps1` now runs from `%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe`.

#### Earlier in this changelog cycle

- **`platforms/windows/build/engine-smoke.ps1` (V15.8c).** Spawns the engine, asserts the ready event has the schema-required fields (version, pid, workerCap, physicalMemoryGB), sends shutdown, asserts clean exit. Compatible with Windows PowerShell 5.1; no `pwsh` dependency. Verified end-to-end on a real Windows box (NVIDIA RTX 2060, clean exit 0).
- **UNC path containment tests (V15.8c).** 2 new tests in `util/path_safety::tests` for the SEC-7 restore-from-trash check: nested UNC paths match their authorized root; cross-server UNC paths don't accidentally collide (different `\\srv\share` prefixes are distinct containment domains).
- **identity_clustering invariant tests (V15.8c).** 2 new tests: all-identical embeddings collapse to one cluster (guards against the union-find or DBSCAN refinement splitting a singular identity); 5 orthogonal unit vectors produce 5 distinct cluster IDs (guards against false-merge on maximally-dissimilar inputs).
- **SCRFD `detect()` implementation (V15.8b).** Full post-processing against the Buffalo_L SCRFD-10g (insightface) reference: anchor decoding for strides 8/16/32, 2 anchors per spatial location, 5 facial landmarks, distance-encoded bbox, NMS @ IoU 0.4, score threshold 0.5, coordinate remap from letterbox-resized back to original image space. Defensive parsing: wrong-variant ONNX silently degrades to empty result rather than producing nonsense scores. People-tab face crops now work on Windows (pending golden-set verification).
- **EP chain regression tests (V15.8b).** 7 new tests across `models::runtime` parameterized over NVIDIA / AMD / Intel / Qualcomm / Other / None vendors. Documents the expected `priority_chain` ordering as a regression guard. Includes a global invariant test: every vendor's chain must terminate at CPU and (if vendor != None) include DirectML.
- **`nms` + `iou` helpers tests (V15.8b).** 5 unit tests in `models::scrfd::tests` cover greedy NMS cluster pickup, identical/disjoint/half-overlap IoU, empty input handling, and horizontal-eyes-zero-roll pose estimation.
- **WAL checkpoint debug-assertion (V15.8b).** `dbwriter::flush` now asserts `conn.is_autocommit()` before the periodic `PRAGMA wal_checkpoint(PASSIVE)` call. Catches a future regression where someone adds a `BEGIN` before the checkpoint block.
- **IPC schema parity (V15.8).** Added 5 events (`restructurePlan`, `restructureApplyResult`, `bulkActionResult`, `clipTextEmbedding`, `mergeSuggestions`) and 1 command field (`startScan.rescan`) to `shared/ipc-schema/ipc.schema.json`. Both Rust and C# already implemented these; the schema was simply behind.
- **Reserved Windows device names (V15.8).** `is_safe_filename` now rejects `COM0` and `LPT0` (in addition to existing `COM1..9` / `LPT1..9`), matching Microsoft's Naming Files documentation. Covered by a new proptest.
- **Per-entry zip-bomb cap (V15.8).** `util/zip::extract_into_parent` now caps a single entry at 1 GiB (half the cumulative 2 GiB cap), so a single bomb entry can't consume the whole budget.
- **Embedding byte-order round-trip proptest (V15.8).** Confirms `floats_to_le_bytes` + `f32::from_le_bytes` is byte-for-byte lossless, including NaN payloads. Guards against a future switch to `to_ne_bytes` silently corrupting embeddings when DBs move between architectures.
- **HMAC bit-sensitivity proptests (V15.8).** Two new proptests: appending any byte to the message changes the MAC; appending any non-zero byte to the key changes the MAC. Catches the regression we'd see if the key were silently truncated at the block boundary instead of pre-hashed.
- **PathRedactor edge-case tests (V15.8).** UNC paths now keep only the last 2 Normal components (no server/share leak); drive root `C:\` collapses to `…`; app-structural paths (anything under `…\FileID\…`) still pass through unchanged. The redact function itself was fixed to filter on `Component::Normal` only so the Prefix/RootDir parts never leak.
- **FTS5 round-trip assertion strengthened (V15.8).** The existing FTS5 sanity test now asserts the matched rowid equals `files.id` and that a known-absent word returns zero hits — was just `COUNT(*) == 1` before.
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
- **5 brand-gold hex literals consolidated into `{StaticResource GoldBrush}` (V15.8b).** `SettingsView.xaml` cuDNN success pill border + foreground, `SidebarEngineStatus.xaml` status dot fill, and `SidebarProcessingControl.xaml` warning banner border + foreground icon. Brand-color drift detection is now centralized in `Theme.xaml`'s `GoldColor` definition.
- **Incremental-rescan skip query now excludes prior failures (V15.8).** `scan_session.rs` SELECT for "already current" paths now requires `failed = 0` in addition to `scanned_at >= modified_at`. Files that errored on a previous scan retry automatically.
- **Trash-log HMAC enforcement (V15.8).** Removed the no-HMAC backward-compat read path in `commands/trash_log.rs::read_batch`. Any entry without an HMAC suffix is now warned + skipped. The pre-V14.7.2 grace expired months ago.
- **SEC-5 TOCTOU defense-in-depth (V15.8).** `pipeline/restructure_apply.rs::apply` now reparse-point-checks the destination's ancestor chain BOTH before and after `create_dir_all`. The pre-check catches pre-planted junctions; the post-check catches anything that appeared during the call.
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
- **`db/migrations.rs` SQL-case comment (V15.8c).** Comment claimed GRDB lowercases column types — actually wrong. GRDB's `Database.ColumnType.text` returns `"TEXT"` (uppercase) verbatim. Rust SQL uses UPPERCASE which correctly matches; comment rewritten to reflect reality.
- **`redact_path_for_log("C:\\")`** was returning `"…/C:/\\"` (leaking the drive letter). Now correctly returns `"…"` because Prefix + RootDir components are excluded from PII consideration. UNC paths now redact to `"…/<user>/<file>"` instead of leaking the server\share prefix.
- **6 cargo clippy warnings (V15.8)**: deny.toml lint rename (`unchecked_duration_subtraction` → `unchecked_time_subtraction`), orphan doc-comment in `discovery.rs`, redundant `.into_iter()` in `bulk.rs`, two manual-checked-division branches in `tagging.rs`, and a `sort_by` → `sort_by_key` in `restructure.rs`.
- **`is_safe_filename("A\\")`** previously accepted because `Path::components()` strips trailing separators. Fixed by an explicit slash check. Caught by `proptest`.
- **Non-deterministic cluster IDs** in `identity_clustering`. Fixed by sorting HashMap iteration. Caught by `proptest`.
- **`stable_path_hash`** was duplicated between `main.rs` and `dbwriter.rs`. Consolidated into `util/path_safety.rs`.
- **macOS engine smoke** now reliably detects engine startup. The grep targets `engine.stderr` (where macOS `IPCSink` writes) instead of `engine.stdout` (where it doesn't).

### Removed
- **`shell/sleep.rs` (V15.8)** — duplicate of `platform.rs::SleepGuard`; only the platform.rs version was ever called.
- **`pipeline/discovery.rs::enumerate` + `Discovery::new` (V15.8)** — the wrapper-style convenience constructors had no production callers (scan_session uses `Discovery::new_with_skip` directly).
- **`db::open_reader` (V15.8)** — the C# app opens its own SQLite connection; nothing in Rust needed a read-only handle. Restore from git if a future in-process reader appears.
- **`fast_image_resize`** unused dep dropped from `Cargo.toml`. It was declared but never imported.
- **22 inline command handlers** removed from `main.rs` (moved to `commands/*` submodules).
- **2 .NET file-bloat blocks**: `EngineClient.cs` command facade extracted to `EngineClient.Commands.cs`; `ModelSlot` class extracted to `Services/ModelSlot.cs`.

### Security
- **SEC-3 DLL search lockdown hoisted to fn main entry (V15.8b).** `SetDefaultDllDirectories` was previously called inside `async_main` AFTER logging/state-dir init, leaving a window where a planted DLL pulled in during logger init could be loaded before the lockdown. Now the very first statement in `fn main` before tokio spins up. See DECISIONS.md 2026-05-17.
- **TDR safety — clip_text.rs::session.run was missing the classify_inference_error wrap (V15.8b).** A DirectML TDR during a `embedTextQuery` IPC call would have been mis-classified as a regular session error; the engine would have kept retrying against a dead device. Now uniformly guarded across all 5 `session.run` sites in `models/`.
- **Process-file GPU-dead short-circuit (V15.8b).** Once `coord.mark_gpu_dead()` fires, `process_file` returns immediately for every remaining queued file (empty embeddings, `failed=false`) instead of attempting doomed inference. Drains a stalled Discovery queue in microseconds instead of stalling on GPU calls forever.
- **SEC-5 TOCTOU defense (V15.8).** Bracketed reparse-point check around `create_dir_all` in restructure apply. Closes the pre-existing window where a junction planted on a pre-existing folder under `library_root` could redirect a move outside the root. See DECISIONS.md 2026-05-17.
- **SEC-7 trash-log HMAC enforcement (V15.8).** Removed the no-HMAC accept path. Any tampered or legacy entry without an HMAC suffix is now rejected. See DECISIONS.md 2026-05-17.
- **Reserved device name expansion (V15.8).** `COM0` + `LPT0` added to `is_safe_filename`'s rejected set. Microsoft's Naming Files docs list these as reserved even though the original numbering started at 1.
- **Zip per-entry bomb cap (V15.8).** 1 GiB per-entry limit complements the existing 2 GiB cumulative limit — a single huge entry can no longer exhaust the budget before others are inspected.
- **SEC: `is_safe_filename` defense-in-depth.** See Fixed above; the `renameFiles` destination check still applied, but the function's documented "single Normal path component" guarantee was leaky.
- **Telemetry-string scan** posture preserved: 22 deny-listed substrings + outbound traffic restricted to 6 allowed hosts (HuggingFace, GitHub, nvidia.com download, nvidia.com developer, plus 2 XAML namespace tokens). CI binary scan + new source-URL scan both enforce.
- **CI source URL allowlist** is the new defense layer: catches a contributor who adds a brand-new URL not on the deny-list. Flips posture from "anything except these 22 strings" to "only these 6 documented hosts".

---

## Earlier versions

Versions V11–V15.2.1 predate this CHANGELOG. Their release notes live in commit messages and `shared/docs/STATE.md` (top-of-file entries, latest-first). Anyone wanting the history can `git log --oneline` or read STATE.md from the bottom up. Future releases (V15.3+) populate this file at tag time.

[Unreleased]: ./compare/V15.2.1...HEAD

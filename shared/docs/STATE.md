# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.

## 2026-05-16 — V15.3.1 macOS CI fix + V15.3.2 test/bench expansion + privacy gates

Two-pass session.

**V15.3.1 — Make all 3 GitHub workflows green again.** The `macOS app` workflow had been red since V15.2 because the engine-startup smoke step asserted `grep -q '"executionProvider"' engine.stdout`, but the macOS `EngineInfo` struct has no such field (executionProvider is the Windows-only ORT execution-provider picker output; macOS dispatches through MLX + ANE + CoreML with no exposed enum). Two iterations to land the fix: first removed the bogus assertion (commit 131780f); then the diagnostic dump showed engine.stdout was 0 bytes because the macOS engine writes IPC events to STDERR (per `apple/.../IPCSink.swift:108`, `FileHandle.standardError.write(contentsOf: blob)`). Changed the ready-event grep to scan engine.stderr instead (commit 06dcecc). Windows engine writes to stdout — that asymmetry is documented in both workflow files now. All 3 CI surfaces green on `main`.

**V15.3.2 — Tier-1 test + bench + privacy gates.**
- **N7 IPC round-trip tests.** Added two tests to `ipc::tests`: `every_command_variant_round_trips` encodes + decodes every `CommandPayload` variant (26 today) and asserts `std::mem::discriminant` survives; `start_scan_root_path_round_trips` proptests arbitrary `[\PC]{1,200}` paths through StartScan encode/decode. Catches serde rename drift between Rust + Swift schema and missing `#[serde(default)]` regressions.
- **N7 dbwriter ingest-idempotence tests.** Three new tests against `pipeline::dbwriter` exercising `INSERT_FILE_SQL` directly: duplicate inserts produce 1 row (ON CONFLICT contract); duplicate inserts UPDATE size/modified (not just IGNORE); proptest with random mix asserts `row count == distinct paths` regardless of insertion order. Guards the scan resume cursor + People-tab dedup invariants.
- **N3 criterion bench scaffold.** Restructured the engine crate as lib+bin (added `[lib] name = "fileid_engine" path = "src/lib.rs"` re-declaring the 13 submodules) so `benches/*.rs` can `use fileid_engine::*`. Two bench targets shipped: `tagging_hashes.rs` (compute_dhash + resize_rgb_nearest at multiple input sizes) and `face_clustering_5k.rs` (cluster() on 5K synthetic 512-d L2-normalized embeddings). Smoke-verified with `cargo bench -- --quick`: dhash ~360ns regardless of input; resize_rgb_nearest ~184ns. Dev compile cost +30% (modules build once for lib + once for bin); runtime cost zero (shipped bin still gets release LTO).
- **N9 cargo audit re-tightened.** Flipped `.github/workflows/windows-engine.yml` from `continue-on-error: true` back to `cargo audit --deny warnings`. Paired with a new `actions/cache@v4` step that caches `~/.cargo/advisory-db` keyed weekly so the audit corpus stays stable across CI runs. Triage path documented in DECISIONS.md (bump dep version OR add `--ignore RUSTSEC-YYYY-NNNN` WITH a rationale entry; never silent).
- **N9 source URL allowlist scan.** New CI step (both Windows + macOS workflows) scans every `*.{rs,cs,xaml,xaml.cs,swift}` source for any `https?://` URL and asserts every host is on the 6-entry allowlist (`huggingface.co`, `github.com`, `developer.download.nvidia.com`, `developer.nvidia.com`, plus the two XAML namespace identifiers). Source-scan (not binary-scan) because a binary URL scan drowns in false positives from ORT/rustc/windows-rs strings. Flips the no-telemetry posture from "ship anything except these 22 deny-listed strings" to "ship only these 4 documented egress hosts". Belt + suspenders.

**Test counts:** Rust 74 (was 71, +3 dbwriter), IpcSchema 30, FileID.App.Tests 28, FileID.Theme.Tests 16 = **148 total** (was 127, +21 net counting the new IPC tests + criterion smoke).

**Still pending (NEXT.md V15.3):** N5b mock-heavy .NET tests (gated on EngineProcessManager + IpcDispatcher extraction from `EngineClient.cs`), Tier-2 macOS extractions (user verifies on Mac), Windows XAML user-control extraction, parity tests, chaos harness, Phase 10 a11y, Phase 11 release engineering, Phase 14b code-comment hygiene sweep.

## 2026-05-15 (afternoon) — V15.3 Phase 6 + 7 + 11 CI hardening

Continuation of the morning's V15.3 engagement. This session locked in the lint + test + CI gates from Phases 6, 7, and 11 of the polish-mochi plan.

**Rust lint gate (Phase 6):** `cargo clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings` is now **clean**. Approach: targeted `[lints.clippy]` allows for style-only pedantic rules (`uninlined_format_args`, `doc_markdown`, `manual_let_else`, etc.) with documented justifications, leaving correctness lints as `warn → deny`. Per-site fixes for the 4 real lints that remained (PathBuf debug formatting in `restructure_apply.rs`, BITMAPINFO struct-init in `shell/thumbnail.rs`, &&str to_string in `logging.rs`, `!=` redundancy in `pipeline/deep_analyze.rs`). Zero `TODO`/`FIXME` in production code; zero `.unwrap()` outside `#[cfg(test)]` + `fn main()`; 33 `#[allow(dead_code)]` annotations remain as documented Phase 5+ placeholders.

**.NET lint gate (Phase 6):** `dotnet format --verify-no-changes` is now **clean**. Approach: ran `dotnet format` once to auto-apply IDE0003 (this. simplification) across all view code-behind files; added IDE1006 (private-field-prefix style) to `Directory.Build.props` NoWarn list with a documented justification. `<TreatWarningsAsErrors>true</TreatWarningsAsErrors>` + `<AnalysisLevel>latest-recommended</AnalysisLevel>` + `<EnforceCodeStyleInBuild>true</EnforceCodeStyleInBuild>` already in place; no csproj edits needed.

**Property tests (Phase 7):** `proptest = "1"` adopted as Rust dev-dep. 9 property tests now ship across `util/path_safety`, `util/zip`, and `pipeline/face_clustering`. **proptest paid for itself by catching two real bugs the example tests missed:**
- `is_safe_filename("A\\")` was accepted because `std::path::Path::components()` silently strips trailing separators. Fixed by adding an explicit `contains('/') || contains('\\')` reject before the components walk. Comment cites the proptest test as the regression guard. Security-relevant: this function is the path-traversal guard for `renameFiles`.
- `identity_clustering::cluster` produced **non-deterministic cluster IDs across runs** because `for (_, members) in root_members` iterated a HashMap in random order. Fix: collect into a `Vec`, sort by root, iterate sorted. Without this, a re-scan of the same library could renumber the People-tab clusters between sessions (user-visible: "I named Person #1 as Mom, and after a re-scan she's Person #5 now"). Comment cites the proptest test.

**.NET test expansion (Phase 2):** `SafeOpenTests` shipped with 17 cases including a `[Theory]` over 14 executable extensions (`.exe`, `.lnk`, `.bat`, `.ps1`, `.vbs`, etc.) confirming SEC-9's allowlist rejects each. Total `FileID.App.Tests` count: **28** (was 11). Remaining .NET test classes (`EngineProcessManagerTests`, `IpcDispatcherTests`, `ModelInstallerServiceTests`, `ReadStoreTests`, `AppSettingsTests`, etc.) deferred to NEXT.md N5 — each needs significant mock infrastructure (Process, HttpClient, in-memory SQLite).

**Perf scaffolding (Phase 3):** Added `[profile.release-pgo]` to `Cargo.toml` for PGO instrument-train-use flows (8–15% expected on CPU-bound paths; build-time-only cost). Removed `fast_image_resize = "4"` from deps — was declared but never imported, audited via grep. Verified `serde_json::to_writer` is already the direct path in `ipc/sink.rs:90` (the perf-candidate was already realized). Criterion bench scaffold deferred (needs lib+bin crate restructure to expose `pub fn`s to a `benches/` target — tracked in NEXT.md N3).

**CI gate landing (Phase 8):** `.github/workflows/windows-engine.yml` now runs:
- `cargo fmt --check` (formerly placeholder).
- `cargo clippy --all-targets -- -D warnings` (formerly narrowed to specific lint groups).
- `cargo deny check` (new gate, enforces `engine/deny.toml`: license allowlist + advisory + duplicate-version + source allowlist).
- `cargo audit` (was `continue-on-error: true`, now a hard gate).
- Rust toolchain bumped from 1.78 → 1.90 to match `rust-toolchain.toml`.

`.github/workflows/windows-app.yml` now runs:
- `dotnet format --verify-no-changes` (new gate, x64 only).
- `dotnet list package --vulnerable --include-transitive` with an explicit fail on hits (new gate, x64 only).
- `dotnet test FileID.sln` (was IpcSchema-only + `continue-on-error: true`, now runs all test projects + fails on red).

**Pre-commit hook (Phase 11):** `tools/git-hooks/pre-commit` shipped — bash script that runs on every `git commit` to catch what's fixable locally faster than CI can: privacy-string scan + `cargo fmt --check` + `cargo clippy --no-deps -D warnings` + `dotnet format --verify-no-changes` + `swift-format lint` (if installed). Designed to finish in < 15 seconds on a warm cache. `tools/git-hooks/README.md` documents the one-command install: `git config core.hooksPath tools/git-hooks`. `CONTRIBUTING.md` references this.

**Final test count this session:** 69 Rust + 30 IpcSchema + 28 App.Tests = **127 tests, all green** (was 105 at start of session, +22; was 44 at engagement start, +83).

**Still pending (NEXT.md V15.3 follow-ups):** macOS Swift extractions (user verifies on Mac), Windows XAML user-control extraction, remaining .NET test classes, criterion benches (needs lib+bin restructure), cargo-fuzz harness, Phase 9 robustness suite (UI E2E, large-library stress, fault injection, migration roll-forward), Phase 10 a11y + i18n readiness, Phase 11 release-engineering polish (reproducible builds, signing, CI cache).

## 2026-05-15 — Phase 1 bloat reduction + Phase 2 test seed + Phase 3 perf wins (Windows)

Per a comprehensive "trim bloat + comprehensive tests + push perf" engagement (plan in `~/.claude/plans/i-want-you-to-polished-mochi.md`). Phase 1 reorg + Phase 2 test seed + Phase 3 perf wins applied to the Windows side; macOS work pending (user verifies on Mac).

**Windows Rust engine** — `main.rs` 3,463 → 678 LOC (−80.4%) without a single behavior change.
- New `commands/` directory (one submodule per IPC domain): `hardware`, `embed`, `restructure`, `face_clustering`, `bulk`, `trash`, `trash_log`, `deep_analyze`, `prewarm`, `scan`.
- New `util/` directory: `hmac` (HMAC-SHA256 hand-roll + log-tamper key), `path_safety` (filename/traversal guards + `stable_path_hash` — de-duplicated with `dbwriter.rs`), `zip` (hardened extract with slip + bomb + symlink defenses).
- New `logging.rs` (tracing init + panic-hook factory) and `ipc/bounded_read.rs` (`BoundedRead` enum + `bounded_read_line` + `drain_to_newline`).
- `cargo test --release` clean: **58 passed, 0 failed** (was 44 before this work; +14 new).

**Windows .NET app** — `internal sealed partial class EngineClient` split:
- `ViewModels/EngineClient.cs`: 1,378 → 970 LOC (kept process lifecycle, stdout/stderr loops, Apply event router, observable surface, `Set<T>` helper).
- `ViewModels/EngineClient.Commands.cs` (new, 419 LOC): every `*Async` command facade + AutoPilot orchestration (`RunAutoPilotAsync`, `AwaitPhaseAsync`, `AutoPilotStage` enum).
- `Services/ModelInstallerService.cs`: 1,017 → 735 LOC.
- `Services/ModelSlot.cs` (new, 282 LOC + header): `ModelSlot` class + `ModelInstallStatus` enum split out as separate class.
- `dotnet build` clean; `dotnet test` clean (30 IpcSchema tests pass).

**Phase 3 perf wins (Windows engine):**
- `pipeline/tagging.rs`: replaced the **double image decode** (`image::ImageReader::open(&p)` × 2 per file) with a single `memmap2::Mmap` and two `ImageReader::new(Cursor::new(&bytes))` calls. Saves the second open + read per file across every scan (~5 s on a 50k library, more on slow disks).
- `db/mod.rs`: added `PRAGMA cache_spill = 0` to `SETUP_PRAGMAS`. Pins the 64 MB page cache instead of spilling to a temp file mid-transaction. Worst-case write is a 100-row batch (well under cache); spill never wins.

**Phase 2 tests** added inline for the new modules:
- `util/hmac` — 2 RFC 4231 test vectors + long-key + constant-time-eq edge cases.
- `util/zip` — round-trip extract + zip-slip rejection.
- `ipc/bounded_read` — line read, CR/LF strip, EOF, partial-line-at-EOF, oversized rejection, drain resync.
- `util/path_safety` — preserved + already had safe-filename + traversal-rejected tests.

**Documented in `DECISIONS.md`** under five new 2026-05-15 entries: (a) main.rs decomposition rationale, (b) EngineClient partial-class split rationale, (c) mmap decode fast path, (d) `cache_spill=0`. Existing perf candidates (batched CLIP inference, prepare_cached audit, PGO, ORT GPU residency check) are listed in the engagement plan but deferred — they need a criterion benchmark harness or shipped-binary measurements before merging.

**Still pending (per the engagement plan):**
- macOS Swift refactors (LibraryView/PeopleView/RestructureView decomposition; SankeyFlowView layout extraction; ReadStore split + GRDB `cachedStatement` migration; FileIDEngineMain dispatcher extract; FaceClustering decomposition). User to execute + verify on macOS hardware.
- Windows XAML user-control extraction (SettingsView, RestructureView, WelcomeSheet, DeepAnalyzeView).
- `tagging.rs` helper extraction (image_io + geometry submodules) — deferred as secondary cleanup.
- Phase 2 .NET test projects (`FileID.App.Tests`, `FileID.Theme.Tests`) and Phase 2 Swift test extensions (`AppTests/`, extended `EngineTests/` + `SharedTests/`).
- Phase 3 remaining perf candidates needing measurement: batched CLIP image inference, per-worker thread-local buffer pools, `prepare_cached` audit across hot paths, vectorized L2-normalize, JSON encoding via `to_writer` direct, ORT GPU residency check, PGO release profile.

## V15.2.1 (2026-05-14) — Fix three V15.2 regressions + one-button GPU pack

V15.2 shipped three regressions that broke first-launch on the user's machine. Forensics: `engine.jsonl` showed clean engine teardown after the engine was killed by the new C# watchdog; `app.log` showed the rest of the failure cascade.

**Regression 1 — Stdout watchdog killed idle engines.** V15.2's 5-min idle watchdog (`EngineClient.StdoutLoopAsync`) tripped after the engine auto-installed llama runtimes and went legitimately quiet waiting for user input. The watchdog can't distinguish "engine hung" from "engine idle waiting for user"; it punished idle. **Fix:** removed entirely. The engine's parent-PID watchdog covers the inverse case (C# app dying); GPU TDR is caught by V14.9-Y's `is_gpu_dead`; per-command timeouts (WaitForReadyAsync, CudaAutoInstaller's 30 min) are the right granularity.

**Regression 2 — Respawn CAS gate double-bookkeeping.** Immediately after Bug 1 fired, the respawn path set `_isStarting=1` *before* calling StartAsync; StartAsync's own strict V15.2 CAS saw "already starting" and bailed. Net: every auto-respawn was silently dropped. **Fix:** removed the outer CAS in `OnProcessExited`. StartAsync's own gate handles the race.

**Regression 3 — `ModelSlot.PropertyChanged` thread-affinity crash.** After Bug 2 left the engine dead, Install all fired and `slot.Fail("Engine not running")` invoked PropertyChanged from a thread-pool thread. The welcome sheet's x:Bind forwarded it to `TextBlock.Text` → `COMException 0x8001010E` (RPC_E_WRONG_THREAD). Same class of cross-thread XAML violation as the V15.2 BitmapImage fix, different surface. **Fix:** `ModelSlot.Set<T>` now captures the UI DispatcherQueue at construction and marshals every PropertyChanged invocation through `TryEnqueue` when called off the UI thread.

**Feature — One-button GPU Acceleration Pack on welcome sheet.** Per the user's ask. A 4th row appears on the welcome sheet:
- **NVIDIA**: "Unlocks ~15% faster scanning on NVIDIA GPUs (~430 MB)." Live Install button → engine downloads cuDNN via `cudnn_runtime_x64` registry arm. Becomes "Installed" badge once sentinel lands.
- **AMD**: "DirectML is already optimal for your AMD GPU — no install needed." No badge, no button.
- **Intel**: same, "Intel".
- **Qualcomm**: same, "Snapdragon" (DirectML + QNN).
- **CPU only**: "No GPU detected — scanning will run on CPU."
- **Detection pending**: "Detecting GPU…" until engine Ready event arrives.

Wired through the existing `ModelInstallerService` pattern. New `ModelSlot Accelerator` property; new `AcceleratorIsRealInstall` flag distinguishes "real cuDNN install" from "pseudo-installed for non-NVIDIA"; `UpdateAcceleratorForVendor` adapts on engine Info events. Engine side is unchanged (cuDNN registry arm has been there since V14.9-U).

**Cleanup — runtime-pack progress noise.** ~30 `[INSTALL] no slot for model_kind 'llama_runtime_cuda_x64'` warnings per launch came from the auto-installer's progress events reaching `ModelInstallerService` for kinds it doesn't track. Demoted to Debug-level for known auto-installer kinds (`llama_runtime_x64`, `llama_runtime_cuda_x64`, `llama_runtime_vulkan_x64`).

### Files touched (V15.2.1)
- `platforms/windows/src/FileID.App/ViewModels/EngineClient.cs` — removed stdout watchdog; removed outer respawn CAS.
- `platforms/windows/src/FileID.App/Services/ModelInstallerService.cs` — `ModelSlot.Set<T>` UI-thread marshaling; `Accelerator` slot + `AcceleratorIsRealInstall` flag + `UpdateAcceleratorForVendor` + `IsAutoInstallerOnly` helper.
- `platforms/windows/src/FileID.App/Views/WelcomeSheet.xaml` — 4th row for GPU Acceleration Pack.
- `platforms/windows/src/FileID.App/Views/WelcomeSheet.xaml.cs` — `OnAcceleratorActionClicked` + per-row XAML binding helpers (`ShowAcceleratorButton`, `ShowAcceleratorInstalledBadge`, `AcceleratorGlyph`, `AcceleratorIconBrush`, `AcceleratorSize`).

### Verification plan (user)
1. Launch the app. Engine spawns, runtimes auto-install, app sits idle. Wait 10 minutes; engine stays alive (no watchdog respawn line).
2. Welcome sheet shows 4 rows. 4th row reads "GPU Acceleration Pack (NVIDIA) — Unlocks ~15% faster scanning on NVIDIA GPUs (~430 MB)" with live Install button.
3. Click "Install all". All 4 rows download in parallel; progress percentages tick visibly.
4. After installs, scan a folder. Tiles populate with thumbnails. No crash, no `crash-*.txt`. `last-session.txt` ends with `clean_exit=true`.

## Earlier releases (condensed)

Headlines only — for full session notes `git log` or scroll back through this file's history. Decision rationale lives in [`DECISIONS.md`](DECISIONS.md). User-visible release notes live in [`/CHANGELOG.md`](../../CHANGELOG.md).

- **V15.2** (2026-05-14) — Scan crash root-caused: native fast-fail in `ThumbnailService.RenderAsync` from cross-thread BitmapImage construction. Full stability sweep (every P0/P1/P2 audit finding). Last-session breadcrumb detects native crashes the 3 managed sinks miss. CI workflows brought to parity (Windows app publishes + privacy gate + smoke; macOS smoke-launches engine).
- **V15.1** (2026-05-15) — Top-level crash capture (Application + AppDomain + Task scheduler → `crash-*.txt` with last 50 lines of app.log). `_startInFlight` button gate matching macOS `@State startRequested`. cuDNN auto-installer deleted; replaced by Settings → Performance manual button. `StartScanCommand.Rescan` plumbed through DTO + EngineClient.
- **V15.0** (2026-05-15) — Scale to 1M files: streaming discovery, bounded WAL growth, adversarial-input hardening (decompression bomb caps, malformed-image `catch_unwind`, path-traversal SEC), per-file backpressure across the pipeline.
- **V14.9-Y** (2026-05-15) — Safe GPU saturation. TDR safety net + lowered worker priority + concurrency revert (4→2 CLIP, 8→4 SCRFD/ArcFace). Full 15K corpus in 424s @ 35fps, zero hangs.
- **V14.9-V** (2026-05-14) — clip_text install gap, ORT EP wiring, runtime DLL bundling (`onnxruntime.dll` + `DirectML.dll` ship with the build).
- **V14.9-U** (2026-05-14) — Kill the Deep Analyze model-missing banners; auto-install everything on welcome sheet.
- **V14.9-T** (2026-05-14) — Windows live-scan parity with macOS (per-batch summary cards). CUDA registry. Build wizard.
- **V14.9-S** (2026-05-13) — Fixed model-download 404s in welcome sheet (HF repo paths drifted).
- **V14.9-R** (2026-05-13) — Zero-warning Windows build + macOS CI workflow shipped.
- **V14.9-Q** (2026-05-13) — Full code cleanup + warning-banner UI + cross-platform IPC sync.
- **V14.9-P** (2026-05-13) — Windows end-to-end scan completeness pass.
- **V14.9-O** (2026-05-13) — Windows CI unblock + IdentityClustering port + Ctrl+R silent-failure fix.
- **V14.9-N** (2026-05-13) — Welcome ETA garbage + scan stuck on "Discovering" (two user-reported).
- **V14.9-K-M** (2026-05-13) — Risk-tightening + macOS live caption parity + Restructure ApplyBar port.
- **V14.9-G-J** (2026-05-13) — cuDNN verify UX + Deep Analyze live caption + Restructure tier cleanup + scan log access.
- **V14.9-F-A** (2026-05-13) — Start Scan no-op + sidebar-mid-scan crash (Phase A of ship plan).
- **V14.8.5** (2026-05-12) — Downloader timeout + resume rewrite (Qwen 2.5-VL 3B "reading chunk" fix).
- **V14.8.4** (2026-05-11) — Drag, scan-feedback, Settings sync, install-all pre-stamp, telemetry-button removal.
- **V14.8.3** (2026-05-11) — Install-all "Queued" caption + start-scan crash defenses + honest NVIDIA acceleration.
- **V14.8.2** (2026-05-11) — GPU Performance Packs removed (no shippable URLs).
- **V14.8.1** (2026-05-11) — Welcome-sheet install error cross-wiring fix.
- **V14.8** (2026-05-11) — Parity + GPU coverage + hardening pass.
- **V14.7.16** (2026-05-06) — Sidebar toggle button, new icon, [INSTALL] log trail, smoke harness.
- **V14.7.15** (2026-05-05) — Strict-parity strip + bug audit fixes.
- **V14.7.12** (2026-05-05) — Welcome sheet 1:1 macOS parity rewrite.
- **V14.7.11** (2026-05-05) — Welcome polling NPE + full UI/repo audit fixes.
- **V14.7.4** (2026-05-05) — UI is unbroken: encoding, dynamic resize, accessibility, downloader maxed out.
- **V14.7.1–V14.7.3** (2026-05-05) — Encoding fix, FileID logo wiring, bulletproof startup, V14.7 NEXT.md queue closed.
- **V14.7** (2026-05-05) — Unified build dispatcher + comprehensive audit pass.
- **V14.6** (2026-05-05) — Deep Analyze + ship plumbing + pixel-perfect polish.
- **V14.5** (2026-05-03) — Security pass + bug sweep + every macOS-only feature except VLM.
- **V14.4** (2026-05-03) — Real thumbnails, smooth LavaLamp, working welcome, every macOS UX surface.
- **V14.3** (2026-05-02) — Real ML loop + every shell helper + bulk action sheets + WiX MSI.
- **V14.2** (2026-05-02) — Tier-by-tier parity push (Settings, AutoPilot scaffold, preview sheet, cheat sheet, tab crossfade, real tags).
- **V14.1** (2026-05-02) — Window-size fix + UX polish + perf wins from the audit.
- **V14** (2026-05-02) — Ship-plan execution: LavaLamp restored, Restructure E2E, perf surface, IPC additions.
- **V13** (2026-05-02) — Quality sweep + Install All works + GPU/perf surface.
- **V12.2** (2026-05-02) — App launches end-to-end + clean Desktop install + consolidated README.
- **V12.1** (2026-05-02) — Bug fixes + unified build script + WiX Burn bundle (Pattern 2).
- **V12** (2026-05-02) — Phase 2 → 8 scaffolds across the Windows port.
- **V11** (2026-05-02) — Phase 1 of Windows port: app shell + theme parity + sidebar + welcome.
- **V10** (2026-05-02) — Multi-platform repo restructure + Phase 0 of Windows port.
- **V9** (2026-04-30) — V1 deletion, organizational pass, security audit.
- **V8.5** (2026-04-30) — Restructure V3, Sankey perf + polish, V5 cleanup pass.
- **V7** (2026-04-30 evening) — Restructure redesign (Sankey + dual-pane Tree) + Deep Analyze coverage extended to video + doc.
- **V2** (2026-04-29) — Face clustering V2 (IdentityClustering, two-pass density) + split-process rewrite (engine as child of app over JSON stdio).

---

Earlier history is in `~/.claude/plans/in-media-library-i-temporal-acorn.md`.

# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## V15.3.2 — Tier-1 test/bench/privacy expansion (2026-05-16)

Following V15.3.1: shipped IPC 26-variant round-trip + StartScan path proptest (`ipc/mod.rs`), dbwriter ingest-idempotence proptest (3 cases against `INSERT_FILE_SQL`), criterion bench scaffold via crate `lib+bin` restructure with two benches (`tagging_hashes.rs`, `face_clustering_5k.rs`), `cargo audit` re-tightened to hard gate w/ weekly advisory-DB cache, and a source URL allowlist scan (both Windows + macOS workflows) that asserts every `https?://` URL in source matches the 4-host egress allowlist. Test count: 74 Rust + 30 IpcSchema + 28 App + 16 Theme = **148**. Detail in DECISIONS.md 2026-05-16 entries.

---

## V15.3.1 — macOS CI fix (2026-05-16)

Single change: removed the `executionProvider` grep assertion from `.github/workflows/macos.yml`'s engine smoke step. That field is Windows-only (ORT EP picker output); macOS engine never emitted it, so the step failed 100% of the time on macOS regardless of engine health. Pre-existing — failing since V15.2. Fix detail in `DECISIONS.md` 2026-05-16 entry.

---

## V15.3 — Polish engagement follow-ups (2026-05-15)

The polish-mochi engagement landed Phases 1–8 partially. Plan: `~/.claude/plans/i-want-you-to-polished-mochi.md`. Done so far: Windows-side `main.rs` decomposition (3463→678 LOC), `EngineClient.cs` + `ModelInstallerService.cs` partial splits, 135 tests across Rust + IpcSchema + App.Tests (up from 44), Phase 6 lint gates green (`cargo clippy -D warnings`, `dotnet format --verify-no-changes`), 3 perf wins (mmap decode, `cache_spill=0`, prepare_cached hoist), CI gates tightened, pre-commit hook + `tools/git-hooks/`. proptest caught 2 real bugs: `is_safe_filename("A\\")` accepted (SEC); cluster IDs non-deterministic across re-scans (UX). Both fixed.

### Still pending

**N1 — macOS Swift extractions (user verifies each on Mac).**
- `Database/ReadStore.swift` (999) split + GRDB `cachedStatement` migration — **largest single macOS read-path perf win**.
- `LibraryView.swift` (1465), `PeopleView.swift` (1428), `RestructureView.swift` (1478) — subview extraction. Also a SwiftUI body-diff perf win.
- `SankeyFlowView.swift` — remaining path-math after the `SankeyLayout.swift` nested-types extraction.
- `FileIDEngineMain.swift` (758) → `IPCDispatcher.swift` + `EngineLifecycle.swift`.
- `FaceClustering.swift` (1019) → `ArcFaceEmbedder.swift` + `HNSWClusterer.swift` + `IdentityMerger.swift`.

**N2 — Windows XAML user-control extraction (UI smoke required).** Six controls: `PrivacyDisclosureCard`, `PerformancePackCard` (from `SettingsView.xaml`); `StatHeroCard`, `RecommendationCard` (from `RestructureView.xaml`); `ModelInstallerCard` (from `WelcomeSheet.xaml`); `ModelPickerCard` (from `DeepAnalyzeView.xaml`).

**N3 — Phase 3 perf candidates needing criterion benches.** Already shipped: mmap decode, `cache_spill=0`, `prepare_cached` hoist, PGO release profile (`[profile.release-pgo]`), `serde_json::to_writer` (audited — already direct), `fast_image_resize` (audited + removed as unused dep). Still ahead, each gated on a bench delta recorded in `DECISIONS.md`:
- Batched CLIP image inference (1/call → 8/call).
- Per-worker thread-local buffer pools in `pipeline/tagging::process_file`.
- Vectorized L2-normalize via `wide::f32x8`.
- `crossbeam-channel` in IpcSink hot path (bench vs. tokio mpsc).
- ORT GPU residency audit.
- Criterion bench infrastructure (needs crate `[lib]` + `src/lib.rs` re-exports).

**N4 — .NET app perf candidates needing measurement.**
- `System.Text.Json` source generators for `FileID.IpcSchema` types.
- `SqliteCommand` reuse in `ReadStore.cs`.
- Batch UI event dispatch (16ms / 60Hz coalescing) in `EngineClient.cs`.

**N5 — Remaining .NET test classes.** Done so far in this engagement: `PathRedactorTests`, `UndoStackTests`, `SafeOpenTests`, `AppSettingsTests` (36 cases total). Still ahead, all need mock infrastructure:
- `EngineProcessManagerTests` — mock `Process`.
- `IpcDispatcherTests` — synthetic stdout stream.
- `EngineClientTests` — state machine.
- `ModelInstallerServiceTests` — mock HTTP via DelegatingHandler.
- `ReadStoreTests` — in-memory SQLite.
- New `FileID.Theme.Tests` project: `SpringEasingTests`, `ReducedMotionTests`, `BadgePillTests`, `ThemedSegmentedControlTests`.

**N6 — macOS Swift test expansion (user runs on Mac).** New `AppTests/` target: `EngineClientStateMachineTests`, `ReadStoreTests`, `ClusterSuggestionsTests`, `CLIPTokenizerParityTests`. Extend `EngineTests/`: `ScanCoordinatorTests`, `JobQueueTests`, `TaggingTests`, `DBWriterTests`, `IdentityClusteringTests`, `DeepAnalyzeStateMachineTests`, `RestructureTests`. Extend `SharedTests/`: `StreamingDownloadTests`, expanded `TagWriterTests`, `PathRedactionTests`.

**N7 — Advanced testing.** Done: 9 Rust proptests across `util/path_safety`, `util/zip`, `pipeline/face_clustering` (caught 2 real bugs). Still ahead:
- Rust proptests for `pipeline/dbwriter` (ingest idempotence), `models/clip_tokenizer` (round-trip), `ipc/mod.rs` (every variant round-trip).
- `cargo-fuzz` harness for `ipc::mod.rs` decoder + `pipeline::dbwriter` row deserializer; weekly cron.
- .NET property tests via `FsCheck.Xunit` (for `PathRedactor`, `UndoStack`, `IpcCoder`).
- Swift property tests via `@Test(arguments:)` parameterized.
- Cross-platform parity tests in `shared/parity-tests/`: `path_hash`, `dHash`, CLIP tokenizer, FolderClassifier, HNSW assignments. **Biggest single regression guard.**
- Snapshot tests (macOS) via `swift-snapshot-testing` for the six main views.

**N8 — Lint sweep finalization.** Done: `cargo clippy --all-targets -- -D warnings` clean (tuned `[lints.clippy]` Cargo.toml with documented allows for style-only pedantic rules; fixed 4 real lints); `dotnet format --verify-no-changes` clean. Still ahead:
- Sweep 33 remaining `#[allow(dead_code)]` annotations — most are Phase 5+ placeholders, but the audit should retire any that are now used.
- Swift: write `.swift-format` config + add CI `swift-format lint --strict` gate (user runs on Mac).

**N9 — CI gate landing.** Done in `.github/workflows/windows-engine.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo deny check`, `cargo audit --deny warnings` (hard gate, paired with an `actions/cache` of `~/.cargo/advisory-db` keyed weekly for stability). Done in `windows-app.yml`: `dotnet format --verify-no-changes`, `dotnet list package --vulnerable` (hard gate), `dotnet test FileID.sln` runs every project. Still ahead:
- `swift-format lint --strict` job in `macos.yml`.
- Coverage gate (drop > 2 pp blocks merge against `COVERAGE.md` baseline).
- Parity gate (depends on N7 parity tests existing).
- Fuzz cron (depends on N7 cargo-fuzz harness existing).

**N10 — Docs polish.** Done: `COVERAGE.md`, `TESTING.md`, `CONTRIBUTING.md` shipped. Still ahead:
- Refresh `ARCHITECTURE.md` component + IPC sequence diagrams.
- Refresh `ONBOARDING.md` 10-minute new-contributor guide.
- Refresh per-platform `CLAUDE.md` to reflect the new `commands/` + `util/` (Windows) and (forthcoming) `Database/Queries/` (macOS) module maps.

### Robustness + a11y + release engineering (Phases 9–11 — new scope)

**Phase 9 — Robustness suite.** WinAppDriver + XCUITest E2E smoke. Large-library stress (50K, 100K, 500K). SIGKILL-mid-scan recovery. Two-app-instance race. Disk-full simulation. Network drop. GPU TDR. TOCTOU + ACL edge cases. Image decompression bombs. Unicode (NFC vs NFD). Migration roll-forward + `PRAGMA integrity_check`. DB backup + restore. Memory soak (10× iterate runs; RSS plateau < 50 MB growth).

**Phase 10 — Accessibility + i18n readiness.** `AutomationProperties.{Name,HelpText}` on every interactive control (Windows); `accessibilityLabel` + `accessibilityHint` on every interactive view (macOS). Keyboard-only walkthrough. Color-blindness audit on gold/lavender/cyan/pink palette. Reduced-motion respect. High-contrast mode. String extraction to `.resw` / `.strings` (English-only fine; wires future translation work). IPC error codes (not English strings).

**Phase 11 — Release engineering.** Reproducible builds via `SOURCE_DATE_EPOCH`. EV cert + `notarytool` signing. Anti-malware false-positive procedure documented in `SHIP.md`. CI cache via `actions/cache` (5–15 min/run saved). `git tag vX.Y.Z` → automated signed-build artifact upload. Pre-commit hook shipped at `tools/git-hooks/pre-commit`. Editor config bundle: `.vscode/{extensions,settings,launch}.json`.

---

## Older follow-ups (archived)

Items from V15.2 down to V14.7 follow-up queues are no longer load-bearing — most were closed at the time they were written, the rest were rolled into V15.0–V15.3 work. The original text lives in `git log shared/docs/NEXT.md`. Genuinely-still-pending leftovers from those rounds:

- **V15.1-N1 Rescan UI affordance** — `StartScanCommand.Rescan` is wired through the IPC DTO + `EngineClient.StartScanAsync(rootPath, rootDisplay, rescan)` but has no UI surface. Add a Sidebar context-menu "Re-scan everything" or a Settings → Library "Force re-scan files even if up to date" toggle.
- **V15.1-N4 / V14.9-Y-N2 WIC native JPEG decode** — `Win32_Graphics_Imaging` features already in Cargo.toml. `IWICImagingFactory::CreateDecoderFromFilename` is generally 15–30% faster than zune-jpeg on photo JPEGs. Pure code add in `pipeline/tagging.rs::load_image_rgb`. Higher priority since V15.0 incremental rescan exposed JPEG decode as the dominant per-file CPU cost on warm-cache scans.
- **V14.9-Y-N3 Real-time VRAM monitor** — `IDXGIAdapter3::QueryVideoMemoryInfo` polled per batch to populate a Settings card showing VRAM pressure; would make the empirically-derived VRAM_PER_POOL_INSTANCE_MB constant in tagging.rs auditable rather than guessed.
- **V14.9-Y-N4 FP16 ONNX variants** — generally 1.5–2× throughput on consumer GPUs that support FP16 (most do). Cost: weight retraining/conversion, careful eval against the deterministic clustering parity guard.

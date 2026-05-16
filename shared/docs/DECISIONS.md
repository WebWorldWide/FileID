# Architecture Decisions Log

> Append-only. One entry per non-obvious decision. Future sessions read this to understand *why* the code looks the way it does — not just *what* it does.

> **Format:** `## YYYY-MM-DD — Title`
> Body: short paragraph stating the decision, the alternatives considered, and the reason for the choice. If a decision is later reversed, add a new entry that supersedes the old one (don't edit history).

---

## 2026-05-16 — `cargo audit` re-tightened to hard gate (V15.3 N9)

Reverses the earlier softening (2026-05-15: `continue-on-error: true` because the CI advisory DB drifted from the local one). The new posture pairs two changes:

1. **Hard gate.** `.github/workflows/windows-engine.yml` step "Audit dependencies" runs `cargo audit --deny warnings`. Any advisory against any direct or transitive dep fails the build.
2. **DB cache.** Adds an `actions/cache@v4` step before audit that caches `~/.cargo/advisory-db` keyed on `runner.os` + `github.run_number`. Because the cache key includes the run number, every run gets its own entry, but `restore-keys` falls back to the most-recent entry — so most runs reuse the prior DB instead of pulling whatever HEAD `RustSec/advisory-db` happens to be at. This bounds advisory churn between CI runs, eliminating the "CI red on Monday morning because someone published a transient advisory" failure mode that drove the original softening.

Triage path when this gate fires: (a) bump the dep version, OR (b) add `--ignore RUSTSEC-YYYY-NNNN` to the command WITH a one-line rationale appended to this DECISIONS.md. Never `--ignore` without the DECISIONS entry. Concurrent `cargo deny check` step continues to enforce `engine/deny.toml` (license + ban + source allowlist + duplicate-version) — `cargo audit` covers the advisory dimension specifically.

Local-verification reference: at lock time (2026-05-16) `cargo audit` exits 0 against `Cargo.lock` containing 372 deps after the criterion bench scaffold landed.

---

## 2026-05-16 — `criterion` adopted as Rust micro-bench dep (dev-only); engine restructured lib+bin

V15.3 N3. Two coupled changes:

**1. `criterion = "0.5"` dev-dep.** Standard Rust bench framework (no realistic alternative — `iai` measures cache misses but not wall time; `divan` is newer and less battle-tested). `default-features = false` + `cargo_bench_support` only: skips the plotters/HTML-report machinery, which we don't need in CI. Zero runtime impact, zero shipped-binary bloat, zero telemetry. Used to track regressions on `compute_dhash`, `face_clustering::cluster`, and (forthcoming) `ipc::sink`, `clip_tokenizer`, `HNSW` insert/search.

**2. Engine restructured from bin-only to lib+bin.** Adds `[lib] name = "fileid_engine" path = "src/lib.rs"` alongside the existing `[[bin]]`. `src/lib.rs` declares the same 13 submodules as `src/main.rs` (`pub mod commands;` etc.). This lets `benches/*.rs` and any future integration tests `use fileid_engine::*` without going through stdin/stdout. The bin still owns its own `mod` declarations and compiles its own copy (~30% dev-compile cost; runtime cost zero — the shipped bin still gets release LTO independently). The alternative — refactoring `main.rs`'s 678 LOC of setup into a lib `pub fn run()` so the bin becomes a one-liner — was deferred as out-of-scope for the bench-enablement goal; the duplicate-compile trade-off is the standard Cargo workaround for bin-only crates wanting bench scaffolding without touching the bin's entry path.

Two bench targets initially: `tagging_hashes.rs` (dhash + resize_rgb_nearest at multiple input sizes) and `face_clustering_5k.rs` (cluster() on 5K synthetic 512-d L2-normalized embeddings, sample-size = 10 because clustering 5K faces is a multi-second operation). Both verified locally with `cargo bench -- --quick`.

---

## 2026-05-16 — macOS smoke drops `executionProvider` assertion

`.github/workflows/macos.yml`'s engine-startup smoke step was failing on every push because it asserted `grep -q '"executionProvider"' engine.stdout` — but the macOS `EngineInfo` struct (`platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift:124`) has no such field. The check was added in V15.2 with a comment claiming "parity with windows-engine.yml's startup + EP probe," but the parity assumption was wrong: `executionProvider` exists on Windows because the engine picks between ORT execution providers (DirectML / CUDA / OpenVINO / QNN); on macOS the ML pipeline runs on MLX + Apple Neural Engine + CoreML, dispatched by the OS without an enum to expose. The assertion could never succeed on macOS regardless of engine health.

Removed the 5-line block; kept the load-bearing `"ready"` event check (proves the engine reached IPC handshake and exited cleanly on stdin EOF). The Windows engine's own `executionProvider` smoke (`windows-engine.yml`) is unchanged — it's correct for Windows. Future cross-platform smoke parity should compare via a smaller invariant set: `version` field present, `pid` present, exit-on-stdin-EOF within budget.

---

## 2026-05-15 — V15.3 Phase 6 + 8 polish: lint-gate tightening, CHANGELOG adoption

Multiple coordinated edits in one engagement; logging as one entry for digestibility.

**Rust clippy posture.** The Cargo.toml `[lints.clippy] pedantic = "warn"` config + a CI gate of `-D warnings` generated ~413 errors against the existing codebase, the majority style-only pedantic noise rather than real bugs. Approach: keep the pedantic group at `warn`, then add per-lint `allow` entries with one-line justifications for the style-only rules (`uninlined_format_args`, `doc_markdown`, `too_many_lines`, `too_many_arguments`, `manual_let_else`, `cast_possible_wrap`, `map_unwrap_or`, `manual_midpoint`, `manual_is_multiple_of`, `unchecked_duration_subtraction`, `redundant_closure`, `needless_continue`, `needless_range_loop`, `large_stack_arrays`, `single_char_pattern`, `ptr_eq`, `needless_borrow`, `match_same_arms`, `manual_range_contains`, `type_complexity`, `items_after_test_module`, `result_large_err`, `trivially_copy_pass_by_ref`, `many_single_char_names`, `struct_field_names`, `ptr_cast_constness`, `stable_sort_primitive`, `if_same_then_else`). Real correctness lints stay `deny`. Fixed the 4 actual problems per-site: `&&str.to_string()` in `logging.rs`, `format!("{:?}", PathBuf)` in `restructure_apply.rs`, the BITMAPINFO struct-init pattern in `shell/thumbnail.rs`, and a `match`-as-if-let in `pipeline/deep_analyze.rs`. Result: `cargo clippy --all-targets -- -D warnings` is now a green hard gate.

**.NET format posture.** Ran `dotnet format FileID.sln` once to auto-apply `IDE0003` (this. simplifications) across every view code-behind file. Added `IDE1006` (private-field-underscore-prefix naming convention) to `Directory.Build.props`'s `NoWarn` list — WinUI 3 code-behind has x:Name'd fields that show up as un-prefixed and mass-renaming would touch every code-behind with no correctness gain. Result: `dotnet format --verify-no-changes` is now a green hard gate.

**CI gate landing.** `.github/workflows/windows-engine.yml`: clippy step narrowed-to-deny replaced with full `-D warnings`; added `cargo deny check` step (enforces `engine/deny.toml`'s license + advisory + dup-version + source allowlists); `cargo audit` flipped from `continue-on-error: true` to hard gate; Rust toolchain bumped 1.78 → 1.90 to match `rust-toolchain.toml`. `.github/workflows/windows-app.yml`: added `dotnet format --verify-no-changes` and `dotnet list package --vulnerable` (hard) gates; `dotnet test` widened from IpcSchema-only with `continue-on-error: true` to every project in `FileID.sln` as a hard gate.

**Pre-commit hook.** Shipped `tools/git-hooks/pre-commit` (bash; works on Windows Git-Bash + macOS). Privacy-string scan + `cargo fmt --check` + `cargo clippy --no-deps -- -D warnings` on changed Rust files + `dotnet format --verify-no-changes` if any .cs changed + `swift-format lint` if installed and .swift changed. Designed to run in < 15 s on a warm cache. One-command install per `CONTRIBUTING.md`: `git config core.hooksPath tools/git-hooks`.

**CHANGELOG.md adopted.** Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). One section per shipped version with Added/Changed/Fixed/Removed/Security. Versions prior to V15.3 not back-filled — their notes live in commit messages + `STATE.md` (top-of-file entries, latest-first). Future tagged releases populate this file at tag time.

**`fast_image_resize` dropped from `Cargo.toml`.** Audit found zero `use fast_image_resize` / `fir::` references across `engine/src/`. The dep was declared as a Phase-3 perf candidate but never imported. Removed to slim the dep tree; will re-add at the call site if a future criterion bench (NEXT.md N3) shows it's needed.

**PGO profile added.** `[profile.release-pgo]` in `Cargo.toml`. Two-pass: `RUSTFLAGS="-Cprofile-generate=/tmp/pgo-data"` build + `iterate.ps1` train + `RUSTFLAGS="-Cprofile-use=/tmp/pgo-data/merged.profdata"` re-build. Inherits `release` so LTO + opt-level + strip stay aligned. Expected 8–15% throughput on CPU-bound paths.

## 2026-05-15 — `is_safe_filename` rejects trailing path separators (Windows, SEC)

`util::path_safety::is_safe_filename` is the path-traversal guard for the `renameFiles` IPC handler — it must accept only single-component Normal names. Adding property-based tests via `proptest` (V15.3 Phase 7 dev-dep) immediately found the minimal failing input `"A\\"`: the function accepted it because `Path::components()` silently strips trailing separators, so a "name" ending in `\` looks like one Component::Normal("A"). Fix: defensively reject any input containing `/` or `\` before reaching the components walk. Test `util::path_safety::tests::any_string_with_slash_is_rejected` (proptest) is now the regression guard. No prod exploit was reachable — bulk rename's destination check still applied — but the defense-in-depth posture of "this function rejects anything that isn't strictly a filename" was leaky. proptest paid for itself on its first run.

## 2026-05-15 — `proptest` adopted as Rust property-testing dep (dev-only)

V15.3 Phase 7: added `proptest = "1"` as a Rust dev-dep so we can write randomized-input invariant tests next to the example-based ones. Dev-only — zero runtime impact, zero shipped-binary impact, doesn't enter the release binary's privacy-string scan surface. Initial four invariants land on `util/path_safety.rs`: (1) any string containing `/` or `\` is rejected; (2) any string with leading or trailing whitespace is rejected; (3) `stable_path_hash` is case-insensitive (NTFS invariant); (4) `stable_path_hash` is deterministic. Alternatives considered: `quickcheck` (older, smaller; same idea), `arbitrary` + a hand-written generator (more boilerplate). `proptest` won because of its built-in shrinking — when a property fails, it shrinks the input to the minimal counterexample, which is how it surfaced `"A\\"` immediately.

## 2026-05-15 — `cargo-deny` configured at `engine/deny.toml`

V15.3 Phase 6: added `deny.toml` to enforce four invariants at PR time (once the Phase 8 CI gate lands): (1) every dep's license is on an SPDX allowlist (Apache-2.0, MIT, BSD-{2,3}-Clause, ISC, Unicode-3.0, Zlib, MPL-2.0, CC0-1.0, 0BSD) — no GPL/AGPL leakage; (2) no RUSTSEC-flagged versions; (3) `multiple-versions = "warn"` flags accidental v0.x / v1 splits that bloat the binary; (4) `unknown-registry = "deny"` + `unknown-git = "deny"` prevents accidental git-dep introduction. Tool-only (no Cargo.toml dep) — `cargo install cargo-deny` for contributors, `cargo deny check` for the gate. Alternatives: `cargo-bundle-licenses` (read-only, no enforcement) — rejected because we want enforcement at PR time.

## 2026-05-15 — `FileID.App.Tests` xUnit project (Windows, Phase 2)

The .NET test surface was IpcSchema-only (30 tests). V15.3 Phase 2 adds `Tests/FileID.App.Tests/` targeting the same WinUI 3 TFM as the app (`net8.0-windows10.0.19041.0`) with `<UseWinUI>true</UseWinUI>`, `xunit` + `coverlet.collector` + `xunit.runner.visualstudio`, plus an `[assembly: InternalsVisibleTo("FileID.App.Tests")]` declaration in `FileID.App/AssemblyInfo.cs` so xUnit can exercise `internal` types like `PathRedactor` and `UndoStack`. 11 tests land first (PathRedactor: 6, UndoStack: 5); remaining classes (`EngineProcessManagerTests`, `IpcDispatcherTests`, `ModelInstallerServiceTests`, `ReadStoreTests`, `AppSettingsTests`, etc.) are listed in NEXT.md N5. Test framework choice locked: xUnit + coverlet match the existing `FileID.IpcSchema.Tests` project so contributors only learn one stack.

## 2026-05-15 — `COVERAGE.md`, `TESTING.md`, `CONTRIBUTING.md` shipped

V15.3 Phase 5 + 8 docs: three new files under `shared/docs/`. `COVERAGE.md` is the per-module line-coverage rollup with targets + actuals + exempt-list (LavaLamp, GPU shaders, Media Foundation video, ORT session loads, `fn main`); it's the source of truth for the > 2 pp drop merge gate landing in Phase 8. `TESTING.md` is the testing philosophy + per-platform commands + how-to-add guide (example/property/integration/parity/fuzz/snapshot). `CONTRIBUTING.md` is the 30-minute onboarding guide for new contributors with the seven hard rules (no telemetry, path redaction, no new deps without DECISIONS entry, single-writer DB, no `--no-verify`, no silent lint suppression, no touching LavaLampBackground). All three documents reflect the actual code shape as of 2026-05-15 — they will rot, see NEXT.md N10 for the polish-pass cadence.

## 2026-05-15 — Engine `main.rs` decomposed into `commands/` + `util/` (Windows)

Phase 1 cleanup: the engine `main.rs` had grown to 3,463 LOC because every IPC command handler lived in one file. Split it into a `commands/` directory (one submodule per domain: `hardware`, `embed`, `restructure`, `face_clustering`, `bulk`, `trash`, `trash_log`, `deep_analyze`, `prewarm`, `scan`) plus a `util/` directory (`hmac`, `path_safety`, `zip`) and a `logging.rs` + `ipc/bounded_read.rs`. Result: `main.rs` 3,463 → 678 LOC (−80.4%) with zero behavior change; the dispatcher (`handle_line`) now delegates to `commands::*::handle_*`. Bonus: `stable_path_hash` is no longer duplicated between `main.rs` and `dbwriter.rs` — single source in `util/path_safety.rs`.

Why directory-based, not partial files (Rust `#[path = "..."] mod foo;`)? Because the existing pattern in this crate is already directory-based (`db/`, `ipc/`, `models/`, `pipeline/`, `shell/`), and command-domain submodules give a clearer mental model for new readers than "main + extension files."

Why keep `ipc/mod.rs` (880 LOC) intact? The big enum lives there for serde wire-shape parity with the schema. Splitting that enum across files requires custom serialization for every variant; the trade isn't worth it.

## 2026-05-15 — `EngineClient.cs` + `ModelInstallerService.cs` split via partial/sibling files (Windows)

The WinUI app's `EngineClient.cs` (1,378 LOC) bundled process lifecycle, IPC dispatch, command facade, and AutoPilot orchestration in one sealed class. Refactored to `internal sealed partial class EngineClient`; the command-facade methods (`StartScanAsync`, `PauseScanAsync`, all `DeepAnalyze*Async`, `ApplyTagsAsync`, etc.) + AutoPilot orchestration (`RunAutoPilotAsync`, `AwaitPhaseAsync`) moved to `EngineClient.Commands.cs`. The main file keeps process spawn/respawn, stdout/stderr loops, `OnProcessExited`, `Apply` event router, observable property surface, and `Set<T>` helper. Public API unchanged. Result: 1,378 → 970 + 419 LOC across two files; same compiled output.

Same approach for `ModelInstallerService.cs` (1,017 LOC): moved the `ModelSlot` class + `ModelInstallStatus` enum (already a distinct class in the same file) into a sibling `ModelSlot.cs` (282 LOC), leaving the orchestrator at 735 LOC.

Alternatives considered: (a) introduce DI-style helper classes (`EngineProcessManager`, `IpcDispatcher`) — rejected for now because everything in `EngineClient` accesses private state, and an extraction would require either passing the whole client by reference or making fields internal-with-friend-access; (b) leave as-is — rejected, the file had grown past comprehension. The `partial class` split is a zero-risk first cut; deeper extraction can land in a later pass if profiling motivates it.

## 2026-05-15 — Image-decode mmap fast path in `pipeline/tagging.rs` (Windows perf)

The Rust `load_image_rgb` opened each file **twice** through `image::ImageReader::open(&p)` — once to peek dimensions (for the 50-megapixel safety cap) and once for the actual decode. Comment in the original code acknowledged "~100 µs per reopen." At 50k files that's ~5 s wasted per full library scan; worse on spinning disks and network shares. Replaced with a single `memmap2::Mmap::map(&file)` followed by two `ImageReader::new(Cursor::new(&mmap[..]))` calls — both peek and decode read from the same memory region with no second open or copy. Dependencies didn't change (`memmap2` was already pulled in). Tests still green. No measured benchmark yet (criterion harness deferred to a follow-up), but the win is structural: one syscall + one mmap vs. two opens + two read paths, on every image in every scan.

## 2026-05-15 — `PRAGMA cache_spill = 0` added to SQLite setup (Windows perf)

Default SQLite behavior under memory pressure is to spill dirty pages from the 64 MB page cache into a temporary file mid-transaction. The engine's worst-case write transaction is a 100-row tagged-file batch (~few KB of dirty pages), well under the cache size, so spill never helps — it only ever costs an unexpected fsync to a temp file. Added `PRAGMA cache_spill = 0` to `SETUP_PRAGMAS`. Read-only connections pick up the pragma harmlessly (no-op on read-only).

## 2026-05-14 — WinUI 3 DispatcherObjects must be constructed on the UI thread (V15.2)

The Windows app crashed on Start Scan after a few tiles appeared, with NO `crash-*.txt` produced despite V15.1 wiring three managed crash sinks (`Application.UnhandledException`, `AppDomain.CurrentDomain.UnhandledException`, `TaskScheduler.UnobservedTaskException`). Forensics: engine processed 100 files cleanly then got `stdin EOF` + `BrokenPipe` — the C# app died hard. That signature is a native fast-fail (`RaiseFailFastException`), and `RaiseFailFastException` terminates the process before any managed handler runs.

Root cause: `ThumbnailService.RenderAsync` did `var bmp = new BitmapImage();` on its `Task.Run` worker thread, then marshalled `SetSourceAsync(bmp, thumb)` to the UI dispatcher, then returned the BitmapImage to be assigned into `Image.Source` via XAML data binding. WinUI 3's composition layer detects cross-thread `DispatcherObject` access during the next frame and fast-fails the process.

Decision: every WinUI 3 `DispatcherObject` (BitmapImage, BitmapSource, anything inheriting `DependencyObject`) **must** be constructed on the UI thread. Marshalling later mutations to UI thread is not enough — the constructor itself binds the object to whatever thread runs it. In `ThumbnailService`, the fix is to construct AND populate AND own the `StorageItemThumbnail` stream inside one `dispatcher.TryEnqueue` lambda; the worker thread only holds the request and the resulting `TaskCompletionSource`.

Corollary: V15.1's three managed crash sinks are necessary but not sufficient. They cannot intercept native fast-fail. The V15.2 last-session breadcrumb (`DebugLog.BeginSession` / `MarkCleanExit` / `DetectPriorAbnormalExit`) writes `last-session.txt` at launch with `clean_exit=false`, flips it on graceful shutdown, and on the NEXT launch emits a forensic `session-died-without-handler-{ts}.txt` if the prior session lacked the marker. This is the only path that survives a native crash.

Alternatives considered: (a) wrap every BitmapImage interaction in a top-level COM-thread-affinity check at the .NET layer — rejected, the check would itself run on the wrong thread; (b) drop the worker pipeline entirely and decode thumbnails synchronously on the UI thread — rejected, the shell-thumbnail roundtrip is ~5-20 ms per file and a 200-tile refresh would stall the UI for 1-4 seconds; (c) use `SoftwareBitmapSource` instead of `BitmapImage` (cross-thread-safer) — deferred, would require redoing the XAML bindings + storage caching; the construct-on-UI-dispatcher fix is sufficient.

## 2026-05-15 — Revert V14.9-U's silent cuDNN auto-install; replace with a manual Settings button (V15.1)

V14.9-U made cuDNN auto-fetch from NVIDIA's public CDN on every engine-ready on NVIDIA hardware. The legal framing (NVIDIA's own CDN, no redistribution) is still sound — the policy issue is product/UX, not legal.

Three problems surfaced over the following week:

1. **Silent ~430 MB download.** PRIVACY.md's "every network egress is initiated by you, with visible UI" line is technically satisfied by the existing model-install card UI, but in practice users opened the app and saw no acknowledgement that a download was starting; the "FileID is on-device software" framing got muddied.
2. **Startup VRAM pressure during the most TDR-hang-prone window.** V14.9-W/X investigation into the user's hard hangs identified concurrent DirectML session init + CUDA EP probe as a candidate stressor. Removing the auto-install eliminates one of the contending paths during the first 5-10 seconds after engine spawn — the window where TDR has been most likely to fire.
3. **The 10-15% speedup doesn't justify those costs at the current target hardware.** DirectML at 38 fps on a 6 GB RTX 2060 is fine for V1. Power users on a 24 GB RTX 4090 can opt in.

Decision: delete `CudnnAutoInstaller.cs` and the matching `App.xaml.cs` hook. Add a single "Install" button in Settings → Performance that drives `EngineClient.PrewarmModelAsync("cudnn_runtime_x64")` — the same code path the deleted auto-installer used. Keep the engine-side `registry::cudnn_runtime_x64` arm and the `register_dll_dirs_under(&models_dir.join("cudnn"))` startup call (no-op when dir absent) so the manual button still works end-to-end.

`AppSettings.DisableAutoInstallCudnn` is kept (no `[Obsolete]` annotation needed since it's an `app-settings.json` field, not a public API) — users who explicitly set it should not have a stale entry surprise them later, and the field's absence is now the default.

Alternatives considered: (a) keep auto-install but add a first-launch toast — rejected, the toast wouldn't change the underlying startup-time GPU pressure, and the 10-15% gain doesn't merit defending an automatic behavior the user actively flagged; (b) gate auto-install behind a Settings opt-in checkbox so the default is off but the auto path stays — rejected, that's strictly worse UX than a single one-shot Install button (two clicks instead of one, plus a hidden background download timing the user can't observe); (c) move the install button to the welcome sheet so first-time users see it on day one — deferred (see NEXT.md V15.1-N3); welcome sheet is already dense with four required model rows.

This supersedes the 2026-05-14 cuDNN auto-fetch entry below. The 2026-05-14 entry's legal analysis (NVIDIA's CDN is a legitimate downstream-fetcher source, identical to HuggingFace for model weights) remains correct and now describes the *manual* fetch the Settings button performs.

## 2026-05-14 — Auto-fetch cuDNN from NVIDIA's public CDN (policy reversal of V14.8.2)

PACKS.md (since V14.8.2) said cuDNN auto-fetch was deferred pending redistribution-license review. The rationale at the time: every cuDNN distribution channel we knew of was either NVIDIA's developer portal (registration + per-user EULA) or a third-party mirror (clear redistribution problem). Bundling required negotiating NVIDIA's license for FileID specifically.

NVIDIA now publishes the cuDNN Windows redistributables on a public CDN at `developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/` with no registration and no per-user EULA gate — the same channel any `pip install nvidia-cudnn-cu12` user pulls from (the wheel content is the same archive). Anyone can fetch from there; it is NVIDIA themselves distributing.

Decision: auto-fetch cuDNN from that CDN on NVIDIA hardware. The legal framing is identical to fetching Qwen weights from HuggingFace — the vendor controls the channel; we are an end user pulling from the canonical source, not redistributing. The new `CudnnAutoInstaller.cs` triggers on engine-ready + NVIDIA detection, opt-out via `AppSettings.DisableAutoInstallCudnn`. The user sees the download progress through the existing model-install card UI.

PRIVACY.md updated to disclose the new egress (`developer.download.nvidia.com`) alongside HuggingFace (model weights) and GitHub releases (llama.cpp runtimes). PACKS.md cuDNN section rewritten to describe the new auto-install behavior.

Alternatives considered: (a) keep cuDNN BYO with a Settings button — rejected, defeats the "everything just works" goal the user has consistently pushed for; (b) bundle cuDNN into our own composite ZIP under a redistribution license — rejected, both the engineering cost and the legal-review cost are out of proportion to the 10-15% scanning perf gain; (c) only auto-install when the user opts in via Settings — rejected, the auto-installer is the opt-in (it fires only on NVIDIA hardware and is single-flag-opt-out), no need for a second opt-in layer.

What's still deliberately BYO: full CUDA Toolkit install (cudart, nvcc, etc.). The engine's `system_cuda_toolkit_dir()` probe detects a system install and the auto-installer skips our private cuDNN drop in that case — no duplicate footprint.

## 2026-05-14 — Auto-install the Vulkan llama.cpp runtime at engine-ready time

Deep Analyze's previous flow surfaced a "Install runtime" banner the first time the user opened the tab. Users routinely missed it and assumed Deep Analyze was broken — captioning would silently no-op. The CudaAutoInstaller pattern (silent install of the CUDA llama.cpp pack on NVIDIA boxes) had proven that automatic install was the better default; this extends the same pattern to the base Vulkan runtime every Windows user needs.

Decision: `LlamaRuntimeAutoInstaller.cs` fires the `llama_runtime_x64` prewarm on engine-ready for every Windows user (no GPU-vendor gate — Vulkan covers NVIDIA + AMD + Intel + Adreno on one binary). Opt-out via `AppSettings.DisableAutoInstallVulkanRuntime`. The two Deep Analyze banners (`RuntimeBanner` + `CudnnInfoBanner`) and their click handlers were removed entirely — install progress shows through the existing welcome-sheet style download cards.

Side note: this also makes `--no-wipe` builds stop surfacing "AI not loaded" advisories on machines where the user had only installed VLM weights but never the runtime — the auto-installer now provides what was previously a separate manual step.

## 2026-05-14 — `build.sh` exposes an interactive wizard by default; legacy flag mode preserved for CI

The flag soup had grown to 12 boolean switches (`--no-wipe`, `--debug`, `--no-run`, `--no-desktop`, `--tests`, `--arm64`, `--vlm-native`, `--fast`, `--sign`, `--preserve-models`, plus the target). The user-reported friction wasn't any single flag — it was remembering which *combination* meant "iterate without wiping models" vs "full fresh install" vs "CI release". Common workflows had become tribal knowledge.

Decision: when `build.sh` is run with no arguments, drop into a plain `read`-based wizard that asks (1) platform, (2) one of five presets (Fresh install / Iterate / Tests only / CI release / Custom), and (3) preset-specific follow-ups (wipe scope when "Fresh install"). The wizard echoes the equivalent flag invocation before running so a power user can copy it for next time. Legacy `./build.sh -windows --no-wipe --debug` continues to work unchanged — CI and existing scripts don't break. The wizard is opt-in via "no args"; opt-out by passing any flag.

Alternatives considered: (a) a separate `setup.sh` wizard, leaving `build.sh` alone — rejected, two entry points means new users learn the wrong one; (b) a curses/dialog TUI — rejected, adds a runtime dep (`dialog`/`whiptail` not on every dev box) for marginal UX gain over `read`; (c) flag aliases like `--preset=iterate` — rejected, still requires memorizing alias names, doesn't address the "what *is* the iterate preset" question.

Related: introduces `-PreserveModels` to `build-all.ps1` so the wipe can spare the multi-GB `Models/` subdir while still nuking the DB, logs, and sentinels. Previously the wipe was all-or-nothing.

## 2026-05-14 — `llama_runtime_cuda_x64` lives in the engine registry, not as ad-hoc plumbing in the C# auto-installer

The `CudaAutoInstaller.cs` service hardcoded `ModelKind = "llama_runtime_cuda_x64"` and a per-install `SentinelDir = "llama.cpp-cuda"` constant, but the engine's `registry.rs` had no match arm for that kind. Every prewarm short-circuited at `LookupResult::Unknown` and surfaced "Add it to engine/src/models/registry.rs" as a user-visible toast. The PACKS.md doc had advertised the artifact for weeks.

Decision: add the arm to `registry.rs` as a sibling of `llama_runtime_x64` (Vulkan), extracting into `Models/llama.cpp-cuda/` (matches the folder the engine's `register_dll_dirs_under` already calls and the C# constant already pointed at). Drop the `SentinelDir` constant from `CudaAutoInstaller.cs` and route its "already installed?" probe through the canonical `Models/.sentinels/{id}.installed` path that `ModelInstallerService.HasEngineSentinel` uses. The two systems now share one source of truth — adding a future runtime can't introduce the same drift again.

Alternatives considered: (a) hardcode the URL + extraction path directly in the C# auto-installer and bypass the engine — rejected, splits the model catalog across languages; the registry is the canonical place; (b) leave the auto-installer's separate `.fileid-installed` sentinel and have the engine write *both* sentinels — rejected, the dual-write is silent failure waiting to happen, the canonical path is good enough.

## 2026-05-13 — Pre-flight sentinel check routes through the canonical registry, not hand-rolled paths

The previous `main.rs::handle_start_scan` pre-flight hand-rolled `<Models>/MobileCLIP/.fileid-installed` and `<Models>/arcfaceMobileFace/.fileid-installed` and checked existence. The canonical writer in the same file used `registry::sentinel_path(&model)`, which returns `<Models>/.sentinels/<model.id>.installed`. These two paths could never agree — every scan failed with "models missing" even after a successful prewarm. The reported "scan does nothing" symptom was dominated by this divergence.

Decision: the pre-flight now iterates a list of required model kinds (`["mobileclip_s2", "arcface", "clip_text"]`) and calls `registry::lookup_full(kind)` + `registry::sentinel_path(&model)` for each — sharing the same source of truth as the writer. Read and write paths can no longer drift without a registry-layer change.

Alternatives considered: (a) maintain a constant of hard-coded sentinel paths next to the registry — rejected, two-place changes still drift; (b) abstract a `is_installed(kind: &str) -> bool` helper on the registry module — equivalent in correctness, more verbose without buying anything since the consumer is one site.

## 2026-05-13 — Sentinel write is atomic (write-tmp + rename) with parent-dir create

The previous sentinel writer (`tokio::fs::write(&sentinel, …).await`) had two failure modes on a fresh install: (a) `.sentinels/` doesn't exist yet → `NotFound`, surfaced only as `tracing::warn!` and never as an `IpcEvent::Error` (the welcome row kept spinning); (b) the process is killed mid-write → half-written sentinel that subsequent runs treated as "installed" but whose payload didn't match.

Decision: ensure parent dir via `tokio::fs::create_dir_all(parent)`, write to `<sentinel>.tmp`, then `tokio::fs::rename(tmp, sentinel)`. Either the sentinel exists with full content or it doesn't exist. Every failure path now emits a structured `EngineError` event (`sentinel_dir_create_failed`, `sentinel_write_failed`, `sentinel_rename_failed`) so the welcome row stops spinning with a clear message.

Alternatives considered: (a) just-ensure-parent + plain write — rejected, doesn't address mid-write kill; (b) use the `tempfile` crate for atomic-write helpers — would have added a dep for a 4-line pattern, not worth it; (c) just retry on failure — doesn't help when the dir genuinely doesn't exist.

## 2026-05-13 — `redact_path_for_log` on the Windows engine mirrors macOS verbatim

User file paths under `C:\Users\<name>\...` were being emitted to the local `engine.jsonl` log unredacted. The privacy gate at CI scans for telemetry SDK strings, not personal-info-in-paths — so log files shared with support could leak names + folder semantics. macOS already had `redactPathForLog(_:)` at `platforms/apple/shared/Sources/FileIDShared/PathRedaction.swift` (keep last 2 path components, pass through app-structural paths under Application Support).

Decision: port the helper as `platform::redact_path_for_log(impl AsRef<Path>) -> String` with identical semantics for Windows: keep last 2 components, pass through paths whose lowercase contains `\fileid\` or `/fileid/` or `appdata\local\fileid` (the app-structural set on Windows). Three `#[cfg(test)]` tests pin the behavior. Wrap at the highest-traffic log sites first (scan entry, restore-from-trash refusal, image decode failure, video keyframe failure) — sweeping every log call site is a follow-up.

Alternatives considered: (a) regex-based sanitizer at the tracing-subscriber layer — rejected, captures everything blindly and can mangle legitimate JSON; (b) opaque log IDs replacing paths entirely — rejected, debugging becomes much harder without the filename suffix; (c) match macOS behavior exactly — chosen. Cross-platform consistency is more important than per-platform optimization.

## 2026-05-13 — Separate `LastWarning` channel (not a queue, not a clobbered `LastError`)

The engine emits both blocker errors and non-fatal warnings as `IpcEvent::Error`. The app's existing `LastError` slot served both, so a per-file image-decode warning could overwrite a session-level "face detection model not installed" banner before the user saw it. Two clean designs:

1. **Queue of warnings + a dismiss-all action.** Captures every warning but adds UX complexity (which one shows? do we stack badges?) for marginal benefit on a desktop app where most users have at most one warning per session.
2. **Single `LastWarning` slot, distinct from `LastError`, with kind-based routing.** Simple, lossless for the dominant case (one or zero warnings per session), trivially dismissed.

Chose (2). Routing in `EngineClient.Apply(IpcEvent.error)` is an explicit whitelist: `stages_skipped_missing_models`, `discovery_partial`, `checkpoint_failed_at_shutdown`, `cuda_dll_registration_failed`. Anything else stays a `LastError`. The yellow `#FFCC00` banner in `SidebarProcessingControl.xaml` reads from `LastWarning`. Dismiss = set to null. If a session ever ships multiple distinct warnings, the banner shows the latest — acceptable cost given the simplicity win.

## 2026-05-13 — Cross-platform IPC schema is symmetric, but mac engine returns `not_implemented_yet`

Windows C# defines 14 commands the Swift IPCProtocol didn't (`planRestructure`, `applyRestructure`, `applyTags`, `renameFiles`, `trashFiles`, `mergeClusters`, `embedTextQuery`, `renamePerson`, `markPersonsAsUnknown`, `findMergeSuggestions`, `embedImageQuery`, `restoreFromTrash`, `revertMerge`, `verifyCudaPack`). Two options:

1. **Schema-only, keep Swift tight.** Schema documents the wire, Swift only includes cases the mac engine actually handles. Cleaner per-platform, but the schema diverges from reality on mac.
2. **Schema + Swift cases + dispatch stubs returning structured errors.** Mac decodes every Windows command but emits `IPCEvent.error(kind: "not_implemented_yet")` for the 13 unrelated ones and `not_applicable_on_platform` for `verifyCudaPack`. Wire symmetric; failure paths clear.

Chose (2). The cost is 14 dispatch cases + 14 case enums + 2 DTO structs (`RestructureMove`, `RenameEntry`). The win is that cross-platform tooling (test corpus harness, IPC fuzzer, future shared C# client targeting mac) can route the same command shapes against either engine without per-platform special-casing. Round-trip tests in `Tests/SharedTests/IPCProtocolTests.swift::windowsCommandsRoundTrip` lock the wire shape.

Each `not_implemented_yet` message names the planned implementation milestone (V14.10) so the failure isn't mysterious to a future developer or user.

## 2026-05-13 — Strip narrative comments; keep WHY-only

Per `CLAUDE.md`'s "default to no comments" rule, V14.9-P's 15 narrative comments — explaining the previous bug, the alternatives considered, and the rationale — were stripped. The rationale lives in this DECISIONS.md, the per-finding STATE.md entry, and `git blame`. Comments retained name a non-obvious WHY in the immediate vicinity: a workaround, a subtle invariant (e.g. atomic write), or a Sendable-capture allowance. The bar going forward: if removing the comment wouldn't confuse a reader, it shouldn't exist.

## 2026-05-13 — `.gitignore` scopes the Windows `Models/` rule to App/installer/dist trees

The previous `platforms/windows/**/Models/` rule was case-sensitive — on case-sensitive filesystems it skipped `src/engine/src/models/` (lowercase, our Rust module), but on case-insensitive Windows filesystems it could match and silently gitignore the entire Rust module. Switched to three narrowed rules: `platforms/windows/src/FileID.App/**/Models/`, `platforms/windows/installer/**/Models/`, `platforms/windows/dist/**/Models/`. The engine source tree is now guaranteed unaffected regardless of filesystem case sensitivity.

## 2026-05-13 — Windows `face_clustering` delegates to `identity_clustering` (1:1 mac parity)

Mac uses a two-tier architecture: `FaceClustering.swift` orchestrates I/O and persistence; `IdentityClustering.swift` is the algorithm (two-pass density + Pass 3 quality validation). Windows had `face_clustering.rs` doing both — and the algorithm was a simpler single-pass connected-components at cosine ≥ 0.70, not the same algorithm mac uses. Same library scanned on both machines would produce different person clusters.

Decision: keep `face_clustering` as the orchestration layer (preserves the existing public API `cluster(&[FaceRow]) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)` so `main.rs::handle_run_face_clustering` doesn't need to change) and have it delegate the clustering math to a new `pipeline/identity_clustering` module. This mirrors mac's split exactly.

Alternatives considered: (a) inline the two-pass algorithm directly into `face_clustering.rs` — rejected, then the algorithm isn't independently testable and mac/Windows drift again over time; (b) rip out `face_clustering` and let `main.rs` call `identity_clustering` directly — rejected, would require touching `main.rs::handle_run_face_clustering` (which is in the middle of a +736-line upstream rewrite and we don't want to fight merges).

The kNN inside `face_clustering`'s delegation closure is brute-force O(n²d). Acceptable for ≤ a few thousand faces (matches the existing complexity of `uncertain_pairs()`). If face counts grow past ~10K we swap in `instant-distance` for HNSW — separate decision, separate commit.

## 2026-05-13 — Restore Windows engine `models/` from local stash instead of generating stubs

The upstream commit `231bff5` landed `mod models;` in `main.rs` and consumer imports in `pipeline/tagging.rs` + `pipeline/deep_analyze.rs` but **did not commit the `models/` directory itself**. CI failed every run with E0583. The local stash held 9 files (`arcface.rs`, `clip_text.rs`, `clip_tokenizer.rs`, `mobileclip.rs`, `mod.rs`, `registry.rs`, `runtime.rs`, `scrfd.rs`, `vlm.rs`) whose public APIs matched the consumer call sites verbatim — these were clearly written for those very commits but never pushed.

Decision: restore from `stash@{0}^3` rather than generate stubs. The stash files are ~54 KB of real ORT-backed model wrapping (ArcFace/SCRFD/MobileCLIP/CLIP-text/VLM); stubs would gut the Phase 1 ML pipeline that's already in progress. Three small additive patches closed the API drift between the stashed files and the new `main.rs` (a `ModelFile` type alias, `system_cuda_toolkit_dir()`, `probe_cuda_pack()`) without touching the existing functions.

Alternatives considered: (a) delete `mod models;` from `main.rs` — rejected, the consumer imports in `tagging.rs:24` and `deep_analyze.rs:123` would push the error one file over; (b) stub the module with `unimplemented!()` bodies — rejected, the scan pipeline would compile but silently fail at runtime on every face/embedding inference call.

## 2026-05-12 — Windows engine downloader: phase-specific timeouts, not a blanket request cap

The Windows engine's `reqwest::Client` previously used `.timeout(Duration::from_secs(300))` as a single total-per-request cap. That worked for the 14 MB ArcFace and 220 MB MobileCLIP-S2 downloads but reliably killed the 2.1 GB Qwen 2.5-VL 3B GGUF on any connection slower than ~7 MB/s — the body stream simply ran out the 300 s wall clock and reqwest aborted with what surfaced to the user as "reading chunk". Bumping the wall-clock cap to 30 min would have worked for most users but still fails ARM tablets on Wi-Fi and creates a worst-case where a single dead socket holds a request open for half an hour.

We switched to `.connect_timeout(30s) + .read_timeout(120s)`. `read_timeout` (reqwest 0.12.5+, the engine pins 0.12.28) only triggers when **no bytes arrive** for the configured duration, so a slow-but-progressing stream never trips it. The simple-download path was simultaneously rewritten to retry with HTTP `Range:` resume on stream errors (matching the parallel range path's existing retry loop), so even a hard connection reset mid-2GB-stream now recovers cleanly.

Alternatives considered: (a) keep the blanket timeout and just bump to 30 min — rejected, see above; (b) use the OS-level TCP keepalive — rejected, reqwest doesn't expose it portably and the failure mode is server-side aborts more often than dead sockets; (c) chunk the download into smaller HTTP requests with a Range loop — that's what `download_parallel` already does, and the new range-support probe (one-byte `GET Range: 0-0`) gets us onto that path even when HEAD doesn't advertise `Accept-Ranges: bytes` (HuggingFace CDN behavior behind 302).

## 2026-04-25 — v2 skunkworks rewrite, key architectural calls

The v2 rewrite supersedes the per-batch v1 work. These decisions are the load-bearing ones — the rest follow.

**1. Split-process daemon, not single-binary.** Engine (`fileidd`, the Swift CLI) is spawned as a child of the SwiftUI app via `Process` API. App lifetime = engine lifetime. Reasons: (a) UI never blocks the engine, engine never blocks the UI — no MainActor coupling means no v1-style "12 of 59,034, 0.1/s" UI lies; (b) crash isolation — a Vision/CoreML crash takes the engine, not the user's session; (c) easy to restart the engine without restarting the app. Considered SMAppService daemon (rejected — login items approval friction; engine doesn't need to outlive the app).

**2. stdin/stdout newline-delimited JSON for IPC, not XPC.** Both processes know each other via parent-child relationship; LSP / ripgrep `--json` / git plumbing all use this pattern. Trivially debuggable (`./fileidd | jq .`). XPC remains a future option behind the same `IPCCommand`/`IPCEvent` Codable surface — for child-of-app there's no actual benefit to XPC's ceremony.

**3. GRDB.swift over SwiftData.** SwiftData's `@ModelActor` was the v1 result-loop funnel. GRDB gives explicit transaction control, async writes that don't fight the actor system, FTS5 + extension support, and a well-documented migration framework. v2's `Database` actor wraps a single `DatabasePool` (engine writes) and the app uses a separate read-only `DatabaseQueue` — SQLite WAL allows concurrent readers without blocking the writer.

**4. Bounded `AsyncChannel` between every pipeline stage.** `swift-async-algorithms` `AsyncChannel` is the bounded backpressured channel Swift's `AsyncStream` lacks. This is *the* fix for the v1 result-loop funnel: Discovery → channel → 14 workers → channel → DBWriter, each stage paced by the next. No actors funneling, no MainActor on the hot path, no atomic-counter drift between stages.

**5. DBWriter batches inserts (100 files OR 50 ms, whichever first).** SQLite's per-transaction commit cost is dominated by fsync. Batching 100 inserts into one transaction amortizes the cost from "per-file" to "per-batch" — at ≥1000 tx/s, this floor is well above any realistic Vision throughput, so SQLite stops being the bottleneck. The 50 ms ceiling bounds latency for small batches.

**6. Resume cursor inside the SAME transaction as the file inserts.** `UPDATE scan_sessions SET last_file_index = ?` runs in the same write block as the per-file inserts. SQLite atomicity guarantees: a crash can't leave the cursor pointing past the last truly-committed file. (M5 polish: read this on engine startup to skip already-scanned files.)

**7. Pre-warm CoreML before workers spawn.** The v1 Batch 17/18 collapse (0.2 files/s) was caused by 14 concurrent first-load races on the MobileCLIP model. v2 calls `MobileCLIPService.shared.preWarm()` from `runScan` BEFORE the worker pool starts — one inference on a 32×32 dummy image to compile the .mlpackage, load the ANE pipeline, and pay the first-call cost once. Combined with `inferenceSem = DispatchSemaphore(value: 2)` inside `embedImage` to bound concurrent ANE access, no thrashing.

**8. `MLModel.compileModel(at:)` then load the .mlmodelc.** Skipping the explicit compile step caused `MLModel(contentsOf:)` to fail silently on the .mlpackage in M3 testing. Compiling first and loading the cached .mlmodelc is the documented path; CoreML's transparent compile inside `MLModel(contentsOf:)` is unreliable for sandboxed binaries.

**9. Structured JSONL log (`scan.jsonl`), not freeform text.** `JSONLog.shared` writes one JSON object per line — `{"t":..., "lvl":..., "ev":..., "sess":..., "extra":{...}}`. Every error gets logged with redacted file path. Future "scan got slow" investigations start with one `jq` query. (Replaced an earlier freeform `scan.log`.)

**10. Design language carried forward from the early FileID prototype.** `LavaLampBackground.swift`, `Theme.swift`, and the NavigationSplitView shell came from the original single-process prototype. AppDelegate transparent-titlebar trick preserved (keeps traffic-light buttons while letting the LavaLamp extend to the top edge). Non-negotiable preservation per user preference.

**Things explicitly cut (documented in `docs/NEXT.md` for the next session):** SigLIP 2 accuracy embedder, vectorlite HNSW extension, AI Models picker UI, face clustering, Restructure proposal engine, full crash-resume read path, MediaPreviewOverlay full port, soak test + CI perf bench, notarization. Each cut is an intentional scope decision, not an omission.

---

## 2026-04-25 — Batch 12: VisionWorkerPool actor → class — REVERTED same day

Tried replacing the actor pool with `final class + NSLock`. User ran the build and reported throughput collapsed to ~0.5 files/s (vs Batch 11's 13.8 files/s baseline). Reverted within minutes.

**What I claimed when I shipped it.** "Mechanical, low-risk." "The body still runs concurrently — only the executor hop is removed." "Safe even if it isn't the bottleneck."

**Why it was actually risky.** A perf-sensitive concurrency primitive on a 14-worker fan-in is never low-risk. The `actor` version had a property I didn't appreciate: actor methods *serialize* state observations, which means subsequent `acquire` calls implicitly see the most-recently-released worker. The continuation-based class version may have created a starvation pattern under high concurrent contention — or, more likely, the actor's serialization was incidentally pacing the CoreML/ANE warm-up so 14 workers didn't all hit `model.prediction()` at exactly the same instant. Either way, the actor version performed measurably better in production, and we now know that empirically.

**Real lesson.** "Mechanical and low-risk" is a thing I should not say about concurrency primitives without measurement first. The profiler (Batch 12 thread 2) is what should have shipped alone — and the deactor revisited only if PHASE-PROFILE actually showed actor-hop latency dominating per-file wall time.

**What stays.** The PHASE-PROFILE instrumentation and the Reveal-in-Finder button. Profiler data from the next user scan is what tells us where the actual 14% utilization bottleneck lives.

## 2026-04-25 — Batch 12: PHASE-PROFILE — instrument before fixing CLIP / DataStore

User reported the scan running at 13.8 files/s on M1 Pro — about 14% of the theoretical 100 files/s the per-file `total=140ms` log line implies for 14 workers. The prior batch's STATE.md said this was "within expected band" — that was wrong, and a self-inflicted lesson: instrumentation should have come before documentation.

**Where the missing 86% lives — candidates, none yet proven.**

a) **CLIP embed.** ~100–200 ms per image file inside `MobileCLIPService.embedImage`. Confirmed there's no per-call lock (the explore agent's claim that `imageLoadLock` is held during inference was wrong — that lock only gates the one-time `MLModel(contentsOf:)` load). But all 14 workers call into the same MLModel instance, and CoreML may serialize predictions on the ANE depending on the model's compute units. Invisible from the Swift side; visible only from per-file timing.

b) **FileIDDataStore @ModelActor insert.** Per-file `await store.insertScanResult(...)` is in the result loop. The result loop is single-threaded — every file across all 14 workers funnels through this one await. If insert takes 30 ms, the loop limits to 33 files/s. If 50 ms, 20 files/s. The observed 13.8 files/s is in this ballpark.

c) **Result-loop iteration cost itself.** Beyond `store.insertScanResult`, the body does a dict removal, calls `viewModel.recordFileCompleted`, optionally flushes faces, optionally commits a batch save. Each of these is fast individually but they all run serially in the same task.

d) **NAS I/O.** SMB NAS over SMB. CGImageSource reads are synchronous; 14 concurrent reads may serialize at the network layer. Not in-app fixable; only diagnosable by re-running on a local SSD.

**Alternatives considered.**

- *Apply the obvious fix first (move CLIP off the per-file path).* That's a real change touching the whole image pipeline. If CLIP isn't actually the bottleneck (and we don't yet know it is — see the lock retraction above), the surgery wastes time and may regress label quality. The explore agent's first take ranked CLIP as the top suspect with high confidence; reading the actual code disproved the lock claim. So: not yet.
- *Replace the whole worker pool with a different concurrency design.* Same problem — premature without a profile.
- *Add Instruments-style profiling.* Heavyweight; the user can't easily share Instruments traces.

**Decision.** Add a per-batch `PHASE-PROFILE` line to `scan.log` that captures p50/p95/total wall time for the three measurable spans inside the result loop (`workerWith` = time inside `pool.with { ... }`, `storeInsert` = time on the data-store actor write, `resultLoopIter` = time per `for await` iteration body), plus a derived `workerWall  workers × Xs = Ys   utilization=Z%` line and `availMB`/`residentMB`. The scan-log buffer pattern from Batch 11 is reused (`nonisolated(unsafe) static` + `NSLock`); snapshot is flushed at `commitBatchSave` time so it appears chronologically after the per-file rows for that batch.

**Why this beats guessing.** Two minutes of instrumentation in the user's next SMB NAS scan tells us which span dominates `batchDur`. If `storeInsert.total ≈ batchDur`, the data store is the funnel and the next batch moves writes off the per-file critical path. If `workerWith.total / (batchDur × 14) < 0.4`, the worker pool is starved — look upstream at the result-loop dispatch. If neither, we're bottlenecked on something the profiler doesn't cover yet (NAS I/O is the prime remaining suspect) and the next batch adds a per-file `loadCGImage` span.

**Honest retraction.** The "13.8 files/s is within expected band" line in the prior batch's STATE.md was wrong. 14 workers on M1 Pro should be far closer to 100 files/s; the gap was real and present, and the right move was instrumentation, not narrative.

## 2026-04-24 — Batch 15: Discovery — kill the per-file MainActor hop and the per-file stat

User reported Discovery taking 15+ minutes on a 58K-file library — far too slow for what should be enumerator + filter. Investigation found three compounded causes:

1. **Per-file `await viewModel.isCancelled` and `await viewModel.isPaused`.** Both are @Published on a @MainActor class. Each call hops to MainActor's executor. On a busy run loop (drain timer at 80 ms, Library grid re-renders, tooltip decoration), each hop can serialize for several ms behind UI work. 58K files × 5 ms × 2 hops = ~10 minutes of pure scheduling.
2. **Per-file `resourceValues(forKeys: [.creationDateKey, .fileSizeKey])`.** Needed a stat() per URL to read creation date and file size for the FileRecord init. On SMB NAS / SMB / network volumes, that's a network round-trip per file. 58K × 10 ms = ~10 minutes of blocking I/O.
3. **`includingPropertiesForKeys: [..., .contentTypeKey]` on the enumerator.** `.contentTypeKey` forces UTType / Spotlight metadata resolution per URL on network volumes, adding more per-file latency.

**Decision.** Three coordinated changes:

(a) **Drop the FileStream `actor`.** It's a `final class @unchecked Sendable` now. Discovery is single-owner by construction (only the scan task touches it), so the actor's executor hop bought nothing — it just added overhead per call. The class is `@unchecked Sendable` because it's passed by reference into a `Task.detached` and only used from the scan task.

(b) **Batch the enumerator output.** New `nextBatch(count: 1024)` API. Pulls a thousand URLs per call so the per-call overhead (lock, scheduling) is paid 56× less often. Also amortizes the cancellation/pause check across the batch.

(c) **Move cancellation/pause polling off MainActor.** New `nonisolated var isCancelledAtomic / isPausedAtomic` on AppViewModel. The @Published setters write to NSLock-protected mirrors via `didSet`; the discovery loop reads from those mirrors without an actor hop. Discovery now uses zero MainActor hops in the steady state; only the prologue/epilogue (phase transitions, status text) require MainActor.

(d) **Drop per-file `resourceValues` from FileStream.** FileRecord.init already reads them lazily on insert as a fallback. Discovery just enumerates and filters by extension. The 500 MB skip-large-files guard moved to `processFile` where the per-file stat happens anyway as part of the existing pipeline. Discovery does no syscalls per file beyond what the enumerator itself does.

(e) **`includingPropertiesForKeys: nil`** so the enumerator doesn't prefetch UTType.

(f) **Run discovery in `Task.detached(priority: .userInitiated)`** so it doesn't compete with MainActor-bound UI work for execution time.

**Why this is the right architecture.** Discovery is fundamentally I/O-bound (enumerator latency dominates on local disk; network latency dominates on NAS). The app's job is to add zero overhead on top of that I/O. The previous design added 10+ minutes of pure overhead. This design adds essentially zero — discovery should now take whatever the underlying filesystem can serve at, no more.

**Why not also defer to a background CFRunLoop or use a custom dispatch queue.** Tested; `Task.detached` with `.userInitiated` priority gives the same wall-clock with fewer moving parts. The FileManager.DirectoryEnumerator is already optimized internally by Apple for sequential reads.

## 2026-04-24 — Batch 15: `@Attribute(.externalStorage)` on big blobs

Audit identified clipEmbedding (~1 KB × N rows) and serialized face prints (~2 KB × 50 × identities) as the dominant inline-blob load on SwiftData saves. SwiftData supports `@Attribute(.externalStorage)` to automatically split blobs into sidecar files under the store directory. The SQLite row carries only a pointer; the blob itself doesn't enter the WAL.

**Alternatives considered.**
- *Split FileRecord into thin / thick entities.* Audit's original suggestion. Achieves the same goal but requires a SwiftData schema migration (risky without test coverage) and ripples through every fetch site. externalStorage is a one-line change with the same effect.
- *Manual disk-backed cache à la FacePrintCache.* Already done for face prints during scan. Adding more such caches inverts SwiftData's value (it stops being the source of truth for fields it should own). externalStorage keeps SwiftData authoritative.

**Decision.** Add `@Attribute(.externalStorage)` to: FileRecord.bookmarkData / clipEmbedding / deepAnalysis, PersonRecord.representativeFaceCropData / featurePrintsData. Combined with the Batch 14 WAL checkpoint, this keeps per-save fsync time bounded throughout a long scan.

**Why no migration concern.** The user's `run.sh` wipes the SwiftData store on every build (fresh-on-compile is set). Existing installs see the new schema on the next build. Production installations would need a migration, but the user is the only user; deferred.

## 2026-04-24 — Batch 15: dead code purged in one pass

Audit identified an orphan `applyFolderStructure` chain that was kept (deprecated + fatalError) for "historical reference." It's been there a few sessions; the actual restructure flow now lives entirely in FolderOrganizationView. Keeping a fatalError-on-call function as documentation is worse than just deleting and pointing future readers at git history.

**Decision.** Delete entirely:
- `AppViewModel.applyFolderStructure()`
- `MediaProcessor.applyFolderStructure(root:)`
- `FileIDDataStore.folderRestructurePlan(...)` + `MovePlan` struct
- `FileIDDataStore.updateURLAfterMove(oldPath:newPath:)`
- `FolderOrganizationView.categoryName(for:)` — was a byte-identical duplicate of `fileIDCategory(for:)`. Audit flagged this as a real divergence-risk foot-gun: a future edit to one but not the other would silently change Restructure's apply behaviour vs. its preview.

Also `FileRecord.scenePrintData` and `FileRecord.facePrintsRawData` — both already noted as stale in earlier batches; the comments said "kept for older stores" but with fresh-on-compile there are no older stores.

## 2026-04-24 — Batch 14: traffic lights — `.toolbar(.hidden, for: .windowToolbar)` was the killer

Batch 13 tried to fix the missing window buttons by removing `.windowStyle(.hiddenTitleBar)` and explicitly unhiding the standardWindowButtons via `isHidden = false` in AppDelegate. The user reported the buttons still didn't appear. The cause: Batch 11 had also added `.toolbar(.hidden, for: .windowToolbar)` + `.toolbarBackground(.hidden, for: .windowToolbar)` to the `NavigationSplitView` in `MainWindowView.swift` as belt-and-suspenders against a fullscreen white bar. On macOS 26 those modifiers hide the *entire* window toolbar layer, including the standard close / minimize / zoom buttons. `isHidden = false` on a button whose parent layer is hidden is a no-op.

**Decision.** Remove both `.toolbar(.hidden, ...)` and `.toolbarBackground(.hidden, ...)` from MainWindowView. The primary Batch 11 fix (the `.underWindowBackground` material on the WindowGroup root) is sufficient on its own to prevent the white bar. The buttons appear back where the OS expects them.

Also hardened AppDelegate: factored window setup into `configureMainWindow()` and call it twice — sync at didFinishLaunching, then async on the next main-queue tick. SwiftUI's WindowGroup can be slow to attach an NSWindow, so the sync call sometimes operates on `windows.first = nil` or an auxiliary panel. The async retry catches the case where the real window only becomes available a tick later. The window picker now filters to titled visible windows that aren't NSPanels.

**Why not a SwiftUI WindowAccessor.** A `NSViewRepresentable` that captures `nsView.window` is cleaner architecturally, but on macOS 26 the AppDelegate path is more reliable. The two-pass approach is ~10 lines and ships today.

## 2026-04-24 — Batch 14: tab switching — reverted Batch 5's scan-time unmount

Batch 5 introduced the scan-time `shouldMount` gate that unmounted inactive tabs to bound SwiftData notification fan-out during scan. Combined with the Batch 5 query bounds (CleanupView fetchLimit=500, FileGrid fetchLimit=2 000), it solved the 17 K-file throughput cliff at the time. But it created a new failure mode: switching from Library → Cleanup mid-scan triggered fresh `@Query` initialization for *all four* of CleanupView's descriptors, blocking the main thread for 1-3 s.

**Audit math.** With Batch 5's query bounds in place, keeping all six tabs mounted costs roughly +450 ms per save batch (saveEvery=400, ~25 s wall) → ~1.8 % throughput overhead. Switching to a previously-unmounted tab during scan costs 1-3 s of UI lock-up. The 1.8 % is invisible to users; the 1-3 s lock-up is the user's loudest complaint.

**Alternatives considered.**
- *Async-mount with placeholder.* Would show "Loading…" for the duration of the @Query fetch. Cleaner UX but requires per-view refactoring and the `@Query` macro doesn't expose a defer hook.
- *Hand-cache view data into AppViewModel.* Audit's Strategy 3. Best long-term architecture but ~8-10 hour refactor; we'd be inverting the data-ownership model on every tab.
- *Pre-warm tabs during idle.* Doesn't help during the scan when they're most needed.

**Decision.** Revert the unmount gate — every tab mounted at all times. Pay the 1.8 % throughput cost for instant switches. Bounded the previously-unbounded queries in PeopleView (`fetchLimit = 5_000`) and AcceptChangesView (`fetchLimit = Hardware.gridFetchLimit`) so the per-machine scan-time fan-out stays predictable on big libraries. The Batch 5 decision (DECISIONS.md "Unmount inactive tabs *during scan*") is now superseded.

**Why this isn't a regression.** The original 17 K-file cliff that motivated Batch 5 was caused by FileGrid's *unbounded* @Query (now fetchLimit=2 000) plus its O(N) per-body filter (now cached). With those root causes fixed, the unmount gate became defense against a problem that no longer exists.

## 2026-04-24 — Batch 14: tooltips — `.contentShape(Rectangle())` on icon-button hover regions

User reported tooltips weren't showing on the Pause / Cancel / Export action buttons during scan. Investigation: the buttons use `Label(...)` inside a `Button` with `.frame(maxWidth: .infinity)` for layout, then `.buttonStyle(.plain)`, then `.help(...)`. The `.frame(maxWidth: .infinity)` expands the *visible* layout, but the *hover* hit-test region defaults to the intrinsic Label size (icon + text bounding box). Hovering over the button's visible padding/background triggered no hover event, so `.help` never fired.

**Alternatives considered.**
- *Use `.buttonStyle(.borderedProminent)` etc.* The system styles set up hit-testing automatically but override the custom appearance the user wants.
- *Wrap the Label in a ZStack with a Color.clear background.* Would force layout but adds noise and doesn't change the hit-test default.
- *Set a specific `.frame(width:)`.* Defeats the responsive layout.

**Decision.** Add `.contentShape(Rectangle())` between `.buttonStyle(.plain)` and `.help(...)`. The Rectangle uses the *layout* size (the maxWidth-expanded frame), so hover hit-testing matches the button's visual area. Five sites updated: Pause, Cancel, Export, Reset (sidebar), Delete-data (Settings), Dismiss-merges (PeopleView). The sidebar tab buttons already had this pattern — they weren't broken.

## 2026-04-24 — Batch 14: SQLite WAL checkpoint — fix the long-running cliff

User reported "incredibly long wait time after running for a while." The audit identified SQLite WAL growth as the dominant suspect. SwiftData wraps Core Data wraps SQLite with WAL journal mode; every `ModelContext.save()` appends to `<store>-wal` but never explicitly checkpoints it. SQLite's auto-checkpoint at `wal_autocheckpoint = 1000` pages can fall behind on a long scan, growing the WAL to hundreds of MB. Each subsequent `save()` then has to fsync against an ever-larger WAL.

**Alternatives considered.**
- *Reduce save frequency.* Already large (saveEvery=400 on 16 GB). Going larger inflates the in-memory ModelContext, trading one form of slowness for another.
- *Split FileRecord into "thin" and "thick" entities.* Long-term win — clipEmbedding (~1 KB) and serialized face prints would no longer bloat every save. ~4-hour schema migration; deferred.
- *Use SwiftData's built-in checkpointing.* SwiftData doesn't expose a checkpoint API; raw SQL is the only path.

**Decision.** New `SQLiteCheckpoint.swift` opens a separate sqlite3 connection (via the system `import SQLite3` module) to the SwiftData store file and runs `PRAGMA wal_checkpoint(TRUNCATE)`. SQLite handles connection-level locking via its own busy-timeout, so this is concurrency-safe with SwiftData's writers — at worst we get SQLITE_BUSY, which we treat as "try next round." Called from `commitBatchSave` every 8 batches (≈ every 3 200 files at saveEvery=400, ≈ every 3 minutes at 18 files/s). The actual checkpoint duration plus WAL size before/after are logged to scan.log so the user can verify it's working.

**Why TRUNCATE not RESTART or PASSIVE.** TRUNCATE actually shrinks the WAL file on disk after merging; PASSIVE only merges what it can without blocking; RESTART forces all writers to switch to a new WAL file. TRUNCATE is the strongest option and the audit flagged "WAL on disk persists across runs" as part of the cliff — TRUNCATE addresses that explicitly.

**Why a separate sqlite3 connection.** SwiftData hides the underlying NSPersistentStoreCoordinator, so we can't reach into its connection. Opening a separate connection is fine: SQLite is designed for multi-process access. We use `SQLITE_OPEN_NOMUTEX | SQLITE_OPEN_READWRITE` since we serialize call sites ourselves.

**Why every 8 batches and not every save.** Each checkpoint is ~50 ms on M1 with a small WAL. Doing it every save (every ~25 s) would be 50/25000 = 0.2 % overhead — fine but unnecessary. Every 8 batches keeps the WAL small enough to check point quickly while not interrupting the scan rhythm. If WAL grows faster than expected (rare data mix), the SLOW SAVE warning surfaces it.

## 2026-04-24 — Batch 14: HNSW thrash gate — wall-clock cooldown between rebuilds

Batch 13's HNSW drift gate (`drift > max(50, count/2)`) could fire 5-10 times during clustering on libraries with rapidly-growing identity counts — each rebuild ~500 ms, perceived as a stall. Audit suggested a higher floor and a wall-clock cooldown.

**Decision.** Two changes: (1) drift floor bumped 50 → 200 (so a tiny library doesn't rebuild after only +25 centroids), (2) `hnswMinRebuildIntervalSec = 8` cooldown — even when drift would justify a rebuild, skip if the last one was less than 8 seconds ago. The phase-2 sample fallback covers staleness in the cooldown window. Each rebuild now logs identities/nodes/duration to scan.log so future tuning is data-driven.

**Why 8 seconds.** Each rebuild is ~500 ms; 8 s gives 16× headroom so users don't perceive cumulative stalls. Coincides with roughly the cadence of one batch save at saveEvery=400, which is a natural rhythm.

## 2026-04-24 — Batch 13: HNSW for centroid search, with flat scan as the safety net

User asked for face recognition that scales past 5 K identities. The existing centroid pre-filter is O(N) — fine at 1 K, ~30 s stall on PeopleView at 5 K, intractable at 50 K.

**Alternatives considered.**
- *IVF (inverted-file flat).* Needs a coarse k-means pass on every full rebuild; we'd have to add a clustering step that takes its own seconds-to-minutes. HNSW skips that — it's incremental.
- *Annoy / ScaNN bindings.* Both are C++; a Swift port is a non-trivial dep. The user's "no third-party Swift packages" rule applies.
- *Lower the existing 50-sample-per-identity cap.* Reduces phase-2 cost but doesn't fix the phase-1 O(N) loop, which is the dominant cost at high N.
- *Use Apple's `NLEmbedding` / Vision computeDistance.* Both work on opaque observations, not on raw float vectors that can be indexed.

**Decision.** Pure-Swift HNSW (~330 LOC) in `Sources/Services/HNSWIndex.swift`. Used as a phase-1 candidate filter in `clusterSync` — not as the source of truth. Top-20 candidate identities come back from HNSW; phase-2 sample fallback runs against those candidates. A stale HNSW (one that's missed recent `maybeRebuildCentroids` mutations) costs at most a tiny bit of recall — never a wrong assignment, because phase-2 still iterates the full snapshot if phase-1's best is below the strict threshold.

**Why phase-1 only, not phase-2.** Phase-2 is the correctness layer. HNSW is approximate by design (recall ~95 % at default params). Putting an approximate index between the user's faces and the cluster assignment would silently lose matches at the long tail. Phase-2 sample-fallback is O(K × M) on the *candidate set* (K = ~20 identities), which at M = 50 samples is 1 000 distance ops — fast even without an index.

**Why ~500 identities as the HNSW threshold.** Below 500, the flat O(N) scan is ~250 µs on M1 — the HNSW build cost (~50 µs per insert × 500 = 25 ms) plus query setup is pure overhead. Above 500, the flat scan crosses 1 ms and grows linearly; HNSW stays at log N.

**Why drift-based rebuild, not eager updates.** Centroids mutate on every face assignment via `maybeRebuildCentroids`. Eagerly removing + re-inserting would be ~100 µs per centroid change × thousands of changes per scan = seconds of pure index churn. The drift gate (rebuild when centroid count drifts >50% since last build) means at most a handful of full rebuilds per scan, each ~500 ms on M1 for 50 K centroids. The phase-2 fallback covers any matches a stale index missed.

**Why a custom Swift HNSW instead of Accelerate's `BNNS` / Core ML kNN search.** `BNNS` doesn't expose ANN — only brute-force kNN. Core ML's nearest-neighbour models require a fixed feature length and the model conversion adds opacity. A direct Swift implementation is reviewable, dependency-free, and uses Accelerate for the inner loop where it actually matters (vDSP_vsub + vDSP_svesq for L2 distance).

## 2026-04-24 — Batch 13: traffic lights — `.windowStyle(.hiddenTitleBar)` removed entirely

User reported the standard close / minimize / zoom buttons are missing. Cause: `.windowStyle(.hiddenTitleBar)` on the `WindowGroup` removes the entire titlebar surface, which takes those three buttons with it. The companion config (`.titlebarAppearsTransparent = true`, `.titleVisibility = .hidden`, `.fullSizeContentView`) was set up to handle a *transparent* titlebar — exactly the scenario where you keep the buttons but hide everything else. The `.hiddenTitleBar` style was over-killing.

**Alternatives considered.**
- *Re-show the buttons via `standardWindowButton(.closeButton)?.isHidden = false`.* Doesn't work — `.hiddenTitleBar` removes the buttons at the AppKit layer, not just sets their hidden flag.
- *Custom drag region + custom buttons.* Reinventing what AppKit already gives us, plus drag-affordance issues on macOS 26.
- *Switch to `NSWindow` subclass.* Conflicts with SwiftUI's WindowGroup lifecycle.

**Decision.** Drop `.windowStyle(.hiddenTitleBar)` from the WindowGroup. The existing transparent-titlebar config in AppDelegate already handles the visual goal (the LavaLamp / underWindowBackground material extends to the top edge). Explicitly re-show the three standard buttons in case any future titlebar tweak hides them. macOS standard back in place; no compromise on the immersive look.

## 2026-04-24 — Batch 13: face name as `person:<name>` tag, not a separate metadata column

User wants face recognition to be useful — clustering alone produces a People tab full of unnamed silhouettes. The leverage is making named clusters searchable everywhere else in the app.

**Alternatives considered.**
- *Add a `personName: String?` field to FileRecord.* Would require schema migration. The Library tab's search already runs against `aiTags`; adding another searchable field would need new query plumbing.
- *Compose names at query time from PersonRecord joins.* Every Library fetch becomes a join; the SwiftData query model doesn't make joins natural.
- *Tag with raw name (no `person:` prefix).* Collides with Vision-emitted tags ("Alice" the name vs hypothetical "Alice" tag) and breaks namespace isolation.

**Decision.** Canonical `"person:<name>"` tag fanned out to every FileRecord in the cluster's `fileIDs` set. Same `aiTags: [String]` field the existing search already filters on; no schema change; namespace-prefixed so collisions are impossible. Centralized formatter in `FaceClusteringService.personTag(for:)` so search, JunkScorer, and rename can never disagree on capitalization.

**Why fanout at rename time, not query time.** Query-time composition would mean every Library fetch joins against PersonRecord. SwiftData @Query doesn't compose joins naturally; we'd be hand-rolling fetch-then-merge for hundreds of grids per second of scrolling. Fanout cost is one fetch + N tag-mutations at rename time — paid once, queryable forever after.

**Why drop the old tag on rename.** A user typo'd as "Allice" then corrected to "Alice" would otherwise leave both tags on every photo. The old name is captured before mutation, dropped from each file in the same pass that adds the new one.

## 2026-04-24 — Batch 13: FolderRestructure errors are visible, not swallowed

The audit caught: `catch {}` in the apply loop, no manifest entry for failed moves (so undo couldn't restore them), no surface for "permission denied" / "disk full" / "destination exists." The user's complaint that restructure "doesn't really work" was almost certainly this — the operation appeared to succeed but silently lost files.

**Alternatives considered.**
- *Pre-validate every move before starting.* Doesn't catch race conditions (file deleted between check and move) and doubles the disk I/O.
- *Atomic transaction (move all-or-nothing).* macOS doesn't expose multi-file atomic move; you'd have to copy-then-delete with a temp area, which doubles disk usage on a 100 K-file restructure.
- *Per-file error dialog.* Modal hell on a 1000-file run.

**Decision.** Collect failures into an array as the loop runs. After the loop:
- Single summary log line: `Restructure: moved N, K failed, J already in place.`
- First 20 per-file failures inline in the in-app log (visible to the user).
- Full failure list to NSLog so Console.app captures everything.
- Same-name conflicts: numeric suffix disambiguation (`foo (1).jpg`) — never overwrite, never silently drop.
- Manifest only includes successful moves so undo restores exactly what was changed.
- `undoChanges` creates parent directories before reverse moves (handles "user closed source folder, then hits Undo") and reports successes vs. failures separately.

The user gets the same summary number they used to get, but now they can see *why* a failed file failed.

## 2026-04-24 — Batch 12: hard cap on `pendingFaces`, not a redesign of the flush trigger

User reported intermittent crashes on the 50K-file library; no fresh `.ips` was on disk. Audit identified `pendingFaces` as the most likely candidate: the existing soft `liveClusterThreshold = 2_000` only flushes at batch-save boundaries (every `saveEvery = 400` files). A face-dense run — wedding album, group shots, dance recital — can push the buffer well past 2 K *between* commits. At ~2 KB per print and ~10 prints per face-dense file, 100 faces × 4 files = 4 000 prints in ~10 ms of wall time, growing to 8 K+ before the next save. On 16 GB Macs that's the difference between "scan completes" and "Jetsam SIGKILL during clustering."

**Alternatives considered.**
- *Lower `liveClusterThreshold` to 500.* Trades structural fix for a magic number. Solves the 16 GB case at the cost of more cluster-task wakeups on every machine, including 64 GB Mac Studios that don't need them.
- *Move clustering inline into the result loop.* Removes the buffer entirely but reintroduces the actor-hop-per-face overhead that the original handoff design eliminated. Net throughput hit estimated at 10–15%.
- *Per-file cap (e.g. "skip clustering for files with > 30 faces").* Hides face data; a real wedding album loses cluster signal.

**Decision.** Add a *hard* cap (`pendingFacesHardCap = 10_000`, ≈ 20 MB) checked inside the result loop. The soft threshold still drives normal flush cadence at batch-save boundaries; the hard cap only triggers in the face-dense edge case. `flushFacesIfReady(_:force:)` gained a `force: Bool = false` parameter to bypass the soft threshold without duplicating the swap-and-dispatch code. The two thresholds work together: 2 K = "we have enough work to amortize the actor hop, flush at next natural break" and 10 K = "the buffer is approaching memory-pressure territory, flush *now* regardless of cadence." The explicit two-tier approach makes the policy legible — anyone editing the file can see that "normal" flushes target throughput while the hard cap targets memory safety.

**Why the cap value is 10 K.** A clustering actor flush of 10 K prints on M1 takes ~1.5 s end-to-end (NSKeyedUnarchiver + L2 distance + SwiftData inserts). Flushing more frequently than that wastes actor-hop overhead; flushing less frequently leaves the buffer growing past 20 MB into Jetsam-risk territory on 16 GB systems. 10 K is the highest cap that keeps the worst-case dispatch latency under "noticeable to PeopleView."

## 2026-04-24 — Batch 12: `Hardware.residentMB()` returns -1 on failure, not 0

The two mach kernel calls (`task_info` for resident, `host_statistics64` for free) can fail under low-memory conditions, sandboxing changes, or kernel-extension interference. Both functions returned `0` on failure — indistinguishable from "actually 0 MB used / free." Most call sites are NSLog/scan.log diagnostics where the wrong value just looks weird, but `canSafelyLoadLargeModel()` reads `availableMemoryMB() >= required` and would have *passed* the gate (`0 >= 3000` is false, so the gate would block; but the gate's intent is "block if measurement is unavailable" not "block if measured zero").

**Alternatives considered.**
- *`Optional<Int>`.* Cleaner type-system signal but every call site has to handle the optional. Most calls are inside `String(format:)` for log lines where Optional<Int> is awkward.
- *Throw on failure.* Same problem — non-throwing callers (NSLog format strings) would have to wrap in try?.
- *Keep returning 0 and document.* Loses the "couldn't measure" signal entirely.

**Decision.** Use -1 as a sentinel. Update `canSafelyLoadLargeModel()` to gate on `avail >= 0 && avail >= required` so the sentinel is treated as "don't risk it" — matches the function's documented intent (avoid SIGKILL during a 3 GB MLX upload; a measurement failure is "can't prove it's safe" which is "unsafe"). Diagnostic call sites unchanged; `-1` shows up in scan.log as a visible "memory query failed" instead of a misleading "0 MB". The HardwareTests case `testCanSafelyLoadLargeModelDoesntFalsePositiveOnSentinel` enshrines the contract — it can't directly inject a sentinel without a test seam, but it documents the requirement and runs the function so a future change that returns `0` on failure is more likely to trip a real bug.

## 2026-04-24 — Batch 12: cooperative yields, not full reactive rewrites

`FaceClusteringService.rebuildPeopleFromStoredPrints()` and `suggestedMerges()` are both long actor-isolated functions that block other actor calls for their full runtime. On a 9 K-print library, the rebuild can hold the actor for ~20 s, blocking PeopleView fetches that target the *same actor*. The audit flagged this as a UX issue (frozen tab) but not a crash.

**Alternatives considered.**
- *Move clustering off the actor entirely.* The clustering state (`identitySamples`, `centroidsCache`) is the actor's *raison d'être*; moving it out replaces clean isolation with hand-rolled locks.
- *Stream chunks via a `AsyncSequence` or callback.* The work IS chunkable, but the result has to be presented atomically (all-or-nothing rebuild — partial rebuilds would surface non-deterministic identity counts mid-run).
- *Use a separate background actor.* Doubles state — same data lives on two actors that have to stay in sync.

**Decision.** Add `await Task.yield()` every 64 inner-loop iterations. Yields are no-ops if no other actor work is queued, so steady-state cost is near zero. Other actor calls drain between yield points, keeping PeopleView responsive without changing the overall correctness model. Combined with `if Hardware.isUnderCriticalMemoryPressure { break }` checks for OS pressure — yielding doesn't help if we're already past the cliff, but the pressure check ensures we exit before the cliff if the OS is signalling.

**Why 64 and not 16 or 256.** 64 blobs ≈ 1 MB of unarchive work, ≈ 50 ms wall time on M1. Below that, yield overhead dominates the work between yields. Above that, individual UI freezes get noticeable. 64 is the sweet spot for "yields cheap, unfreeze frequent."

## 2026-04-24 — Batch 12: `suggestedMerges` gets a 2-second deadline + 256-pair cap

Even with the centroid pre-filter (Batch 5), `suggestedMerges()` is O(N²) in identity count. At 5 K identities the pre-filter runs ~12.5 M centroid-pair comparisons before any sample fallback — fast in absolute terms (~3 s) but slow enough to stall PeopleView's first-paint when the user opens the tab.

**Alternatives considered.**
- *Move to `async` and `Task.yield()` like rebuildPeople.* Would help responsiveness but not throughput. The user-visible win is "show me the suggestions you have, fast" not "use less main-thread time to compute all of them."
- *Compute eagerly post-scan and cache.* Already done — `cachedMergeSuggestions` is set on success. The 2 s deadline kicks in only on the first call after a cache invalidation.
- *Lower the centroid prune bound.* Trades correctness (more false-negatives) for speed.

**Decision.** Add a 2-second wall-clock deadline checked every 16 outer iterations, an `isUnderCriticalMemoryPressure` abort, and a `uuidPairs.count >= 256` `break outer` cap. The UI surfaces only the top suggestions anyway — beyond 256 pairs the user stops scanning the list. Cache the *partial* result so re-calls don't redo the work; the cache invalidates on `merge()` (correct: a manual merge invalidates the staleness assumption). Net effect: PeopleView's "Suggested Merges" returns in ≤ 2 s on any library size; users with > 5 K identities see the top 256 matches instead of stalling indefinitely.

**Why partial-and-cached vs. partial-and-not-cached.** Caching makes the second open of PeopleView instant. The stale-result risk window is bounded by user actions: as soon as they merge or split a person, the cache invalidates. The alternative (recompute every open) penalizes the common case ("open PeopleView, browse, close, reopen") to avoid a rare staleness ("open PeopleView, see partial, close, *something external changed identities*, reopen"). External identity mutation paths all go through `merge()` or the rebuild flow, both of which invalidate.

## 2026-04-24 — Batch 12: explicit `NSLog` on scan.log write failure, not silent `try?`

`flushPerFileScanLog()` and `writeScanLogLine(_:)` previously wrapped every disk operation in `try?` — write, synchronize, atomic-fallback. Disk-full, permission-denied, volume-gone all produced missing scan.log lines with no signal. When the user reports "scan.log just stopped" we currently have no way to say *why*.

**Alternatives considered.**
- *Throw all the way up.* The scan engine treats logging as best-effort; making it throw forces every caller to handle a failure that's diagnostic, not functional.
- *Buffer failures and surface in UI.* Adds state for a rare condition; Console.app is already the right venue for this signal.
- *Switch to OSLog.* Larger surgery; the file-based scan.log has features (tail in crash.log via the CrashSentinel reporter) that OSLog can't easily provide.

**Decision.** Wrap the write/synchronize calls in `do { ... try ... } catch { NSLog(...) }`. The user sees no behaviour change unless the write *fails*, in which case Console.app gets a line they can paste back. `try?` is preserved on the file-handle creation (`FileHandle(forWritingTo:)` failing isn't a "real" failure — the atomic-write fallback handles it).

## 2026-04-24 — Batch 11: full-screen white bar was a vibrant-material / split-view-toolbar interaction, not a layout bug

User reported "When I full screen I get this huge white bar" above the Settings header. Windowed mode was clean; the white band appeared only in full-screen.

**Evidence.** `Sources/FileIDApp.swift:43` applied `.background(VisualEffectView(material: .hudWindow, blendingMode: .behindWindow))` to the root `MainWindowView`. `MainWindowView` nests everything in a `NavigationSplitView`. `AppDelegate.applicationDidFinishLaunching` sets `window.styleMask.insert(.fullSizeContentView)` + `.isOpaque = false` + transparent titlebar. In windowed mode, the titlebar is transparent and the dark LavaLamp/content fills correctly. In full-screen, macOS inserts an auto-hide region for the menubar at the top of the window — and the `NavigationSplitView` has an internal toolbar strip even when you don't add toolbar items. That strip renders with the system-default light background in full-screen because `.hudWindow` is a *light* vibrant material — it doesn't propagate behind the split-view's own chrome layer.

**Alternatives considered.**

- *Override the `NSWindow` subclass directly.* Would require replacing `WindowGroup` with a custom `NSWindowController`, which conflicts with SwiftUI's lifecycle and breaks `.modelContainer` injection. Too invasive.
- *Paint the LavaLamp layer over the toolbar area.* `.ignoresSafeArea()` is already on the LavaLamp canvas, but `NavigationSplitView`'s toolbar strip is drawn *above* SwiftUI's safe area in the composite order. You can't paint over it from inside the split view.
- *Add an explicit empty toolbar.* Makes the strip more explicit but doesn't change its background color.

**Decision.** Two coordinated changes at the SwiftUI level:

- Swap the root VisualEffectView material from `.hudWindow` → `.underWindowBackground`. The `.underWindowBackground` material is the macOS idiom for "opaque dark surface that fills the entire window including toolbar strips" — it's what Apple uses on Finder's sidebar area.
- Add `.toolbar(.hidden, for: .windowToolbar)` + `.toolbarBackground(.hidden, for: .windowToolbar)` to the `NavigationSplitView` in `MainWindowView`. Belt + suspenders: suppress the default toolbar entirely (we don't put anything there), and even if a toolbar sneaks in later, the system-default background stays hidden.

**Why the fix is SwiftUI-side rather than AppKit.** `fullSizeContentView` was already set — the window mask wasn't the problem. The problem was the *material color* and the *split-view's default toolbar background*, both of which are SwiftUI-layer concerns. AppKit overrides would fight the SwiftUI compositor.

## 2026-04-24 — Batch 11: scan-log buffer with per-batch fsync (not per-file)

User asked whether 13.8 files/s is reasonable and whether there's perf headroom. The steady-state math (9 workers × ~500 ms worker-wall-time per file including Vision + CLIP + face archive + EXIF + dHash) is within expected band for an M1 Pro — no secret 2× win is hiding anywhere. But one real small win: `MediaProcessor.writeScanLogLine` was doing `FileHandle(forWritingTo:)` + write + `synchronize()` + close **per file**, with 9 workers racing the same `~/Library/Logs/FileID/scan.log` path. That's ~14 fsyncs/s serialized at the VFS layer.

**Alternatives considered.**

- *Drop `synchronize()` entirely and rely on OS buffering.* Loses crash forensics — a SIGKILL mid-scan means the last N lines never hit disk, and the CrashSentinel stanza composed on next launch may miss the file that was in flight.
- *Move scan.log writes onto a dedicated logging actor.* Cleaner architecturally but a bigger surgery and doesn't solve the fsync-per-file problem — an actor would still need to decide when to flush.
- *Per-actor instance buffer.* `processFile` is `nonisolated` on the MediaProcessor actor, so it doesn't have direct access to actor-local state without an `await` hop. The `await` would serialize all workers against the actor queue — worse than the fsync contention we're trying to fix.

**Decision.** Cross-actor shared buffer as a `nonisolated(unsafe) static var` protected by an `NSLock`. `appendScanLogPerFile(_:)` pushes to the buffer without opening any handle. `flushPerFileScanLog()` drains the buffer in one open + write + fsync + close — called from `commitBatchSave` (every `saveEvery`=400 files) and once more at scan end. Phase-boundary, discovery, Deep Analyze headline lines, and `appendScanLogExternal` (called from `ClusterCircuitBreaker`'s detached task) continue to write immediately — low-volume and crash-forensics-sensitive.

**Why the buffer is safe for crash forensics.** We lose at most `saveEvery`=400 per-file lines on crash. The CrashSentinel marker (written to a separate file on every file-start) captures the in-flight file independently of scan.log — so we still know what was processing when the crash happened. The scan-log tail's main use is "did file X finish successfully before the crash"; losing the last 400 lines means we know the last successfully-flushed batch, which is fine for narrowing the failure window.

**Why `nonisolated(unsafe) static`.** The alternative (actor-local instance buffer) requires `await`-ing the MediaProcessor actor from `processFile`, which would serialize all 9 workers against a single actor queue and cost more wall time than the fsync-per-file it replaced. `NSLock` + `nonisolated(unsafe)` gives lock-free fan-in with just a short critical section — the right trade.

## 2026-04-24 — Batch 11: "best" is a UX word, not a ranking word — rename without changing the ranking

User said "I am confused by the date and best thing just does not make sense to a normal user." The immediate instinct is to reword "best" to something else. The right fix is to stop hiding the criterion behind a subjective word at all.

**Evidence.** `CleanupView.swift:117-122` — `keeperRank` ranks duplicates by quality (aesthetic score) → size → **earliest creationDate** → path depth. `:192` tooltip and `:202` confirmation mentioned "best copy per group (highest quality, largest file, earliest date)". `MainWindowView.swift:868` and `CleanupView.swift:537` render `file.creationDate.formatted(…)` with no label — and `creationDate` is filesystem creation time, which for re-imported libraries is often today's date even for a 2019 photo.

**Alternatives considered.**

- *Change the ranking to "keep newest".* Rejected. Newest-on-disk often means the re-imported copy that *lost* EXIF during the re-import — so "newest" would actively regress the duplicate-dedup use case. The original ranking is pragmatic: keep the file most likely to have original EXIF + full size.
- *Change the ranking to "keep highest resolution".* Already done — `quality` (aesthetic score) is the first tiebreaker, and `size` is the second. We already keep the highest-resolution copy where it matters.
- *Read EXIF `DateTimeOriginal` at scan time and store it as a `photoCaptureDate` field.* This would be the right fix for the date-display problem, but it's a SwiftData schema change + a scan-time EXIF read + UI changes. Out of scope for this batch; flagged as Batch 12+ scope if the user actually wants photo-capture dates shown prominently.
- *Keep "best" but add a hover tooltip explaining it.* Half-fix — the word "best" still sits on the primary button, so the first-read confusion remains.

**Decision.** Reword every surface the user reads: drop "best," use "sharpest, largest copy" (which is what the ranking actually does on the first two criteria), and in the confirmation dialog explain the earliest-date tiebreaker so the user knows *why* we keep the oldest file. Ranking logic stays untouched — the confusion was copy, not logic. For the bare `creationDate` Text, add a `.help` explaining that it's filesystem creation time, not photo-capture time. Cleanup rows switch `.abbreviated` → `.numeric` so the year shows for cross-year duplicates.

**Why not ship the photoCaptureDate field now.** The user's feedback was "does not make sense," which is a comprehension problem solved by better copy. Adding a new SwiftData field would be a meaningful schema migration (store invalidation or migration code) for a symptom that a `.help` tooltip plus better wording resolves. If the user sees the Batch 11 build and still wants the displayed date to be photo-capture-date rather than on-disk-date, the schema change is a reasonable Batch 12.

## 2026-04-24 — Batch 10: no live tree rebuilds during scan (SwiftUI AttributeGraph ceiling, not memory)

User hit a SIGABRT after a 76-minute SMB NAS scan that had reached ~29 K of ~58 K files. Symptoms read as OOM ("ran for a very long time then started beach balling a lot then crashed outright") and the user asked for "some kind of temp file or database system … not everything is loaded in." Investigation found the crash is **not** a memory problem, and the "new DB layer" is the wrong abstraction.

**Evidence.** `~/Library/Logs/DiagnosticReports/FileID-2026-04-24-163532.ips` — `EXC_CRASH / SIGABRT`, fault-thread top-down: `__pthread_kill → abort → AG::precondition_failure → AG::data::table::grow_region() (.cold.1) → AG::data::table::alloc_page → AG::Graph::add_attribute → ModifiedElements → TransitionBox → ForEachState → OutlineGroup → DynamicContainerInfo.updateItems → GraphHost.flushTransactions → NSHostingView.beginTransaction → NSRunLoop.flushObservers`. Fires on the **main thread** inside SwiftUI's own AttributeGraph, not a Jetsam SIGKILL (no kernel-panic thread, no Jetsam summary). The `.cold.1` variant of `grow_region` is Apple's slow-path for "the dynamic-attribute page table hit its internal precondition cap."

**Root cause.** `AppViewModel.rebuildTreeFromAccumulator()` ran every 500 ms during the scan (6th drain-timer tick). It rebuilds a brand-new tree of value-type `FileTreeNode` instances from `treeAccumulator`; the tree is rendered by `OutlineGroup(viewModel.fileTree, children: \.children)` inside `List { Section { … } }`, which SwiftUI wraps in `TransitionBox` for section animations. On the SMB NAS library, `treeAccumulator` had thousands of entries (one per sub-path). Every 500 ms SwiftUI diffed the previous tree against a freshly-minted one — all-new value-type instances, wide and deep — and allocated AG attributes for the churn. At ~9 000 rebuilds × thousands of rows × a `TransitionBox` diff context, AttributeGraph's internal page table saturates. Rebuilding less often doesn't help because the cap is on total allocations during the view's lifetime, not on rate.

**Alternatives considered.**

- *Cut the rebuild frequency from 500 ms to 5 s.* Still allocates thousands of attributes per rebuild; just delays the crash. Same failure mode on a longer scan.
- *Stable identity per tree node.* The IDs are already path-derived and stable; the issue is value-type reconstruction + `TransitionBox` diff, not identity.
- *Replace `List`+`Section`+`OutlineGroup` with a plain `ScrollView { LazyVStack { … } }`.* Viable but large refactor (loses selection, disclosure state, sidebar styling), and the user has not asked to redesign the sidebar. The current shape works fine post-scan.
- *Bound the accumulator.* Defense-in-depth, but 1 000 keys × 9 000 rebuilds still eventually overruns AG.

**Decision.** Suspend the live tree rebuild for the duration of the scan. `drainAtomicState` gates the rebuild call on `!isProcessing`; `finishNamingPhase` fires one explicit rebuild after `enterPhase(.ready)` so the user sees the final tree when they land on Review. `MainWindowView.swift` adds `&& !viewModel.isProcessing` to the `Section("File Hierarchy")` predicate so the container isn't even rendered during scan — zero `OutlineGroup`/`ForEach`/`TransitionBox` work. Defense-in-depth: `recordTreeProgress` caps paths at 6 components so deeply-nested libraries don't explode the accumulator.

**Why not "a new database system" as the user asked.** SwiftData already *is* a lazy disk-backed store; row-level data is not "all loaded in." The in-memory pressure during scan comes from **SwiftUI-side state** (`fileTree`, `treeAccumulator`, the thumbnail NSCache) — not from SwiftData fetches, which Batch 5 already bounded with `fetchLimit`. Adding another persistence layer would be duplicative and would not have prevented this crash. The honest fix is "stop pushing data into SwiftUI views during scan," not "stop pushing data into SwiftData."

## 2026-04-24 — Batch 10: time-box PDFs with fast OCR, skip very large ones

Scan log showed PDFs burning 28–38 s each with `recognitionLevel = .accurate`, `usesLanguageCorrection = true`, up to 10 pages. Each PDF holds a Vision worker slot for its full duration — a PDF-heavy subfolder stalls the pipeline and produces the beach-balling the user saw. For FileID's actual use — extracting keyword tags like "Invoice" / "Receipt" / "Tax_Document" — `.accurate` OCR is overkill; `.fast` with no language correction catches the same keywords at ~10× the speed. Added `VisionWorker.ocrFast` and switched `MediaProcessor.processPDF` to `ocrFast`, capped at 3 pages (first few pages carry the genre-defining vocabulary), and added a 20 MB short-circuit that tags as `["PDF", "Large_Document"]` without any OCR (large PDFs are usually scanned manuals whose rasterized images don't OCR well at `.fast` anyway, and the size+name already gives cleanup/restructure enough to act on). Expected per-PDF wall time: 28–38 s → ~500 ms–1 s.

## 2026-04-24 — Batch 10: `TagTaxonomy` humanization on scan, not migration

User saw "Optical Equipment" on thumbnails — these are `VNClassifyImageRequest`'s raw taxonomy labels (`optical_equipment`, `bottled_and_jarred_packaged_foods`, `natural_phenomenon`). No translation step existed anywhere between Vision and SwiftData writes. Options considered:

- *Post-process existing rows with a SwiftData migration.* Fresh-on-compile is on (Batch 8) — every launch wipes the store, so a migration would be rewriting data that's already destined for deletion on next launch.
- *Translate at display time in the view layer.* Would leave raw taxonomy in `FileRecord.aiTags`, polluting search and the CategoryMatcher logic that routes to UI categories.
- *Translate at scan write time.* Chosen. `MediaProcessor.processFile`'s terminal dedupe now calls `TagTaxonomy.humanize(tags)` — one line swap, applies on write. Unknown labels pass through unchanged so internal tag contracts (`Tax_Document`, `Invoice`, `Screenshot`, date tags, `PDF`, `Large_Document`, CLIP labels) are untouched.

## 2026-04-24 — Batch 10: Deep Analyze intensity is a user-facing choice, not a heuristic

Batch 4 added chunking + memory-pressure backoff to Deep Analyze, but default of 64 files/chunk with 50 ms pauses between chunks still visibly hitches the rest of the Mac on a 16 GB machine when Safari is open. Rather than make one new "smart" default, exposed three explicit tiers (`performance` / `balanced` / `gentle`) as a segmented `Picker` in Settings. Default moves to `balanced` (32/250 ms). Rationale: Deep Analyze is *batch* work — users care about "don't kneecap my Mac" more than "finish in the shortest wall-clock time," but the ones who do want the fast path shouldn't be denied. A picker makes the tradeoff legible and reversible without code changes. `gentle` additionally waits for a safe memory window (`Hardware.canSafelyLoadLargeModel()`) between chunks — this is the "don't destroy the system" tier the user asked for.

## 2026-04-24 — Session B (UI perf + horsepower + VLM lineup)

User feedback after Session A: Library scrolling "unbelievably slow," Cleanup tab switch lags the whole system, "use a lot more horsepower," remove the Deep Analyze icon from thumbnails, add Gemma 4 (or closest equivalent) plus other model options.

**1. FileCard rewrite (`Sources/MainWindowView.swift`).** The per-card body had a `GeometryReader`, `.regularMaterial`, `.ultraThinMaterial`, multiple `.shadow(...)` calls, a `.blur(radius: 1)` border, a horizontal `ScrollView` for tag chips, and a Deep Analyze button — repeated across ~40 visible cards. Rewrote to use flat `Color.white.opacity(0.04)` backgrounds, no GeometryReader, a single-line tag summary (top 3 joined with `·`), and a hover-only trash button. Dropped the per-card `.transition(cardTransition(index:))` stagger animation entirely. Switched `@Bindable var file` → `let file` since the card doesn't write per-field; SwiftData `@Query` parent picks up the trash mutation through normal change tracking.

**2. CleanupView caching + CleanupFileCard rewrite (`Sources/CleanupView.swift`).** `categoryBreakdown`, `screenshots`, `activeFiles`, `totalReclaimableMB`, and `duplicateGroupsSummary` were all computed properties — every body eval ran four `.reduce` passes over four 500-row arrays plus a Dictionary grouping + sort for duplicates. Cached all five into `@State` and recomputed only on `@Query.count` / `selectedTab` `.onChange` hooks. Same flat-background card rewrite as FileCard. Extracted the header into `headerLeftContent` / `actionButtons` ViewBuilders to dodge the Swift type-checker timeout that fired when the body got too big.

**3. Hardware caps bumped (`Sources/Services/Hardware.swift`).** `workerCap` now `performanceCoreCount + max(1, efficiencyCoreCount/2)` instead of P-cores only — E-core helpers soak up I/O-bound work (file enumeration, EXIF reads, thumbnail decode) while P-cores stay pinned on Vision. Added `efficiencyCoreCount` via `hw.perflevel1.physicalcpu`. Thumbnail caches tripled: 16 GB Mac → 1 200 MB (was 400) / 1 500 entries (was 500); 24 GB → 2 000 MB / 2 500; 48 GB+ → 4 000 MB / 4 000. `saveEvery` doubled: 16 GB → 500 (was 250); 24 GB → 1 000; 48 GB → 1 500 — at 100+ files/s the previous 250 fired SQLite WAL fsync every ~2.5 s; now ~5–15 s commit cadence.

**4. VLM lineup expansion (`Sources/Services/AIModelRegistry.swift`, `DeepAnalyzeService.swift`, `AIModelDownloadService.swift`, `SettingsView.swift`).** User asked for "Gemma 4." Verified via WebFetch that Gemma 4 weights exist on HuggingFace (`google/gemma-4-*`, `mlx-community/gemma-4-*`) but the pinned `mlx-swift-examples 2.29.1` (latest release as of Oct 2025) `VLMRegistry` only knows the Gemma 3 architecture — loading Gemma 4 .safetensors would fail in the loader. Shipped the closest-available lineup that the framework can decode today:

- **Qwen2.5-VL 3B (4-bit)** — kept as default. `mlx-community/Qwen2.5-VL-3B-Instruct-4bit`.
- **Qwen3-VL 4B (4-bit)** — `lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit`. Newer architecture, better OCR.
- **Gemma 3 4B (QAT 4-bit)** — `mlx-community/gemma-3-4b-it-qat-4bit`. Closest live "Gemma 4" stand-in.
- **Gemma 3 12B (QAT 4-bit)** — `mlx-community/gemma-3-12b-it-qat-4bit`. Heaviest, ~7 GB.
- **SmolVLM Instruct (4-bit)** — `mlx-community/SmolVLM-Instruct-4bit`. ~600 MB, 2× faster.
- **PaliGemma 3B (8-bit)** — `mlx-community/paligemma-3b-mix-448-8bit`. Strong on grounding/OCR.

`AIModelKind` gained an `isVLM` discriminator. New VLMs use empty `relativePaths` as a marker meaning "MLX-managed download" (file lists vary per model and many are sharded). `AIModelDownloadService.runDownload` branches on `isVLM && relativePaths.isEmpty` and routes through a new `downloadVLMViaMLX` helper that calls `VLMModelFactory.loadContainer` from a detached Task, reports coarse fractionCompleted progress, then immediately drops the loaded `ModelContainer` and clears MLX's GPU cache (we just wanted bytes on disk). `DeepAnalyzeService.activeKind` reads `UserDefaults("deepAnalyzeActiveModel")`; `ensureLoaded` notices when the wanted model differs from `loadedKind`, drops the current container + clears the GPU cache, then loads the new model. New `gpuCacheBudgetMB(for:)` per-model cache cap (8 192 for Gemma 3 12B, 1 024 for SmolVLM, 3 072 for the rest). New Settings Picker bound to that UserDefaults key, only listing currently-installed VLMs.

**5. Removed Deep Analyze icon from thumbnails (per user request).** The purple `sparkles` button on every `FileCard` is gone. The MediaPreviewOverlay still has its Deep Analyze button (full-preview, not thumbnail). The `ProcessingGridView` toolbar still has the run-on-library button.

**Risk:** The `AIModelDescriptor.isInstalled` check for VLMs is now "config.json exists in MLX hub cache." If MLX's downloader is interrupted between writing config.json and the safetensors, isInstalled returns true but the model fails to load. Mitigation: `ensureLoaded` catches the failure and surfaces it; the user can re-download from Settings → AI Models.

**Why no `.contentShape(...)` on the LazyVGrid scrolling area:** SwiftUI's `ScrollView` doesn't need explicit hit-test shape — the LazyVGrid children handle their own gestures.

**Why the `.id("\(selectedTab)-\(sortByAesthetic)-\(isProcessing)")` on FileGrid stays:** still needed so the `@Query` reinitialises with new sort descriptors when the user toggles Date ↔ Best. Was tempted to drop it but the @Query pattern doesn't expose runtime-mutable sort.

## 2026-04-24 — Session A: bundled Vision pass + interleaved discovery + dropped "Unclassified"

User asked for a major perf+accuracy overhaul (`~/.claude/plans/i-need-you-to-refactored-cherny.md`). Session A lands the structural perf wins:

1. **One `VNImageRequestHandler` per image, not 3+N.** `VisionWorker` previously created a fresh handler for `classify`, `scenePrint`, `facePrints`, `ocrText`, *plus* a separate handler per detected face for feature-print extraction (a 5-face photo allocated 5 extra handlers). Handler construction decodes the image and allocates GPU textures — doing it N times per file was the dominant per-file cost. New `VisionWorker.runPrimaryPass(_:) -> VisionPass` builds **one** `VNImageRequestHandler` and runs `[classifyReq, animalReq, faceRectReq]` in a single `perform()`, then runs all face feature-print requests in a *second* `perform()` on the same handler using `regionOfInterest` per face (no per-face cropping, no per-face handler).
2. **Stop the double CLIP image-encoder pass.** `MediaProcessor` was calling `MobileCLIPService.shared.embed(cgImage)` then `MobileCLIPService.shared.classify(cgImage, topK: 5)` — the `classify` method internally re-ran `embedImage(cgImage)`. New `classify(usingEmbedding:topK:)` overload accepts a precomputed vector. ~100–200 ms per file saved when CLIP is loaded.
3. **Interleaved discovery + tagging (Phase 1 of the seven-phase plan).** Old code drained the entire `FileStream` enumerator into `var discovered: [...]` before spawning a single Vision task — leaving every P-core idle during 5–30 s of NAS/external enumeration. New `DiscoveredQueue` actor (continuation pool, same pattern as `VisionWorkerPool`) is fed by a detached discovery `Task` and consumed by the existing `withTaskGroup`. The phase transition `.discovering → .tagging` now fires on the **first** file received; `viewModel.totalCount` updates live with the discovery count and locks at the end.
4. **Removed the `["Unclassified"]` literal.** `VisionWorker.classify` returned `["Unclassified"]` when no scene labels passed the 0.50 confidence threshold. New behavior: returns `[]`. The downstream pipeline already filters generic Vision tags; an empty tag set is more honest than a fake label that pollutes search/cleanup.

**Risk: face-print vectors will shift across re-scan.** Per-face feature prints are now extracted via `regionOfInterest` on the original image's handler instead of from a separately-decoded cropped CGImage. The padding (15%) and `imageCropAndScaleOption = .scaleFill` are preserved, so the distribution should be very close — but not byte-identical. Existing `FacePrintCache` entries will produce slightly different cluster IDs on the first re-scan after this change. `FaceClusteringService.l2` already returns `.infinity` on dimension mismatch (per the 2026-04-23 entry below) so the change cannot silently corrupt clusters; the worst case is one round of "duplicate identities" that the next merge-suggestion pass surfaces.

**Why not AsyncStream for the discovery queue:** AsyncStream's `AsyncIterator` isn't `Sendable` enough for Swift 6 strict concurrency to allow it to cross actor boundaries. Wrapping the iterator in a small actor wrapper triggered "cannot call mutating async function on actor-isolated property" errors. The continuation-pool actor (`DiscoveredQueue` with `[CheckedContinuation]` waiters) is the same pattern `VisionWorkerPool` already uses, so it's consistent with the codebase and trivially Sendable.

**Why no `LEGACY_FACE_CROPS` `#if`:** the original face-print path is deleted outright. The user is the sole developer, the change is reviewable, and the cluster-id reshuffle is recoverable via re-clustering. A compile-time fallback would add maintenance weight for no real benefit.

Sessions B and C of the same overhaul plan (tag-richness via TagTaxonomy / EXIF / NLTagger / GeocodeQueue / face-name propagation; CLIP tokenizer port + 400-label vocabulary) are landing separately.

## 2026-04-24 — Unmount inactive tabs *during scan* (amending ZStack keep-alive)

The 2026-04-23 ZStack keep-alive (see entry below) trades per-tab-switch fetch cost for 6× live `@Query` subscriptions that persist across the scan. Batch 5 scan.log showed the unintended consequence: throughput cliff from 80 → 6.7 files/s at ~17 K files, with resident memory jumping 294 → 587 MB. Every `store.save()` fired SwiftData change notifications that re-materialized all six `@Query` result sets on the main actor. The unbounded `FileGrid` query materializing 17 K rows + O(N) `filtered` per body eval was catastrophic at scale.

**Decision:** Extend `TabHost` with `mounted: Bool`. Policy: while `viewModel.isProcessing`, only the Library + active tab are mounted; all other tabs render `Color.clear`. Idle behaviour is unchanged (all six mounted, instant switches).

Also added `fetchLimit = 2_000` to `FileGrid`'s descriptor and cached `filtered` into `@State` so re-sort / scroll / hover don't re-filter the full table.

This amends but does not supersede the 2026-04-23 decision. The ZStack keep-alive is still the right call for idle UX; the Batch 4 pass just under-scoped the scan-phase cost model (6× notifications × unbounded query = O(N×6) per batch save, which is fine at 2 K rows and lethal at 17 K).

**Tradeoff:** tab switches during a scan cost one fresh mount (~100 ms for CleanupView's 500-row descriptor; Library is always-mounted so switching *back* is free). The user watches Library during scans anyway, so this lands on the right side of the tradeoff.

## 2026-04-24 — Off-main wipe + `isWiping` splash + `removeAllAsync`

`AppViewModel.startProcessing` previously ran two long operations synchronously on the main actor before spawning the scan task: `FacePrintCache.removeAll()` (17 K file deletes) and `await store.wipeForNewScan` (17 K `FileRecord` + `PersonRecord` deletes with live `@Query` observers). User scan.log showed a 27-minute stall between Cancel and the next Discovery on a 17 K-file library.

**Decision:** Three-part refactor.
1. New `@Published var isWiping` on `AppViewModel`. `MainWindowView.MainContent.body` renders a centered `WipingSplash` (ProgressView + "Clearing previous scan…") while true. The six-tab ZStack is *not* mounted during the wipe — every `@Query` is torn down, so `modelContext.delete(model:)` fires SwiftData notifications into nothing.
2. `FacePrintCache.removeAllAsync()` added — dispatches the 17 K directory delete onto the existing `writeQueue` so `startProcessing` doesn't wait on disk.
3. `FaceClusteringService.rebuildIndex()` call immediately after wipe dropped entirely — the wipe just deleted every `PersonRecord`, so the rebuild has nothing to do. `rebuildIndex` still runs at `setUp` (launch) and resume, where it actually matters.

**Why not a chunked delete inside `wipeForNewScan`:** the single-shot `modelContext.delete(model:)` is already batched internally by SwiftData. The dominant cost was notification fan-out to six `@Query` observers, not the delete itself. With the splash tearing every observer down, the single-shot delete should be O(rows) not O(rows × views). Chunking is kept as an option in the Batch 5 plan if a user re-run shows otherwise.

## 2026-04-24 — Resume detection via incomplete `ScanSession` predicate

`startProcessing` unconditionally wiped on every Start click — even when the user pressed Cancel mid-scan and then re-clicked Start on the same folder, which semantically is Resume. User's scan.log showed exactly this: 17 K files tagged, Cancel, Start on the same folder → triggered a wipe that threw away every bit of work.

**Decision:** New `FileIDDataStore.hasIncompleteScanSession(forFolder path: String) -> Bool` fetches `ScanSession` with `completedAt == nil && folderPath == path`. `startProcessing` checks this before wiping; on match, it skips wipe + `FacePrintCache.removeAll` + `rebuildIndex` and calls `runScan(folderURL:..., resuming: true)` directly. Status label shows "Resuming previous scan…".

**Why not prompt the user:** default-to-resume matches user intent in the common case (Cancel-and-retry). The explicit "start fresh" path already exists (`startNewScan()` on `AppViewModel`) and can be surfaced as a follow-up if users hit a case where resume is wrong.

**Edge case:** if the incomplete `ScanSession` was written hours/days ago and the folder contents have diverged on disk, resume will still pick up from the old cursor. Acceptable — the next full scan still catches everything the watcher didn't, and the user always has startNewScan as an escape hatch.

## 2026-04-24 — Live-cluster threshold bumped to 2 000 prints (from every batch)

Batch 3 added a post-batch `FaceClusteringService.shared.clusterBatch(prints: handoff)` detached Task so PeopleView would populate mid-scan. At 250 files × ~10 faces avg × 500 existing identities × 3 centroids = millions of L2 ops per batch, serialized through the `@ModelActor`. Each `clusterBatch` also ends with `try? modelContext.save()` — which fired SwiftData notifications that hit PeopleView's `@Query`. Combined with the tab-unmount fix above, the cluster pulse is the last per-batch main-actor-notification pressure source left.

**Decision:** Accumulate `pendingFaces` across batches; only fire the detached cluster task when `pendingFaces.count >= 2_000` (new `fileprivate static let liveClusterThreshold = 2_000` in `MediaProcessor`). The post-scan synchronous tail flush at `MediaProcessor.swift:284` picks up any remainder, so no prints are lost.

**Why 2 000:** on a typical library with ~10 faces per file, that's a 200-file window — roughly every 5 batches at `saveEvery = 250`. Net effect: cluster pulses drop ~5× while PeopleView still populates within a minute of scan start.

**Why not gate on time instead:** count-based is cheaper (no timer) and directly proportional to work-to-do, which is what we actually care about. A 10 s timer would fire with 2 faces on a document-heavy corpus and with 50 K faces on a photo dump.

## 2026-04-23 — ZStack keep-alive for tab views (instead of `.id()` recreate)

The sidebar tab shell in `MainWindowView` previously wrapped content in `Group` with `.id(viewModel.activeTab)`. That `id()` forces SwiftUI to destroy and recreate the entire subtree on every tab switch, so each switch re-runs every `@Query`'s initial fetch. On a 59 K-file library `CleanupView` took 1–3 s to draw after every switch — the user called it "incredibly slow."

**Decision:** Replace with a `ZStack` of six `TabHost { ... }` wrappers. Every tab stays mounted; `TabHost` gates visibility via `opacity` + `.allowsHitTesting(_:)`. `@Query` subscriptions persist, so SwiftData's change notifications update all six views in place and switching is instant.

**Alternatives considered:**
- `TabView` — has its own ceremony (picker bar, swipe gestures) we didn't want.
- A view cache keyed on `activeTab` — more complex than ZStack and offers nothing over it on a fixed set of six tabs.
- Keep `.id()` but add per-view pagination to lower fetch cost — treats the symptom, not the cause; doesn't help views like PeopleView that intentionally load everything.

**Tradeoff:** 6× live `@Query` subscriptions. SwiftData's change notification delivery is shared and cheap; the real cost is paid once per launch instead of per switch. Memory budget was explicitly OK'd by the user ("we are using less than a gigabyte" on a 16 GB machine).

## 2026-04-23 — `PersonRecord.fileIDs` added as authoritative cluster membership

`PersonRecord` originally stored `sampleFileURLs: [URL]` (≤8, for card thumbnails) and `featurePrintsData: [Data]` (the raw face-print bytes used for cosine matching). There was no authoritative list of every `FileRecord.id` in a person's cluster. Once Batch 4 needed a People-detail view that shows *all* of a person's photos plus a "Not this person" action that moves photos between clusters, the missing link became the blocker.

**Decision:** Add `var fileIDs: [UUID] = []` to `PersonRecord`. `FaceClusteringService.clusterSync` appends on update/create; `merge(sourceID:targetID:)` concatenates deduped. `FaceClusteringService.rebuildIndex` gains a one-shot backfill that scans `sampleFileURLs` for legacy libraries (gated by a per-version `UserDefaults` flag so it only runs once per upgrade).

**Why not a SwiftData inverse relationship?** Would require declaring `@Relationship(inverse:...)` on both sides and a migration to populate on existing stores. The `[UUID]` approach is ORM-agnostic, JSON-migrate-safe, and lets the reassign flow treat cluster membership as a simple set operation. The matching flow uses `featurePrintsData` for actual recognition work — `fileIDs` is purely the "who belongs to this cluster" index.

**Why `FileRecord.id` → persistent by design.** `FileRecord.id: UUID` is the stable key across the store (also used as `FacePrintCache`'s filename). Safer than URLs, which change when users move files through the Restructure tab.

## 2026-04-23 — Streaming Deep Analyze with chunked fetch instead of one big load

The crash repro was: Deep Analyze → Full Sweep → click Run on a 25 K-file library → app OOMs around 11 GB resident. Root cause is three-part:
1. `FileIDDataStore.deepAnalyzeTargets(fullSweep:)` fetches the entire `FileRecord` table into `ModelContext` before compactMapping.
2. The call site in `MediaProcessor.runDeepAnalyzePassIfEnabled` assigned that 50 K-entry array to a single `let targets`, pinning the whole object graph for the full pass.
3. Qwen 2.5-VL 3B holds ~3 GB on MLX GPU cache indefinitely; per-file `loadImage` decoded up to 768 px CGImages with no autorelease between iterations.

**Decision:** Stream in 64-file chunks. New paginated `deepAnalyzeTargetIDs(fullSweep:limit:)` + `deepAnalyzeTargetCount(fullSweep:)` return tiny `DeepAnalyzeTarget { id; url }` structs — no `FileRecord` objects held across chunks. The per-file `analyze()` wraps CG decode in `autoreleasepool`. Between chunks: `DeepAnalyzeService.trimCaches()` (`MLX.GPU.clearCache`) + 50 ms sleep, escalated to 500 ms when `Hardware.isUnderMemoryPressure`. `unload()` is called at end of pass to release Qwen (~3 GB) and reset MLX cache cap — re-loading costs ~10 s so don't call between chunks.

**Why offset-0 each loop instead of tracking an offset cursor:** the predicate is `deepAnalysis == nil`. Every completed file drops out of the result set, so a fresh fetch gives the next chunk naturally — and the pass becomes trivially resumable after force-quit (relaunching Run picks up where it left off, no state to save).

**Why not autorelease around the whole `analyze()` call from `MediaProcessor`:** `autoreleasepool { Task { ... } }` is synchronous; the `Task` escapes the pool immediately. The pool has to wrap the synchronous CG decode, which lives inside `DeepAnalyzeService.analyze` — the async `await` on the actor naturally drains between files.

## 2026-04-23 — `Hardware.isUnderMemoryPressure` promoted from `VisionWorker.MemoryPressureLogger`

The diagnostic `MemoryPressureLogger` in `VisionWorker.swift` was read-only (it `NSLog`'d pressure events without exposing state). The new Deep Analyze streaming loop needs to *decide* between a short 50 ms inter-chunk sleep and a longer 500 ms backoff. Rather than duplicating `DispatchSource.makeMemoryPressureSource`, promote it to `Hardware.swift` and expose `isUnderMemoryPressure` / `isUnderCriticalMemoryPressure` / `residentMB()` as the single source.

**Why not Combine:** A `@Published Bool` would require a `MainActor` observer and cross-actor hops we don't need — the chunk loop just reads it synchronously between chunks.

**Why `static var`:** The pressure source is a process-level singleton. The backing `PressureMonitor` is `@unchecked Sendable` (stored state guarded by `NSLock`); `_pressure` is an `Int32` storing level (0 normal, 1 warning, 2 critical). Writes happen only from the pressure queue's event handler; reads are cheap and don't need to wait.

## 2026-04-20 — Force Xcode toolchain via `DEVELOPER_DIR` in `run.sh`

`@Model` from SwiftData expands at compile time via the `SwiftDataMacros` plugin, which ships **only with Xcode**, not with the Command Line Tools. On a developer machine where `xcode-select -p` points at `/Library/Developer/CommandLineTools`, `swift build` fails with `external macro implementation type 'SwiftDataMacros.PersistentModelMacro' could not be found`.

**Decision:** `run.sh` always sets `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` before invoking `swift build`, and bails with a clear error if Xcode isn't installed.

**Alternatives considered:**
- Telling the user to run `sudo xcode-select -s ...` — too easy to forget; not portable.
- Adding an explicit macro plugin dep to `Package.swift` — SwiftData macros aren't published as a standalone SPM package; this isn't possible today.
- Switching to XcodeGen + `xcodebuild` — `project.yml` exists but adds complexity for no current benefit. Re-evaluate if SPM bites us again.

## 2026-04-20 — Replace `ModelContext.reset()` with context recreation

Three call sites in `MediaProcessor.swift` (preview-name pass, duplicate detection, folder restructure) used `context.reset()` to drop tracked objects between batches. The current SwiftData SDK (Swift 6.3.1, macOS 26 SDK) no longer exposes `reset()` on `ModelContext`.

**Decision:** Replace each `context.reset()` with `context = ModelContext(container)` and promote the surrounding `let context` to `var context`. For the parameter case in `runDuplicateDetection(context:)`, shadow the parameter as a local `var`.

**Why:** Equivalent semantics — drop tracked objects, rely on a fresh context for the next batch. Cheap to allocate. Keeps the OOM-mitigation intent of the original code intact.

**Note:** This is a band-aid. The right Phase 1 design is a single context with batched saves every ~1000 files instead of recreate-per-batch. Revisit when the perf engine lands.

## 2026-04-20 — Add `ThumbnailView` SwiftUI wrapper

`AcceptChangesView`, `PeopleView`, and `FolderOrganizationView` all referenced `ThumbnailView(url:)` but no such SwiftUI view existed — only the `ThumbnailService` actor that returns `NSImage`. The build was previously masked by the `context.reset()` errors which bailed earlier in compilation; once those were fixed, the missing-type error surfaced.

**Decision:** Add `Sources/ThumbnailView.swift` as a thin SwiftUI wrapper over `ThumbnailService.shared.getThumbnail(for:)`. Renders a placeholder while loading, swaps in the `NSImage` when the task completes, and re-runs on `url` change.

**Why:** The three call sites all expect identical behavior (URL in, sized thumbnail out, async-loaded). Centralizing in one place avoids duplication and keeps the QuickLook-backed cache in `ThumbnailService` as the single source of truth.

## 2026-04-21 — VisionWorker: @unchecked Sendable + pool owns workers

`VNRequest` objects are not thread-safe to share across concurrent `perform()` calls (they mutate `.results` in place). But they ARE safe to reuse sequentially within one task.

**Decision:** `VisionWorker` is `final class` with `@unchecked Sendable`. The pool guarantees one-owner-at-a-time via actor-isolated acquire/release. Each TaskGroup task borrows a worker, does all its Vision work, then releases.

**Why not actor per worker:** Actors add suspension overhead on every call. Since each worker is owned by exactly one Task at a time, actor isolation buys nothing here — the `@unchecked Sendable` + pool-ownership invariant is sufficient and faster.

## 2026-04-21 — Face clustering: L2 distance on raw floats instead of computeDistance()

`VNFeaturePrintObservation.computeDistance()` requires two live `VNFeaturePrintObservation` objects. Deserializing N centroids from `NSKeyedArchiver` on every incoming face would be O(N) NSKeyedUnarchiver calls.

**Decision:** Store centroid as a `[Float]` running mean in memory; compare using raw L2 distance. The `distanceThreshold` of 0.65 was chosen to approximate Vision's own metric empirically. If testing shows over- or under-merging, adjust in `FaceClusteringService.distanceThreshold` and document here.

**Why not K=3 centroids:** The running-mean centroid is O(1) per update vs O(K×N) k-means. K-means brings marginal benefit for N < 1000 identities and would complicate the merge() logic. Add K-centroids later if empirical testing shows it matters.

## 2026-04-21 — OfficeDocReader uses /usr/bin/unzip instead of Foundation zip APIs

Foundation doesn't ship a built-in zip extraction API (unlike Java's ZipInputStream or Python's zipfile). The `Compression` framework only handles raw deflate/lz4/zlib, not the zip container format.

**Decision:** Shell to `/usr/bin/unzip` (always present on macOS, part of Info-ZIP). Unzip to a UUID-named temp directory, parse XMLs with NSXMLParser, then `defer { removeItem }`.

**Alternatives considered:** ZIPFoundation (third-party — forbidden), manual zip parsing (fragile), reading `.docx` as a FileWrapper (doesn't work for zip), embedding a C zip library (no deps policy).

## 2026-04-21 — FolderOrganizationView: HSplitView + LazyVStack replaces canvas

The knowledge-graph canvas was O(N×M) connection lines + a 6000×6000 DotGridCanvas rendered at all times. With 50K files, this caused visible GPU load even when the tab wasn't in focus.

**Decision:** Replace with `HSplitView` containing two `ScrollView { LazyVStack }` panes. No canvas, no connection lines. The split handle is native macOS affordance (better than zoom/pan). `LazyVStack` only renders visible rows, keeping memory and GPU usage flat as file count grows.

**Tradeoff:** Loses the visual "flow" of connections between current and proposed folders. The explicit folder-count badges and color coding compensate for readability.

## 2026-04-23 — `FaceClusteringService.l2()` treats dimension mismatch as infinite distance

`VNGenerateImageFeaturePrintRequest` returns different embedding dimensions across Vision revisions — e.g. a 512-dim observation from an older macOS build vs a 2048-dim observation after a macOS upgrade. Prior code used `let n = min(a.count, b.count)` and compared only the first N components, which silently **partial-matched** two feature-prints taken at different revisions. Consequence: after the user upgraded macOS, the first scan would merge unrelated identities because the leading components of two different-dim embeddings can land within the 0.65 threshold by coincidence.

**Decision:** `l2(a, b)` now returns `.infinity` when `a.count != b.count`. A cross-revision comparison is treated as a non-match, so the new scan creates a fresh identity rather than polluting an old one.

**Alternatives considered:**
- **Truncate to min dim and scale** — not valid; the two embeddings aren't projections of each other, they're different models.
- **Re-extract feature-prints on detected dim change** — heavy (re-run Vision over the whole corpus); punt until we see a concrete need.
- **Drop the old embeddings entirely on version change** — equivalent to the chosen approach but louder. The `.infinity` approach lets the normal clustering path "self-heal" as new scans lay down fresh identities with the current revision.

**Why this is the right default:** A spurious merge silently corrupts the People view — the user has no UI to split identities back apart. A missed merge just creates a duplicate identity that the next merge-suggestion pass will surface. Err toward duplicate-then-merge, never toward silent-wrong-merge.

---

## 2026-04-25 — v2 hardening: auto-respawn, orphan sweep, face-clustering job model

**Decision:** Engine auto-respawn with bounded backoff (3 attempts at 1s/4s/16s within 60s); post-scan orphan sweep with 5000-row cap; face clustering as a one-shot, idempotent job triggered via IPC, **not** an inline-during-scan computation.

**Why auto-respawn (vs "tell user to relaunch the app"):** A panicked engine takes the user's session — but the user's intent ("scan this folder") hasn't changed. Auto-respawn within bounds preserves intent. The 1s/4s/16s backoff gives breathing room for recoverable transient causes (e.g. memory spike during pre-warm) without log-spamming on a deterministic crash. The 60-second window means a "transient" crash a minute ago doesn't count against the budget. After 3 misses we go `.crashed` and surface a Settings-level retry button — at that point it's a real bug, not a hiccup.

**Why orphan sweep is post-scan and capped (vs continuous + uncapped):** Files the user deletes from Finder leave broken-tile rows in Library. Two extreme designs were rejected: (a) continuous file-system watching (`DispatchSource.makeFileSystemObjectSource` per file) — way too many fds at 60K-file scale; (b) re-stat every row at every Library refresh — adds a stat per tile per render, kills scroll perf. The chosen design runs once at end-of-scan, scoped to the scan root via `path_text LIKE rootPath/%`, only on rows the scan didn't touch (`scanned_at < scanStart`), capped at 5000 candidates per pass. The cap is intentional: a 60K orphan sweep would itself be a 30-second pause; capping at 5000 means worst-case ~3s, and the next scan picks up where this one left off.

**Why face clustering is a one-shot job (vs inline during scan):** Three reasons. (1) Clustering is O(N) per face but each face needs O(log N) HNSW lookup against all prior faces — coupling that to per-file work means later files in a scan get progressively slower, and we'd have to rebuild the index across runs anyway. (2) The user wants to look at clusters AFTER scans complete, not during — making it on-demand keeps scan throughput unchanged. (3) Idempotent rebuild from `face_prints` makes "re-cluster" a safe operation when threshold tuning lands. Per-face print extraction stays inline (during tagging) because the cropped-face Vision request runs on the SAME `VNImageRequestHandler` as the face-rect detection, which is essentially free — the print itself is what we want anyway, so paying for it inline is the cheapest place.

**Why HNSW is rebuilt every clustering run (vs persistent + incremental):** Clustering runs are user-initiated and the data shape changes (new prints, deleted files). A from-scratch HNSW build over 50K face prints takes ~1-2 seconds on M1 — not worth the complexity of a persistent index file + invalidation logic + corruption recovery. If clustering ever exceeds 10s on a real library, persistent HNSW becomes worth it; until then, build-once-per-job is right.

**Why ThumbnailService stays single-shot QL API (`generateBestRepresentation`), not the multi-rep one:** `generateRepresentations(for: .all)` calls the update block once per representation type — and our `CheckedContinuation.resume` was firing on each, hence the 2026-04-25 SIGTRAP crash. The single-shot API gives us one callback, one resume, no race. The quality difference at 192px tile size is invisible.

---

## 2026-05-02 — Multi-platform repo restructure (Phase 0 of Windows port)

**Decision:** Move every macOS source file into `platforms/apple/` (one mechanical commit), reserve `platforms/windows/` and `platforms/linux/` as siblings, and hoist a top-level `shared/` directory holding `ipc-schema/`, `docs/` (this file lives there now), `test-corpus/`, and `scripts/`. Each platform's CLAUDE.md lives next to its code; the root `CLAUDE.md` is a router.

**Why this layout (vs keeping macOS at root + adding `windows/` sibling):** Symmetry. The moment cross-platform work lands, asymmetric layouts force readers and tooling to special-case "the original platform" — every doc would say "see app/ on macOS, src/FileID.App/ on Windows" and pattern-matching breaks. Symmetric `platforms/<os>/` lets every reference disambiguate by prefix and lets future-Linux slot in with no further restructure.

**Cost paid:** every macOS path in `run.sh`, `iterate.sh`, `Package.swift`, scripts, and docs ostensibly changed. In practice the script paths use `$(dirname "$0")`-derived `PROJECT_DIR` and Package.swift's `path:` strings are relative — both auto-resolved correctly under the new root. Only doc cross-references and gitignore patterns needed manual updates.

## 2026-05-02 — Windows engine in Rust + UI in WinUI 3, with WinAppSDK 1.6+

**Decision:** Windows engine binary is Rust (`fileid-engine`, `cargo build --release`); Windows UI is WinUI 3 unpackaged desktop app (.NET 8/9, C#, XAML). Two binaries shipped together via WiX MSI installer. Both built for `x86_64-pc-windows-msvc` AND `aarch64-pc-windows-msvc` from day one.

**Why Rust for the engine (vs C# .NET 8):** ONNX Runtime DirectML / CUDA / OpenVINO / QNN bindings via the `ort` crate are best-in-class on Rust; `llama-cpp-2` gives clean Rust→llama.cpp bindings; `rusqlite` with bundled SQLite + FTS5 matches the macOS GRDB schema byte-faithfully; `tokio` channels translate the Swift `AsyncChannel` + actor scan pipeline 1:1; no GC pauses on the hot path; release builds with `lto = "fat"` produce a single 15–25 MB statically-linked .exe with zero runtime. Cross-compile from x64 to ARM64 is `cargo build --target aarch64-pc-windows-msvc` with no friction. Same crate compiles unchanged for Linux when Phase 5 lands.

**Why WinUI 3 for the UI (vs Avalonia):** User explicitly chose max-native Windows fidelity over cross-platform UI reuse. WinUI 3 gives DWM-rendered Mica + Acrylic (not a software approximation), `SpringScalarNaturalMotionAnimation` from `Microsoft.UI.Composition` (real GPU spring physics — no math port from SwiftUI's `.spring(response:dampingFraction:)` needed), Win2D for hardware-accelerated custom canvas (LavaLamp + Sankey port), and the same Composition pipeline DWM uses. Linux UI is now a clean-slate decision in Phase 5 rather than a constrained extension of an Avalonia codebase. Tradeoff accepted: the Linux UI will be a separate codebase, not a reuse of the Windows one.

**Why unpackaged + WiX MSI (vs MSIX):** Standard `C:\Program Files\FileID\` install. No Microsoft Store dependency, no MSIX sandbox restrictions on file access. WiX v4 produces both `FileID-x64.msi` and `FileID-arm64.msi` from the same project. Self-contained .NET publish (`--self-contained true`) bundles the runtime so users don't need .NET installed; users get a single `FileID.exe` + companion DLLs.

## 2026-05-02 — IPC schema canonicalization + breaking change to startScan

**Decision:** The wire protocol moves to `shared/ipc-schema/ipc.schema.json` as the single source of truth. Per-platform DTO files (Swift `IPCProtocol.swift`, Rust `ipc/mod.rs`, future C# `Generated.cs`) are hand-maintained mirrors of the schema until codegen lands. The `IPCCommand.startScan` payload changes from `(rootBookmark: Data, rootPathDisplay: String)` to `(rootPath: String, rootDisplay: String?)` — security-scoped bookmarks have no Windows analog and the macOS app is unsandboxed today.

**Why a JSON Schema rather than a Codable-first or proto-first approach:** JSON Schema is language-neutral, the macOS engine already speaks JSON Codable, and the schema documents the existing Swift Codable wire format precisely (externally-tagged unions with `_0` wrappers for single-positional cases). Future codegen can target it without a wire-format renegotiation. Cap'n Proto / FlatBuffers were rejected: too much schema-evolution ceremony for our IPC volume, and they'd force a wire breaking change.

**Why hand-maintained mirrors:** Phase 0's scope is "stand up the contract and prove cross-platform compatibility." A real codegen toolchain (quicktype, custom Python, etc.) is a Phase 4 polish item. Until then, every PR that touches `ipc.schema.json` must update all three DTO files in the same commit and run round-trip tests on each platform.

**The breaking change is staged:** the macOS engine + app still use the legacy `rootBookmark` payload as of this commit (the user verifies Swift compiles on a Mac). The Rust engine implements the NEW payload from day one. A follow-up commit (clearly labeled, Mac-side only) deletes the bookmark code path.

## 2026-05-02 — Zero telemetry, ever, as a product feature

**Decision:** No analytics SDK, no crash-reporting service, no update pings, no model-download instrumentation. Local-only logs to `%LOCALAPPDATA%\FileID\logs\` (Windows) / `~/Library/Logs/FileID/` (macOS). The only network code in the engine is the user-initiated HuggingFace model downloader. CI grep-gates every shipped binary for telemetry-related strings (Sentry, Application Insights, GA, Segment, Mixpanel, Amplitude, PostHog, Datadog, Bugsnag, Rollbar, Honeycomb, NewRelic, Raygun) — zero hits required for release.

**Why this is a feature not an oversight:** Users open FileID against their personal photos, work documents, financial scans. Even "anonymous" telemetry leaks structure ("user X scanned 47K files in folder Y, used Deep Analyze 3 times"). The product proposition is on-device privacy; telemetry would compromise the proposition. Documented in `shared/docs/PRIVACY.md` and surfaced in the Settings tab "What we don't do" panel.

## 2026-05-02 — GPU acceleration: DirectML + Vulkan baseline, optional Performance Packs

**Decision:** Out-of-the-box install ships ONNX Runtime with DirectML EP + CPU EP, and llama.cpp with Vulkan + DirectML + CPU backends. This covers NVIDIA, AMD, Intel discrete, Intel iGPU, AMD iGPU, and Snapdragon Adreno without any extra runtime install. Power users opt into Performance Packs via Settings: NVIDIA CUDA Pack (~600 MB), Intel OpenVINO Pack (~300 MB), Snapdragon NPU Pack (~150 MB). Auto-suggested when matching hardware is detected.

**Why DirectML universal default (vs CUDA-required):** CUDA + cuDNN runtime is a 600 MB+ download and only benefits NVIDIA users. DirectML ships in Windows, works on every D3D12-capable GPU, and gets within 10–20% of CUDA for our model sizes. Bundling CUDA by default would bloat the install for the majority of users (Intel + AMD + Adreno) who don't benefit. Performance Packs pattern lets us serve the long tail without weighing down the base case.

**Why Vulkan for llama.cpp baseline:** Vulkan in llama.cpp is mature and runs at 80–95% of CUDA perf on NVIDIA, full-tilt on AMD (where ROCm on Windows is unreliable), and full-tilt on Intel Arc + iGPU. Single backend covers all three vendors. CUDA backend remains opt-in for NVIDIA users who want maximum throughput.

## 2026-05-02 — Windows on ARM (Snapdragon) is first-class from day one

**Decision:** Build matrix includes `aarch64-pc-windows-msvc` from Phase 0; CI runs on `windows-11-arm` runners; ship `FileID-arm64.msi` alongside `FileID-x64.msi`. Snapdragon X Elite Hexagon NPU access via ONNX Runtime QNN EP (Snapdragon NPU Performance Pack).

**Why first-class (vs ship x64 only and let WoA emulate):** The Hexagon NPU is the closest hardware analog to Apple's Neural Engine on Windows. Native ARM64 + QNN EP gives Snapdragon WoA users the same power-efficient ML inference profile macOS users get on M-series. x64 emulation on WoA loses both performance and power efficiency for what is otherwise a compelling "M1-like" Windows machine. All our deps (ORT, llama.cpp, pdfium, Win2D, windows-rs, WinAppSDK, .NET 8/9 self-contained) have ARM64 builds — no blockers found at plan time.

## 2026-05-11 — [EP] log trail mirrors [INSTALL] trail; AddDllDirectory is the pack-discovery contract

**Decision:** `create_session` emits a positive-outcome `tracing::info!("[EP] built session", ep, vendor, adapter, model)` line in `runtime.rs:245` whenever an EP successfully builds a session. Pack extraction additionally walks the extracted root + one subdir level and calls `AddDllDirectory` on any dir containing `.dll` (via `platform.rs::register_dll_dirs_under`); the same helper is replayed at engine startup for previously-extracted packs.

**Why the [EP] tag (vs leaving the silent positive path):** Diagnostic clarity. The engine already logged `[EP] failed to build; trying next` on the negative path. Without a paired positive line, a user reporting "scanning feels slow on my NVIDIA box" had no way to confirm from `app.log` which EP actually committed. The new line is structurally identical to V14.7.16's `[INSTALL]` discipline: every meaningful state transition logs once.

**Why AddDllDirectory (vs symlinking pack DLLs next to the engine):** SEC-3 locked the default DLL search to System32 + the engine binary's dir. Symlinking would put third-party DLLs next to the trusted engine binary — a smaller attack surface than PATH planting but still mixes installer-managed and user-extracted files in the same dir. `AddDllDirectory` adds a single trusted dir to the per-process search list and leaves the engine's own directory clean. The walk is one level deep because all observed pack layouts (CUDA, OpenVINO, QNN) keep DLLs flat or in one bin/ subdir — deeper recursion would invite long-tail false positives.

**Why replay on startup (vs only post-install):** Without replay, packs installed in a prior session were invisible on the next launch. `AddDllDirectory` is per-process state, not per-machine state — packs need re-registration every engine spawn.

## 2026-05-11 — Audit findings: half were already shipped

**Decision:** When a multi-agent audit produced a "missing parity" gap list, several items (Cleanup per-group menu V14.7.6, People multi-select merge FEAT-CRIT-1, FilePreviewSheet sibling nav V14.7.2, Settings install cards) turned out to already be implemented. Verified by grepping for the named symbols + reading the corresponding views; only the actual gaps (rainbow-shimmer hero, install card rate/ETA, [EP] log line, AddDllDirectory wiring) got engineering work.

**Why verify before implementing:** A multi-agent audit reads excerpts and infers gaps from absence-of-reference. Treating its output as authoritative would have produced duplicate work or, worse, replaced working code with a fresh implementation that subtly broke established behavior. The audit's value is in the AREAS it flags, not the CONCLUSIONS it draws.

**Worked example:** The audit reported "ReadStore concurrent-SQLite race" as a high-severity bug. Reading `ReadStore.cs:106-110` showed the `_gate` IS acquired before any query work — the comment at `:100` describes a delegation path, not the bug. False positive. Treated as a sanity-check anchor (and the comment's wording reviewed for future readers) but no code change.

## 2026-05-11 — GPU Performance Packs removed (no shippable URLs)

**Decision:** Drop the CUDA / OpenVINO / QNN Performance Pack registry entries and the welcome-sheet + Settings install UI. Keep `llama_runtime_x64` (Vulkan llama.cpp from ggml-org's GitHub releases) — it's a real downloadable URL used by Deep Analyze. DirectML becomes the universal GPU path for every D3D12-capable vendor (NVIDIA / AMD / Intel); CPU is the floor for Snapdragon X and no-GPU machines.

**Why removed (per vendor):**
- **CUDA** — Microsoft's `onnxruntime-win-x64-cuda12-*.zip` (real, ~150 MB) ships the ORT CUDA EP but NOT cuDNN, a hard LoadLibrary dependency. Bundling cuDNN means building + hosting our own composite ZIP under NVIDIA's redistribution license. An engineering project + ongoing legal review, not a URL swap.
- **OpenVINO** — Intel publishes the OpenVINO runtime, but ORT's OpenVINO EP needs a specific Intel-built ONNX Runtime distribution that isn't redistributed as a standalone ZIP. Wiring two parallel ORT installs that share weights costs more than the perf win.
- **QNN** — Qualcomm SDK is behind a developer-portal terms-acceptance gate. There is no public download URL we can point at.

**Why keep `llama_runtime_x64`:** The ggml-org GitHub release URL is real, public, and live. Used by `vlm.rs` to spawn `llama-mtmd-cli.exe` for Deep Analyze. Vulkan backend covers NVIDIA + AMD + Intel + Adreno on one binary — no separate per-vendor build needed.

**Why this isn't a scanning regression:** The engine's EP priority chain (`runtime.rs::priority_chain`) already routed everyone through DirectML (or CPU) as the fallback whenever a pack wasn't installed — which was 100% of the time, because the packs never existed. The packs were "max performance" upgrades, not "make it work" plumbing. Per the 2026-05-02 decision ("DirectML universal default ... within 10–20% of CUDA for our model sizes"), DirectML is honest about what it delivers on every vendor.

**Re-introduction path:** Bring back any pack only after three preconditions hold for that pack — (1) a composite ZIP that includes EVERY runtime DLL the EP needs at LoadLibrary time, (2) a license-compliant mirror with the vendor's redistribution license carried inside, (3) Authenticode signatures preserved on every shipped DLL. Today none of CUDA / OpenVINO / QNN clears all three; if any one does later, re-introduction is additive to `registry.rs` + `ModelInstallerService.cs` + the welcome/settings views. The defensive `AddDllDirectory` wiring and `is_*_pack_present` probes stay in place so a power user manually installing a pack-shaped directory still gets the EP picked up.

## 2026-05-11 — NVIDIA acceleration via two honest paths (CUDA llama.cpp + system-CUDA probe)

**Decision:** Deliver real NVIDIA performance through two complementary paths that don't require us to ship cuDNN:
1. **CUDA llama.cpp for Deep Analyze** — `llama_runtime_cuda_x64` registry entry pointing at ggml-org's official GitHub release. The CUDA backend uses cuBLAS + custom kernels, no cuDNN needed. Works on any modern NVIDIA driver. 15-25% VLM speedup vs the Vulkan default.
2. **System-CUDA toolkit probe for scanning** — at engine startup, search `CUDA_PATH` / `CUDA_PATH_V12_X` / `%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\V*\bin\` for the user's existing CUDA Toolkit + cuDNN install. If found, `AddDllDirectory` the bin dir so ORT's CUDA EP can load — `priority_chain` then prepends CUDA for NVIDIA hardware automatically. 10-15% scanning speedup for the subset of NVIDIA users (ML researchers, deep-learning gamers) who already have CUDA installed.

**Why these and not "bundle cuDNN":** cuDNN's NVIDIA redistribution license requires a partner agreement + license file shipped inside any redistributed bundle. That's an engineering + legal project, not a code change. The two paths above sidestep that:
- llama.cpp CUDA build doesn't need cuDNN at all — it's a real, redistributable, MIT-licensed binary.
- System-CUDA probe consumes the user's own cuDNN install — we never touch it, just point the loader at it.

**Why these and not "DirectML FP16 tuning":** the `ort` 2.0.0-rc.10 Rust crate doesn't expose FP16 / graph-opt knobs on its DirectML builder (Phase 1 audit confirmed). Upstream feature request territory, not a shippable change today.

**Coverage:**
- NVIDIA + CUDA installed → CUDA EP for scanning + CUDA llama.cpp for VLM. Full NVIDIA performance.
- NVIDIA without CUDA → DirectML for scanning + Vulkan llama.cpp for VLM. ~80-90% of native. Settings → Performance offers the "Get cuDNN" link + the "Install CUDA llama.cpp" button.
- AMD / Intel / Snapdragon → DirectML or CPU per V14.8.2 (unchanged).

**Trade-offs accepted:** the ~20% of NVIDIA users who don't have CUDA Toolkit installed get a Settings affordance pointing at NVIDIA's developer portal. They have to register an NVIDIA developer account to download cuDNN. That's a real friction step — but it's NVIDIA's friction, not ours, and clicking "Get cuDNN" sends them to the canonical source. We never lie about what we can deliver.

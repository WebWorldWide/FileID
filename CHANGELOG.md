# Changelog

All notable changes to FileID are tracked here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per `shared/docs/PRIVACY.md` and `CLAUDE.md`: this project ships no telemetry, no analytics, no crash-reporter SDKs. The CI privacy gate scans every release binary against a 22-string deny-list before publication.

## [Unreleased]

### Added

#### 2026-06-13 — Restructure parity + safer moves (stability campaign)

- **macOS Restructure now uses the same on-device butler as Windows.** The Restructure tab is driven by the engine's planner instead of a separate app-side classifier, so both platforms produce the same organized layout from one source of truth — full month names, photo / video / audio buckets, GPS-derived Places, and `Documents/<year>`.
- **Moves never overwrite or silently skip.** When a destination filename is already taken, the file is auto-renamed `name (2).ext` (on both macOS and Windows) instead of being dropped or colliding; a move whose source has changed since the plan is skipped rather than misapplied.
- **Renamed or moved files keep their tags, faces, and OCR (macOS).** Moving a file inside your library no longer loses its analysis on the next scan — the app recognizes it as the same file and carries everything forward, matching Windows.
- **Restructure planning and applying can now be cancelled** and report progress while they run, on both platforms.

#### 2026-05-20 — V16.8 VLM activated + faster, Settings tidied

- **The on-device AI model now actually runs.** The bundled llama.cpp runtime was too old (it lacked the multimodal binary and predated the Qwen2.5-VL model), which is why image analysis failed with a "runtime not found" message. The app now ships a current runtime and auto-replaces the stale one on next launch — Deep Analyze (and the new VLM tagging) work without any manual steps.
- **Whole-library AI tagging is far faster.** Instead of reloading the multi-GB model for every file, the app now loads it once and keeps it resident (a local server it manages and shuts down automatically), so tagging your library runs in seconds-per-file instead of reloading each time.
- **Settings tidied toward the macOS layout.** Removed a redundant "Models" info card and a non-functional "Performance profile (coming soon)" picker; trimmed three Windows-only toggles macOS doesn't have (kept "Cleanup"); and folded the verbose hardware diagnostics into a collapsed "Advanced" section, matching macOS. The Windows-only acceleration controls (GPU/EP override, CUDA llama.cpp, cuDNN) stay — macOS doesn't need them because it uses the Neural Engine.
- **NVIDIA GPUs now actually use CUDA for AI analysis.** The CUDA runtime was being installed but never used (the app only looked in the Vulkan folder), so the promised CUDA speedup never engaged. The app now prefers the CUDA runtime on NVIDIA and falls back to Vulkan automatically if CUDA isn't usable. (The CUDA runtime download is larger now because recent llama.cpp ships its CUDA libraries separately; opt out via Settings → Performance if you'd rather stay on Vulkan.)

#### 2026-05-20 — V16.7 VLM-generated tags (optional, higher-accuracy)

- **Deep Analyze now also tags.** When you run Deep Analyze over your library, the on-device vision-language model now produces short scene/content tags (e.g. `dog`, `beach`, `birthday party`) in addition to its caption and smart-rename — written as a distinct tag source that takes display priority over the fast scan-time tags. This gives genuinely-described-by-an-AI tags for the photos you care about, on top of the instant CLIP tags every file gets during a scan. There's a single switch to make these the *only* tags (drop the fast tagger entirely) once you've decided the VLM tags are what you want. **Note:** requires an up-to-date local AI runtime — the bundled one is currently too old to run image analysis; updating it is a tracked follow-up. The "runtime not found" message has been corrected to say "too old — update it" when that's the actual cause.

#### 2026-05-19 — V16.5 CLIP zero-shot scene tagging + force re-tag

- **Scene tags now use CLIP zero-shot.** Library cards label photos with scene/content words (`Beach`, `Kitchen`, `Document`, `Dog`, `Sunset`…) by scoring each image's MobileCLIP embedding against a curated vocabulary with the matched MobileCLIP-S2 text encoder — the same scene-style taxonomy macOS gets from Apple Vision. This replaces the MobileNetV3 ImageNet **object** classifier, whose labels were object oddities (`breakwater` instead of `beach`), and it needs **no separate model download** (both CLIP halves are already installed for search) — so the "downloading something for identifying" step is gone. Tags now carry a confidence score; a card's two chips show the highest-confidence labels. It's also faster: a per-file ONNX inference + image resize is replaced by a tiny vector score on an embedding already computed.
- **"Re-scan everything (force re-tag)"** button in Settings re-tags every file in the current library folder — even ones already scanned — so a tagging or threshold change is visible without deleting the database.

#### 2026-05-19 — V16.3 chip + diagnostics + video COM hardening

- **File-type chip on Library cards.** Every card now leads its chip row with a structured, gray file-type chip (`Image` / `Video` / `Audio` / `PDF` / `Document`), visually distinct from the gold AI-tag chips that follow. Complements the existing thumbnail-corner kind icon badge — the badge is glanceable, the chip is text-readable. Suppressed for unknown-kind files so a meaningless "File" chip never appears. 16 new VM unit tests.
- **Classifier diagnostics in Settings.** Settings → Diagnostics gained a Classifier line showing installed/not, ImageNet class count, confidence threshold, top-K (8), and model size. Lets a user confirm at a glance whether scene tags should appear, instead of tailing `engine.jsonl`. Disk-probe only (sentinel + labels file) — no IPC change.
- **Broken-image placeholder on Library cards.** When the thumbnail service exhausts its fallback chain and returns null, the card now shows a muted procedural image-glyph placeholder instead of shimmering forever. Distinguishes "preview failed" from "still loading." Rendered in XAML (no asset binary).

### Fixed

#### 2026-06-13 — Cross-platform stability campaign (audit-2026-06-10)

- **Data safety: an interrupted scan can no longer wipe a file's tags, person, or recognized text (macOS).** Previously a face/text-recognition timeout could clear analysis a file already had; every destructive re-write is now gated on that step having actually run.
- **People grouping is deterministic and no longer discards your edits.** Face clustering now produces the same groups on every run and reads your renames / merges / mark-as-unknown safely, so re-clustering never loses People-tab edits (macOS, matching Windows). It also no longer silently does nothing after you cancel a scan.
- **"Restart Engine" no longer leaves two engines writing the same library at once (macOS).** The previous engine is stopped before the new one starts.
- **Incremental rescans are faster and still correct.** Unchanged files are skipped at discovery while genuinely deleted files are still pruned and newly-installed models still backfill, so re-scanning an unchanged folder is effectively instant.
- **Lighter, faster macOS scans.** Bounded-memory semantic search, decoupled database commits, cached queries, and a tighter image-decode path; a full large-library scan stayed well under the memory target with no crashes.
- **Windows app polish:** reliable bulk-tag confirm/undo, single-flight Apply (no accidental double-apply), correct selection and rename identity, and a CUDA toggle that takes effect.

#### 2026-05-20 — V16.6 thumbnails persist during scans, accurate tags, faces detected

- **Thumbnails now stay on screen during a scan (and fill in progressively, like macOS).** Previously the Library grid was torn down and rebuilt every second while scanning, so thumbnails blanked out the instant they loaded and never stuck. The grid now updates in place — existing cards (and their loaded thumbnails) persist, and new files slot in as they're found. No re-scan needed; rebuild and relaunch. (Side fix: your selection no longer silently clears when the grid refreshes mid-scan.)
- **Scene tags are accurate now (and absent when nothing matches).** The tagger was scoring every image with a confident label even when none fit — a snapshot would come back "Storm/Diagram/Machinery". It now keeps a label only when the image genuinely matches it (real cosine-similarity threshold instead of a peaky softmax), so you get correct tags or none, not confident-but-wrong ones. Requires a re-tag to take effect (Settings → re-scan, or build with a DB wipe). The match threshold is the main tuning knob and the stored scores are now real similarities, so it can be dialed in against your library.
- **Faces are detected again (People tab).** The face detector was reading its model's outputs in the wrong order and silently finding zero faces in every image (the log was full of "tensor undersized — skipping"). It now identifies each output by shape, so detection works regardless of how the model orders them. Requires a re-scan to populate People.

#### 2026-05-20 — V16.5c invisible Library tiles + tab-switch crash hardening

- **Library tiles were invisible (looked like "thumbnails not loading" AND "tags not showing" at once).** Every Library card — its thumbnail, filename, and tag chips — lives under one container whose entrance animation faded it in from transparent. Under a live scan the grid re-realizes cards about once a second, and the fade could be interrupted and left stuck at fully transparent, so loaded thumbnails and computed tags rendered into an invisible card. The card is now always fully visible the instant it appears; the entrance is a subtle scale-in "pop" that can never hide content. (Forensics on a real session: 8,611 thumbnails were assigned to cards and 24,762 tags were in the database across 100% of files — they just couldn't be seen.) No re-scan needed: rebuild, relaunch, and existing thumbnails + tags appear.
- **App crashed when clicking sidebar tabs.** Switching tabs mid-scan could hard-crash the app (a native fast-fail that bypassed all error handling — intermittent, timing-dependent). Two causes: the tab switcher built the incoming view before the outgoing one finished animating out, and a fast second click could orphan that view as a "zombie" still reacting to engine events forever; and a thumbnail that finished loading just after a tab switch could touch UI elements that were already torn down. Both are fixed — the next view is built only once its swap actually commits, the outgoing view always tears down cleanly, and late thumbnail loads bail out if their tab is gone.
- **Scene tagging now degrades gracefully on unusual model exports.** If a machine's installed CLIP text model can only encode one prompt at a time (a "batch-pinned" export), building the scene-label vocabulary used to fail outright and silently disable all scene tags. It now falls back to encoding prompts one at a time, so scene tags still work. (Common exports — including the one this project installs — are unaffected and keep the fast batched path.)

- **Thumbnails "rendering from anything" on scroll.** A recycled Library tile kept the previous file's bitmap bound to its `Image.Source` and briefly flashed it before the new image loaded, because the recycle handler reset only the image's opacity, never the bitmap. The tile now releases its thumbnail on recycle, which also frees off-screen bitmaps so a large library doesn't accumulate them in memory. Builds on the V16.4 thumbnail-trigger fix.
- **People: double-tapping a person card did nothing.** The double-tap handler resolved the cluster from `el.DataContext`, which `x:Bind` leaves unset on `ItemsRepeater` elements (and unlike drag/drop it had no fallback), so the person-details sheet never opened. Fixed with the same index→DataContext bridge used for Library tiles.
- **Library thumbnails never rendered (root cause, not the fallback chain).** `OnRepeaterElementPrepared` guarded on `el.DataContext is not FileTile` and returned on every tile, because **x:Bind in the ItemsRepeater ItemTemplate doesn't populate the realized element's `DataContext`** — so `LoadThumbAsync` (the only caller of `ThumbnailService.RequestAsync`) never ran. No thumbnail had rendered in any session (the disk cache was empty); five prior rounds patched the unreachable fallback chain. Fix: resolve the tile from the authoritative repeater `args.Index` and set `el.DataContext = tile`, which also repairs the sibling DataContext-based handlers (clearing / tap / drag).
- **Scene-tag coverage too sparse.** Lowered the classifier confidence threshold 0.30 → 0.20 after a 3.3K-photo scan showed 66% of personal photos cleared zero labels at 0.30 (MobileNet/ImageNet-1k produces a diffuse softmax on out-of-distribution personal photos). Recovers coverage; ImageNet labels stay object-specific (a Places365 scene model is the tracked follow-up for `beach`/`kitchen`-style relevance). Requires a re-scan to re-tag existing files.
- **Video keyframe extraction missing per-thread COM init.** `shell::video::keyframe_25pct` initialized Media Foundation (`MFStartup`) but never `CoInitializeEx`, so only the one decoder-pool thread that won the `MFStartup` race had a COM apartment — every other decoder thread (and the Deep Analyze `spawn_blocking` threads) would fail `MFCreateSourceReaderFromURL` with `CO_E_NOTINITIALIZED`. Added a thread-local `CoInitializeEx(COINIT_MULTITHREADED)` guard, matching the per-thread COM init the other `shell::*` modules (trash / thumbnail / tags / reveal) already do. Fixes video thumbnails + video scene tags failing on most files.

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

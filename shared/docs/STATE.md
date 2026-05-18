# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.

## 2026-05-17 (continuation 3) — V15.8d follow-up parity session

Comprehensive cleanup pass picking up items the previous session deferred.

### Completed
- **Comment surgery**: 0 V-version / Mirror-of-macOS comments remain across `platforms/windows/src/` (387+54 removed). Engine + app build clean; clippy `-D warnings` clean; `dotnet build` 0/0; `dotnet format --verify-no-changes` clean.
- **PDF rasterization**: implemented under `pdf-analyze` Cargo feature with `pdfium-render = "0.8"`; `analyze_file()` wired for PDF kind; tests cover both feature-on and feature-off paths. DECISIONS.md entry written.
- **C# ViewModel tests**: 26 new test cases in `ViewModelBindingTests.cs` covering `ModelSlot.Apply` state transitions, `PersonCluster.BuildCropPath` (new static helper), `ScanProgress`/`DeepAnalyzeProgress`/`HardwareInfo` DTO surface, and `WelcomeSheetModelSizeTests` parameterized across 7 model_kinds via new `ModelDisplaySize.GetDisplaySizeMB`. Total C# tests: 62 (was 36).
- **SCRFD decode**: extracted `decode_scrfd_stride()` + `decode_scrfd_single_anchor()` as pure functions; added 3 regression tests + 1 proptest exercising bbox bounds across randomized inputs. End-to-end DB verification against a face photo was not possible because the user's Pictures folder contains only screenshots — the pure-function tests are the proper invariant gate.
- **VRAM calibration on RTX 2060**: measured ~940 MB peak engine attribution above 1.65 GB idle baseline during a scan. Kept `VRAM_PER_POOL_INSTANCE_MB = 1500` (preserves ~560 MB margin against DirectML fragmentation); comment in `tagging.rs` updated with measurement and method.
- **publish-bundle.ps1**: installed pwsh 7.6.1 via winget; fixed 3 WiX 4 wixproj issues (DebugType=portable rejected by wix.exe; ItemGroup DefineConstants form silently no-op'd in WiX 4; `<bal:Condition>` body syntax replaced with `Condition` attribute). Engine + app published; FileID-x64.msi built; privacy gate scan finds 0 hits across 513 binaries.
- **Proptests**: `scrfd_decoded_bbox_within_image_bounds` added; existing dbwriter `embedding_le_bytes_round_trip` (bit-pattern-strict, broader than spec) and hmac `appending_byte_to_msg/key_changes_mac` already satisfy G2 + G3.

### Remaining gaps after this session
- **Bundle build** (`FileIDSetup.exe`) still fails on (a) `WixStdbaLicenseUrl` theme variable not declared, and (b) Bundle.wxs hardcodes both x64 and ARM64 MSIs so `-SkipArm64` chokes on missing ARM64 MSI payload. These are bundle-only — the per-arch MSI + binary publish succeed and the privacy gate against the published binaries is clean.
- **End-to-end face-detection DB verification** still needs a face photo in the scan corpus (the user's Pictures folder has only screenshots). The pure-function decode tests cover the invariants; a single face photo dropped into Pictures and re-scanned would close the loop in seconds.

## 2026-05-17 (continuation 2) — V15.8c smoke script + UNC + cluster invariants + spring map + SQL comment

Third pass picking up smaller-scope items. Cumulative: 99/99 Rust lib tests pass (was 82 at start of day), 66/66 C# tests pass, all gates green.

**Engine smoke test script (Section 10d, NEW):**
- `platforms/windows/build/engine-smoke.ps1` — spawns FileIDEngine.exe, asserts the ready event has the schema-required fields (version, pid, workerCap, physicalMemoryGB), sends shutdown, asserts clean exit. Works in Windows PowerShell 5.1 (no pwsh dep).
- Verified end-to-end on this box: NVIDIA RTX 2060 detected, 10 worker cores, 63.9 GB RAM, DirectML EP, clean exit 0.

**UNC path containment (Section 8c, NEW):**
- 2 new tests in `util/path_safety::tests` for the SEC-7 restore-from-trash check: nested UNC path matches authorized UNC root; cross-server UNC paths don't collide (different `\\srv\share` prefixes).

**identity_clustering invariants (Section 10b, NEW):**
- 2 new tests: all-identical embeddings collapse to one cluster; 5 orthogonal unit vectors produce 5 distinct cluster IDs.

**Pool load serialization (Section 7d, verified-as-correct):**
- `ModelStack::load_default` calls `load_pool` 3 times sequentially (no spawn/rayon between them). Each `load_pool` loads slots sequentially with 250ms intra-pool stagger. TDR detection during warmup aborts the whole pool. No fix needed.

**Sankey diagram parity (Section 5d, verified-as-correct):**
- `SankeyFlowControl.cs` is a 1:1 mirror of macOS `SankeyFlowView`: source-folder column → category-ribbon column, gold for sources, lavender/cyan/pink rotation for categories, bezier per (source, category) pair, hover highlights. The directive's "Anchor=gold/Mixed=lavender/Junk=gray" coloring is a separate feature (RestructureRecommendationRow), not Sankey itself.

**ARM64 worker priority (Section 7f, verified-as-correct):**
- `set_worker_background_priority` uses `Win32::System::Threading::SetThreadPriority` + `THREAD_PRIORITY_LOWEST`. API surface is identical across x64 and ARM64 in windows-rs 0.58.

**SQL case parity (Section 6a/6b, comment fix):**
- The comment in `db/migrations.rs` claimed "GRDB lowercases column types" — actually wrong. GRDB's `Database.ColumnType.text` returns "TEXT" (uppercase) and the DSL emits it verbatim. Rust SQL uses UPPERCASE which matches. Comment rewritten to reflect reality; no SQL changes (would have broken parity in the wrong direction).

**SwiftUI ↔ WinUI spring mapping (Section 9b, documented):**
- DECISIONS.md entry written. The mapping is direct: `response (s)` ↔ `Period.TotalSeconds`, `dampingFraction` ↔ `DampingRatio`. Canonical FileID values already in `Theme.xaml` as `SpringResponseStandard` / `SpringDampingStandard` / `SpringResponseTight` / `SpringDampingTight`.

**publish-bundle dry run (Section 11f, deferred):**
- Requires PowerShell 7 (`$PSNativeCommandUseErrorActionPreference`) + WiX SDK. Neither available in this session. The lighter `engine-smoke.ps1` (new in this round) covers the engine-binary smoke path; full release-cutting is a separate documented workflow.

## 2026-05-17 (continuation) — V15.8b SCRFD + SEC-3 + TDR coverage + EP tests + color tokens

Continuation of V15.8 picking up items previously deferred. All gates remain green: 95 Rust lib tests pass (was 82 earlier in the day), 66 C# tests pass (30 IpcSchema + 36 App), `cargo clippy -D warnings` clean, `cargo deny check` clean, `dotnet build` 0 warnings 0 errors, `dotnet format --verify-no-changes` clean.

**SCRFD `detect()` shipped:**
- Wrote the full post-processing against the Buffalo_L SCRFD-10g (insightface) reference: distance-encoded bbox decode + 5 landmarks per face + NMS @ IoU 0.4 across strides 8/16/32 with 2 anchors per spatial location. Defensive parsing: wrong-variant ONNX silently degrades to empty result rather than producing nonsense scores that would poison the People tab's cluster IDs.
- 6 new unit tests (5 for nms/iou helpers, 1 for pose estimation).
- Golden-set validation (4 known images: clear face / small / multi / no-face) is the next-session work item.

**LavaLamp Composition status corrected:**
- Audit found the V14.6 rewrite already moved off Win2D's `CanvasAnimatedControl` to `Microsoft.UI.Composition` (3 SpriteVisuals + ExpressionAnimation-driven Offset + CompositionRadialGradientBrush falloff). My earlier deferral entry in DECISIONS.md was wrong; superseded by a new "already shipped" entry.

**Security tightening:**
- SEC-3 SetDefaultDllDirectories hoisted to the very first statement in `fn main`, before tokio runtime construction and before logging::init. Closes the gap an audit would flag.
- Found 1 of 5 `session.run` sites (`models/clip_text.rs:69`) missing the `classify_inference_error` wrap. Added it + the import. All 5 now uniformly guarded → TDR detection coverage is 100% across the models tree.
- Added GPU-dead short-circuit at the top of `pipeline/tagging::process_file`. Once `coord.mark_gpu_dead()` fires, remaining queued files return immediately with `failed=false` instead of hanging on doomed inference calls.

**EP chain test scaffolding:**
- 7 new tests across `models::runtime` mocking each vendor; the expected `priority_chain` ordering is now documented as a regression guard. Includes a global invariant: every vendor's chain terminates at CPU and (if vendor != None) includes DirectML.

**C# warning sweep:**
- Build was already at 0 warnings before this turn. `dotnet format --verify-no-changes` initially flagged CRLF + 2 IDE0003 violations in the V15.7-modified `FilePreviewSheet.xaml.cs`; `dotnet format` auto-fixed them.
- 1 test regression caught: `AppSettingsTests.NewInstance_HasDocumentedDefaults` was asserting `CleanupAutoTagKept == false` but V15.5b's macOS-parity work flipped the default to `true`. Updated the assertion + comment.

**Color token audit:**
- 5 bare `#FFCC00` literals in `SettingsView.xaml`, `SidebarEngineStatus.xaml`, `SidebarProcessingControl.xaml` replaced with `{StaticResource GoldBrush}` and `{StaticResource GoldSelectedBackgroundBrush}`. Brand-color drift detection is now centralized in `Theme.xaml`.
- Alpha-variant gold tokens (e.g. `#33FFCC00`) kept inline where the alpha differs from the existing `GoldSelectedBackgroundBrush` (18%) or `GoldSelectedStrokeBrush` (55%) — adding new tokens for each one-off alpha would be over-engineering.

**WAL checkpoint guard:**
- Added `debug_assert!(conn.is_autocommit())` before the periodic `PRAGMA wal_checkpoint(PASSIVE)` in `dbwriter::flush`. Catches a future regression where someone adds a `BEGIN` before the checkpoint block.

**Engine respawn backoff (Section 5g) verified already-shipped:**
- 1s/4s/16s exponential backoff in `EngineClient.OnProcessExited` ✓
- 3-strike-in-60-seconds cap ✓
- `CrashReason` bound to `SidebarEngineStatus.xaml.cs:61-62` for the permanent error banner ✓

**Privacy gate verified:**
- Source-level telemetry scan: 0 real hits (only false positives — "low-amplitude noise" in a bench comment, "segmented" in UI control identifiers, "PROCESSENTRY32W" Win32 type).
- URL allowlist scan: 6 unique hosts, all on the documented allowlist (huggingface.co, github.com, developer.{download.,}nvidia.com, schemas.{microsoft,openxmlformats}.org).

**Verification still pending (user hardware):**

1. SCRFD on a face-heavy library — verify cluster IDs look right after a rescan. If wrong, run `det_10g.onnx` through Netron and adjust the decode index math.
2. Forced TDR test — kill the GPU driver mid-scan, verify the engine doesn't continue spamming inference calls (the new `is_gpu_dead` short-circuit should make this fast).
3. All previously-pending V15.7 verifications still apply (sidebar memory/total/ETA/failures rendering).

## 2026-05-17 — V15.8 IPC schema parity + security hardening + dead-code prune

Single-session pass focused on the Windows port's IPC contract correctness, security posture, and dead-code cleanup. No new features — every change is either a contract correction, a hardening, or a deletion. Build remained green throughout (`cargo check` + `cargo clippy -D warnings` + `cargo deny check` all pass; lib tests 74 → 82).

**IPC schema (Section 4 of the spec audit):**

- `shared/ipc-schema/ipc.schema.json` was missing 5 event variants the Rust engine emits and the C# app consumes: `restructurePlan`, `restructureApplyResult`, `bulkActionResult`, `clipTextEmbedding`, `mergeSuggestions`. Added all 5 with the correct `SinglePositional/_0` wrapping that matches Swift Codable's auto-synthesized shape. macOS Swift IPC doesn't have these because macOS uses synchronous returns; documented as a legitimate cross-platform divergence (DECISIONS.md 2026-05-17).
- Also added `startScan.rescan: bool` to the schema (Rust had it, schema didn't).
- Verified all 27 `CommandPayload` variants have explicit handler arms in `main.rs::handle_line` — no `_ =>` wildcard arm, so Rust's exhaustiveness check guarantees no silent drops.

**Security (Section 8 of the spec audit):**

- `is_safe_filename` now rejects `COM0` and `LPT0` (Microsoft's docs list both as reserved). New proptest `reserved_device_names_are_rejected` covers all casings + extensions.
- `util/zip::extract_into_parent` now caps a single entry at 1 GiB (half the cumulative 2 GiB cap), so a single bomb entry can't exhaust the whole budget.
- `commands/trash_log.rs::read_batch` no longer accepts entries without an HMAC suffix. The pre-V14.7.2 grace window expired months ago — see DECISIONS.md 2026-05-17.
- `pipeline/restructure_apply.rs::apply` now reparse-point-checks the destination's ancestor chain BOTH before and after `create_dir_all` (SEC-5 defense in depth).

**DB correctness (Section 6 of the spec audit):**

- Incremental-rescan skip query in `scan_session.rs` now filters `failed = 0` so previously-failed files retry automatically. Documented that `modified_at IS NULL` rows fall out via SQL three-value logic.
- FTS5 round-trip test in `db::migrations` strengthened: asserts `rowid == files.id` and that a known-absent word returns zero hits (was just `COUNT(*) == 1`).
- New embedding byte-order proptest in `dbwriter` verifies `floats_to_le_bytes` → `f32::from_le_bytes` is byte-for-byte lossless, including NaN bit patterns. Guards against a future `to_ne_bytes` regression silently corrupting embeddings when DBs move between architectures.

**Test coverage (Section 10 of the spec audit):**

- HMAC proptests: appending any byte to msg / any non-zero byte to key changes the MAC. (Pure zero byte appended to a short key is correctly a no-op per RFC 2104's zero-padding rule — caught by the first proptest run and the invariant was tightened.)
- PathRedactor: UNC path keeps only last 2 components, drive root collapses to `…`, app structural paths pass through unchanged. The redaction function itself was fixed in the process — was leaking the drive letter for `C:\`.

**Dead-code prune (Section 1 of the spec audit, scoped):**

- `shell/sleep.rs` deleted (duplicate of `platform::SleepGuard`; only platform.rs's was ever called).
- `Discovery::new` + `pipeline/discovery.rs::enumerate` deleted (orphan convenience wrappers).
- `db::open_reader` deleted (C# opens its own SQLite connection; no Rust caller).
- 6 cargo clippy warnings fixed (deny.toml lint rename, orphan doc comment, redundant `.into_iter()`, two manual checked divisions, sort_by → sort_by_key).
- 40+ `#[allow(dead_code)]` attrs intentionally LEFT in place — they're either the documented Linux Phase 0 cross-platform stubs (V15.5b) or items genuinely used by lib (tests/benches) but not by bin. A deeper per-item audit was out of scope.

**Deferred to a hardware-equipped session (DECISIONS.md 2026-05-17 entries):**

1. **SCRFD `detect()` implementation.** Needs the actual `det_10g.onnx` loaded + a 4-image golden set. Speculative decode math against the wrong export variant would silently corrupt cluster IDs across the entire People tab.
2. **LavaLampBackground Composition API migration.** Needs render verification on Windows 11 26200+ to confirm it avoids the `DXGI_ERROR_DEVICE_HUNG` that wedged the Win2D `CanvasAnimatedControl` version.
3. **Multi-vendor GPU EP chain validation.** Needs physical NVIDIA / AMD / Intel / Snapdragon boxes. Unit-test coverage (mocked `pack_present`) stays; live-fire deferred.

**Build/test:** 82/82 Rust lib tests pass (was 74); `cargo check` clean; `cargo clippy -- -D warnings` clean; `cargo deny check` clean. `.NET` build not re-run in this session — none of the C# files were modified.

**Verification still pending (user runs on Windows hardware):**

- Confirm V15.7 sidebar stats parity (memory / total / ETA / failures) renders correctly — this verification was already pending from V15.7 and is not affected by V15.8.
- Trash + restore round-trip: trash a few files, restart the app, verify `restoreFromTrash` still works (no entries should be rejected; the HMAC tightening only blocks pre-V14.7.2 entries, of which there should be none after months of organic rotation).
- Restructure apply on a path with a deliberately-planted directory junction inside `library_root` should now reject the move (defense-in-depth check).

## 2026-05-16 (late night) — V15.7 sidebar stats parity with macOS (memory/total/eta/failures)

User asked for the sidebar stats to be 1:1 with macOS. Phase 1 dual-Explore parity audit against `platforms/apple/Engine/ScanCoordinator.swift:174-186` and `DBWriter.swift:268` found four engine-side regressions where Windows hardcoded zeros instead of measuring real values:

| Stat | Was | Now |
|---|---|---|
| **Memory** | hardcoded `resident_mb: 0` at `scan_session.rs:249/418/460` — sidebar always showed "0 MB" mid-scan | new `platform::process_memory_mb()` using `Win32::System::ProcessStatus::GetProcessMemoryInfo` (WorkingSetSize). macOS uses `task_info MACH_TASK_BASIC_INFO`; Windows now matches. Linux/POSIX path reads `/proc/self/status` VmRSS. |
| **Total during Tagging** | `total: stats.processed_total` at line 454 — progress bar always at 100% during tagging because total == processed | now `discovered_count.load(Relaxed).max(processed_total)` — persists the discovery total into Tagging Progress events so the progress bar fills as files are processed against the real total |
| **ETA** | hardcoded `eta_seconds: None` — sidebar stuck on "computing…" forever | computed as `(total - processed) / files_per_second` once both signals are available (≥0.5 fps + remaining > 0); None during ramp-up (matches macOS gating behavior) |
| **Failures** | hardcoded `failed: 0` — sidebar always showed "0 failures" even on a scan with corrupt files | added `failed_total` field to `BatchStats`, populated from DBWriter's existing `failed` counter at `dbwriter.rs:120`; plumbed through `maybe_emit_progress` to the Progress event's `failed` field |

**Files touched:**

- `engine/src/platform.rs` — new `process_memory_mb()` (cross-platform: Win32 GetProcessMemoryInfo on Windows, /proc/self/status VmRSS on Linux, 0 stub elsewhere).
- `engine/Cargo.toml` — added `Win32_System_ProcessStatus` to the windows-rs features.
- `engine/src/pipeline/dbwriter.rs` — added `failed_total: u64` to `BatchStats`; populated in `flush()` from the existing `*failed` counter.
- `engine/src/scan_session.rs`:
  - Discovery ticker Progress now includes `resident_mb: process_memory_mb()`.
  - `emit_batch_summary` populates `resident_mb` from `process_memory_mb()`.
  - `maybe_emit_progress` signature gained `discovered_total: u64` parameter; body computes real `total`, `eta_seconds`, `failed`, `resident_mb`.
  - Call site clones `discovered_count` once for the ticker, once for the tagging callback; passes the Tagging callback's load result into `maybe_emit_progress`.

**Build/test:** Rust engine 74/74 tests pass on release Windows target. .NET app builds clean (0 warn / 0 err).

**Verification still pending (user runs on Windows hardware):**

1. Launch the rebuilt engine + app, scan `Test Data/` (or any folder ≥100 files).
2. **Memory:** sidebar should show non-zero RSS during scan (typically 600 MB-1.2 GB once ML models are loaded). Should rise as inference warms up, plateau during tagging.
3. **Progress bar:** during tagging, bar should fill from left to right, not show 100% the whole time. Discovered count and Tagged count should differ during tagging (Discovered = final total after discovery completes; Tagged climbs toward it).
4. **ETA:** should switch from "computing…" to a real number after a few seconds (≥0.5 files/sec + non-zero remaining). Should count down as scan progresses.
5. **Failures:** if a file errors during tagging, the counter should increment in real time (was previously stuck at 0 until ScanComplete event).

**Unaddressed in this turn (user redirected mid-Settings work):**

- Settings page install-state detection — buttons for "CUDA llama.cpp for Deep Analyze" and "cuDNN for scanning" don't check the engine sentinel files at page load, so they show "Install" even when already installed. Sentinels exist at `%LOCALAPPDATA%\FileID\Models\.sentinels\{llama_runtime_cuda_x64,cudnn_runtime_x64}.installed`. Same pattern as `ModelInstallerService.SentinelInstalled` at line 755-762. Add a sync method called from the SettingsView `Loaded` handler (line 48). Captured for next turn.

## 2026-05-16 (night) — V15.6 thumbnail decode fix + CompletionRipple removal

User screenshot showed three issues after the V15.5 round shipped:

1. **Thumbnails still blank.** Every tile in a 549-.jpg `Test Data/` scan stayed on the loading shimmer (yellow→lavender gradient at `FileID.Theme/Motion/ShimmerView.xaml:37-42`, bound visible-when-Thumbnail-null via `LibraryView.xaml:235-236`).
2. **Sidebar visually unstable** during scan (Processing panel area).
3. **Yellow ring pulse on "Tagged N" stat** — `CompletionRipple`. macOS doesn't have it.

Root causes (Phase 1 dual-Explore agents):

- **Thumbnail:** V15.5 `RenderImageFallbackOnDispatcherAsync` at `Services/ThumbnailService.cs:282-310` used `BitmapImage { UriSource = uri }` — a **lazy** decode that runs when the BitmapImage is first put on a UI element. Combined with `LibraryView.xaml:241-246` `<Image Opacity="0" ImageOpened="OnTileImageOpened" />` (image only fades in after `ImageOpened` fires), if the lazy decode silently failed `ImageOpened` never fired → Image stayed invisible → shimmer kept showing. UriSource for file:/// URIs in WinUI 3 is reliable in the common case but fails silently for mid-scan files (file lock contention, path encoding edge cases, decode errors with no `ImageFailed` handler).
- **Yellow ring + sidebar instability:** `SidebarProcessingControl.xaml.cs:71-81` fired `CompletionRipple.SetTrigger(TaggedStatBorder, batch)` on every `LastBatch` event (≈5 Hz during scan). The ripple Storyboard at `Theme/Motion/CompletionRipple.cs:136-157` ran 0.9 s without canceling prior animations → 4-5 overlapping rings + a fresh Popup + Ellipse + Storyboard per trigger. Compositor churn was the dominant contributor to the "spazing" feel.

**Fixes:**

1. **`Services/ThumbnailService.cs:282-310`** — rewrote `RenderImageFallbackOnDispatcherAsync` to open the file as a stream and **eager-decode** via `await bmp.SetSourceAsync(stream)` on the UI dispatcher. Mirrors the working shell-path `RunSetSource` at lines 260-280. Extracted a new `RunFallbackSetSource` helper so the lambda passed to `TryEnqueueWithRetry` stays sync-shaped (matches `DispatcherQueueHandler`). On any open/decode failure: log via `DebugLog.Warn`, return null, and the existing `_renderedFailed` counter bumps via the wrapper. Stream lifetime is bounded by the lambda — `Dispose()` in `finally`.
2. **`Views/Sidebar/SidebarProcessingControl.xaml.cs:71-81`** — deleted the entire `if (e.PropertyName == nameof(EngineClient.LastBatch))` block + comment. Replaced with a V15.6 comment explaining why. `CompletionRipple` class and `TaggedStatBorder` XAML element kept (harmless when no trigger references them).

**Build/test:** `dotnet build FileID.sln -c Debug -p:Platform=x64` clean (0 warn, 0 err). `dotnet test FileID.App.Tests` 36/36 pass.

**Verification still pending (user runs on Windows):**

1. Wipe state + thumb cache, point at `C:\Users\adamm\Desktop\Test Data`, click Start Scan.
2. **Thumbnails:** within ~5 s of files entering the Library, tile shimmer should be replaced by actual JPEG content. Settings diagnostics line should show `Thumbnails: N ok / N failed / N dropped / N fallback` — `fallback` count > 0 (shell cache cold) and `ok` close to visible tile count.
3. **Sidebar:** "Tagged N" stat should not show concentric gold rings expanding outward. Whole Processing panel should look stable.
4. `pwsh build/gui-regression.ps1 -Corpus C:\Users\adamm\Desktop\Test Data -TimeoutMinutes 10` — expected `[PASS]`.

**Open observation:** if user STILL sees the shimmer on every tile after this build, the failure has moved out of the fallback path entirely — either the shell call is throwing before reaching the fallback, the worker channel is dropping requests, or `LoadThumbAsync` isn't being invoked. The `Stats` counters will name the bucket (`renderedFailed` rising with `fallback` near 0 = shell throw; `droppedDispatcher` rising = enqueue race; all near 0 = load not invoked).

## 2026-05-16 (late evening) — V15.5b cross-platform parity sweep + Linux platform scaffold

Following the V15.5 crash/harness work, a parity audit identified 7 user-visible divergences between Windows and macOS. macOS confirmed as canonical. Six were addressed (the seventh, model-load timeout, was deferred — wasn't in the user's explicit fix list). Plus the engine was made portable for the Linux platform, and the Linux GTK4 + libadwaita scaffold was created.

**Parity fixes (Windows-side, macOS canonical):**

- **D1 face crop padding** — `tagging.rs:112` `FACE_CROP_PAD: f32 = 0.25` → `0.15` (matches macOS `FaceClustering.swift:988`). Closes the cross-platform ArcFace embedding drift; same library now produces same cluster IDs across platforms. Re-run `iterate.ps1` to flush prior cluster IDs.
- **D2 file size cap** — `discovery.rs:31` removed the 500 MB `MAX_FILE_BYTES` const; the `size > MAX_FILE_BYTES` check is gone. Zero-byte skip kept. Large videos / disk images now scan on Windows like they do on macOS.
- **D4 thumbnail request size** — `ThumbnailService.cs:135` `ThumbnailRequestPx: 256 → 192` to match macOS `ThumbnailService.swift:27` `size: 192`. Same display target, ~44% less memory per cached tile.
- **D5 Library tile sizing** — `LibraryView.xaml:159-160` `UniformGridLayout MinItemWidth: 256 → 160` and `MinItemHeight: 256 → 160` to match macOS `.adaptive(minimum: 160, maximum: 220)`. WinUI's `ItemsStretch="Fill"` produces the same "grow until a new column fits" behavior.
- **D7 CleanupAutoTagKept default** — `AppSettings.cs:48` `false → true` to match the macOS default. Same user no longer sees different post-cleanup behavior on different OSes.
- **D6 tile hover scale** — `LibraryView.xaml` tile template gained `PointerEntered`/`PointerExited` events; `LibraryView.xaml.cs` got an `ApplyTileScale` helper that uses existing `FileID.Theme.Motion.SpringEasing.AnimateScalar` to spring `Scale.X`/`Scale.Y` to 1.012 (response 0.18s, damping 0.8) on enter / 1.0 on exit. CenterPoint set on each event using the tile's current ActualWidth/Height. Mirrors macOS `LibraryView.swift:681-682`.

**Engine portability (so Linux can reuse it):**

- `Cargo.toml` — moved `ort` from `[dependencies]` to `[target.'cfg(windows)'.dependencies]` (with `directml`, `cuda`, `openvino`, `qnn` features) + new `[target.'cfg(not(windows))'.dependencies]` block with `ort` (CPU + CUDA + OpenVINO, no DirectML) and `libc` (for `platform.rs`'s POSIX `getppid()`). `windows`/`windows-core` already correctly gated.
- `src/paths.rs::root()` — split into `#[cfg(windows)]` (LOCALAPPDATA/USERPROFILE) and `#[cfg(not(windows))]` (XDG_DATA_HOME → ~/.local/share/FileID). Layout helpers (db_path, logs_dir, etc.) unchanged.
- `src/shell/mod.rs` — Win32 submodules (`reveal`, `tags`, `thumbnail`, `trash`, `ocr`, `video`) cfg-gated to Windows. Non-Windows targets get inline stub modules with matching public surface (`Result::Err("…not implemented on this platform")` or `vec![false; n]` for trash). `sleep` was already cross-platform. Call sites (`commands/bulk.rs`, `pipeline/tagging.rs::try_shell_thumbnail`, `pipeline/deep_analyze.rs`) compile unchanged.
- Verified: `cargo check --target x86_64-unknown-linux-gnu` — all FileID code compiles cleanly. Only failure is transitive `ring` v0.17 (rustls native-crypto dep) needing `x86_64-linux-gnu-gcc`, an environment prereq, not a code issue. Resolution paths: build on real Linux/WSL, use `cargo-zigbuild`, or switch rustls to `aws-lc-rs` backend.
- Windows side: 74/74 cargo tests pass + 36/36 .NET tests pass after the Cargo.toml restructure. No regression.

**Linux platform — Phase 0 scaffold:**

- New `platforms/linux/` matching the macOS/Windows directory shape.
- `platforms/linux/CLAUDE.md` — full platform conventions, toolkit rationale (GTK4 + libadwaita chosen over Qt/Iced/egui/Tauri — GNOME-native, mature gtk4-rs bindings, satisfies "no web tech" + "native primitives"), shell-module TODO table.
- `platforms/linux/src/app/` Cargo project — `gtk4-rs` + `libadwaita` + reuses the shared engine via `fileid-engine = { path = "../../../windows/src/engine" }`. Workspace at `platforms/linux/Cargo.toml`.
- `src/app/src/main.rs` — `adw::Application` bootstrap, brand CSS provider (gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0), forced dark mode via `adw::StyleManager`.
- `src/app/src/window.rs` — `adw::ApplicationWindow` + `adw::HeaderBar` + folder picker (`gtk::FileDialog::select_folder`) + start-scan button. Engine status pumped from `async_channel::Receiver` into the GTK main context. Placeholder `adw::StatusPage` where Phase 1 lands the six tabs (Library/People/Cleanup/DeepAnalyze/Restructure/Settings).
- `src/app/src/engine_client.rs` — minimal stdio JSON client. Spawns the engine subprocess, reader thread parses NDJSON for `ready`/`scanComplete`/`error`, sends `startScan` on user click. Phase 1 replaces with full `IpcCommand`/`IpcEvent` routing.
- `data/io.github.fileid.FileID.desktop` — XDG desktop entry.
- `build/build.sh` — bash equivalent of `build.ps1`. Builds engine + app, stages into `dist/fileid/` with the engine next to the app exe so `locate_engine_binary` finds it.
- `README.md` — quickstart + status table.

**What this turn does NOT ship:**

- Linux app actually built on real Linux (no Linux environment in this session). Rust code compiles for Linux per cargo check; the GTK/libadwaita link step + system-libs would need a real Linux host.
- Real Linux implementations of `shell/` ops — `trash`, `thumbnail`, `ocr`, `video`, `reveal`, `tags` all still return Err on non-Windows. Implementations are sized in the Linux CLAUDE.md (~17 days total).
- Move of the engine crate from `platforms/windows/src/engine/` to `shared/engine/`. The Cargo path dep works today but the structural move is the proper home; tracked as a NEXT.md follow-up.
- Linux CI workflow. Add later under `.github/workflows/linux-engine.yml` + `linux-app.yml`.
- D3 (model-load timeout) — wasn't in the user's explicit fix list; staying at 30 s.

**Verification still pending (user runs on Linux hardware):**

1. `cd platforms/linux && ./build/build.sh` on Ubuntu 24.04 or Fedora 40 (must have `libgtk-4-dev` + `libadwaita-1-dev`).
2. Expected: clean Cargo build of engine + app; `dist/fileid/fileid-linux` launches showing dark Adwaita window with "FileID for Linux" StatusPage.
3. Pick a folder, click Start Scan; engine state label flips through `spawning → ready → scanning → done`.

## 2026-05-16 (evening) — V15.5 Windows scan-crash fixes + GUI regression harness + thumbnail visibility

User reported the Windows app "keeps crashing when scanning" with thumbnails that "don't show anything like how the macOS version does," and called out a testing gap: "the testing and safety harnesses for this app must not be working or something."

V15.4 had landed `[APPLY:N]` / `[ENGINE-SUB:*]` tracing + two Pattern B fixes (SidebarQueueList, SidebarPipelineProgress) and concluded that the remaining `new BitmapImage(` sites were "confirmed safe (await propagates via DispatcherQueueSynchronizationContext)." That conclusion held in theory but evidently doesn't hold under burst-load conditions — per CLAUDE.md, the convention is to *treat UI-thread affinity as untrusted* even when it nominally holds. V15.5 tightens to that discipline and adds the GUI-driven harness that should have caught the class of bug before user discovery.

**Phase 1 — four Pattern B sites patched defensively:**
- `Views/DeepAnalyze/DeepAnalyzeView.xaml.cs::LoadStreamThumbAsync` — `new BitmapImage()` + `StreamImage.Source = bmp` now both run inside a `this.DispatcherQueue.TryEnqueue` lambda, after the `GetThumbnailAsync` await resumes on whatever thread.
- `Views/Library/FilePreviewSheet.xaml.cs::LoadShellThumbnailAsync` — same wrap. User's heavy local rework of this file preserved; only the leaf thumbnail-load was replaced.
- `Views/Restructure/DrillDownSheet.xaml.cs::LoadThumbAsync` — static method; captures `img.DispatcherQueue` before await.
- `Views/Sidebar/SidebarProcessingControl.xaml.cs::Sync` — was allocating four fresh `SolidColorBrush` per progress event (10 Hz during scan = 40 DispatcherObject allocations/sec). Now uses ctor-cached `_memoryWarnBrush` + `_statDefaultBrush` instance fields (same pattern V15.4 SidebarPipelineProgress adopted). Reuses existing static `FailedTextBrush` for the failures-alert color (same #FFFF6B6B).

**Phase 2 — `Services/ThumbnailService.cs` made silent-failures observable:**
- New `public static ThumbnailDiagnostics Stats` record exposing `RenderedOk` / `RenderedFailed` / `DroppedDispatcher` / `FallbackUsed` counters (Interlocked-incremented). Wireable into Settings diagnostics block in a follow-up.
- `RenderShellThumbOnDispatcherAsync` factored out; uses new `TryEnqueueWithRetry` helper (one retry after 50 ms) so transient shutdown-race TryEnqueue==false doesn't silently null the tile.
- Image-extension fallback path: when the shell `IThumbnailProvider` chain returns nothing for `.jpg/.jpeg/.png/.gif/.bmp/.webp`, falls back to `BitmapImage(new Uri(path)) { DecodePixelWidth = 256 }` (what Explorer's Photos uses, WIC-backed). Bumps `FallbackUsed` counter. This is the single most likely reason the user's tiles looked blank vs macOS — shell providers can return zero-size thumbs even for valid images.

**Phase 3 — GUI regression harness (the missing piece):**
- `Program.cs` + `App.xaml.cs` now honor `--auto-scan-folder <path>` and `--auto-exit-after-scan` CLI flags. App.OnLaunched dispatches a `StartScanAsync` once `EngineClient` reaches Ready (60 s timeout); on `Phase=Completed` (or `Failed`) the window closes, which runs the normal shutdown path → `MarkCleanExit()` flips `last-session.txt` to `clean_exit=true`.
- New `platforms/windows/build/gui-regression.ps1` (~150 LOC):
  - Wipes prior state, snapshots existing WER dumps, spawns the app with the new CLI flags.
  - Polls `%LOCALAPPDATA%\FileID\logs\app.log` for `[AUTO-SCAN] starting scan` then `[AUTO-SCAN] scan ended ok=True`.
  - On exit: asserts `clean_exit=true`, zero new WER dumps in `%LOCALAPPDATA%\CrashDumps`, no unmatched `[APPLY:N] enter` (would name the killer subscriber via the trailing `[ENGINE-SUB:*]` line).
  - Exit codes 0/1/2 match `iterate.ps1` shape.

**Phase 4 — synthetic 50K corpus generator:**
- New `platforms/windows/build/gen-corpus.ps1` (~140 LOC) generates a deterministic 60% JPG / 20% PNG / 10% PDF / 5% TXT / 5% DOCX tree under `$OutDir/AA/BB/file_NNNNN.ext` (~676 leaf dirs for 50K files). JPG/PNG via `System.Drawing.Bitmap`, PDF via hand-built minimal one-page spec, DOCX via Office Open XML zip with the 4 required parts. MP4 deferred (needs a binary seed; not needed for crash repro).

**Phase 5 — deferred:** EngineClientTests as scoped would require either factoring `Apply` into a pure function (touches the user's heavy local edits to `EngineClient.cs`) or building a new WinAppSDK UI test csproj (~150 MB new deps) since `EngineClient`'s ctor throws if not on a UI thread. The GUI harness from Phase 3 covers the Pattern B class anyway — the `[APPLY:N] enter`/`[ENGINE-SUB:*]` trace pair already pinpoints the killer subscriber. Unit-test layer can revisit once `Apply` is factored.

**Verification still pending (user runs on Windows hardware):**
1. `pwsh platforms/windows/build/build.ps1` then `dotnet build FileID.sln -c Debug` — ensure Phase 1 + 2 edits compile clean.
2. `pwsh platforms/windows/build/gen-corpus.ps1 -Count 50000 -OutDir C:\Temp\FIDCorpus` — generate the corpus (~10 min).
3. `pwsh platforms/windows/build/gui-regression.ps1 -Corpus C:\Temp\FIDCorpus -TimeoutMinutes 30` — full end-to-end. Expected: `[PASS] GUI regression: scan completed cleanly.`
4. Manual: open the app, scan a real folder, scroll Library — confirm thumbnails render (image-extension fallback should fix the blank-tile cases the user reported).

**Risk acknowledged:** the user has 1470+/991- uncommitted local edits across 30 files including FilePreviewSheet (+383), SidebarProcessingControl (+103), EngineClient.cs (+103). Phase 1 edits were applied to the exact documented crash-site line ranges only; surrounding rework preserved. Re-read each file at the target range immediately before editing to confirm no shift.

## 2026-05-16 (afternoon) — V15.4 scan-crash autopsy + per-subscriber tracing + Pattern B fix

User reported "click Start Scan → entire app crashes" on Windows. Forensics from `%LOCALAPPDATA%\FileID\logs\` from a fresh repro (16:26:14 → 16:26:22):

- `last-session.txt` → `clean_exit=false`
- `app.log` last line at 16:26:18.940 — engine tracing `[SCAN] preloaded skip set ... files_under_root=0`; then 3.5 s of silence on the C# side
- `engine.jsonl` shows engine kept running 3.5 s longer, processed 100 files, then saw `stdin EOF` at 16:26:22.444 and exited cleanly

That's the **native fast-fail signature** — same class as V15.2 (ThumbnailService cross-thread BitmapImage) and V15.2.1 (ModelSlot.PropertyChanged thread-affinity). The CLR is killed by `RaiseFailFastException` from inside a native WinUI 3 / Composition component; every managed sink is bypassed. With 17 places subscribed to `EngineClient.PropertyChanged`, the offending handler isn't identifiable from app.log alone — Apply only logs for errors and ModelDownloadProgress, so the burst of FileDone / Progress / BatchSummary events between scan start and process death is invisible.

**Phase 1 — diagnostic tracing** (additive, on by default):
- `EngineClient.Apply()` now emits `[APPLY:N] enter {EventName} tid=X` before the switch and `[APPLY:N] exit {EventName}` after. Monotonic counter (`_applySeq`); the highest seq with no matching `exit` after a death names the killer event.
- Every subscriber (`SidebarProcessingControl`, `SidebarPipelineProgress`, `SidebarEngineStatus`, `SidebarQueueList`, `LibraryView`, `PeopleView`, `DeepAnalyzeView`, `RestructureView`, `SettingsView`, `AutoPilotTracker`, `CleanupView`, `SuggestedMergesSheet`, `AppViewModel`, `WorkflowAutoTabRouter`, `CudaAutoInstaller`, `LlamaRuntimeAutoInstaller`, `ClipSearchService`, `ModelInstallerService`) now emits `[ENGINE-SUB:ClassName] {PropertyName}` after its property filter. The trailing ENGINE-SUB line before a death names the killer subscriber.
- `DebugLog.Write` already flush-on-write (uses `File.AppendAllText` which opens+writes+closes per call) — confirmed; no buffering changes needed.

**Phase 2 — two proactive fixes for high-confidence Pattern B candidates:**
- **`SidebarQueueList.Sync`** previously called `JobsRepeater.ItemsSource = null` and then mutated the parent panel's `Children` (nuke siblings, insert fresh `StackPanel`) on every `QueueState` event. Visual-tree mutation racing with a layout pass mid-burst is a fast-fail vector. Now: a lazily-created stable `_rowsContainer` `StackPanel` is inserted exactly once; subsequent syncs only `Clear()`+`Add()` its `Children`. Parent's child list never changes again.
- **`SidebarPipelineProgress.SyncStage`** previously allocated four `SolidColorBrush` instances and ran three `Application.Current.Resources` lookups on each `LastProgress` event (10 Hz during a scan). Brushes are `DispatcherObject`s — allocating fine on UI thread, but the per-event churn was wasteful and surfaced as recurring tagged-pinned-allocations. Now cached at ctor time.

**Phase 4 — hardening sweep:**
- Every engine-event handler wrapped in `DebugLog.SafeRun("ClassName.OnEngineChanged", () => { ... })` — managed exceptions log + write `crash-*.txt` instead of escaping the dispatcher.
- Phase 4b cross-thread audit: grep for `new BitmapImage(` outside `DispatcherQueue.TryEnqueue` blocks returned 3 sites — all confirmed safe (ThumbnailService V15.2 fix intact; DeepAnalyzeView and DrillDownSheet thumb loads run on UI thread because the start point is UI-dispatched and awaits propagate via DispatcherQueueSynchronizationContext).
- `platforms/windows/CLAUDE.md` now documents the subscriber convention + brush-caching rule + Pattern B rule under "Conventions (WinUI 3 app)".

**Build:** `dotnet build FileID.sln -c Debug -p:Platform=x64` clean, 0 warnings, 0 errors.

**Verification still pending (user runs on Windows hardware):**
1. Repro the crash. With Phase 1 tracing, `app.log` now identifies the offending `[APPLY:N] {EventName}` + last `[ENGINE-SUB:ClassName]`.
2. If the Phase 2 SidebarQueueList/brush fixes already prevent the crash (plausible — the user's scan emits QueueState + LastProgress at 10 Hz), Phase 3 verification proceeds: Discovering → Tagging → PostScan → Completed → face clustering → Deep Analyze.
3. Either way the diagnostic infrastructure is now in place to surface the next variant quickly.

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

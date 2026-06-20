# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.
>
> **Trimmed to a lean baseline (2026-05-21).** Only the most-recent entries are kept here; everything older lives in `git log`.

## 2026-06-20 (Linux foundation) — engine + CLI + GTK app all build on Linux (CI-green); CLI MVP; engine Linux restructure fallback

The Linux build foundation is now CI-verified end to end (new `.github/workflows/linux.yml`, 3 jobs on ubuntu-latest, all green):
- **Engine (cross-platform Rust) — green.** `cargo fmt`/`clippy -D warnings`/`test` pass on Linux + telemetry scan. The "engine is cross-platform-clean" claim is now concretely proven on an actual Linux host (was only inferred).
- **CLI (`fileid`) — green.** Builds + the model-free smoke test passes on Linux. A new cross-platform front-end (`platforms/cli/`) linking the engine in-process: scan (FTS), search, info, people, dedupe (exact + phash near-dup), restructure --plan; `--json/--quiet`. For headless/NAS/scripting.
- **GTK4 app (`fileid-linux`) — green (compiles).** The Phase-0 scaffold builds once the dependency train was aligned: it pinned `gtk4 0.7` against `libadwaita 0.6`, which transitively needs `gtk4 0.8` (two gtk4 majors both linking native gtk-4 → Cargo conflict). Fixed to the single `gtk4 0.8 / libadwaita 0.6 / glib+gio 0.19` train (GTK 4.14 / libadwaita 1.5 — what ubuntu-24.04/Fedora 40/Arch ship); committed the resolved Cargo.lock.
- **Engine restructure file-move/symlink** now has a portable `#[cfg(not(windows))]` impl (`std::fs::rename` + EXDEV copy-fallback for NAS mounts; `std::os::unix::fs::symlink`) — Restructure apply works on Linux. cargo-verified, 2 new portable tests.

This is the FOUNDATION (everything compiles + the engine/CLI are functional on Linux). Remaining for a feature-complete Linux GUI is tracked in NEXT.md. Note: the GTK app's *behavioral* verification needs an actual Linux box (CI only proves it compiles), same as C# is CI-verified.

## 2026-06-20 (CLI MVP) — new cross-platform `fileid` command-line front-end (in-process over the engine)

Added a fourth client alongside apple/windows/linux: a cross-OS **`fileid` CLI** at `platforms/cli/` (its own standalone Cargo workspace, pinned Rust 1.90 to match the engine). It links the shared engine crate as a path dependency (`fileid-engine = { path = "../windows/src/engine", default-features = false }`) and integrates **in-process** — calling the engine's public library surface directly rather than spawning the engine binary. **Verified on this macOS host: `cargo clippy --all-targets -- -D warnings` clean, `cargo build` clean, `cargo test` green (1 smoke test).**

Architecture choice (in-process vs spawn): the MVP is read/query + plan, and search/info/people/dedupe have **no IPC command** — the macOS/Windows apps run them as direct read-only SQL — so the CLI does the same via `db::open_read`. The engine's `startScan` IPC hard-gates on ML models (`mobileclip_s2`+`arcface`), which is incompatible with a model-free CLI, so `scan` is a model-free FTS indexer writing through `db::open_writer` (engine schema + migrations; `doc_fts` filled by the v15 triggers). `restructure --plan` reuses the engine's public, pure, model-free `pipeline::restructure::classify` rule cascade. Net: zero contract drift, single self-contained binary, no engine-binary dependency.

Commands (each mapped to the engine surface): `scan <path> [--rescan]` (model-free index → `files` + `doc_text`); `search <query> [--similar]` (FTS5 over `doc_fts`+`ocr_fts` + filename fallback; `--similar` returns a clear needs-models notice); `info <path-or-id>` (metadata/tags/people/snippet); `people` (persons + face counts); `dedupe [--exact|--similar]` (content_hash groups / phash Hamming union-find, default threshold 8 mirroring the engine); `restructure --plan [root]` (read-only plan). Global flags `--json`/`--quiet`/`--no-color`/`--db`; library location resolves `--db` → `$FILEID_DB` → `$CFFIXED_USER_HOME` → engine default (`$XDG_DATA_HOME`/`%LOCALAPPDATA%`), so it reads the same DB the desktop apps build. Smoke test is model-free + isolated (`--db` tempdir): creates text/markdown files, scans, asserts search + info + re-scan-skip + plan. No engine/Swift/C#/GTK files were modified. Docs: new `platforms/cli/README.md`, ARCHITECTURE front-ends note, CONTRIBUTING "Build the CLI", DECISIONS entry, NEXT follow-ons (apply commands, semantic-search wiring, full-pipeline `scan --models`, TUI).

## 2026-06-20 (quality audit loop) — perf + accuracy audit across all features; near-duplicate detection feature

Workflow-driven audit campaign: 8 subsystems each FIND→adversarial-default-reject-VERIFY, fix verified safe/high-value findings, re-audit. Shipped (all: swift build clean, 260/260 Swift tests pass, CI green):
- **N+1**: `filesWithPersonTags` 407 per-person queries → 1 JOIN (mirrors Windows `NamedPersonFileIdsAsync`).
- **Tag dedup+cap**: visionTags now case-insensitively deduped + capped at 16 before write (mirrors Windows `tagging.rs`) — fixes "Dog"/"dog" dupes + unbounded tags.
- **Document content search**: macOS now stores extracted doc text into `doc_text` at scan (v15 trigger fills `doc_fts`), so PDFs/docs are keyword-searchable — was a parity regression vs Windows. Verified e2e (4/4 distinctive tokens matched their files; real DB untouched).
- **Representative face by max `face_quality`** (was `.first`) — sharper People cluster thumbnails, mirrors Windows.
- **Person search** adds title/middle_name/suffix (both platforms were missing them).
- **Approximate-dup disclosure**: duplicate groups whose largest member exceeds the 16 MB full-hash cap are matched by a composite fingerprint, not byte-exact SHA — now badged "~ likely match" in Cleanup (Windows already disclosed this).
- **Windows parity**: duplicate keeper selection is now quality-first (aesthetic/size/created/path) instead of alphabetical, mirroring macOS.
- **`FILEID_INFERENCE_CONCURRENCY`** env knob on the 4 model inference semaphores (default 4 kept).
- **NEW FEATURE — perceptual near-duplicate detection** (Cleanup "Visually similar" mode): uses the already-computed-but-unused dhash; union-find on Hamming distance; default threshold 8 (validated on real images: resize/re-encode → Hamming ≤1; nearest distinct photo = 24, a 3× safety margin). No auto-select for delete; "review — not identical" warning. **macOS only so far** (Windows mirror is the remaining lockstep item).
- Restructure proposal-row string precompute (minor perf).

Audit verifiers correctly REJECTED marginal items: CLIP batching (RAM++ Swin-L@384 is the dominant scan cost, not CLIP) and "synchronous applyPlan" (a full scan of 3372 rows is ~10 ms, not the claimed 100 ms).

## 2026-06-20 — fix(macOS): 4 user-reported issues fixed + Windows lockstep confirmed — preview nav, face clustering, restructure sorting, performance

User reported macOS being slow, with broken Restructure sorting, broken face clustering, and totally-broken preview navigation (arrow keys + clicking). All four root-caused and fixed. Verified: `swift build` clean (debug+release), 253/253 Swift tests pass, on-corpus e2e green (CFFIXED_USER_HOME-isolated copy of the user's library), Windows confirmed already in lockstep.

**Preview navigation (LibraryView.swift) — rebuilt.** Root cause: `.sheet(item: $selected)` swapped the sheet's bound identity on every nav so content didn't update in place; no `@FocusState` so `.onKeyPress` arrows never received key focus; and `openPreview` dropped the full-library sibling upgrade after the first nav (the `selected?.id == seedID` gate). Re-architected to a single stable `.sheet(isPresented:)` whose displayed file is driven by `previewSelectedID: Int64?`; `step()` mutates the id in place; added a `@FocusState` focus-grab + a tag-field typing guard (mirrors Windows `focused is TextBox`); fixed the sibling upgrade to track by id; nav buttons disable-don't-hide. Mirrors Windows SetSiblings/HandleKeyDown.

**Face clustering (ArcFaceService.swift, FaceClustering.swift).** On-hardware diagnosis DISPROVED the suspected CoreML-EP failure — the pipeline is healthy (CoreML binds, ORT auto-appends CPU, 991/991 embed, 407 persons). The user's all-NULL embeddings / 0 persons came from clustering never *completing* in their running app session (stale bundled engine, or quit before the post-scan auto-trigger finished), not a code defect. Hardening still landed: CoreML→CPU EP fallback + `FILEID_FACE_EP` env (mirrors Windows runtime.rs; helps CoreML-less Macs), the discarded `load()` bool is now captured and surfaced as `face_cluster_embedder_load_failed` (instead of a wrong "install the model" prompt or silent NULL), plus `arcface_preprocess_failed` logging. Post-scan auto-enqueue confirmed correct as-is. The real DB was additively backfilled to 407 persons during diagnosis (adds embeddings+persons only; nothing destroyed).

**Restructure (RestructureSemantic.swift + SankeyFlowView.swift + RestructureView.swift).** Mirrored the Windows BL-01 fix: `usedGroupNames` is now threaded across all time-gap segments (was fresh per segment → two different events could mint the same folder name and merge). Added count-desc/name-asc sort tie-breakers to Sankey sources/destinations + proposal grouping (determinism; matches Windows). On-corpus: 20 folders / 20 distinct leaf names (BL-01 active; disambiguation suffixes present), sensible grouping.

**Performance (PeopleView.swift, DeepAnalyzeViews.swift, ReadStore.swift, ThumbnailService.swift).** macOS was catching up to Windows. Throttled the People/DeepAnalyze `store.version` reload storms (1000+ DB queries/scan → ≤1/s; copied LibraryView's leading-edge + trailing-debounce). Replaced the N+1 correlated face-count subquery in `persons()` with a `GROUP BY` join (indexes already present). Thumbnail cache: in-memory 800→4000 (+512 MB cost limit), added an on-disk SHA256(path|mtime|size|px) JPEG cache + a 6-way decode concurrency gate (NAS-friendly). Windows audit confirmed it already had all three (10 Hz throttle, GROUP BY, L1+L2 thumbnail cache) — macOS now matches.

**Threshold decision:** kept macOS face auto-merge at 0.65/0.55 (NOT Windows' 0.75) — an on-corpus sweep showed 0.75 zeroes the auto-merge polish and increases fragmentation (34→39 persons, all 5 verified-correct merges lost). See DECISIONS.md.

**Lockstep:** RAM++ tagger + BGE doc embeddings are already implemented on macOS (byte-faithful to Windows). BGE model isn't installed in the user's library (install via Settings → rescan to cluster the 1737 docs by content); RAM++ is installed + wired.

**Follow-ups (same day):** preview-nav focus hardened — the sheet now grabs key focus after a brief defer (`.task { try? await Task.sleep(.milliseconds(50)); keyFocus = true }`) so arrows work without a click. Face-cluster fragmentation investigated on the real embeddings: it is intrinsic to age progression (same child across years overlaps the different-people cosine band), so auto-merge thresholds stay at 0.65/0.55 and consolidation is left to the already-wired "Suggest merges" UI (which surfaces the 0.50–0.64 candidates) — see DECISIONS.md.

## 2026-06-19 (parity audit) — fix(parity): macOS/Windows UI parity audit — 8 bugs fixed: statusBanner error icon, dismissedDeepAnalyzeHint persistence, FaceEmbedderCard copy, dead lastSeenVersion, FaceClusteringInFlight banner (Windows), LastScanProcessedFiles PropertyChanged, ApplyBarHint text, FaceClusteringBanner wiring

Full six-tab parity audit via parallel agents (macOS inventory + Windows inventory). Verified: Swift build clean, 253/253 Swift tests pass, Rust clippy -D warnings clean.

**macOS (RestructureView.swift + LibraryView.swift + SettingsView.swift):**
- **statusBanner always showed gold checkmark** even for error messages ("Engine unavailable", "Couldn't compute a plan", etc.). Fixed: added `isError: Bool = false` parameter + `@State private var statusIsError`; error sites set `statusIsError = true`, success sites set `false`; banner shows orange `exclamationmark.triangle.fill` for errors.
- **`dismissedDeepAnalyzeHint` not persisted**: `@State` var reset on every tab navigation, so the "Run Deep Analyze" hint banner couldn't be permanently dismissed. Fixed: changed to `@AppStorage("restructure.dismissedDeepAnalyzeHint")`.
- **FaceEmbedderCard copy outdated**: description said "Pre-converted from Buffalo (Immich) ONNX" — the app now uses SFace (Apache-2.0). Fixed: updated to accurate SFace description.
- **Dead `lastSeenVersion` state** (LibraryView): `@State private var lastSeenVersion: Int = -1` declared but never read or written. Removed.

**Windows (EngineClient.cs + EngineClient.Commands.cs + LibraryView.xaml.cs + RestructureView.xaml.cs):**
- **`LastScanProcessedFiles` missing PropertyChanged**: auto-property with `private set` bypassed the `Set(ref ...)` helper, so any XAML binding to this property would never update after scan completion. Fixed: added backing field + `Set(ref _lastScanProcessedFiles, value)`.
- **FaceClusteringBanner always Collapsed**: banner existed in XAML but `SyncBanners()` hardcoded `Visibility.Collapsed` unconditionally. Fixed: added observable `FaceClusteringInFlight` bool property to EngineClient (backed by `_faceClusteringInFlight`, set true at both `AutoTriggerFaceClusteringAsync` call sites on UI thread, cleared on `FaceClusteringCompleteEvent` and `face_clustering_failed`, dispatched via `_ui.TryEnqueue` from catch block); `SyncBanners` now checks it. Mirrors macOS `faceClusteringInFlight`.
- **ApplyBarHint wrong text for real moves**: hint always said "Originals stay put - applying creates shortcuts you can review" — inaccurate when user applies real moves (originals move). Fixed: "Shortcuts leave originals in place · Moves are permanent but undoable."

## 2026-06-19 (latest) — fix(quality): 11 bugs fixed across macOS+Windows — cross-segment name collision, zero-timestamp guard, audio year folders, IPCSink false-positive pinning, transcribeAudio timeout, scan queue guard, planRestructure shutdown race, resolveTargets data corruption, audio year tag format

Two parallel code-review audits (macOS engine + Windows engine) surfaced 11 bugs/quality issues. All fixed and verified: Rust clippy -D warnings clean, 370 Rust tests pass, 253/253 Swift tests pass, 11/11 iterate.sh assertions GREEN.

**Windows engine (restructure_semantic.rs + restructure.rs + audio_meta.rs):**
- **BL-01 CRITICAL**: `semantic_classify` — cross-segment name collision. After time-gap segmentation, each segment's `semantic_classify_profiled` call had its own local `used_group_names` HashSet that was discarded after the call, so two separate beach-trip segments could both produce a `Beach/` folder, merging photos from different events. Fixed: `used_group_names` is now passed as `&mut HashSet<String>` across all segment calls.
- **BL-02**: Zero/near-zero timestamp (FAT32 zero-epoch, corrupt mtime) silently placed files in `Photos/1970/January/` with Review confidence. Fixed: `ts_valid = ts > 86400` guard — invalid timestamps produce Ask confidence and flat (dateless) parent folders (`Photos/` not `Photos/1970/`). Applied to image, video, doc, and audio branches.
- **BL-03**: Audio files routed to flat `Audio/` with no year structure even when a valid timestamp was present. Fixed: now routes to `Audio/<Year>/` (mirrors Video → `Videos/<Year>/`). Flat `Audio/` used only when ts_valid is false.
- **WR-02**: Audio date tags emitted raw year strings (`"2019"`) instead of the `Year_NNN` format used by all other file kinds, producing double tags (`"2019"` + `"Year_2019"`) in the Library chip row. Fixed: `audio_meta.rs` now emits `"Year_2019"` format.

**macOS engine (FileIDEngineMain.swift + IPCSink.swift + DeepAnalyzeNaming.swift + DeepAnalyzeRunner.swift + Restructure.swift):**
- **CR-01 CRITICAL**: `transcribeAudio` had no timeout. SFSpeechRecognizer never delivers `isFinal` for silence-only files, unsupported codecs, or very short recordings — the `withCheckedContinuation` parked forever, stalling all subsequent Deep Analyze work. Fixed: wrapped in `withTaskGroup` race against 30-second timeout.
- **CR-02**: `resolveTargets` in DeepAnalyzeRunner coalesced nil DB columns with `?? 0` / `?? ""` defaults, producing `Target(id: 0, path: "")`. The id=0 UPDATE silently matched zero rows; the empty path resolved to the process working directory as a "file to analyze". Fixed: `compactMap` with explicit `guard let rowID: Int64, rowID > 0, !path.isEmpty` — invalid rows are dropped, not faked.
- **CR-03**: `startScan` enqueued jobs unconditionally. A rapidly-clicking or misbehaving app could pile up unlimited scan jobs. Fixed: rejects with `scan_already_queued` error if `JobQueue.shared.hasActive(category: .scan)` is true.
- **WR-01**: `planRestructure` spawned an unregistered `Task.detached`. A `.shutdown` arriving during plan generation would call `_exit(0)` before the plan task emitted `restructurePlan`, leaving the Restructure tab stuck on "Computing plan…". Fixed: task is now registered via `coordinator.setActiveRestructure` so `awaitActiveRestructure()` in `main()` drains it before exit.
- **WR-02 IPCSink**: `criticalNeedles` used bare strings like `"error"` — matched ANY JSON string value containing that word (e.g. a tag named "ready", a path component "scanComplete", a caption word "error"). Fixed: needles now use `"error":{` form to match JSON *object keys* only, not string values.
- **macOS rule cascade (Restructure.swift)**: Zero-timestamp guard and audio year structure applied identically to Windows (BL-02 + BL-03 mirror).

**Tests added:**
- Rust: `audio_year_subfolder` — asserts dated audio goes to `Audio/2024/`, not flat `Audio/`
- Rust: `zero_timestamp_gets_ask_confidence` — asserts zero-ts image gets Ask confidence and no 1970 folder
- Swift: Updated `videoAudioBuckets` and `missingTimestampYear` tests to match new behavior

## 2026-06-19 — feat(restructure): time-gap segmentation + richer time encoding + ask-deselection + CVD Sankey palette

Deep investigation into why restructure underperforms. Two-agent audit identified root causes; 8 improvements implemented across both engines and the macOS UI. All verified: Rust clippy -D warnings clean, 253/253 Swift tests pass.

**Root causes diagnosed:**
1. No time-gap event segmentation — photos from different days competed in the same cluster
2. Time encoding only had day-of-year (2 floats); same-day events in different years were indistinguishable
3. `ask`-confidence moves started selected — users could accidentally apply low-confidence moves
4. Sankey ribbons were all gold regardless of destination — no visual distinction between categories
5. `pass2Margin` not getting the granularity delta — asymmetry at `tight`/`loose` granularity settings
6. Face, GPS, path signals not fused (documented gap; tracked for future work)

**Changes (both engines — byte-faithful lockstep):**
- **Time-gap event segmentation**: `classify()` (macOS) / `semantic_classify()` (Rust) now pre-segment photos by capture-time gap (default 2 h, `FILEID_RESTRUCTURE_TIME_GAP` env override) before clustering. Events separated by hours/days never compete in the same cluster. Photos without timestamps cluster independently as a trailing group.
- **Richer time encoding**: `dayOfYearCyclical` → `timeFeatures` returning 5 values: day-of-year sin/cos (seasonality), time-of-day sin/cos (morning vs evening), log-compressed absolute year (separates same-calendar-day events across years). Fused-vector capacity updated from +2 to +5.
- **`pass2Margin` gets granularity delta** (`0.08 + d * 0.5`): margin now scales proportionally when `FILEID_RESTRUCTURE_GRANULARITY=tight/loose`, eliminating the asymmetry where cosines shifted but margin didn't.
- `#[derive(Clone)]` added to `SemanticFile` (Rust) to enable per-segment cloning.

**macOS UI:**
- **`.ask` proposals start deselected** (`RestructureView.swift`): "No clear signal — the decision is yours" moves are unchecked by default; user must explicitly select them before applying.
- **Sankey Okabe-Ito CVD-safe palette**: destination nodes now each get a distinct color (blue, vermilion, green, sky blue, orange, purple, yellow); ribbons carry the destination color so the user sees "what does this become?" from the hue. Source nodes are neutral (`.secondary`) so destinations dominate. Ribbon palette is CVD-safe for all common deficiency types.

**Windows UI:**
- **`.ask` proposals start deselected** (`RestructureView.xaml.cs`): `ask`-confidence rows deselected in a suppressed pass alongside the existing `_deselectedFileIds` restoration.

**Still tracked / deferred:**
- Face identity signal in fusion vector (requires person→file DB join + multi-hot block)
- GPS signal in fusion vector (reverse geocoding or coordinate normalization)
- VLM group naming (Qwen2.5-VL label-then-reason; per-call model reload too slow)
- `staysPutFiles` in IPC (IPC schema change needed)
- Win2D Sankey color upgrade (Windows uses a different Sankey rendering path)

---

## 2026-06-19 — fix: 3 final bugs (macOS: WelcomeSheet VLM error hidden, SettingsView blocking DB read; Windows: SettingsView SqliteConnection leak)

3 final bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **WelcomeSheet.swift** (MEDIUM): VLM install error was silently dropped. `vlmProgressLabel` returns "Failed: …" when `vlmLastError` is set, but that label was only rendered inside `if inProgress { }`. When an error fires, `vlmRequested` becomes false → `vlmInProgress = false` → the block is never entered. Added `else if let label = progressLabel { Text(label).foregroundStyle(.red) }` so errors surface below the title row.
- **SettingsView.swift** (LOW): `store.recentSessions()` called synchronously on the main actor in `.onAppear` and `.onChange(of: showAdvanced)`. GRDB's `DatabaseQueue.read` blocks the calling thread. Wrapped in `Task { }` so it runs cooperatively without stalling the run loop.
- **SettingsView.xaml.cs** (HIGH): `PopulateRecentScansAsync` opened `SqliteConnection` without `using` — leaked one OS read handle per Settings tab visit. Changed `var conn` to `using var conn`.

## 2026-06-19 — fix: 7 bugs (macOS: preview sibling race, DA error stale progress, DA url force-unwrap; Windows: SqliteConnection leak, ReadStore missing OpenAsync, _deselectedFileIds race, restructure_semantic empty filename)

7 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **LibraryView.swift** (MEDIUM): `openPreview` race — async task that upgrades `previewSiblings` only gated on `selected != nil`; if user arrowed to a different photo before the task completed, the original photo's sibling list replaced the new photo's. Fixed: capture `row.id` at spawn time, gate on `selected?.id == seedID`.
- **EngineClient.swift** (MEDIUM): `deepAnalyzeProgress` not cleared on `deep*` engine errors. `deepAnalyzeInFlight` was cleared but `deepAnalyzeProgress` was not, leaving a frozen "Working…" progress card with no way to dismiss it. Fixed: added `deepAnalyzeProgress = nil` alongside `deepAnalyzeInFlight = false`.
- **DeepAnalyzeViews.swift** (LOW): `ModelInstallStatus.isInstalled` force-unwrapped `.urls(...).first!` — crashes if sandbox disallows the document directory. Changed to `guard let base = ... else { return false }`.
- **DeepAnalyzeView.xaml.cs** (HIGH): `RefreshNamePeopleGateAsync` opened a `SqliteConnection` without `using` — leaked a read handle on every call (tab load + face-cluster-complete events). Changed to `using var conn`.
- **DeepAnalyzeView.xaml.cs** (MEDIUM): `RunApplyAsync` used `ReadStore` without calling `OpenAsync()` before the first query. All three Apply buttons (Apply Tags, Apply People, Apply All) threw `NullReferenceException` inside the store and silently failed, showing 0 tagged/peopled. Added `await store.OpenAsync()`.
- **RestructureView.xaml.cs** (MEDIUM): `DeepAnalyzeComplete` handler called `_deselectedFileIds.Clear()` (a static field shared across view instances) after an `await` without re-checking `_unloaded` — could clobber the new view instance's selection state if the view was recreated during the await. Added `if (_unloaded) return;` after `await RefreshDeepAnalyzeHintAsync()`.
- **restructure_semantic.rs** (LOW): `file.source.file_name().unwrap_or_default()` produced an empty `OsStr` for paths ending in `/` or `..`, causing `dest_dir.join("")` to silently resolve to the directory itself. Changed to `let Some(name) = file.source.file_name() else { continue }`.

## 2026-06-19 — fix: 4 more bugs (macOS: face-name inheritance threshold, restructure feedback skip; Windows: RAM++ 0-tag wipes prior tags, tagging visual_tagger_ran scope)

4 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **FaceClustering.swift L1034** (HIGH): Wave-1 name-inheritance threshold used floor division — `3 / 2 = 1` face could claim a 3-face prior cluster (33% overlap satisfying the "≥ 50%" comment). Fixed to ceiling: `(n + 1) / 2` → requires 2 of 3 faces.
- **Restructure.swift L771-778** (LOW): `appliedPairs` (learn-from-corrections feedback) was gated inside `if let h = undoHandle`, so moves were silently uncredited when the undo journal failed to open (disk-full / sandbox). Decoupled: always collect pairs when `recordUndo=true`, write to journal separately.
- **tagging.rs L1960-1966** (MEDIUM): `visual_tagger_ran = models.ram_plus.is_some()` — true whenever RAM++ model is loaded, even when it ran and emitted 0 tags. `tags_evaluated=true` then caused dbwriter to wipe previously-stored content tags on re-scan of abstract/solid-color images. Fixed: hoisted `ram_plus_ran` (set to `ram_emit_count > 0`) before the image block; `visual_tagger_ran` now requires RAM++ to have emitted at least one tag, or CLIP scene tags to be enabled with an embedding.
- **tagging.rs L1719** (bookkeeping): `let mut ram_plus_ran` was declared inside the `if let Some((rgb, w, h)) = image_source` block, making it inaccessible at the `visual_tagger_ran` site. Hoisted to function scope.

## 2026-06-19 — fix: 6 bugs (macOS: unknown-person survivor, displayName double-space, nonisolated statics; Windows: _lastAnyProgressAt race, Whisper/Bge subscriptions, thumbnail drain block)

6 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **ReadStore.swift** (HIGH): `mergePersonsBatch` set `isNamed=true` for `is_unknown` persons, causing Unknown clusters to beat genuinely unnamed persons in survivor selection and swallow their faces. Fixed to `false`.
- **ReadStore.swift** (MEDIUM): `displayName` with suffix produced `"John Smith  Jr"` (double-space). `", Jr".replacingOccurrences(", ", " ")` yields `" Jr"` (leading space) before join. Simplified to `parts.append(s)`.
- **DeepAnalyze.swift** (MEDIUM): `compareCallsSinceClear` and `compareSampleLogged` declared `nonisolated(unsafe) private static`, opting out of actor isolation though only used inside the actor method `compareFaces`. Promoted to actor instance variables.
- **ModelInstallerService.cs** (MEDIUM): `_lastAnyProgressAt` was a plain `static DateTime` field written from PropertyChanged callback thread and read from thread-pool watchdog without synchronization. `volatile` is illegal on struct fields in C#; changed to `long _lastAnyProgressAtTicks` with `Interlocked.Exchange`/`Read`.
- **ModelInstallerService.cs** (LOW): `Whisper` and `Bge` slots not subscribed to `OnSlotPropertyChanged`. Added missing subscriptions to prevent perpetual stale aggregates if either slot is added to `IsBusy`/`CoreModelsInstalled` in the future.
- **ThumbnailService.cs** (MEDIUM): `Thread.Sleep(50)` in `TryEnqueueWithRetry` blocked the single drain worker for 50ms on every compositor-shutdown `TryEnqueue` failure, stalling all queued thumbnail requests. Removed; immediate retry is sufficient.

## 2026-06-19 — fix: 10 more bugs (Windows: mobileclip embedding corruption, face crop leak, downloader progress; macOS: mergeTags duplicate, mergePersons null, undo identity skip)

10 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

1. **mobileclip.rs (HIGH) — integer division truncates last embedding in batch**:
   `embed_dim = total / batch` silently discards a remainder if ORT output isn't cleanly divisible.
   The last embedding in any batch gets the wrong dimension, corrupting cosine similarity in semantic
   search with no error emitted. Fix: bail if `total % batch != 0`.
   File: `platforms/windows/src/engine/src/models/mobileclip.rs`

2. **dbwriter.rs (HIGH) — `.filter_map(ok)` silently drops row errors → stale face crop files never pruned**:
   `stale_face_ids` was collected with `.filter_map(|r| r.ok())`, silently dropping any row error.
   The face DELETE still ran, but the silently-dropped IDs were never added to `crop_ids_to_prune`,
   leaving `face_crops/<id>.jpg` files on disk forever. Fix: `.collect::<rusqlite::Result<Vec<_>>>()?`
   so any row error surfaces and aborts the transaction rather than leaking files.
   File: `platforms/windows/src/engine/src/pipeline/dbwriter.rs`

3. **identity_clustering.rs (MEDIUM) — `dim == 0` returns contradictory cluster_ids/cluster_count**:
   Returned `cluster_ids: vec![0; n]` (all faces in cluster 0) with `cluster_count: 0`. Callers
   that iterate `0..cluster_count` create zero People entries, orphaning all n faces from the tab.
   Fix: return `cluster_ids: (0..n).collect()`, `cluster_count: n` (each face its own singleton).
   File: `platforms/windows/src/engine/src/pipeline/identity_clustering.rs`

4. **downloader.rs (MEDIUM) — final 100% progress event never emitted in `download_parallel`**:
   The 10 Hz throttle could suppress the last chunk's progress; a stale no-op block at the end
   silenced the "silence unused variable" intent. The progress bar stalled at ~99% permanently.
   Fix: emit unconditional final event inside the drainer when `rx.recv()` returns `None`.
   File: `platforms/windows/src/engine/src/downloader.rs`

5. **scan.rs (LOW) — panic in `ScanSession::new_with_options` leaves scan_state permanently set**:
   If the scan task panicked before the `*scan_state_release.lock() = None` cleanup, the slot
   stayed occupied and every subsequent `startScan` returned `scan_already_running` until restart.
   Fix: RAII `ScanStateGuard` struct whose `Drop` clears the slot on any unwind.
   File: `platforms/windows/src/engine/src/commands/scan.rs`

6. **TagWriter.swift (MEDIUM) — `mergeTags` allows case-variant duplicates from the `new` array**:
   `lowerExisting` was built once from `existing` and not updated as tags were appended. Two tags
   in `new` like `["vacation", "Vacation"]` both passed the guard and were written as distinct
   Finder tags. Fix: promote `lowerExisting` → `lowerSeen: var Set` and update on each insertion.
   File: `platforms/apple/shared/Sources/FileIDShared/TagWriter.swift`

7. **TagWriter.swift (LOW) — `undoBulkAdd` strips tags without identity verification for nil-identity journal entries**:
   The size/mtime guard was entered only if BOTH were non-nil; entries from older builds (nil fields)
   bypassed it entirely and could mangle an unrelated replacement file. Fix: `guard let` skips the
   undo for any entry with missing identity data (safer than mangling a different file).
   File: `platforms/apple/shared/Sources/FileIDShared/TagWriter.swift`

8. **Database.swift (MEDIUM) — `mergePersons` representative_face_id stays NULL during initial re-scan**:
   The `COALESCE(SELECT ... WHERE arcface_embedding IS NOT NULL, representative_face_id)` always
   evaluates to `representative_face_id` (NULL) while the v12 reset is in progress and no embeddings
   exist yet. Any merge during the initial re-scan leaves the target person with no representative
   face — the People UI shows no crop until the next re-cluster. Fix: add a second fallback
   sub-select (any face_print, ignoring embedding) before falling back to the existing value.
   File: `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift`

9. **Database.swift (LOW) — `Int($0)` truncation in `mergePersons` args**:
   `validSources: [Int64]` were cast to `Int` before being appended to the GRDB args array.
   Safe on 64-bit macOS (same width) but fragile. Fix: pass `validSources` directly.
   File: `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift`

10. **CleanupViewModel.cs (LOW) — dead null guard on content_hash read**:
    `(byte[])reader[3]` throws `InvalidCastException` for DBNull (the null guard can never catch it).
    The WHERE clause already filters, but the guard masked a future regression risk.
    Fix: `if (reader.IsDBNull(3)) continue;` before the cast.
    File: `platforms/windows/src/FileID.App/ViewModels/CleanupViewModel.cs`

## 2026-06-19 — fix: 6 bugs across macOS + Windows (trash DB deadlock, person ID corruption, actor blocking, data race)

Six bugs fixed and verified (253 Swift tests + Rust clippy -D warnings clean):

1. **trash.rs T-1 (HIGH) — DB mutex held during PowerShell restore (30 s stall)**:
   `handle_restore_from_trash` acquired `db.lock()` at the top and held it through
   `restore_batch_from_recycle_bin`, which shells out to PowerShell (up to 30 s). Every DB reader
   (including the UI's `ReadStore`) was blocked for the full duration of any undo operation.
   Fix: compute filesystem-only `pre_occupied` before the lock; read `allowed_canonical` in a
   short-lived block (lock drops immediately after); PowerShell runs with no lock held; re-acquire
   for the post-restore transaction. File: `platforms/windows/src/engine/src/commands/trash.rs`

2. **trash.rs T-2 (HIGH) — wrong person ID on revert-merge when `source_person_id` was recycled**:
   `INSERT OR IGNORE INTO persons (id=source_person_id)` silently no-ops when that id is already
   held by a *different* person (SQLite auto-incremented past it and reused the value). The
   subsequent `SELECT id FROM persons WHERE id = source_person_id` then returns the occupant,
   and all reverted face-prints land on the wrong person. Fix: check `execute()` rows-changed
   (0 = conflict); on conflict, do a plain `INSERT INTO persons` (no id) and use `last_insert_rowid()`.
   File: `platforms/windows/src/engine/src/commands/trash.rs`

3. **trash.rs T-3 (MEDIUM) — `let _ = tx.execute(...)` silences real DB errors after restore**:
   After a successful on-disk restore, `INSERT OR IGNORE INTO files` used `let _ =`, discarding
   errors from disk-full / corruption / schema mismatch — the file exists on disk but never
   appears in the Library. Fix: `tx.execute(...)?` — `OR IGNORE` already returns `Ok(0)` for
   constraint conflicts, so `?` only fires on genuine failures.
   File: `platforms/windows/src/engine/src/commands/trash.rs`

4. **restructure.rs R-1 (MEDIUM) — silent signals load failure**:
   The `spawn_blocking` signals load matched only `Ok(Ok(v))` + a catch-all `_`; a real
   `Ok(Err(anyhow_error))` or a panic `Err(JoinError)` both silently fell through to empty maps
   with no log line, making restructure silently degrade instead of reporting the cause.
   Fix: explicit `Ok(Err(err))` and `Err(err)` arms with `tracing::warn!`.
   File: `platforms/windows/src/engine/src/commands/restructure.rs`

5. **VLMDownloader.swift (HIGH) — `sha256HexOfFile` blocks actor thread**:
   For partially-downloaded models (no sentinel, right size), the sha256 verification was called
   synchronously on the actor thread — blocking for up to ~27 s per file (e.g. 13.5 GB Mistral)
   with no cancellation possible. Fix: offload to `DispatchQueue.global(qos: .utility)` via
   `withCheckedContinuation`, suspending the actor during the hash.
   File: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift`

6. **VLMDownloader.swift (LOW) + TLSPinning.swift (MEDIUM)**:
   - Sentinel write failure was `try?`-silenced; offline Macs hit HF on every DA launch with no
     log entry. Fix: log `vlm_sentinel_write_failed` via `JSONLog.shared.warn`.
   - `TLSPinningSessionDelegate.pinningRejected` was a plain `var` written on URLSession's
     delegate queue and read on the actor thread — data race under Swift 6. Fix: `NSLock`
     with a computed getter; writer uses lock/unlock in the delegate callback.
   Files: `VLMDownloader.swift`, `platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift`

On-hardware verified (prior session): TrueNAS corpus 60K files, face clustering 3.75 s / 39 faces → 18 persons.

## 2026-06-19 — fix: face clustering "never completes" (macOS HNSW O(ef²) → O(ef log ef))

Three fixes for the face clustering hang reported on macOS:

1. **Root cause fixed — `HNSWIndex.searchLayer` was O(ef²·M)**:
   The beam search used sorted `[(Int32, Float)]` arrays as priority queues. `removeFirst()` is O(ef)
   (shifts all elements); `insertSorted` is O(ef) (array shift after binary-search position). With
   `efConstruction=200` and up to 200K faces, the HNSW build took an estimated 30+ minutes — explaining
   "never completes." Fixed by replacing the sorted arrays with proper binary heaps: `MinHeap` for the
   candidate frontier (O(log ef) extract-min, O(log ef) insert) and `MaxHeap` for the bounded result
   window (O(1) peek-max, O(log ef) evict). Complexity drops from O(ef²·M) → O(ef·M·log ef) per
   `searchLayer` call — approximately 1000× fewer operations for N=200K, ef=200, M=16.
   File: `platforms/apple/engine/Sources/FileIDEngine/Models/HNSWIndex.swift`

2. **Cancellation check added to HNSW build loop**:
   The 200K-face insert loop had no cancellation check, so a Cancel/Shutdown during HNSW build would
   either be ignored until the loop finished or kill the process mid-transaction. Now polls
   `clusterShouldCancel` every 1,000 insertions — responsive without measurable overhead.
   File: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`

3. **`ArcFaceService.self.env` data race fixed**:
   `load(_:)` read `self.env` without holding `lock`, while `MobileCLIPService`, `BGETextService`, and
   `RamPlusService` were all fixed with the same lock-bracket pattern in the previous session (commit
   `72492b6`). Applied the identical fix: `lock.lock(); let cachedEnv = self.env; lock.unlock()`.
   File: `platforms/apple/engine/Sources/FileIDEngine/Models/ArcFaceService.swift`

Needs hardware verification: `swift build` + face clustering on real Mac against a library with 1K+ faces.

## 2026-06-19 — macOS↔Windows lockstep pass: extension sets aligned + restructure verified byte-faithful

Brought macOS up to Windows (and vice-versa) on every front I can verify headlessly:
- **Extension sets aligned** (the decodable ones, both directions): macOS gained `mts`/`m2ts`
  (AVFoundation) + `odt` (textutil); Windows gained `wmv` (Media Foundation) + `aiff`
  (symphonia) + `ppt`/`xls` (→ Documents/) + a real `odt` extractor. Left divergent only the
  capability-driven formats (macOS-only RAW `orf/rw2/raf`; `flv/mpg/mpeg` that Windows MF
  can't reliably decode → would create failed/looping rows).
- **Restructure verified byte-faithful** (no code change needed): identical pass order (P1
  visual → R3 docs → R1 non-image → rule cascade), identical profiles/thresholds, identical
  non-image bag-of-words signature (filename ∪ whole-lowercased tags — so the new audio
  artist/album tags cluster on both). The rule cascade gives every kind a destination ⇒ no
  file type is un-organizable. Restructure is at production quality on the engine side.
- **Known remaining macOS gap (big, hardware-gated): the RAM++ tagger.** Windows tags images
  with RAM++ (Swin-L, 4585 tags); macOS uses Vision + CLIP-scene tags. Affects the image
  pass's ~22% tag weight + search. A major model addition (ONNX via CoreML + install UI +
  calibration), needs a Mac to verify — tracked, out of this headless pass.

Verified: macOS build + 253 tests; Windows clippy -D warnings + 368 tests.

## 2026-06-19 — content clustering for EVERY file type: audio, code/e-books, 3D models + text-less-doc loop fixed

Restructure now groups every major file type by content, not just images/docs/video — both engines,
lockstep. Branch `feat/content-coverage-all-types` (off main @ d257973).
- **Audio (macOS parity):** macOS did nothing with audio at scan; now `processAudio` reads ID3/Vorbis/
  MP4 metadata (artist/album/title) via a NAS-bounded AVFoundation read → auto tags, so audio clusters
  by artist/album in the non-image pass — matching Windows (symphonia, already shipped).
- **Code + e-books:** ~40 source-code/prose extensions + EPUB classified as `doc` and clustered by
  extracted text (BGE). macOS DocText reads code as UTF-8 + a new `epubText`; Windows `doc_extract`
  mirrors it (read_plain + `extract_epub` + a dep-free `strip_tags`). node_modules/.git already skipped.
- **3D models:** recognize obj/stl/ply/glb/gltf/fbx/usd*/dae/3mf/3ds/off as `model`; `.obj` is
  rendered to a thumbnail at scan and CLIP-embedded (macOS QuickLook→embedImage; Windows obj_render→
  clip) so it clusters with photos/video; other 3D formats group under `3D Models/` + named by Deep
  Analyze. A `.obj`-limited CLIP-backfill carve-out reprocesses existing .obj once.
- **Text-less-doc loop fixed (the "error"):** the BGE backfill carve-out re-walked a doc that yields
  no embeddable text (image-only PDF, iWork, empty file) on every rescan. New `v19_files_text_stage_done`
  column (additive, byte-faithful both engines) gates the carve-out on `text_stage_done = 0`, so each
  doc re-walks once then stops — text-less docs stay skipped, text docs keep their embedding.

Verified: **macOS build + 253 tests; Windows clippy -D warnings + 368 tests** (new: extension mappings,
model + text_stage_done skip-set parity, strip_tags, gate behavior). The added code/e-book/3D extension
sets are byte-identical across engines; migration parity lists + counts updated (18→19) on both.

## 2026-06-18 — doc-embedding backfill on install-then-rescan (both engines) + macOS BGE concurrency hardening

Post-merge audit of the scan-time doc-embedding work found one real lockstep gap and three macOS
concurrency refinements; all fixed, both engines green.
- **Backfill on the "scan first, install BGE later" path (the common case).** BGE is opt-in, so a
  user's first scan predates it; the incremental skip-set then drops those docs by size+mtime and they
  never get embedded — stranding them on weak filename clustering (Windows has no plan-time fallback)
  or re-embedding them at every plan (macOS). Fixed by mirroring the existing CLIP-image backfill
  carve-out for docs: a `text_embeddings`-missing doc/pdf is kept in the pipeline so the rescan
  backfills it — macOS `DBWriter.skipSetTextBackfillExclusionSQL` (gated on `BGETextService`
  installed) ANDed into `Discovery.buildSkipSet`; Windows `SKIP_SET_TEXT_EMBED_GATE` (gated on
  `bge_installed()`) ANDed into the scan_session skip query. Install-gated so it can't force a perpetual
  re-walk; self-healing once embedded.
- **macOS `BGETextService` hardened for scan-time concurrency** (now hit by many doc workers, not the
  old serial plan loop): a `DispatchSemaphore(value: 4)` bounds concurrent ANE inferences and a
  double-checked `loadLock` builds the ORT session exactly once — parity with ArcFace/MobileCLIP. (ORT
  Run is already thread-safe + the tokenizer is immutable, so correctness was fine; these are perf +
  cold-start.)
- Verified: **macOS 251 tests, Windows clippy `-D warnings` + 366 tests** (incl. a new
  `text_embed_gate_reprocesses_embeddingless_docs_only`). A discovery test that asserted an
  embeddingless pdf is skipped was made BGE-install-independent by seeding its `text_embeddings` row.

## 2026-06-17 — macOS embeds doc vectors at SCAN (plan 3 min → 32 s); both engines read the store

Perf + lockstep: macOS embedded document BGE vectors at PLAN time (≈3 min over USB, re-done
every replan); now it embeds them at SCAN like Windows and caches them in `text_embeddings`, so
the plan reads them instantly. Added `processDoc` + a PDF BGE path (a shared `bgeTextEmbeddingBlob`
on visionQueue), a `textEmbeddingBlob` on the DBWriter struct + `insertTextEmbedding` (main +
unchanged-file backfill), and the restructure doc pass now PREFERS the scan-cached embedding
(plan-time only for docs scanned before BGE was installed). **Verified on the owner's library:
a rescan populated text_embeddings 0 → 1076, and the plan dropped from ≈3 min to 32 s at the same
53% doc clustering.** Both engines now read the same scan-time store (tighter lockstep). macOS
build + 251 tests.

## 2026-06-17 — Restructure file-type coverage COMPLETE: video by content + pptx/xlsx + BGE install UI

Closed the last content-clustering gaps so EVERY major file type groups by content, not filename:
- **Video** clustered the last filename-only kind. Windows already CLIP-embedded video keyframes at
  scan (just never used them — restructure filtered `kind='image'`); macOS `processVideo` was a no-op
  (the scan skips AVFoundation to avoid NAS hangs). Now BOTH: the restructure CLIP pass selects
  `kind IN ('image','video')`, and macOS embeds a ~25%-duration keyframe with the same CLIP model,
  bounded by a 6 s off-thread watchdog, run on visionQueue (not the cooperative pool — audit fix).
  **Verified on the owner's library: 364 video moves → 45 content groups at 87% folder-agreement**
  (videos now land with their event photos). 286/295 sample videos embedded (rest timed out gracefully).
- **macOS pptx/xlsx** text extraction (textutil can't read OOXML) via `unzip -p` + a:t/t tag mining —
  mirrors Windows `doc_extract`; ~122 such files now cluster by content.
- **BGE install UI both platforms** — macOS `BGEModelInstaller` + a "Document understanding" Settings
  GlassCard (mirrors RamPlus); Windows a `Bge` ModelSlot + Settings card (mirrors Whisper). So users
  can actually download the ~135 MB model that powers doc clustering (else it falls back to filenames).

3-agent adversarial audit of all the new code: 1 perf defect found + fixed (video embed on the
cooperative pool), rest clean (KeyframeBox lock coverage, the pptx regex, the filter interaction,
both installers vs their proven templates). Windows clippy + 364 tests; macOS build + 251 tests.

## 2026-06-17 — Restructure clusters documents by BGE CONTENT (46% → 53% on the real corpus)

Both engines clustered documents by FILENAME TOKENS only — they never read content. Added a
document-content pass (classify_documents / classifyDocuments) that clusters docs by a BGE-small
embedding. **Windows** already computed + stored these (bge_text + doc_extract → text_embeddings
at scan); it just didn't consume them — small change. **macOS had nothing**, so built it from
scratch + verified on-device: a Swift WordPieceTokenizer (parity-tested port of the Rust one), a
BGETextService (BGE ONNX via ORT/CoreML, mean-pooled like Windows), and DocText (textutil/PDFKit);
embeds at plan time (identical embeddings → lockstep with Windows' scan-time store). Proven by an
owner A/B (49%→57% NN-same-folder) then end-to-end (46%→53% folder-agreement). Calibration trap
hit + fixed: the engine mean-pools BGE (cosines compress high ≈ 0.79), so the A/B's CLS-pooled
thresholds collapsed docs into one folder (24%) until measured + moved to the mean-pooled range
(cluster 0.82 / folder_match 0.78 / auto 0.84). Windows clippy + 37 tests; macOS build + 251 tests
(8 tokenizer parity + 1 on-device BGE). BGE declared for macOS in the manifest (pinned). REMAINING:
the BGE download installer + Settings install card (both platforms) so users get the model — same
pattern as the RAM++ installer / Windows Whisper card (NEXT.md).

## 2026-06-17 — Restructure calibrated on a REAL library: fixed the photo-collapse (2 → 457 groups)

Calibrated Restructure against the owner's real photo library (the "Adlon" external drive) — and
found a genuine latent failure, the actual reason it "sucked": on a real ~3.3k-image personal set,
`planRestructure` auto-merged **109 distinct event folders into ONE "Camera Roll" destination** (2
groups, 38% folder-agreement). Drove it headlessly (release `FileIDEngine` over a FIFO; scanned
Personal + iMac Desktop into the live DB; disabled RAM++ for the scan since it's CPU-bound and the
CLIP embeddings — the calibration input — are produced regardless; numpy distribution analysis +
`planRestructure` scored against existing folders as weak labels).

Root cause (measured on the actual CLIP embeddings): cosines for a *coherent personal library*
compress HIGH (within-event ≈ 0.80, inter-folder centroid p90 ≈ 0.84), but the cluster cosines
(0.50/0.40/0.42) and image-routing bars (0.55/0.72) were tuned for *diverse* images and sat below the
whole distribution → the clusterer merged the entire photo set into one blob that routed to the
nearest catch-all folder.

Recalibrated both engines byte-faithfully (cluster 0.84/0.76/0.76; image folder_match 0.80 / auto_folder
0.86 / auto_coh 0.78 / review_coh 0.70), all now env-overridable (`FILEID_RESTRUCTURE_IMG_*` /
`_CLUSTER_*`). **Validated out-of-box: 2 → 457 event-sized groups, 38% → 70% folder-agreement, biggest
cluster 3254→375.** Rust 37 restructure tests + clippy clean; swift build + 242 tests (one name-agreement
fixture bumped 0.6→0.82 on each side for the new bar). BGE re-evaluation (the `Personal` folder has
~1.3k real docs) + the family-photo `Users` tree are the remaining calibration follow-ups (see NEXT.md).

## 2026-06-17 — Deep Analyze: TRUE AI understanding of audio + 3D (Whisper, 3D→VLM, macOS sound-ID)

Followed the metadata-naming entry below with *real* AI content understanding (owner: "the AI should parse these
things from 3D models to sound and movie files"; "use other models as long as they follow the licenses"). The
audio cascade is now **metadata title → speech transcript → sound event → original name**, and 3D models are
*looked at* by the VLM:
- **Speech (audio)** — Windows: a **whisper.cpp** subprocess (`WhisperRunner`, mirrors the llama.cpp VLM pattern)
  over the 16 kHz mono WAV from the new `audio_decode`; the CPU pack + `ggml-base` model (MIT, sha256-pinned
  `"whisper"` registry entry) install from a new **Settings card**. macOS: **Apple Speech** (`SFSpeechRecognizer`,
  on-device, no download; `NSSpeechRecognitionUsageDescription` added). `name_from_transcript` byte-faithful.
- **3D `.obj` → render → VLM** — Windows: a **hand-rolled software rasterizer** (`obj_render`, no new dep —
  parses `.obj`/`.mtl`, 3/4 camera, z-buffered flat-shaded triangles, 512² PNG via `image`), wired as the
  `"model"` arm of `rasterize_for_vlm`. macOS: the **OS QuickLook 3D generator** (the VLM loader's existing
  fallback) → MLX VLM. Reuses the installed VLM (no new model); falls back to embedded-name metadata on failure.
- **Sound events (non-speech audio)** — macOS: **Apple SoundAnalysis** (`SNClassifySoundRequest`) names field
  recordings / sound effects (rain → "Rain"). Windows YAMNet **deferred** (needs an unverifiable hand-rolled
  log-mel frontend — see NEXT.md/MODELS.md/DECISIONS.md).

All commercial-clean (MIT / Apache / OS frameworks), graceful metadata fallback everywhere. **Verified:** Rust
**364** lib tests + clippy `--all-targets -D warnings` clean; macOS `swift build` + **241** tests. New unit tests:
`collapse_transcript`, `name_from_transcript`, 3× `obj_render`, macOS `transcriptName`/`soundLabel`. No IPC/schema
change. On-device inference quality (VLM/Speech/SoundAnalysis, Windows whisper after install) is hardware-verified.

## 2026-06-17 — Deep Analyze: descriptive names for audio + 3D models (both engines, lockstep)

Deep Analyze's smart-rename now covers audio + `.obj` 3D models, not just image/video/pdf — named from their
EMBEDDED metadata (no VLM, no new model; video already works via keyframe→VLM):
- **Audio** (`.mp3`/`.ogg`/…) → "Artist - Title" from embedded tags (Windows: `audio_meta` `symphonia` probe via
  a new `extract_structured`; macOS: AVFoundation common-metadata in a new `DeepAnalyzeNaming`).
- **3D** (`.obj`) → a descriptive name from the modeler's embedded object/group/material labels (a small
  `.obj`/`.mtl` parser). Needed a new scanned file-kind `FileKind::Model` ("model") on both engines, because
  discovery DROPS `Other` (so `.obj` was never even scanned); audio was already a scanned kind.
- The metadata branch (`analyze_metadata_named_file` / `DeepAnalyzeNaming.metadataResult`) runs BEFORE the VLM
  weights resolve (works without a VLM) and always-handles a matched kind (a metadata-less file is an empty
  success, not a VLM-rasterize bail). Deep Analyze target filters now include `audio` + `model`.

Pure name-builders are byte-faithful across engines, pinned by matching unit tests (Rust 4 + macOS 5).
**Verified:** Rust **354** lib tests + clippy clean; macOS build + **240** tests. No IPC/schema/C# change.
Deferred (needs a MODELS.md/license decision + owner OK): *true* AI audio (Whisper/YAMNet) + 3D (render→VLM).
## 2026-06-17 — Folder-granularity picker (both apps) + app-side audit → 2 more fixes; roadmap reconciled

Closed the last real restructure gap and audited the least-covered area (the apps).

- **Folder-granularity Settings picker (both apps, merged #61, CI-green):** the engine has long read
  `FILEID_RESTRUCTURE_GRANULARITY` but no app surfaced it. Added a segmented Picker (macOS) / ComboBox (Windows)
  in Settings ▸ Restructure; each EngineClient forwards a validated non-default value at spawn (applies on the
  next engine start). Kept on the env mechanism — NO IPC/schema/engine change (the calibrated `granularity_delta`
  hot path is untouched), so zero conformance impact and no regression risk to default users.
- **App-side audit (2 verification-first agents over SwiftUI + WinUI — the engines were audited 4× but the apps
  were not):** the granularity picker audited clean on both. Two real bugs found + fixed: a **C# WinUI native-
  crash class** (`RestartAsync`'s `ConfigureAwait(false)` made `StartAsync`'s State writes raise PropertyChanged
  off-thread; `SettingsView`'s handler mutated `{x:Bind}` TextBlocks directly — the V15.2/V15.4 fast-fail shape;
  fixed by marshaling the batch through `DispatcherQueue.TryEnqueue` like every sibling view), and a **macOS
  People drag-merge data-loss** (dragging a named card onto an unnamed one deleted the typed name; fixed at the
  DB layer — `mergePersons` keeps the typed-named survivor, fail-safe + defense-in-depth). Two engine-lifecycle
  findings deferred with rationale (DECISIONS): "Stop Engine" respawns (crash-recovery-FSM, needs a new state +
  Mac verification) + exit-not-ordered-vs-pump (invasive, self-heals).
- **Roadmap reconciled:** several "remaining" NEXT.md items were already done (mid-review re-plan fix via
  `priorDeselectedIDs`; before/after tree via `TreeDiffView`; per-destination-bucket approval via the
  `.destBucket` drill-down) — corrected to reflect reality.

**Verification:** macOS `swift build` clean (the merge fix is build-verified; ReadStore is app-side, not in the
`swift test` engine suite); Windows app CI-verified. The implementable + headlessly-verifiable work is now
complete; what remains is owner-hardware-gated (threshold calibration on a real library — now a one-tap
experiment via the granularity picker; per-vendor on-hardware UAT; Authenticode/notarization signing).

## 2026-06-17 — Deep whole-codebase audit → 9 verified fixes landed (3 PRs, all CI-green) + a file_ref swap guard

A 4-agent verification-first audit (every finding re-checked against code — this repo has a ~40% audit
false-positive history) swept the restructure pipeline, the cross-platform DB/IPC contract, and the broad
Rust + Swift engines. The freshly-landed learn-from-corrections code audited **clean**. Findings triaged and
landed across three CI-green PRs on `main`:

- **#58 restructure lockstep (6 verified divergences):** macOS computed the Keep/Tidy/Junk tile counts + per-move
  tier on the ALREADY-STRIPPED proposals without the F-C1-004 semantic-claim exemption → the "Keep" tile
  undercounted folders left alone; now computed in `proposeAll` on the full pre-strip set (new tested
  `folderTiersAndCounts` + `PlanResult`). Rust `category_counts` sorted over a HashMap with no tie-break
  (nondeterministic on Windows + diverged from macOS) → count-desc-then-category-asc. `filename_tokens` counted
  graphemes vs Rust scalars → aligned. idf `ln` in f32 (libm-dependent) → f64. `FileDoneEvent.skippedStages`
  non-optional decode → tolerant. + a root-dest guard + a stale migration comment.
- **#59 engine robustness (2 fixed, 1 deferred):** `heic.rs` did WinRT activations with NO COM apartment on the
  apartment-less decoder-pool threads → **every HEIC/HEIF (the default iPhone format) silently failed on Windows**
  with a misleading "codec not installed" message; fixed via the `video.rs::ComScope` MTA pattern (confirmed
  compiling on `windows-engine` CI). `ArcFaceService` lacked the empty-output `baseAddress!` guard its sibling
  `MobileCLIPService` has → crash on a corrupt model; added. The `IPCSink` drainer's actor-held blocking write
  was DEFERRED with rationale (bounded self-healing backpressure; cancellation is independent; the clean fix
  fights Swift 6 `FileHandle` non-Sendability).
- **file_ref swap guard (follow-on PR):** the apply stale-check was path-only — it proved the DB row still
  NAMED the source, not that the file now AT that path was the planned one. Added a positive-evidence-only
  file_ref (NTFS ref / inode) comparison: skip ONLY when both the stored and on-disk refs are known and differ,
  so a same-path swap in the plan→apply window can't move the wrong bytes, and no missing-data case ever
  false-skips. Lockstep `file_ref_swapped` / `fileRefSwapped`, pinned by a pure unit test on both engines + a
  real same-path-swap integration test (real inodes on macOS, `cfg(windows)` NTFS ref on Rust).

**Verification:** Rust **350** lib tests + `clippy --all-targets -D warnings` clean; macOS build + **235** tests.
Both #58 + #59 merged green (incl. the Windows-only HEIC fix on `windows-engine`). The restructure butler is now
best-in-class AND audited; what remains is owner-hardware polish (per-bucket approval UI, granularity slider,
threshold calibration) + ship (signing, per-vendor UAT).

## 2026-06-16 — Learn-from-corrections: instance-based folder memory (both engines, lockstep)

The consuming logic for the v18 `restructure_feedback` table landed, completing the SOTA instance-based
"learn-your-style" loop (no model retraining). A new `restructure_feedback` module on each engine
(`pipeline/restructure_feedback.rs` / `Pipeline/RestructureFeedback.swift`):
- **`record(applied moves, now)`** — every move the user APPLIES is an approved example, so each moved file's
  `filename_tokens` are credited (+1 weight, UPSERT) toward its destination folder's basename. Wired into the
  apply loop (`restructure_apply.rs` / `Restructure.swift`) **alongside the undo journal**, so it shares the
  forward-only gate (stays empty on an undo run) and runs as ONE batched write after the loop. Best-effort —
  a feedback write never fails an apply.
- **`boost(&mut moves)` / `boost(proposals) -> proposals`** — the plan command sums each proposed move's
  (filename tokens → destination folder) feedback weight; at/above `FEEDBACK_AUTO_WEIGHT = 3` it upgrades the
  move to Auto with a "you've filed files like this here before" note. **Additive** — only raises confidence on
  moves the planner already produced, never re-routes — so it can't regress the calibrated image/non-image
  passes. Wired into `commands/restructure.rs` (new `db_for_boost` Arc clone) / `Restructure.proposeAll` on the
  full proposal set, before the anchor strip preserves the upgraded confidence into the emitted plan.

Validated against **authored labeled scenarios** (assistant-as-domain-expert, in lieu of real-data UAT): record
3 "acme invoice" files → /Invoices, then a NEW acme-invoice the planner marked Review is upgraded to Auto; an
unrelated move with no history stays Review; re-recording the same token→folder accumulates weight. Lockstep
parity tests on both engines (Rust 3, macOS 3).

**Both engines fully green:** Rust **349** lib tests + `clippy --all-targets -D warnings` clean; macOS build +
**232** tests (incl. the restructure apply-guard + round-trip suites). `filename_tokens` made `pub(crate)` (Rust)
for reuse; the Swift mirror's `filenameTokens` was already module-internal. Windows app C# is CI-only as always.
Next: commit on a branch → confirm both CI workflows → deep whole-codebase audit.

## 2026-06-16 — Restructure deep-research sweep + 4 verified best-in-class wins

`/deep-research` (27 web sources → 21 verified claims) + a 3-agent codebase audit graded Restructure against
the state of the art. Headline: the architecture already matches or beats the documented field (density
clustering, c-TF-IDF naming, journal-backed reversible apply, barycentre Sankey). Verification caught several
**audit false positives** (image profile IS calibrated; `Path::starts_with` is component-aware so the
"/Library2" containment bug is unreal; Windows already surfaces confidence+reason and already has the Undo
button). Landed + verified (Rust 343 + clippy, macOS build + 226):
- **Incremental crash-safe undo journal** (both engines) — append each inverse move as it happens + periodic
  fsync, instead of one write after the loop, so a crash mid-apply still leaves completed moves undoable.
  This is the research's #1 open question (the field only ships best-effort undo).
- **Single folder-granularity knob** (both engines) — `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose,normal,tight}
  shifts the cluster cosines (HDBSCAN `min_cluster_size` philosophy); one lever for owner calibration.
- **Empty-dir cleanup on undo** (both engines) — undo removes the orphan empty group folders apply created
  (`remove_dir` empty-only, root-contained).
- **Confidence + reason surfaced on macOS** — `RestructureView` rows now show the band badge + the engine's
  "why filed here" (the IPC always carried them; `mapProposals` was dropping them). Windows already had it.

Then a 3-agent **lockstep parity sweep** (instructed to verify values against code, since the prior audit had
false positives): **engine constants/algorithms = zero divergences**; **DB migrations (17) + IPC (31 cmds / 24
events) + conformance = perfect sync** (a library round-trips cleanly). One real bug found + fixed: the new
**person-as-tag name diverged** (macOS used `PersonRow.displayName`, full; Windows used title+first only) —
both now build an identical `personTagName` / `FormatPersonTagName` (title+first+middle+last+suffix joined,
else legacy name). Two agent claims were misses (Windows DOES have the Restructure confidence badge in
`DrillDownSheet` + the Undo button). macOS verified (build + 226); Windows person-tag change is CI-pending.

**All three CI workflows are green on `main`** (macOS app `0f61a04`, Windows engine `6cc3291`, Windows app
`fa40dfb`). Getting macOS green surfaced a real find: the long-standing `nonImageGroupsByFilename` CI failure was
NOT an engine bug — on-runner diagnostics proved the lone no-signal file is excluded before clustering and the
moves never contained it; the failure was the test's `#expect(!moves.contains { $0.fileID == 999 })`
negated-trailing-closure mis-evaluating on the runner's Xcode 16 swift-testing macro (not on local Xcode 26.5).
Fixed by materializing the ids + closure-free `contains`. Also hardened `macos.yml` to stop caching build
products (stale-object hazard) and kept the engine determinism improvements (singleton pre-exclusion + kNN clamp).

Remaining (research-backed, in NEXT.md): list-tier + "apply only high-confidence"; name-based routing signal
(Dropbox finding, needs real-data validation); learn-from-corrections (new migration); per-bucket approval +
before/after tree (large UI); granularity Settings slider; mid-review re-plan fix; file_ref stale guard;
owner threshold calibration. Pixel-level app parity + a full no-bug sweep need on-hardware runs + CI.

## 2026-06-16 — 5-item UX batch: byte-exact dedup, person-name filenames, apply buttons, explainer, auto-advance

Five user-requested changes, all lockstep on both platforms. macOS + both engines verified locally; the Windows
app slices (C#/XAML) are CI-pending (no local WinUI build).
- **Item 3 — person names → Deep Analyze filenames** (the reported bug). Named people are now prepended (deduped,
  ≤3, sanitized) onto the VLM proposed filename + injected into the caption/rename prompts. Byte-faithful
  `apply_person_prefix`/`applyPersonPrefix` + `fetch_face_names`/`format_person_ref` on both engines. Rust
  clippy + 343 tests; Swift build + 222 tests.
- **Item 1 — macOS byte-exact dedup.** Cleanup groups by `content_hash` (SHA-256, CryptoKit, **no new dep**)
  instead of perceptual phash, so only literally byte-identical photos count as duplicates. `ContentHash.swift`
  ports the Windows `content_hash` STRUCTURE (full ≤16 MB; head+4×interior+tail+size composite above), computed
  at scan into the existing `content_hash` BLOB; dedup + counter queries switched to GROUP BY content_hash.
  4 new tests + 226 total green. Values are macOS-local (SHA-256 ≠ Windows BLAKE3) — see DECISIONS.
- **Item 5 — apply buttons.** Deep Analyze tab now has separate **Apply tags / Apply people-as-tags / Apply all**
  (smart-name review path unchanged). New capability: person names written onto files as Finder/Explorer tags
  (DB-only before). macOS app-side via `TagWriter`; Windows reuses `applyTags`/`renameFiles` grouped by tag/person
  — no IPC contract change.
- **Item 2 — auto-advance.** People tab shows a gold "Continue to Deep Analyze →" CTA once ≥1 person is named
  (starts analysis + switches tab); the skip path stays. Both platforms.
- **Item 4 — explainer.** Dismissible, persisted "Tagging vs. Deep Analyze" banner on the Deep Analyze tab. Both
  platforms.

Verified: macOS `swift build` + 226 tests; Windows engine `cargo clippy -D warnings` + 343 tests. Windows app
(items 2/4/5 C#/XAML) is CI-pending.

## 2026-06-16 — Whole-codebase adversarial audit (6 parallel agents) + Windows undo button

Windows "Undo last run" button landed (WinUI `RestructureView` — `CanUndoRestructure`-driven, in the
ApplyBar; CI-pending, no local WinUI build). Then a 6-agent parallel audit swept the ENTIRE codebase
(Rust engine ×3 slices, Swift engine, Swift app, C# app). The **security-critical slice**
(IPC/shell/downloader) and the **ML slice** came back CLEAN — no-telemetry single-egress invariant
confirmed holding, all 32 default models SHA-pinned, file-op/traversal/IPC-framing/watchdog/WAL all
defended. Real bugs found + FIXED, all verified green:
- **P1 (Swift, data-loss + parity):** the macOS `files` UPSERT clobbered phash/GPS/camera/has_faces on
  a stage-skipped rescan — the Rust engine's R3-04 COALESCE/CASE-WHEN hardening was never ported.
  Ported verbatim + added the missing regression test.
- **P1 (Swift app, wedge):** Restructure apply/undo wedged `applying` forever if the engine replied
  with a bare error (db_unavailable: engine alive, no result, no reset) — the `lastErrorSignal`
  handler only cleared `loading`. Now clears `applying` too.
- **P2 (Rust, parity):** `path_search` stored verbatim (not NFC-normalized) at restructure-apply /
  bulk-rename / trash-restore → NFD-accented names unfindable until rescan. Normalized at all three
  (macOS already did). **P2 (Rust, parity):** image-pass *drained* tags so unclustered images lost
  content-tag grouping in the non-image pass — read non-destructively now (matches macOS).
- **P2 (Swift app):** `modelDownloadProgress`/`autoPilotActive` never reset on engine-exit (stale
  download bar + defeated watchdog after a crash); Cleanup per-group delete lacked a double-tap guard.
  Both fixed.

Verified: macOS swift test **222/222**, Rust cargo test **343/343** + clippy -D warnings clean.
Cosmetic/self-limiting P2s (C# dead error-kind, vestigial Tag-is-int drag branch, ClipSearchService
dispose race, ScanCoordinator bumpProcessed perf, the known-deferred bbox-verdict cross-platform key)
tracked in NEXT.md — no user impact; the C# ones are unverifiable without a local WinUI build.

## 2026-06-16 — Restructure R2: one-click "Undo last run" (reversibility) + R1 validated on a real library

**R1 validated on the owner's TrueNAS library** and tuned: real data exposed three naming bugs —
extension tokens leaking on double extensions (`E14.jpg.lps`→"jpg"), English connectors ("Boys
The"), and versioned junk folders (`Desktop 1.0` not caught) — all fixed (filename stopwords +
token-prefix junk detection, both engines). Result is good + conservative: résumés pulled out of
`Desktop 1.0` into a "Nolle Resume" group, copyright docs grouped, diverse docs left in place.

**R2 undo landed.** Apply was one-way. Now `apply` writes an inverse-move journal (each file's
new→original path; truncating → last-run-only) and `undoLast` replays the inverse moves THROUGH
`apply` itself — reusing every safety check (stale-guard, containment, no-clobber, DB update)
rather than duplicating them — then clears the journal so a run can't be undone twice. New IPC
command `undoRestructure`; the macOS app shows an "Undo last run" button after any apply that moved
files (+ corrected "you can reverse this" confirmation copy).

Both engines + the full contract: macOS (`Restructure.undoLast` + dispatch + EngineClient +
RestructureView), Rust (`RestructureApply::undo_last` + `handle_undo_restructure` +
`UndoRestructure` IPC), schema + C# command + C# VM (`CanUndoRestructure`). **macOS swift test
220/220** incl. a real apply→undo round-trip (file relocated → restored → DB + journal correct);
**Rust cargo test 343/343 + clippy -D warnings clean.** Remaining: the Windows app's XAML "Undo"
button (GUI parity, CI). **VLM cluster naming (P2) is CUT to R3** — real-data c-TF-IDF names are
already good, so it's marginal polish, not a ship blocker.

## 2026-06-16 — Restructure R1: butler now organizes ALL file types, not just photos (both engines)

**The fix for "Restructure just sucks."** Root cause: the semantic butler only ran on images
with a CLIP embedding (`Restructure.swift` guard `kind == "image"`); every document, PDF,
video, and download fell through to a 7-bucket date cascade — so a real mixed library dumped
all docs into `Documents/<Year>/` with zero content awareness. R1 adds an **additive non-image
semantic pass**: cluster everything the image pass didn't claim by a **filename-token + tag
bag-of-words** signature (the bag-of-words IS the representative vector — no model needed),
reusing the exact same density clusterer + learn-your-style folder matching under a separate,
tighter `nonImageProfile`. The image path is byte-identical (a `Profile` was extracted with the
old constants), so it cannot regress. Generic dumping grounds (Downloads/Desktop/Temp…) are
barred from being learn-your-style prototypes — the whole point is to move files OUT of them.

Both engines, byte-faithful + lockstep: macOS `RestructureSemantic.swift` + `Restructure.swift`;
Rust `restructure_semantic.rs` + `commands/restructure.rs` (also un-gated tag loading so docs get
their tags too, matching macOS). Owner kill-switch `FILEID_RESTRUCTURE_NONIMAGE=0`; thresholds
env-tunable (`FILEID_RESTRUCTURE_NI_*`) for calibration on a real library before defaults promote.

Verified here: macOS `swift build` + **swift test 218/218** (2 new non-image tests); Rust
**cargo test 343/343** (2 mirrored) + **clippy -D warnings clean**. R3-07B (IPC 64 MiB cap +
macOS newline-scan resume) was already landed in a prior session — verified end-to-end, so
whole-library plans render; only a stale comment needed fixing. **Remaining: R2** (VLM cluster
naming + one-click "Undo last run") and **owner UAT** to calibrate the non-image thresholds on a
real library (NEXT.md). macOS restructure builds + tests green — `RESTRUCTURE.md`'s "written,
unverified" line is now stale.

## 2026-06-16 — RAM++ in the first-run modal (macOS) + Welcome re-show parity (both platforms)

RAM++ was installable only from Settings on macOS — the first-run WelcomeSheet offered
CLIP / Face / Deep Analyze but not the tagger, so a new user silently got the weaker
CLIP/Vision tag fallback unless they hunted for it. Brought macOS to Windows parity:
RAM++ is now a gating row in `WelcomeSheet.swift` (row 2: CLIP → RAM++ → Face → Deep
Analyze), included in "Install all", the `.onAppear` refresh, `allInstalled`, and
`anyInProgress`; `FileIDApp.shouldShowWelcome()` now re-shows the sheet when RAM++ is
missing too. Pure wiring — `RamPlusModelInstaller` (download / SHA-256 / 12-part) already
existed and is identically shaped to the CLIP/ArcFace installers already in the sheet.

**Welcome re-show gating split (both platforms):** the re-show gate is now the three core
sub-1 GB models (CLIP + RAM++ + face); the multi-GB Deep Analyze VLM is install-once /
skippable and no longer re-nags every launch. macOS already had this split implicitly
(`shouldShowWelcome` ⊂ `allInstalled`); Windows now mirrors it via a new
`ModelInstallerService.CoreModelsInstalled` (CLIP+RAM+++ArcFace) consumed by
`MaybeShowWelcomeSheetAsync`, leaving `AllInstalled` (incl. VLM) for auto-dismiss + Done.

Parity audit of the onboarding/model surface also **cleared two suspected drifts**: the
Deep-Analyze VLM is already family+tier-aligned (Qwen2.5-VL-7B ≥16 GB / Gemma-3-4B <16 GB
on both; MLX vs GGUF formats differ by necessity, not drift), and CLIP is byte-identical
(same `Xenova/clip-vit-base-patch32` ONNX, same SHA-256s → embeddings round-trip safely).

macOS verified here: `swift build` clean, full suite **216/216 green**. Windows C# (the
`CoreModelsInstalled` split) is build-pending on CI — no local C# toolchain in this env.

## 2026-06-15 — bbox cross-platform parity landed (#55) — macOS lockstep COMPLETE

bbox parity was the last lockstep item. Root cause: macOS stores face bbox as "x,y,w,h"
NORMALIZED bottom-left (Vision); Windows stores JSON {x,y,w,h,…} in PIXELS top-left
(SCRFD). A library scanned on one OS + opened on the other had its faces fail to crop
(foreign parser → nil → excluded/blank). Fix is **macOS-side read-tolerance only** —
new `FaceBBox.parseNormalized` (FileIDShared) parses BOTH formats → normalized bottom-left,
threaded through the 3 macOS bbox readers (parseBBox/cropFaceCGImage, FaceAlign
matchLandmarks, PeopleView.cropFace). Windows needs NO change: it never reads bbox back
for cropping (saves face-crop JPEGs at scan time, clusters from embeddings; its only bbox
reads are R3-15 string-equality matches). The CSV branch is byte-identical → within-platform
behavior unchanged (safe). FaceBBoxTests verify CSV passthrough + the JSON pixel→normalized
+ origin-flip conversion; full macOS suite 216 green.

**macOS model-stack lockstep is now COMPLETE** — SFace/CLIP swap, RAM++ tagger (+ install),
VLM tags, R3-15, IPC cap, FaceAlign (on by default), detection-recall, and bbox parity all
merged + green. Remaining work is purely on-hardware/GUI UAT + release signing (NEXT.md).

## 2026-06-15 — face quality: detection recall fix + FaceAlign ON by default (#53, #54)

Two face-quality fixes after on-Mac feedback ("faces not detected + different people merged"):
- **Detection recall (#53):** the image fed to Vision face detection was downscaled to
  512 px, so faces <~10% of a 4000 px frame (~50 px) fell at/below Vision's limit and were
  missed. Bumped to 1536 px (CLIP/RAM++/phash/OCR downsample internally → unaffected),
  tunable via `FILEID_SCAN_MAX_PIXELS`. Cluster auto-merge + pass-1 cosines made env-tunable
  (`FILEID_FACE_TIGHT_COS`/`SMALL_COS`/`PASS1_COS`, defaults preserved) for corpus calibration.
- **FaceAlign ON by default (#54):** flipped `FaceAlign.enabled` to default-on (escape hatch
  `FILEID_FACE_ALIGN=0`). Aligned SFace crops are discriminative, so the thresholds (which
  assume aligned input) stop over-merging different people.

The fix for face quality was alignment + detection-resolution + threshold calibration —
NOT retraining the embedder (which would overfit + fork the cross-platform 128-d space +
break licensing). Full macOS suite 213 green; build debug+release clean.

## 2026-06-15 — 5-point FaceAlign landed opt-in (#51); only bbox parity remains (cross-platform, deferred)

FaceAlign (#51) wires `align112` into the macOS face-embed backfill behind
`FILEID_FACE_ALIGN` (default off): one `VNDetectFaceLandmarksRequest` per image →
5 NAMED landmark regions (leftEye/rightEye/nose/outerLips), assigned to template
slots by image-x (so no subject/viewer naming ambiguity), matched to the stored
bbox by center, aligned; falls back to the bbox crop on no match. Deterministic
(no landmark-order guessing), no schema change (landmarks re-derived at embed),
diagnostic `face_align_applied` log. FaceAlignTests verify the similarity fit;
full macOS suite 213 green. **Mac validation:** scan with the flag unset vs `=1`,
compare People clustering (the thresholds already assume aligned input, so it
should tighten, not regress); flip the default once confirmed.

**Only bbox pixel/JSON parity remains** — and it's deliberately deferred, not
merged blind. macOS stores normalized bbox, Windows stores pixels in JSON; safe
cross-platform read-tolerance needs a coordinate-space conversion threaded through
every consumer (parseBBox / cropFaceCGImage / matchLandmarks / PeopleView.cropFace
/ bboxArea), the exact multi-consumer change a prior swap broke clustering on. It
only matters for a library scanned on one OS and opened on the other (single-platform
users are unaffected), so it needs a real cross-platform DB + both-platform
validation — recipe in NEXT.md. Everything else in the lockstep is merged + green.

## 2026-06-15 — R3-15 data-loss fix + lockstep delta follow-ups landed (PRs #48–#49); only FaceAlign/bbox remain (Mac-gated)

Closed the last deferred FIX and the delta-re-audit follow-ups. Both engines build-
green; the new code was delta-re-audited by 5 agents (the macOS frame-scan even
survived a 200k-trial differential fuzz, 0 mismatches) and a delta-2 confirmed dry.

- **#48 R3-15 — durable face-verification keys (both engines, the last data-loss fix).**
  v13's face_a/face_b are face_print ids that churn on every faces_evaluated re-scan,
  so a user "different people" verdict silently stopped blocking the merge. Fix =
  churn-stable (file_id, bbox) keys via coordinated migration **v17** (identical
  identifier both engines; both canonical parity arrays updated + asserted equal),
  verdict-write populates them (Windows; macOS write is app-side/not_implemented),
  apply re-resolves (file,bbox)→current face id with legacy fallback. Additive
  migration + graceful-degrade = bounded blast radius. Churn-survival regression test
  on both engines. Windows 342 tests; macOS MigrationParityTests green.
- **#49 delta follow-ups.** The delta re-audit found: R3-15 was half-closed —
  `handle_find_merge_suggestions` still re-prompted rejected pairs via the churning
  ids (now resolves via the stable key too); RAM++ `FILEID_RAMPLUS_THRESHOLD` was a
  no-op with the per-class sidecar present + env knobs lacked a [0,1] guard; the RAM++
  downloads weren't SHA-256-pinned (now in ModelManifest, with the correct 925 MB size
  + a pre-preflight staging sweep); the VLM tag pass now uses 40-token greedy (Windows
  parity) and parseVLMTags trims/splits on all whitespace. (A ModelManifest parity test
  caught the JSON platform tag — fixed: RAM++ entries are now macos+windows.)

**macOS model-stack lockstep status:** RAM++ tagger (engine + install UI), VLM tags,
SFace/CLIP swap, and now the IPC cap + R3-15 are all MERGED. The only remaining
lockstep pieces are **FaceAlign** (Vision landmark order is undocumented →
Mac-only-determinable) and **bbox pixel/JSON parity** (coordinate-space change +
cluster-threshold retune → needs labeled data). Both are the user's-own-principles
"verify on Mac / tune against real data" items — full recipes in NEXT.md. Everything
mergeable + verifiable-here is on `main` and green. Behavioral validation of the
merged lockstep (install RAM++, scan, confirm tags/scores; Deep Analyze → vlm tags)
is the Mac UAT.

## 2026-06-14 — post-audit: IPC cap (R3-07B) + macOS model-stack lockstep underway (PRs #43–#46)

After the audit converged (round 7), turned to the remaining-work survey's real
code items — closing the last deferred IPC item and starting the macOS lockstep
(bringing the macOS engine onto the known-good Windows commercial-clean stack).
All merged green on `main`; macOS edits are build-verified (swift build debug+
release + unit tests) but their ML *behavior* still needs a Mac + labeled photos.

- **#43 R3-07B/R5-12 — IPC frame cap 32→64 MiB + O(n²)→O(n) macOS frame scan.** Bumped
  all 5 symmetric cap sites (incl. the macOS engine's stdin LineBuffer, a tighter
  16 MiB cap the survey missed). Both macOS framers now carry a scanned-prefix offset
  so a large frame isn't re-scanned every readability tick; LineBuffer made testable
  + 10 SharedTests. Two survey items debunked as already-fixed false positives
  (IpcSchema.Tests runs directly in CI; rename-heal exact-dup is guarded on both
  engines via the round-3 old-path-gone gate).
- **#44 RAM++ primary tagger (engine).** 4585-class RAM++ (RamPlusService, faithful
  port of ram_plus.rs) replaces the weaker Apple Vision classifier; per-tag scores →
  tags.score. Degrades gracefully to Vision tags when the model isn't installed.
- **#45 VLM searchable tags (source='vlm').** Second VLM pass in Deep Analyze mirrors
  the Windows Both-mode; parseVLMTags byte-identical + 4 ported unit tests.
- **#46 RAM++ install UI.** RamPlusModelInstaller + Settings card download the 3 RAM++
  files from the project HF mirror — activates the engine-side tagger.

**Remaining lockstep (NOT yet landed — see NEXT.md):** 5-point FaceAlign (⚠ Vision
landmark order is undocumented → Mac-only-determinable; wrong order = orthogonal
embeddings), bbox pixel/JSON parity (coupled + a prior swap broke clustering →
needs a threshold retune), and R3-15 durable face-verification keys (both-engine
additive migration + verdict write/resolve — a real data-loss fix). These need a
Mac / labeled data / focused care and are the next priorities.

## 2026-06-14 — round 7: the last-uncovered UI surface — 39 defects fixed + 4 delta regressions (PRs #40–#41); audit converged

Swept the surface rounds 1–6 never reached: the big action-bearing **C# Views +
ViewModels** (Library/People/Cleanup/DeepAnalyze + their VMs, EngineClient command
senders, modal sheets, sidebar) and the **remaining Swift Views** (Settings,
Cleanup, TreeDiff, sidebar, window shell, and the uncovered parts of the big three).
17 finder units → 3-lens default-reject vote → domain-expert recipe surfaced
**39 confirmed real defects** (0 P0/P1, 17 P2, 22 P3). All fixed via per-file
parallel fixers, every diff re-verified against live code + read before commit.

- **#40 macOS app (17):** stale thumbnails/previews from offset-keyed lists and
  `.task` without `id:` (Cleanup CopyTile, FilePreview, FinderTags, TreeDiff dup-id);
  heavy sync work off the MainActor (bulk delete+tag, face-reassign write, the 27×
  COUNT(*)/render pipeline strip, a dead per-batch COUNT); missing in-flight guards
  on bulk-merge (+ no silent row-drop on failure); a suggestions sheet force-opening
  over user nav; a bookmark-restore lost-update; an un-cancellable tag read; CLIP
  phantom "Downloading…" + per-tick syscalls; Start-button un-wedge; folder-pick
  readability pre-validation.
- **#41 Windows app (22):** overlapping-refresh races in 3 ViewModels (mirror the
  CI-green LibraryViewModel A4/A5 generation-guard + active-loads); re-entrant
  double-apply on bulk merge / mark-unknown / trash / per-row merge; inflight
  thumbnail-CTS removed by key without identity check (+ leak); the latched
  DeepAnalyzeLast re-applied every progress tick (inflated pill / stale caption);
  Cancel not stopping the Analyze-Selected batch; multi-MB IPC encode + blocking
  recursive wipe deletes on the UI thread; an O(N²)→O(N) selection-maintenance
  coalesce; a stale select-mode snapshot; a dead double row-build; the pipeline
  strip not resetting after a wipe; the per-prefix BulkActionResult reply gate.
- **Round-7 delta (folded into #40/#41):** the delta stage caught a self-inflicted
  regression in **4 files** (its 5th straight non-empty round): the FilePreview
  `.task(id:)` lacked the cancellation guard its own sibling added (stale-poster
  race); the CLIP `presentFilePaths` cache wasn't refreshed on partial-install
  failure; the Windows thumbnail `cts.Dispose()` was unconditional (cross-thread
  concurrent dispose) vs the guarded CleanupView twin; the pipeline-strip reset
  fired on "Clear folder" too (blanked an intact library) → re-derive the floor
  from the DB. All fixed; **delta-2 came back dry.**

**Audit converged.** Tally across the campaign: **64 real defects fixed** over
7 find-rounds (8/20/12/11/7/+delta/39) + 5 delta-rounds, every batch merged
CI-green with the only-`main`/no-open-PRs terminal state held between batches.
The find→verify→expert→read→delta→confirm-dry method covered the engine, data
paths, and the full UI surface on both platforms; round 7 was the last untouched
tier. Gates at HEAD: macOS `swift build` debug+release + 195 tests; Windows
`cargo clippy -D warnings` + 341 tests + .NET app (x64/arm64) + engine
(x64/arm64-native/arm64-cross); all post-merge `main` CI green. Remaining work is
exclusively HARDWARE / GUI / LABELED-DATA UAT + the two deferred coordinated
cross-platform changes — see NEXT.md.

## 2026-06-14 — round 6: action-bearing UI Views audited — 7 defects fixed (PRs #37–#39)

Pushed the same method onto the surface rounds 1–5 left for last: the **action-bearing
UI Views** on both platforms (Restructure, Deep Analyze, People naming, the model-install
Welcome sheet, the file-preview sheet). 7 candidates → 7 real, all merged CI-green; the
delta stage again caught a self-inflicted regression (R6-07), continuing its 4-for-4 record.

- **#37 macOS app Views (5):** R6-01 RestructureView applied the *live* eligible-move set
  at confirm time, not the set shown when the confirm dialog opened — a move that became
  ineligible between present and tap was still applied (P1) → snapshot `pendingMoves` at
  present; R6-02 DeepAnalyzeViews ran 4 Hz on-main DB `COUNT`s for the status header →
  cached via `refreshStatusCounts()` on appear/change; R6-03 Sankey source-tap drilled down
  on the basename, colliding distinct folders that share a leaf name → use the full
  `identityKey` path; R6-06 Sankey cursor `@State` was written on every mouse move even off
  any ribbon (layout thrash) → write only over a ribbon; R6-07 WelcomeSheet keyed the
  Failed-state `onChange` on the error *message string*, so a retry failing with the same
  message never re-flipped to Failed (spun forever) → key on the monotonic `lastErrorSignal`;
  also added a 45 s VLM download-stall watchdog.
- **#38 Windows app Views (2):** R6-04 RestructureView.xaml.cs `_applying`/`_applyingPlan`
  were per-instance, so a re-navigated view could release the plan another apply was mid-flight
  on → made `static` + release-guarded on `ReferenceEquals(plan, _applyingPlan)`; R6-05
  FilePreviewSheet.xaml.cs `OnMediaPlayerFailed` could enqueue against a stale media generation
  after unload → capture `_mediaGen` and bail on mismatch/unload.
- **Round-6 delta (#39):** R6-07's stall watchdog kept its 45 s timer armed through the
  silent post-download MLX cold-load and false-fired "Download stalled" on a legitimately
  loading model → gated the fire on `vlmLastFraction < 0.999` so it arms only while the
  download is actually in flight; the `vlmInstalled` sentinel owns the cold-load phase.

Gates at HEAD: macOS `swift build` debug+release + CI macOS-app green; Windows `cargo clippy
-D warnings` + 341 tests + both Windows workflows green; all post-merge `main` CI green.
Only `main`; no open PRs. Running tally across the campaign: 60 real defects fixed over
6 find-rounds + 4 delta-rounds.

## 2026-06-14 — rounds 4–5 deep audit: 23 more defects fixed (PRs #30–#35); the engine + app are now exhaustively audited

Extended the round-3 method to the surfaces rounds 1–3 didn't reach. Each round:
per-file finders → 3-lens default-reject vote → per-finding domain-expert
re-verification + fix recipe (with cross-platform parity check) → apply + test →
**delta re-audit of the landed diff** → confirm-dry. The delta stage earned its
keep: it caught a self-inflicted regression in **every** round (round-3 delta 2,
round-4 delta 2, round-5 delta 1 — all fixed + confirm-dry'd clean), which the
find+verify passes missed.

**Round 4 — engine files rounds 1–3 skipped (FFI, IPC, migrations, stdio loop,
person-ops, core clustering, discovery).** 14 candidates → 12 real.
- **#30 Windows engine (9):** R4-01 incremental skip-set was a tautological
  stored-vs-stored predicate that silently stranded edited files (P1) → now carries
  (size,mtime) + revalidates vs the live file (macOS already did this); R4-02/08
  Pass-2 clustering O(n²)+singleton-merge (P1); R4-03 cpu_topology FFI heap-OOB UB;
  R4-04 one-byte-per-read stdin; R4-05/06/07 person-ops data-loss (mark-unknown
  leaves sub-fields / merge deletes face-anchored verdicts / merge drops the
  source name); R4-09 EP-arm-on-forced-CPU false-poison; R4-12 CUDA version sort.
- **#31 macOS engine (3):** R4-02/08 Pass-2 twin; R4-10 engine-writer busy timeout
  (was dropping writes under the app's WAL lock); R4-11 cancelScan start-window race.
- **Round-4 delta (#32):** R4-07 over-grafted a name onto an already-named merge
  target; R4-11 cancel attributed to the enqueued (not running) epoch could poison
  a queued scan. Both fixed.

**Round 5 — app-side data paths + model install + C# (rounds 1–4 skipped).**
15 candidates → 11 real (the 12th = the already-deferred R3-07 PART B).
- **#33 macOS app (9):** R5-01 bulk-merge union-find could DELETE a user-named
  person + leave the survivor unnamed (P1) → keeps the named/larger root;
  R5-02 bulkMarkUnknown off-main batch; R5-03 bulk-rename notify-once; R5-04
  bulk-tag dropped off-window selections → id→FileRow model; R5-07 respawn-flap cap;
  R5-08 case-only-rename inode check; R5-09/10/11 off-main + cancellation hardening.
- **#34 Windows+C# (3):** R5-05 model-install legacy-sentinel now SHA256-revalidates
  before vouching (stale installs masqueraded as current); R5-06 C# auto-cluster gate
  reset on crash; R5-07 C# respawn-flap cap (cross-platform twin).
- **Round-5 delta (#35):** R5-11's off-main tag-edit conversion introduced a
  lost-update race + deferred draft-wipe → serialized via a per-editor task chain.

**Tally for the session:** 53 real defects fixed across 5 find-rounds + 3 delta-rounds
(8 + 20 + 2 + 12 + 11), all merged CI-green; 3 deferred with full both-platform
recipes in NEXT.md (R3-15 cross-platform face-verification-anchor migration; R3-07
PART B / R5-12 IPC cap raise). Gates at HEAD: Windows `cargo clippy -D warnings` +
341 tests; macOS `swift build` debug+release + 195 tests; both CI workflows + the
.NET app build green on `main`. On-hardware e2e (isolated Adlon copy) GREEN. Only
`main`; no open PRs. Remaining work is exclusively HARDWARE / GUI / LABELED-DATA /
the two deferred coordinated changes — see NEXT.md.

## 2026-06-14 (round 3) — round-3 deep audit: 20 more defects fixed across both platforms (PRs #25–#27); 1 schema-migration finding deferred

Ran a third, deeper adversarial audit: 16 per-file finders over the highest-risk subsystems →
3-lens **default-reject** verification (32 candidates → 21 survivors) → a per-finding
**domain-expert re-verification + fix-recipe** pass (all 21 confirmed real; 3 had the suggested
fix corrected). Landed in three platform-clean PRs:

- **#25 Windows engine (7):** R3-03 face-clustering unknown→named merge (P1 data-loss);
  R3-04 upsert nulling phash/camera/GPS + flipping has_faces/has_text on a stage-skipped re-scan
  (P1); R3-16 hoist heal `symlink_metadata` out of the writer txn; R3-17 heal size corroboration
  (cross-volume MFT-ref collision — Windows mirror of F-A2); R3-18 SEC-5 non-ASCII case-fold bypass;
  R3-19 `tags_evaluated` wiping auto-tags when no tagger ran; R3-21 scan-notice lost under
  backpressure. clippy clean; 340 tests (+2).
- **#26 macOS engine (9):** R3-01 empty-PARSED-description clobbering a caption (P1, extends F-A6);
  R3-02 unknown-unmark-during-window deletes a person + orphans faces (P1 data-loss); R3-09 HNSW
  `vDSP_distancesq` (no per-call scratch); R3-10 auto-merge `named` predicate widened to
  title/middle/suffix; R3-11 auto-merge re-reads identity under the writer lock; R3-12 semantic
  restructure HNSW above 5_000 files (was O(n²)); R3-13 PDF open-failure recorded as success;
  R3-14 hoist heal `lstat` out of the writer txn; R3-20 download progress monotonic guard.
  swift build debug+release clean; 195 tests (+1, persistCoalesces extended).
- **#27 macOS app (4):** R3-05 `duplicateGroupsAsync` off-main twin (Cleanup tab); R3-06 off-main
  `@Observable` writes (`version`/`lastError`) serialized on main + lock-backed `lastError` shadow;
  R3-07 PART A oversized-frame message made actionable; R3-08 serial AsyncStream event pump so
  `handleEvent` runs in receipt order. Built via `swift build --product FileID`.

**Deferred (see NEXT.md):** R3-15 (a "different people" verdict is lost after a re-scan churns
face_print ids — needs a churn-stable face identity → a NEW append-only migration mirrored
identically on BOTH engines, the C12 fork-bug class, plus Windows-runtime verification; not landed
blind). R3-07 PART B (32→64 MiB inbound cap + O(n²) buffer-scan fix + cross-platform constant
mirror — IPC-contract change, land coordinated).

Method note: the 3-lens skeptic pass over-passes (~40% historical FP), so the per-finding
domain-expert recipe pass is the load-bearing filter; every landed fix was read against the real
code before applying. See DECISIONS.md.

## 2026-06-14 (earlier) — deep-audit batch: 8 confirmed defects fixed across both engines (PRs #23–#24 merged; main green) + full on-hardware e2e

Ran an adversarial multi-agent audit (finder shards → 2-of-3 skeptic verification) over the
highest-risk subsystems, then **expert-re-verified every candidate before fixing** — the skeptic pass
alone still carried a ~40% false-positive rate. Net: **8 genuinely-real defects fixed, 6
false/over-stated rejected with rationale.**

**Windows engine (PR #23, CI-verified — `#[cfg(windows)]` paths don't compile under macOS clippy):**
- **SEC-5 casing bypass** (`restructure_apply.rs`): the reparse-point-in-chain walk compared
  paths with case-sensitive `starts_with` after only one side was normalized, so a drive whose
  casing differed from the root could slip the containment break. Replaced with a component-wise
  case-insensitive `ci_starts_with` (avoids the sibling-prefix bug a lowercased-string compare would
  reintroduce).
- **Verdict swallow** (`bulk.rs handle_find_merge_suggestions`): a `face_verifications` query whose
  error was `.ok()`-dropped could silently skip the "these two are different people" exclusions and
  re-suggest a rejected merge. Now propagates with `?`.
- **`cancelled` flag** (`deep_analyze.rs`): four non-cancel error exits hard-coded `cancelled: true`,
  mislabeling a model-load / DB-query failure as a user cancel. Now reports the real
  `cancel.load(Relaxed)`.

**macOS engine (PR #24, `swift build` debug+release clean, `swift test` 194/194):**
- **F-A2 heal size corroboration** (`DBWriter.healMovedRow`): rename/move heal matched on `file_ref`
  (st_ino) alone; st_ino has no generation number, so a reused inode could re-bind a deleted row onto
  an unrelated new file and hand it the prior file's tags / named person / OCR. Candidate now also
  requires `size_bytes` match. New regression test (reused inode + different size → fresh row,
  inherits nothing).
- **F-A4 restructure conflicts array**: `apply()` never populated `conflicts` despite the `(2)`
  uniquify path. Now reports each uniquified planned dest; existing test updated.
- **F-A5 restructure mkdir error swallowed**: silent `failed += 1` on `createDirectory` failure →
  added a redacted `restructure_mkdir_failed` warning.
- **F-A6 Deep Analyze empty VLM output**: empty generation → `description=""` →
  `COALESCE(?, vlm_description)` overwrote a prior good caption. Now surfaced as `Inference failed:`
  so the runner's `isFailure` skips the destructive write.
- **F-A7 VisionWorkerPool continuation leak**: a task cancelled while parked in `acquire()` leaked
  its continuation (`withCheckedContinuation` ignores cancellation). Now wrapped in
  `withTaskCancellationHandler`; `acquire()`→Optional, `with()`→`T?`, caller breaks on nil.

**Rejected (verified false/over-stated):** #1 file_ref COALESCE (current behavior correct; the "fix"
would leave a stale ref), #8 ReadStore TOCTOU (read-only double-open, low impact), #9 semantic
containment (PathBuf `.starts_with` is component-aware), #10 empty VLM tags (DELETE is inside
`if !tags.is_empty()`), #12 int width (face counts ≪ INT64_MAX), #13 mtime 1s tolerance (deliberate
FS-precision accommodation).

**On-hardware e2e (isolated /tmp copy of 40 Adlon photos, throwaway HOME, real models symlinked
read-only — corpus never modified):** scan 40/40 (0 failed) → face clustering 87 faces/41 persons →
semantic plan 40 moves → **apply 40/40 applied, 0 failed** → DB consistency check: 40 rows = 40 files
on disk, **0 missing, 0 orphans** (path_text refreshed correctly). The 41-from-87 person
fragmentation is the documented F-4 single-linkage limitation (blocked on labeled data), not a
regression.

## 2026-06-14 (later) — backlog cleared to terminal states (PRs #18–#20 merged; main green)

Continued the finish-everything push after PR #16. Five more PRs landed + merged, all CI-green:
- **#18 — macOS concurrency + flaky-CI fix.** `ReadStore` counters worker uses scoped `withLock`
  (Swift-6 lock-in-async gone); R-07 dedicated shutdown mirror so clustering aborts a fresh shutdown;
  AND the real fix for the intermittent CI hang the restored gate exposed — six tests read an IPCSink
  pipe with a BLOCKING `availableData` on the cooperative pool (parallel runs starved the executor and
  wedged the harness to the 12-min SIGALRM). Replaced with a shared non-blocking `WireCapture` (GCD
  readability handler + lock buffer) and added `swift test --no-parallel` as defense-in-depth. The
  process-spawning `ScanCancellationTests` is skipped on the GitHub runner (local + unit coverage kept).
- **#19 — restructure tiers + honest apply bar.** Engine `RestructurePlan` now populates per-move
  `tier` + `folderClassifications` (engine-authoritative Tidy/Keep); the vestigial two-step
  "shortcuts → convert" apply bar (both buttons did the same real-move confirmation; "reversible" copy
  mislabeled an irreversible move) collapsed to one "Apply moves" button.
- **#20 — Windows `hardwareReprobed`** reports the memoized `active_provider()` (actually-bound EP),
  not a fresh probe, so a post-install "Verify" shows "pack ✓ — restart to use it".

**Net:** every code-actionable backlog item is landed (this session also: containment fix, both-platform
apply-cancel, test-gate integrity, Deep Analyze verification). What remains in NEXT.md is exclusively
blocked on a specific resource (hand-labeled face data for F-4; the RTX 2060 for per-vendor GPU; the
owner's Mac/GUI for semantic search + Tidy/Keep render + `walkStreaming` UX) or deliberately deferred
because a blind change would regress a working path (R-11, in-process cancel test, the intentional
face-embedding backfill cap). `main` is the only branch; all GitHub workflows green.

## 2026-06-14 — Xcode-unblocked: Deep Analyze verified, cross-platform apply-cancel fixed, macOS test gate restored (PR #16 merged)

Xcode 26.5 was installed on the dev Mac, lifting the "no `swift test` / no Metal" ceiling. That
immediately surfaced and fixed three real defects and verified the last blocked feature.

**Bugs found + fixed (PR #16, both platforms, all CI-green):**
- **Restructure apply was uncancellable on BOTH platforms.** The F-C6-013 cooperative cancel loop
  existed but neither dispatcher ever set the flag — Windows built a fresh never-set `AtomicBool`
  (`with_cancel` was test-only); macOS ran the apply in a discarded `Task.detached` while `cancelScan`
  used a different mechanism (`ScanCoordinator`). A long apply on a large library could not be stopped.
  Wired `CancelScan`/`cancelScan` → the apply on both sides, with no stale-cancel (fresh apply = fresh
  signal). Deterministic tests added (`apply_dispatch_honors_preset_cancel`,
  `requestCancelCancelsRestructureTask`).
- **The macOS `EngineTests` target hadn't compiled since campaign commit `976a248`** — `Database`
  ambiguous (GRDB vs FileIDEngine) in 11 files, a stale `dbModified:` label, a Swift-6 capture, plus
  never-run tests with latent setup bugs (a `/private` vs `realpath` root mismatch, the R-14 CLIP
  carve-out, and an impossible `cache_spill == 0` assertion). Repaired → **192 tests, 48 suites, green**
  — the first time the macOS suite has actually run.
- **The CI `swift test` step never failed the job.** `if ! cmd; then status=$?` captured the negated
  condition's status (always 0), so a failing OR non-compiling suite reported exit 0 — the macOS test
  gate had silently never failed. Fixed to capture the real exit code. (This immediately caught a
  pre-existing CI-only hang in the process-spawning `ScanCancellationTests` — a leaked engine child
  wedging the harness; after collector/watchdog/stdout-drain hardening it's skipped on the GitHub
  runner and runs on every local `swift test`. Tracked in NEXT for an in-process rewrite.)

**Deep Analyze (MLX VLM) verified end-to-end on-hardware** (the last previously-blocked feature):
placed the cached `mlx.metallib` next to the engine, downloaded Qwen3-VL 4B (3.5 GB). `deepAnalyzeFile`
produced an accurate caption ("A smiling boy in a white Nike shirt hugs a happy, fluffy dog…") + a
smart-rename (`boy_hugging_dog_wooden_background`) in 8.5 s; `deepAnalyzeFolder` captioned 3/3 images;
a corrupt non-image stub was gracefully skipped (processed=0, failed=0, no crash). Caption quality is
genuinely good.

## 2026-06-13 (latest) — on-hardware write-path + perf verification loop; 1 bug found+fixed (PR #13 merged)

Drove the **real release engine** against isolated throwaway sandboxes (`HOME`/`CFFIXED_USER_HOME`
override → disposable DB; synthetic `/tmp` libraries) and the read-only Adlon/TrueNAS corpus, to
exercise every path the earlier campaign couldn't on hardware. No corpus data or the user's library DB
was ever modified.

**Bug found + fixed (merged to main, PR #13, commit `b927031`):** the restructure-apply SEC-7
containment guard (`pathIsContained`) rejected **valid in-root moves** when the library root resolved
through macOS's `/private` shortening. `resolvingSymlinksInPath()` strips `/private` only when the path
EXISTS, so the (existing) root canonicalized to `/tmp/…` while a not-yet-created destination parent
stayed `/private/tmp/…`, breaking the prefix check — every move failed as "escapes_root". Latent in
production (real libraries live under `/Users/…`, never `/private`) but a real hole in a security guard.
Fixed by resolving symlinks against the deepest EXISTING ancestor then re-appending the literal tail;
the escape vector stays closed (true-escape + existing-symlink-escape both still rejected, unit cover
added in `PathContainmentTests`). After the fix the isolated apply run reports `applied=2 failed=2`:
both valid moves land (D-7 → `shared (2).jpg`), `path_text`+`path_hash` refreshed, the stale-plan move
and the `../` escape both fail with files untouched.

**Verified working on-hardware (no change needed):**
- **Restructure apply** — the one write-path never exercised on hardware: D-7 auto-rename, B4
  stale-plan guard, SEC-7 escape rejection, `path_hash` refresh — all confirmed end-to-end.
- **Incremental skip-set** — rescan of the already-scanned 9,745-file Bernadine/CD processed **0 files
  in 2.95 s** (vs 156 s full): ~53× rescan speedup, the unchanged files skipped at discovery.
- **Rename/move heal (F-2)** — rename `foo.jpg→bar.jpg` then rescan: same row id re-bound to the new
  path, attached tag preserved, no duplicate row, `rename_heal` logged.
- **Garbage-file robustness** — the 10 perpetual `image_decode_failed` files on Bernadine/CD are not
  JPEGs at all (UTF-16LE `\\?\C:\Users\…` Windows path stubs / cloud placeholders with `.JPG`
  extensions); the engine fails them gracefully and continues, no crash. Source-data artifact, not a bug.
- **Graceful mid-scan cancel** — cancel during tagging returns a prompt terminal `scanComplete` (178
  files persisted, 0 failed), no crash, no hang.
- **People pipeline / face-embedding backfill** — ran clustering once on the real DB: ArcFace/SFace
  embeddings went `533 → 5533` (exactly +5000 = `maxExtractionsPerRun`), clustered into 947 persons,
  `unmatchedFaces=0`, 52.6 s, no crash. Confirms the lazy backfill (`extractPendingPrints`) embeds a
  fresh window per run and clustering is stable. The DB has 46,079 detected faces (all with Vision
  `print_data`); embedding is the same incremental-per-run backfill as CLIP, so a large library needs
  several clustering passes to fully populate People — by design (`maxFacesPerRun`/`maxExtractionsPerRun`
  caps), documented as a UX item in NEXT, not a bug.
- **DB health** — `PRAGMA integrity_check` = ok and `foreign_key_check` empty after all the scans,
  clustering, apply, and heal operations (59,998 files, 50,518 phashes, 947 persons). **phash/dedup
  foundation** confirmed: byte-identical images get identical phash, a different image a different one.

**Perf assessment (M1 Pro):** decode is already capped at 512 px (`CGImageSourceCreateThumbnailAtIndex`,
EXIF from the same source); CLIP + faces run on the CoreML EP; first-scan CPU is genuinely maxed
(~875 %/1000 %) doing real pipeline work across 14 workers — near-optimal for this tier. The remaining
levers (faces ANE vs CPU+GPU compute-units, model size) are correctness-sensitive and stay ML-UAT
items, not blind flips. Adaptive scaling (PR #13) gives bigger machines headroom without touching the
M1 baseline.

## 2026-06-13 (later) — macOS adaptive hardware scaling (branch `perf/adaptive-scaling-2026-06-13`, commit `f80d768`)

The macOS engine was using M1-Pro-shaped constants for the two stages that should
scale with the machine. Now they're hardware-derived, with the M1 Pro tier left
byte-identical so the verified baseline is untouched:
- `VisionWorker.visionConcurrencyGate`: hardcoded `14` → `Hardware.workerCap`
  (M1 Pro 14; M-Ultra 32) so the Vision/ANE stage scales with cores instead of
  silently feeding a bigger ANE only 14-wide.
- `Hardware.MemoryTier` (low `<12`/balanced `12–48`/high `≥48` GB) mirroring the
  Windows `memory_tier`, driving `DBWriter.maxBatchFiles` (64/100/500). M1 16 GB →
  balanced → 100 (the proven value, unchanged).

On the dev M1 Pro all three reduce to the prior constants — the box is already
CPU-maxed (~875% of 1000% avg during a scan), so the headroom only manifests on
bigger silicon and is **unmeasured here**. Per-chip tuning recipe (gate, ANE
semaphore, worker-cap storage-bound caveat, batch/RSS) is in NEXT.md
(`2026-06-13 (later)`). `swift build` debug + release clean. Runtime memory-pressure
adaptation (F-3) is still spec-only; these tiers are static at startup.

## 2026-06-13 — "audit-2026-06-10" perfection campaign: 131 findings + 2 criticals fixed, 22 self-introduced regressions caught, clean on-hardware macOS scan (branch `fix/audit-2026-06-10`)

The deepest adversarial pass yet — run cross-platform off `main`, closed on this branch.

**Method.** Three adversarial audit workflows (WF-1 unit-correctness, WF-2 cross-platform parity,
WF-3 perf/memory-adaptive) → triage → **7 fix waves** → a **delta re-audit loop (2 rounds)** over
the campaign's own diff. **252 raw findings → 131 fixable + 15 rejected** (each rejection a
deliberate guard or a cited prior ruling). Full record in `shared/docs/audit-2026-06-10/`
(`findings.json`, `TRIAGE.md`, `reaudit-confirmed.json`).

**What landed, by wave:**
- **Wave R — 30 Rust-local engine fixes** (the FIX-LOCAL set, clippy/test-verifiable here):
  EP-guard breadcrumb clear + revision-keyed install sentinel, terminal phase/IPC events never
  droppable + outbound frame cap, trash restore-conflict + batch enumeration + recovery sidecar,
  anchor-strip exempting the semantic butler's moves, Deep Analyze CPU-EP honor / target scope /
  skip semantics, SleepGuard same-thread release, redaction home-username mask, COM apartment
  scoping, pptx member cap, ranged-resume progress, watchdog PID-reuse, parallelized discovery
  syscalls.
- **C2 — IPC contract parity, schema-first.** Every change landed in `ipc.schema.json` first, then
  mirrored across all four DTO targets (Rust → C# → Swift): optional `deepAnalyzeAll` fields,
  `cancelPrewarm.modelKind`, canonical error-kind vocabulary, Windows DA eta/path,
  `discoveryComplete` backstop, v16 `path_search` NFC symmetry.
- **C3 — 44 macOS-engine fixes**, including a **CRITICAL gate-trio**: an ungated `DELETE` meant a
  Vision/OCR/face **timeout silently wiped a file's tags / `person_id` / OCR** (now every
  destructive re-write is gated on the stage having actually run). Also the **face-clustering
  data-loss + determinism cluster** (port of the Windows S0 snapshot-under-lock, fixed-seed HNSW,
  `is_unknown`/verdict guards, union-find de-chaining) and the **butler restructure ENGINE port**
  (path_hash, B4 stale-plan guard, uniquify, sanitize, tiering, Windows-canonical naming).
- **C4 — 21 macOS-app fixes**, including a **CRITICAL dual-writer**: "Restart Engine" spawned a new
  engine without reaping the old one → two writers on one DB (now terminate-and-reap before spawn).
  Plus restructure single-flight apply, off-main merge/search, read-conn UAF guard, Deep Analyze
  staleness, model-download progress reset.
- **C5 — 12 Windows C# fixes**: read-conn drain dispose, single-flight apply, bulk-tag confirm+undo,
  selection/rename identity, Sankey single-parse, log-lock, CUDA toggle.
- **C6 — perf**: macOS discovery-time incremental skip-set (activated), decoupled DB commit, cached
  statements, streamed (bounded-memory) semantic search, Vision autoreleasepool, cancellable +
  progress-emitting restructure apply (both platforms).
- **Feature wave** — **F-2** macOS rename-heal (moved/renamed files keep tags/faces/OCR via APFS
  inode `file_ref`, old-path-gone-gated) and **F-C3-021-app** (route the macOS Restructure tab
  through the engine butler, retire the app-side classifier).

**The delta re-audit earned its keep.** Round 1 found + fixed **22 self-introduced regressions**
(`reaudit-confirmed.json`, R-01..R-22), incl. 2 HIGH: face clustering became a **silent no-op after
any scan-cancel** (a sticky scan-scoped cancel mirror gated the standalone cluster job), and the new
discovery **skip-set defeated the orphan sweep** (skipped files retained a stale `scanned_at`,
saturating the 5000-row prune-candidate cap → real orphans never pruned). Round 2 of the re-audit
loop is in progress.

**Local gates at HEAD:** `cargo clippy --all-targets -D warnings` clean · `cargo test` **336
passing** (was 292 baseline; **+44 regression tests**) · `swift build` debug + release clean (no
Xcode locally → `swift test` is CI-only). **CI green on all three workflows** (`macos.yml`,
`windows-engine.yml`, `windows-app.yml`) on the branch through the C6 batch.

**On-hardware macOS verification** (owner's external drive `Adlon`, `/Volumes/Adlon/TrueNAS`,
**62,746 files, READ-ONLY** — no permanent changes to corpus data):
- Full scan **clean**: 59,633 processed / 54 failed (**0.09 %**, all genuine "Could not decode image"
  on corrupt backup images), **~120 files/s** (Vision-only — no GPU models on this box), **peak RSS
  1,187 MB** (< 1.5 GB target), no crashes. Produced 307,666 tags + 26,678 files-with-faces.
- Incremental skip-set verified: rescan of a 311-file folder processed **0 files in 9.5 ms**.
- Restructure butler plan produced **13,131 moves** (photo/video/audio + GPS Places +
  `Documents/<year>`, Windows-canonical naming) with no crash.
- Mid-scan cancel terminated promptly with final status written, **no hang**.
- **Apply/move write-paths were NOT run on hardware** (would move corpus files) — covered by macOS
  unit tests + the proven Windows port. The on-Mac apply UAT is the remaining macOS gate (NEXT.md).

## 2026-06-10 — Production-readiness campaign closed: zero open findings, perf sweep, hardening live, T4/T5/T7 shipped (branch `fix/bug-audit-sweep`)

The full campaign that started with the 2026-06-09 sweep is **closed with zero open confirmed
findings** across every record. What landed on top of the entry below:

- **Merge reconciliation** — origin/main's 123-commit line (commercial-clean stack, its own
  audits) merged into the branch's 19-commit fix line; R0 merge-interaction review found + fixed
  7 semantic conflicts git couldn't see (`R0-findings.json`).
- **Deferred findings closed** — L1 (schema-conformance suites lock Rust + C# mirrors to
  `ipc.schema.json`; zero drift) and U4 (macOS IPC wire dup'ed off fd 2; MLX/Metal diagnostics
  go to `engine-stderr.log`, wire stays pure JSON — pinned by iterate.sh).
- **Security hardening live** — SHA256 manifest (`shared/models/manifest.json`, registry-locked
  both platforms, VLMs revision-pinned with sentinel), TLS CA-allowlist pinning (11 roots +
  rotation runbook in SECURITY.md), tokenizer DoS bounds. SECURITY.md rewritten to match.
- **macOS features** — T4 Finder-tag undo journal + tile tag dots, T5 bulk-rename perf hoist,
  T7 sign/notarize/DMG pipeline (`scripts/release.sh`; `--skip-notarize` dry run produces a
  92 MB hardened-runtime DMG that passes `codesign --verify` + DR check; real signing is
  owner-gated on the Developer ID cert).
- **Audit Sweep A** (R1–R6 fix-regression + N1–N3/N5–N8 depth lenses): 16 confirmed + 16 lows,
  all adversarially verified, all fixed — incl. the **v14 migration fork** (C12: chains
  re-unified, canonical 16-id list pinned by tests on both platforms), the C14 char-boundary
  truncate panic, NFC-insensitive search (v16_path_search), StablePathHash (SipHash-1-3, shared
  vectors), staging-orphan sweeps, byte-weighted predecode budget, video-thumb semaphore, and
  the L7 newer-DB downgrade guard (`db_newer_than_engine`, both engines).
- **Perf sweep** — 35 candidates from 6 lenses, 27 adversarially refuted, 6 landed (`b653d00`):
  EXIF read off the already-open CGImageSource (2–5 ms/image × 14–32 workers on NAS), COUNT(*)
  badge queries, single-pass restructure stats, arcface clone removal. 2 unproven candidates
  dropped on record (DECISIONS.md).
- **Closing Sweep B** (P1–P5 parity, N4 robustness, R7 delta, loop-until-dry): round 1 → 9
  confirmed fixed (deep-analyze/cluster duplicate-command parity, scanComplete-on-cancel,
  discoveryComplete, face_prints.excluded population, rankByCosine failed-filter, redaction
  parity ×2, sanitizer delegation, Apply default arm); round 2 → 7 fixed (cancelled-scan counts,
  **Windows queueState finally wired** — the SidebarQueueList was bound to an event the engine
  never emitted, command_decode_failed parity, /Volumes redaction both sides, PAR-111-mirror
  busy-bounce exemption on macOS); round 3 → **completely dry** (0 candidates, 22 verifiedClean).
  Record: `audit-2026-06-09-merge/sweep-b-findings.json`.

**CI epilogue:** the macOS workflow's first real execution of the C1 process suite (yesterday's
"green" run had silently never started it) caught one final engine bug — the shutdown IPC
command never exited the engine (`break` inside the command loop's switch broke the SWITCH;
stdin EOF was the only real exit, which the app's pipe-close masked). Fixed with a labeled
break (verified: 0.13 s exit on the shutdown frame with stdin held open), the C1 test harness
made un-hangable (a blocking waitUntilExit inside a task group had turned a slow exit into a
60-min CI hang), and macos.yml now caps `swift test` at 12 minutes with a survivor-process dump.

Local gates at HEAD: `swift build` clean 0 · `cargo clippy --all-targets -D warnings` 0 ·
`cargo test` 292 green · release dry-run DMG green · **CI green all three workflows** (macOS
92/92 incl. the C1 suite; Windows engine x64+arm64; Windows app). **Hardware UAT is the
remaining gate** — see NEXT.md for the checklist. "Zero known bugs" = every recorded finding
closed/accepted + gates green + UAT clean; the C#/.NET side and all runtime behavior verify in
CI/on-hardware only.

## 2026-06-09 — Full cross-platform bug-audit sweep (branch `fix/bug-audit-sweep`)

Ran a read-only multi-agent static audit across macOS (Swift), the Windows Rust engine, and the
Windows .NET app (18 scoped finders + adversarial verifiers): **88 raw → 73 confirmed** (2
critical, 13 high, 28 medium, 30 low) + 4 uncertain. Remediated **72 of 73 confirmed + 3 of 4
uncertain** on this branch (≈30 atomic commits); the lone deferral is the IPC ID-casing drift
(L1 — no runtime impact, needs a coordinated Windows-verified wire rename; see DECISIONS.md).

Highlights:
- **macOS (Swift)** — fixed: cancel/shutdown-during-scan **deadlock** (unbuffered AsyncChannel
  producer never cancelled); `INSERT OR REPLACE` rowid churn that cascade-deleted faces/embeddings/
  **manual person assignments** on every re-scan (now an id-preserving UPSERT + v12 FTS-sync
  triggers + change-detection skip); IPCSink progress-coalescing clobbering `scanComplete`;
  VisionWorker reused-VNRequest race; FTS5 MATCH injection-to-zero-results; person/FTS
  reconciliation on delete; rename apply/undo disk↔DB consistency; CLIPTextEncoder UI-thread
  freeze; download integrity (error propagation, size verify, no double-resume); +others.
- **Windows Rust** — fixed: restructure **data-loss** overwrite (now non-overwriting +
  disambiguation); pause→resume lost-wakeup **deadlock**; image-decode **OOM**; ArcFace **BGR→RGB**
  (cross-platform parity); VLM stderr-pipe **hang**; OCR-never-runs (uninit COM apartment);
  `planRestructure` dead SQL (illegal `GROUP_CONCAT(DISTINCT,sep)`); range-downloader permit
  **deadlock** + 416/stale-part recovery; per-download cancel registry; zip-bomb actual-bytes
  cap; ADS `:` rename guard; long-path moves; +others.
- **Windows .NET** — fixed: WinVerifyTrust egress/UI-block/handle-leak (cache-only revocation,
  off-thread); AppSettings split-brain (single canonical instance); OnProcessExited exit-code
  race; expected-exit latch; `Local\` single-instance (multi-user); install watchdog null
  dispatcher; path-redaction gaps (UNC/space/sibling-username); search debounce ODE; failed-file
  filter consistency; ReadStore leak; Sankey O(N²)→O(N) + debounce; People virtualized
  checkboxes; +others.

**Verification status:** macOS `swift build` (app + engine) is **green**; the swift-testing suite
can't run in this env (no Xcode — CommandLineTools only). Windows: the non-`cfg(windows)` Rust
**`cargo check` is green** (cfg(windows) code + .NET unverifiable on macOS). All Windows build/run
verification and the macOS UAT are pending on the user's hardware — see NEXT.md.

## 2026-06-04 (latest) — Six-workflow deep bug+perf audit: ~35 bugs fixed + 4 self-introduced regressions caught by re-audit (UNCOMMITTED on `main`)

Maximum-coverage adversarial sweep of the whole Windows app+engine off `main` (built on the prior uncommitted sweep). **Six serialized workflows** (concurrent fan-outs trip a server rate-limit → must serialize): (1) engine deep-correctness — 15 subsystems × 3-skeptic refute-by-default verify (18 confirmed); (2) app deep-correctness — 10 areas, UI-thread/async/lifecycle/leak lens (16 confirmed); (3) perf/memory/4 GB-target (7 confirmed); (4) security/data-integrity/concurrency (3 confirmed + the contested rechecks); (5) **re-audit of the fix diff** — caught 4 regressions MY fixes introduced; (6) focused re-audit of the regression repairs — clean. ~270 finder/verifier agents total. Per-finding record: `shared/docs/audit-2026-06-04c/` (engine/app/perf/sec + both re-audits + TRIAGE.md).

**~35 distinct fixes** (≈21 engine/perf/sec + ≈14 app), each batch re-greened. **Gates green:** engine `cargo clippy --all-targets -D` + `cargo test` (all pass; +3 new tests: HNSW determinism, anchor-strip ×2); app `dotnet build` 0/0 + App.Tests + IpcSchema.Tests + `dotnet format`. **NOT committed/pushed** (owner's call) — see `git diff` (39 files; this sweep + the prior uncommitted one).

**HIGH (data-loss / crash / hang):**
- **Face-clustering phase-3 DELETE+re-INSERT silently discarded People-tab edits** (rename/merge/mark-unknown) committed during its lock-free phase-2 window → permanent identity-edit loss. Fix: read the identity snapshot in phase 3 *under the persist lock* (not phase 1), so a concurrent edit is carried forward.
- **HNSW built with an entropy seed** → face clustering nondeterministic on >5k-face libraries (People identities/names hopped on every rescan). Fix: fixed `.seed()` + determinism test.
- **`EngineClient.ReadBoundedFrameAsync` O(n²)** (empirically 132 s for a 4 MiB frame) → multi-minute hang decoding a large `restructurePlan`. Fix: incremental scan offset + flat-chunk newline scan → O(n).
- **Restructure "Keep"/Anchor moves silently applied** despite the UI promising those folders stay untouched (Windows `classify()` always emits a canonical destination; macOS emits no proposals for anchor folders). Fix: engine drops Anchor-folder moves from the plan after counting (Keep tile count preserved) + tests.
- **Mid-scan GPU device-removal left image rows `failed=false`** → permanently stranded in the incremental skip-set. Fix: mark image/video failed when the GPU dies mid-ML.
- **`ModelInstallerService` raised PropertyChanged off the UI thread** (RPC_E_WRONG_THREAD class) → marshal via captured `_ui`.

**MED highlights:** GPU-death dropped Audio/Doc CPU tags (persist them); empty/rescan notice racily dropped on a <250 ms scan (single-shot guard + post-drain fallback); cancel couldn't interrupt an in-flight VLM request (select! on cancel); EP-variant chosen from override-blind `active_provider()` while BGE pins CPU (resolve for the bound EP); ep_guard `.ep_attempt` breadcrumb race (now an armed-EP *set*); WordPiece ASCII-only lowercasing → non-ASCII `[UNK]`; BGE pooling OOB panic on a malformed ONNX (bounds-validate); downloader http-downgrade redirect (https-only) + orphaned `.part` sweep; OCR missing COM init (silently produced nothing — it was the one shell module without `CoInitializeEx`); applyTags COM/sidecar writes + face-crop JPEG encode moved OFF the SQLite writer lock; Library refresh races (generation guard + in-flight counter); per-request thumbnail cancellation; ReadStore + 2 SqliteConnection leaks; FilePreview stale-nav guard; revertMerge wrong `file_count`.

**The re-audit earned its keep — caught 4 regressions in my own fixes (all repaired, gates re-green):** the async `DebugLog` sink lost the last <200 ms of forensic lines on a native fast-fail → **reverted to synchronous** (durability is load-bearing per CLAUDE.md; the perf opt needs a durable-async design); ep_guard's "first-arm-wins" breadcrumb recorded the WRONG EP under heterogeneous concurrent binds → **armed-EP-set breadcrumb** (disables every in-flight guarded EP on a stale crumb — over-disabling is recoverable, a crash-loop is not); the FilePreview stale-nav guard fell through to an unguarded `ShowPlaceholder` that clobbered the current sibling → guard the fall-through; the smart-rename pill reset was undone by a stale `DeepAnalyzeLast` → clear it on run-start.

**Deferred (real, documented, out of this pass — see NEXT.md / TRIAGE.md):** VLM server-death mid-batch CLI fallback; CLIP-tokenizer punctuation (ML-quality A/B); long-path trash manifest (build change); wipe-vs-bulk-handler interlock (benign, deadlock-risk); applyRestructure outbound chunking (narrow, file-move-path risk); AppSettings lost-update (settings-refactor); Sankey "Other" drill-down; startup-auth-on-UI-thread (contested); rename-heal `UPDATE OR REPLACE` FTS desync (narrow, FTS-schema risk). On-hardware verification (RTX 2060 / 4 GB DirectML) remains the gate for the runtime/GPU/COM paths.

## 2026-06-04 (later) — Five-workflow bug-audit sweep: 11 bugs fixed + 1 fix-introduced hang caught by the re-audit (UNCOMMITTED on `main`)

Exhaustive adversarial audit of the whole Windows app off `main`: four parallel find→refute-by-default→verify workflows (engine safety/concurrency/DB · app UI-thread/async/lifecycle/IPC · IPC-contract+perf · recent-diff regression), then a fifth refute-by-default RE-AUDIT + completeness-critic over the fix diff (~50 finder/verifier agents; the workflows had to be **serialized** — 4 concurrent tripped a server rate-limit that aborted every finder mid-task). **11 distinct confirmed bugs fixed; the re-audit caught a hang one of the fixes introduced (fixed + regression-tested).** Gate re-green: engine clippy `-D` + **267 tests** (+1 E4 test) + fmt; app build 0/0 + **131 App.Tests** + **38 IpcSchema.Tests** + format. **NOT committed/pushed** (owner's call) — 10 files (4 app + 6 engine). Per-finding record: `shared/docs/audit-2026-06-04b/`.

**HIGH (app, crash/hang):** People + Cleanup `RefreshAsync` raised `IsLoading`/`ErrorMessage` from the `ConfigureAwait(false)` thread-pool continuation → x:Bind drove `ProgressRing.IsActive`/`StatusText` off the UI thread → RPC_E_WRONG_THREAD native fast-fail on every People/Cleanup refresh (the V15.x DispatcherObject class) → `OnUi()` marshal mirroring LibraryViewModel. `EngineClient.OnProcessExited` could tear down a freshly-respawned engine when the OLD process's queued `Exited` ran after `StartAsync` reinstalled the fields (RestartAsync race) → `sender != _process` stale-exit guard. `restructurePlan` > 1 MiB (~3.5k moves) was silently dropped by the C# read-frame cap → empty Restructure tab on a large library → cap 1→32 MiB + a visible `ipc_frame_too_large` error on any oversize drop.

**MED:** `wipeLibrary` didn't interlock against the now-lock-free face-clustering PHASE-2 → a wipe could be followed by the persist re-inserting phantom `persons` (ghost People cards after a "wipe") → wipe waits on `face_cluster_active`. The new `face_clustering_busy` kind collided with the app's `Contains("cluster")` gate-release → wrongly cleared the auto-cluster single-flight on a busy bounce → exact-match `== "face_clustering_failed"`. SEC-5 junction-TOCTOU (`has_reparse_point_in_chain`) compared a raw parent vs a verbatim `\\?\` root → the ancestor walk broke after the leaf → normalize both via `strip_extended_length` (NOT canonicalize, which follows the junction).

**LOW:** single-file Deep Analyze reported a genuine failure as `cancelled:true` (suppressed the warning) → derive from the cancel flag; restructure new-group folders deduped on the pre-sanitized name → dedup on the sanitized name; merge-suggestions sheet flashed "No likely merges" over "Looking…" → drop the null-reset; BGE text encoder ran single-threaded on CPU on a GPU box (CPU-pinned but inherited the GPU EP's intra=1) → force CPU `p_cores`.

**Re-audit catch (the point of the fifth workflow):** the E4 sanitized-dedup `while` loop could spin forever when a group base name sanitized to ≥~200 chars (every `"{base} {n}"` truncates to the same string) — a NEW hang the fix introduced, invisible to clippy/tests. Fixed by reserving suffix room so each candidate is distinct + bounded; added the `sanitization_colliding_group_names_get_distinct_folders` regression test. The two app-thread HIGHs and the wipe race are the headline user-facing wins.

## 2026-06-04 — Suggested-merges hang fix + over-split tuning + exhaustive perf audit (branch win-face-fix-perf)

Built on `origin/main` (PR #10). Two bodies of work, headless-green, ready to merge to `main` (the push is the owner's).

**Face — suggested-merges hang + over-split (implements `shared/docs/PLAN-suggested-faces-fix.md`).** The People → Suggested-merges sheet hung for minutes because the engine's single `Arc<Mutex<Connection>>` serialized the read-only suggestion query behind the multi-minute clustering write-lock. Fixes: `db::open_read()` (ephemeral `SQLITE_OPEN_READ_ONLY` conn; `handle_find_merge_suggestions` opens its own read conn instead of `db.lock()`); `handle_run_face_clustering` restructured into load (lock) → `cluster()`+`consolidate()` (LOCK-FREE) → persist (re-lock), so the writer mutex is free during the multi-second compute; an engine-side single-flight guard (`face_cluster_active`) bounces a concurrent run; app `WaitForMergeSuggestionsAsync(30s)` with an actionable timeout; auto-cluster dropped on user-Cancel. Over-split: `AUTOMERGE_COS_DEFAULT` 0.85→0.75 (Balanced, env-overridable), the 12k-cluster consolidate no-op replaced by an HNSW centroid neighbor search (cap lifted, brute-parity test), Pass-3 floors exposed as `FILEID_FACE_PASS3_*` env knobs.

**Perf — exhaustive audit for the 4 GB / low-mem target.** 15-finder read-only audit over the whole Windows tree → refute-by-default verify → synthesize (33 confirmed / 7 refuted; the C# list-virtualization + LavaLamp/Win2D dimensions fully refuted — no waste). 21 safe (headless-verified) + 9 hardware-sensitive (applied conservatively, GATED so the 6 GB RTX 2060 reference box is byte-identical) findings applied. Safe highlights: borrowed-view RGB resize drops a full-frame clone per image on the primary RAM++ tagger + CLIP; three SQLite reads moved off the UI thread (fake-async M.D.Sqlite); SemanticSearch top-K lazy materialization; prepared-statement + VRAM/EP-probe caching; HNSW query-buffer reuse; shared thumbnail-cache key; IpcCoder span decode; query-embedding LRU. Hardware-sensitive (pending on-hardware confirmation): memory_tier wired into worker_count/pool/predecode (Low-tier only), VRAM-probe-None fails safe to pool=1, vision semaphore vision_cap=1 only at pool=1, BGE pinned to CPU EP, downloader streaming concat.

**Gates:** engine `cargo clippy --all-targets -D warnings` + 266 tests; app `dotnet build` (WinUI) 0 warnings + 131 App.Tests + 38 IpcSchema.Tests + `dotnet format`. Commits `d7b0159f` (face) + `c07f93e8` (perf). The obsolete local `windows-v16.22-v16.26` branch (RAM++ ONNX + drop Qwen-3B) is superseded by origin/main and dropped (see DECISIONS). On-hardware verification of the hardware-sensitive perf knobs + the 0.75 automerge default remains the owner's gate.

## 2026-06-04 — Face scanning "totally broken" root-caused + fixed (3-workflow audit → gap-verify → re-audit)

On-hardware report (RTX 2060): face scanning totally broken, "WAY too many similar faces",
suggested-merges too slow, and a `clip_text` install-stall toast. Three adversarial workflows: a full
face-pipeline audit (8 finders → refute-by-default verify → completeness critic; 34 findings, 17
confirmed, 2 blockers), a gap-verify of the 8 critic suspects (7 refuted — incl. an EMPIRICAL load of
the on-disk YuNet ONNX proving its 12 output names match `yunet.rs` and the decode math is OpenCV-exact,
so faces detect/embed/cluster correctly), and a re-audit of the fix diff (16 findings → 3 confirmed →
all fixed). **ROOT CAUSE (blocker): `scan.rs` hard-gated EVERY scan on the `clip_text` sentinel, but
`clip_text` (the CLIP *text* encoder) is query-time-only and never used by the scan/face chain — so the
user's stalled `clip_text` install (the toast) aborted ALL scanning with `models_not_installed` → zero
faces.** Removed it from the gate (`[mobileclip_s2, arcface]` only).

**Engine fixes:** clip_text gate (above); ABORT the scan when a pre-flight-required model passed its
sentinel but failed to LOAD (was warn-only → a corrupt/AV-quarantined model stamped every file
scanned-but-faceless and the timestamp-only incremental skip-set then stranded them forever); on a
mid-scan GPU TDR mark only image/video rows `failed=true` so they retry (docs already CPU-processed stay
visible); new verification-aware centroid auto-merge `consolidate()` (default 0.85, env
`FILEID_FACE_AUTOMERGE_COS`, `=1.0` disables) folding over-split duplicate clusters — blocked by BOTH
"different people" verdicts AND differing user names (stable across re-scan); merge-suggestion band
retuned `0.32..0.66 → 0.55..0.97` (drops impostor noise, surfaces stranded same-person fragments);
suggestion sweep releases the writer lock before its O(P²) compute; YuNet output-name contract checked
at load (loud fail vs silent zero-faces); orphaned face-crop JPEGs pruned post-commit on re-scan;
downloader `read_timeout` 120→60s so a stalled install self-heals before the alarm.

**App fixes:** install stall-guard now latches THIS kind's terminal (`Fraction >= 1.0`) via a
PropertyChanged subscription — fixes the false "clip_text stopped responding" toast under Install-All
(the shared progress slot was overwritten by other concurrent downloads); `PrewarmNoProgressTimeout`
90→120s; auto-clustering also fires on Failed/Cancelled scans (faces persisted before a non-Complete
terminal now surface); People grid hides `is_unknown` clusters (matches macOS; makes "mark as unknown"
actually prune); People Re-cluster awaits engine readiness + logs aborts (was a silent no-op).

Headless-green: engine clippy `-D` + **264 tests**; app build 0/0 + **App.Tests 108** + **IpcSchema.Tests
34** + format. Branch `win-face-cluster-merge-perf-2026-06-03`. Clustering thresholds + the 0.85
auto-merge need on-hardware calibration on the labeled `G:\TrueNAS` library (over-split philosophy
unchanged; auto-merge is conservative + env-disable). Deferred items (RAW decode, rotated video,
consolidate 12k cap, suggestions HNSW, content-keyed verifications) in NEXT.md.

## 2026-06-03 — Full-repo Windows bug audit (4 workflows) + production-hardening fix pass

Exhaustive adversarial audit of the whole Windows app (Rust engine + WinUI) via four
find→refute-by-default workflows: **78 confirmed bugs** (~70 distinct; verifiers rejected ~40 false
positives). Full inventory + per-item file:line + fix-status in [`AUDIT-2026-06-03.md`](AUDIT-2026-06-03.md).
Fixed the high-confidence set, THEN drove a fix-all workflow (8 file-disjoint cells) + a hand-built
IPC-contract change to close EVERY remaining deferred item, THEN ran a 3-pass refute-by-default
RE-AUDIT loop (5 → 7 → 1 confirmed) that caught 13 fix-introduced regressions — incl. a
`tags_evaluated` decode-failure/online-only gap, an off-UI-thread `IsLoading` write in Find-Similar, a
masked orphaned-test break, and a cancel that wedged the install slot — all fixed. ~70 distinct bugs
addressed. Headless-gate-green: engine clippy `-D` + fmt + **258 tests**; app build 0/0 + **App.Tests
108** + **IpcSchema.Tests 34** + format. Branch `win-prod-hardening-2026-06-03`, NOT yet merged —
review the branch. The user's flicker report is fully diagnosed + fixed (see below).

**HIGH fixed (engine):** face-clustering wiped every user-assigned name on every scan (snapshot +
member-majority re-attach); timeout/GPU-dead row wiped a file's auto-tags (added `tags_evaluated`
gate, mirroring faces/OCR/doc); restructure move/symlink missing `\\?\` long-path prefix; VLM CLI
stderr piped-not-drained deadlock; `cpu` EP override silently ignored (TDR-recovery escape);
`file_ref` cross-volume MFT collision collapsed two files into one row (heal now requires
old-path-gone for ALL matches); CLIP-text query bound a GPU EP outside the ep_guard window
(crash-loop); wipe-during-scan interleave (engine cancels+waits before truncate).
**HIGH fixed (app):** Library search wrote XAML off the UI thread (fast-fail) → `OnUi` marshal; Deep
Analyze stale `Complete` fought the live UI at 4 Hz on 2nd+ run → cleared on Starting + scan start;
`ModelInstaller.Reset` omitted RamPlus/Accelerator → stuck-spinner.
**MEDIUM/perf fixed:** downloader 200-vs-206 resume corruption + corrupt-part cleanup; rename-heal
LIMIT-1 orphan; pipeline strip blanked-to-grey on completion + 10 Hz redundant redraw + filled-dot
stroke; `ReadStore.RecentAsync` missing `failed=0`; WinVerifyTrust state-handle leak per spawn;
FilePreview rename silent-failure; HNSW per-query O(n²) scratch re-alloc (reusable `Searcher`);
brute-force kNN full-sort → bounded top-k; ram_plus empty-suppress alloc; bounded_read buffer reuse;
heic decode-cap; per-EP `ep_guard` reenable; cancel-flush.

**All previously-deferred items now FIXED** this pass: LibraryView trash false-success (await result,
remove only Ok tiles); Cleanup/People `MergeById` identity-stable merge (kills the ~1 Hz rebuild
flicker + preserves keeper/selection) + People select-mode; TreeDiff ItemTemplate; Sankey
debounce/flow-matrix/touch; ShimmerView/LavaLamp lifecycle + occlusion + live ReducedMotion; per-model
prewarm cancel (engine static registry + schema `modelKind` + C# wiring + slot reset-on-cancel) +
cancel-as-failure + progress-order; ORT_DYLIB_PATH override-aware pin + CPU-override thread-count +
rename no-clobber MoveFileExW; composite `(kind,scanned_at)` index (v14) + `created_at` capture;
schema-drift (`skippedStages`/`currentCaption`/`modelKind`); RuntimeProbe memoize + input-name cache +
Pass-2 centroid + ThumbnailDiskCache cap + watchdog + path-redaction + completed-count. **On-hardware
confirmation still wanted** for the visual flicker fixes (RTX 2060 build-and-look) + GPU/EP paths
(real NVIDIA/Intel box); engine is fully headless-verified. The ~6.5 f/s GPU ceiling is unchanged
(perf fixes target clustering/query, not the RAM++ tagger). **Note:** `FileID.IpcSchema.Tests` is NOT
in `FileID.sln` (a known gotcha that masked a test break this pass) — recommend adding it to CI.

## 2026-06-02 (later 7) — User-reported GPU-pack bugs + 18-bug sweep (PR #8)

Fixed two user-reported Windows bugs + an adversarial-hunt sweep, via a diagnose→hunt→fix workflow chain (38-agent read-only diagnose/hunt → 8-cell file-disjoint fix + 3 verifiers). All headless-gate-green (engine clippy -D + fmt + tests; app build 0/0 + format + 108 tests). Merged to main (PR #8, 420a5ce), all 5 CI jobs green.

- **GPU acceleration pack now installs ONLY on user action** (`CudaAutoInstaller.cs`): removed the NVIDIA auto-install on engine-Ready (the `TryInstallOrtCudaPack` + auto `PrewarmModelAsync(llama_runtime_cuda_x64)`); kept GPU detection so the Accelerator slot still shows status. Installs only via WelcomeSheet GPU button / Settings / Install-all. **OPEN PRODUCT DECISION:** the Intel/OpenVINO auto-install (`TryInstallOpenVinoPack`) was left intact — Intel has no explicit install button, so gating it would orphan Intel's only path. Decide: leave it, or gate it + add an Intel install entry point.
- **Download flicker fixed** (`ModelSlot.cs` + WelcomeSheet/SettingsView bindings): the GPU pack runs two sequential sub-installs into one slot, rewinding `Fraction` 1.0→~0 at the boundary → the bar jumped backward + `IsIndeterminate` re-flapped (marquee↔fill). Now publishes a MONOTONIC `Fraction` (`Math.Max` while Downloading) + sticky `HasStarted`; `IsStarting`/`ShowRateEta` gate on `HasStarted` across all 5 WelcomeSheet + 3 SettingsView rows. Added a per-row in-flight re-entry guard (no duplicate Prewarm on double-click). **Visual needs the RTX 2060 to confirm** (one smooth non-rewinding bar; no auto-download on launch).
- **18-bug sweep** (refute-by-default verified): brush-churn/`Resources[]` (MainWindow/PeopleView/DrillDownSheet → ctor-cache + GetBrushSafe); IPC silent-failure/timeouts (RestoreFromTrash/DeepAnalyzeFile/Prewarm → bounded result-await); lifecycle guards (LibraryView _unloaded + ThumbnailService dispose; FilePreviewSheet post-unload; RestructureView static deselect-set reset); UndoStack batch-id parse guard (no IndexOutOfRange); engine `prewarm.rs` (aggregate parallel-download errors + clean partial on sentinel-write fail + log register_dll_dirs Err) + `scan.rs` (actionable model-load-timeout EngineError). Corrected stale CudaAutoInstaller comments in registry.rs/main.rs.

## 2026-06-02 (later 6) — Verified "what's-left" audit + Windows ship-hardening + RAM++ 256 closure + on-hardware

Answered "what's left for v1.0" with a refute-by-default audit workflow (5 cells vs current main) — it found the persistence docs overstate remaining work; **the sole hard external blocker is the EV cert.** Then landed the high-value doable-here code, ran the on-hardware test (authorized), and definitively closed the 256 question.

- **PR #6 ship-hardening → main (138760c, CI-green all 5 jobs):** image-decode cap (deep_analyze.rs 50 MP); **IPC capital-ID casing aligned Rust+C#+schema** (~25 fields, both round-trip suites pass — closes the long-standing eng-ipc casing drift); per-monitor DPI `WM_DPICHANGED` handler; WiX `RollbackBoundary` (Burn `<Chain>`); single-source version (`VERSION`+`Directory.Build.props`→csproj/WiX/Cargo + drift-guard, kills the 5 hardcoded `0.1.0`). Headless-gated first (engine clippy -D + fmt + 255 tests; app build 0/0 + format + tests).
- **`windows-app.yml`** gained the source-URL allowlist scan (app-only PRs were bypassing the engine workflow's scan).
- **RAM++ 384→256 perf lever — CLOSED as a dead end (definitive).** The prior export had completed (`out256/`, `[1,3,256,256]`); I fp16-converted it to 660 MB and A/B'd tag-F1 vs the 384 model on 60 corpus images with the engine-faithful pipeline = **0.76**, well below the 0.90 gate. fp32-256 scored IDENTICAL 0.76 → resolution-inherent loss (lossy position-bias interpolation), NOT a fp16/threshold artifact. RAM++ stays at 384; the ~6.5 f/s ceiling stands. (Python 3.11 was already present — the real blocker was never the toolchain, it was quality.)
- **On-hardware (RTX 2060 / DirectML, fully ISOLATED state — real 24k-file library verified byte-identical/untouched):** the merged engine ran crash-free — 120 imgs/20 s ≈ 6 f/s, 1128 `source='auto'` tags (accurate concrete nouns), 218 SFace 128-d (512-byte) embeddings, 105 clusters with no mega-blob, peak RAM 4.2 GB at 120-file scale. Validates the IPC-casing + decode-cap changes on real hardware.
- **Record corrected:** `NEXT.md` "(later 6)" lists the ~10 audit-verified already-DONE items (SHA256 pinning + gate, release.yml, AutomationProperties, memory bounding, HNSW, USN, WS7, ARM64, WS6 DB-contract) so future sessions stop re-chasing them, plus the genuine remaining work by blocker (EV cert; Mac behavior-layer; Windows-HW soak/matrix; lower-priority doable-here).

## 2026-06-02 (later 5) — WS6 macOS lockstep: DB-contract half (epoch / tag-source / IPC) — PR #5, build-verify track

Tackled the macOS lockstep that "needs a Mac" by splitting it into the **persisted-bytes contract** (do-able + macOS-CI-build-verifiable from here) vs the **behavior-verifiable** half (needs a Mac). Implemented the former via a 10-cell file-disjoint Workflow + 4 adversarial verifiers, grounded in the **current** Windows engine source (the LOCKSTEP doc was stale on month-name and false-positive on vlm_model — verified each claim against code per the "verify directives" rule). Pushed `macos-lockstep` → **PR #5** (macOS CI `pull_request` building; the only gate, since no Windows source changed).

- **Timestamp epoch** 2001-ref → Unix(1970) across writer **and every reader** (DBWriter, DeepAnalyzeRunner, FaceClustering `persons.*` — a verifier-caught straggler, ReadStore incl. a pre-existing writer/reader mismatch, Restructure — dropped `+978_307_200`). **Scan tag source** `vision`→`auto` (writer + all readers) + rescan DELETE/REPLACE + trim-skip-empty; dropped orientation/capability extra tags; byte-faithful hyphen sanitizer. **IPC contract**: `startScan` reshaped to rootPath/rootDisplay?/rescan (unsandboxed model — no `.entitlements`), +`markPersonsDifferent`/`wipeLibrary` commands, +8 reply events/DTOs, +`EngineInfo.hardware`/`HardwareInfo`, +`EngineError.modelKind`, +`deepAnalyzeAll.tagsOnly`; both switches + round-trip test updated.
- **Reverted** the face-bbox JSON swap — it broke macOS clustering (`bboxArea` CSV-parse) and still wasn't byte-faithful (px vs normalized).
- **Deferred (need a Mac to behavior-verify; in `MACOS_LOCKSTEP_NOTES.md` Part 3):** face bbox coord-space + FaceAlign/landmark embeddings (Part 2 #1), RAM++ CoreML tagger (#3), content-hash + rename-heal, restructure-routing rewrite, VLM-tag gen. Found a pre-existing latent `ID`-vs-`Id` schema/Windows-wire casing drift (not a DB-round-trip blocker).
- **HONESTY:** edit-only; Swift not built here. macOS CI build-verifies compilation; the cross-platform DB round-trip that *defines* lockstep still requires the user's Mac — this is build-verified, not lockstep-verified.

## 2026-06-02 (later 4) — Production-hardening pass cont'd: WS1b/WS3/WS7/WS-CD (5 more verified merges)

Continued the v1.0 plan via investigate→implement→adversarial-verify→gate→merge workflows. All headless-gate-green (engine clippy/fmt/test + app build/format/test), pushed to `main`, CI green:

- **WS1b on-demand video thumbnails** (`91b637e`) — new `generateVideoThumbnail` command + `thumbnailGenerated` event (schema + Rust + C#, round-trip-tested). Engine handler runs `keyframe_25pct` out-of-process, fits-192 + JPEG + base64, echoes `modifiedAt` so the app writes ThumbnailDiskCache with the SAME key the tile computes; ThumbnailService correlates the response back to the awaiting tile (20s timeout). Restores video tiles for the EXISTING library (no rescan) without re-exposing the crash class. Verified by a 3-lens adversarial pass (cache-key round-trip, correlation lifecycle, engine panic-safety) — all clean.
- **WS3 ProposeRenames** (`cb208cd`) — the bound-but-ignored checkbox now functions: new `AnalyzeMode::CaptionAndTags` (caption+tags, rename gate excluded) chosen when `!tags_only && !proposeRenames`; `proposeRenames` threaded through schema/Rust/C#/view, default true (no regression).
- **WS7 18 medium/polish fixes** (`abc06a9`) — a fresh 6-lens audit of current main (refute-by-default, adversarially verified) → 19 findings, 18 fixed, 1 dropped as a false positive (SuggestedMerges "transitive dangling" — mergeClusters deletes the source, dest survives). ThumbnailDiskCache (.tmp-orphan, LRU race, LastAccessTicks×2); engine deep_analyze silent-returns + batch_clip `.expect()`; People mark-unknown silent-fail; WelcomeSheet persistence-not-awaited; remaining GoldBrush/style indexer reads → ThemeHelper; DeepAnalyze warm-up timeout; installer ready-timeout 30→75s.
- **WS-CD pt.1** (`50d73f9`) — `publish-bundle.ps1` signtool `$LASTEXITCODE` check (THE ships-unsigned-silently blocker) + `CI_RELEASE` skip-guards + per-MSI signature verify; `release.yml` tag-triggered Windows CD, ready-but-dormant until the EV cert. (PS parse-clean, YAML valid; CI doesn't build the installer so unverifiable beyond that.)
- **WS3 resumable-scan** — investigated + found ALREADY IMPLEMENTED (discovery skip-set on `scanned_at >= modified_at`); the planned `last_file_index` checkpoint is redundant and deliberately not built (DECISIONS 2026-06-02). WS3 complete.

**Plan status:** WS0/WS1(a,b,c)/WS2/WS3/WS4-a11y/WS5-mem/WS7/WS-CD-pt1 all merged + CI-green. Remaining is externally blocked or needs hardware/toolchain not in this env (NEXT.md "(later 4)"): WS5 256-export (Py 3.11–3.13), WS6 macOS lockstep (a Mac), WS-CD EV cert + WiX-build (RollbackBoundary/version) + network SHA256 population + push-verify CI-gate hardening; plus hardware-verify-only polish (per-monitor DPI, keyboard-E2E UI-automation, HNSW/perceived-speed perf, the optional scan-recovery banner).

## 2026-06-02 (later 3) — Production-hardening pass: 6 verified merges (plan `majestic-foraging-tome.md`)

Drove the approved v1.0 production plan via file-disjoint Workflow fan-outs + verified per-workstream merges. Each workstream: headless gate matching CI exactly (engine `cargo clippy --all-targets -D warnings` / `fmt --check` / `test` from the engine dir for the pinned 1.90 toolchain; app `dotnet build` / `format --verify-no-changes` / `test`), then merge to `main`, branch deleted, untracked strays kept out of every commit. Six landed, all green:

- **WS4 accessibility pt.1** (`7b2b799`) — 161 `AutomationProperties.Name/HelpText` across all six tabs + sidebar + sheets (8-agent fan-out, per-cell adversarial review). 28 WCAG-AA contrast flags deferred to WS7.
- **WS2 silent-failure elimination** (`b98becb`) — 20 callsites surfaced via new `EngineClient.WaitForBulkActionResultAsync` (mirrors WipeLibraryAndWait) + `SqliteErrorTranslator` (DB/IO jargon → actionable copy): Cleanup trash (was fire-and-forget + unconditional refresh — failed deletes looked successful), Restructure plan/apply, DeepAnalyze, Bulk rename/tag, People merge + SuggestedMerges, Settings cancel, onboarding; ReadStore/ClipSearch errors consumed into `LibraryViewModel.ErrorMessage` (UI-thread-marshaled — covers the OpenAsync-throws path that skipped RefreshAsync).
- **WS0 model download-integrity** (`51c3364`) — `check_size_plausible` (loose size-sanity in both download paths; catches truncation / HTML-error-page-as-model even with no pinned hash) + `.part-N` orphan guard (oversized stale part → discard, not "done") + 3 unit tests. Hash VALUES + the non-`None` CI gate deferred to WS-CD (need real artifacts; RAM++ hash not final until the WS5 256-export). Rationale in DECISIONS.md.
- **WS3 pt.1 data-integrity** (`6c608f6`) — engine `db::quick_check` at writer open → `db_integrity_check_failed` EngineError with wipe+rescan guidance (was: silently proceed on a torn-page DB); RestructureView per-file selection persistence across nav (static `_deselectedFileIds` — was reset on every tab switch, silently discarding the user's include/exclude choices).
- **WS1c sweep** (`ee8c680`) — 12 theme-brush (`TextFillColor*` / `SubtleFill*` / `CardStrokeColorDefault`) code-behind reads in imperative sheet-builds routed through `ThemeHelper.GetBrushSafe` — closes the remaining SuggestedMergesSheet `KeyNotFoundException` native-fast-fail shape. Framework styles + the custom GoldBrush (reliably present in the merged dictionary) left as-is.
- **WS5 memory bound** (`2f0d6b9`) — L1 BitmapImage cache re-expressed as a real ~128 MB byte budget (was 5000 entries ≈ ~550 MB of decoded bitmaps; the old "~25 MB" comment counted the encoded size). Holds the 50K-scroll working set bounded; LRU evicts the coldest, a miss just re-decodes.

**Remaining** (NEXT.md "(later 3)" has exact resume steps): WS1b out-of-proc video keyframe (restores video thumbnails — the crash itself is already fixed, this is feature-restore; IPC + engine + app); WS3 ProposeRenames (IPC-crossing) + resumable-scan (ship flag-gated, verify on hardware); WS4 per-monitor DPI + keyboard E2E test; WS7 polish + the 28 contrast flags; WS-CD (all CI/CD, the explicit final phase). Externally blocked: WS5 256-export (needs Py 3.11–3.13), WS6 macOS lockstep (needs a Mac), WS-CD EV cert.

## 2026-06-02 (later 2) — Scan-crash fix: in-proc shell VIDEO thumbnail provider fast-fail (merged to main)

User hit a hard crash mid-scan on the real `G:\TrueNAS\Users` library (~8300 files in). Root-caused from the logs + an adversarial diagnosis workflow (19 agents, 15 candidates, 1 prime suspect, 12 dismissed) and fixed; headless-verified (app build 0/0 + tests + `dotnet format` clean).

- **Root cause (diagnosed from logs, not guessed):** the **engine was innocent** — `engine.jsonl` shows it streaming `[TAGGING] ram_plus_summary` then `stdin EOF; entering shutdown → FileIDEngine exiting cleanly` (it only stopped because its parent died and closed the pipe). The **app died by native fast-fail** — `app.log` ends abruptly at 12:58:11 mid-`[THUMB]` churn with NO managed exception (no WER dump armed). The corpus was `.jpeg + .mov`; `ThumbnailService` excludes **audio** from the in-process shell `IThumbnailProvider` (the documented 2026-05-30 `.mp3`-art crash class — unpackaged WinUI has no DllHost/COM-surrogate isolation, so a flaky native handler's `RaiseFailFastException` tears down the whole process with no catchable exception) but **never added the symmetric VIDEO skip**, so every cache-cold `.mov` invoked the in-proc Media-Foundation video frame extractor — a flaky one fast-failed the app.
- **Fix:** added a `VideoExtensions` skip-set mirroring `AudioExtensions` and short-circuit it BEFORE the shell call in `RenderAsync` (`ThumbnailService.cs`) — video tiles now render the placeholder (a previously-cached keyframe still shows via the L2 disk read). The adversarial workflow **confirmed this as the sole prime suspect** and **dismissed** all 12 other hypotheses (TCS/DrainAsync thread-pool continuation, off-thread DispatcherObject, ItemsRepeater recycle race, disk-cache decode, ProgressEvent-burst subscribers, `Resources[...]` indexing) — verified safe: `RequestAsync` uses a `RunContinuationsAsynchronously` TCS, the `tile.Thumbnail` assignment is `DispatcherQueue.TryEnqueue`-marshaled, recycled tiles are `IsDetached`-guarded, and `RunBytesSetSource` fully `catch`-guards its `async void` body.
- **Follow-up (NEXT.md):** restore LIVE video thumbnails safely via an OUT-OF-PROCESS extractor (shell `IThumbnailCache`, or reuse the engine's scan-time keyframe) — the in-proc shell chain is still used for images (lower risk: WIC fallback + happy path), so out-of-proc is the durable fix for the whole class. WER dump arming (`build/enable-crash-dumps.ps1`) recommended for the next repro.

## 2026-06-02 (later) — Multi-workflow perf + bug + lockstep sweep (branch `perf-bug-lockstep-2026-06-02`)

Three orchestrated workflows (perf-lever analysis, adversarial bug-hunt, Windows↔macOS lockstep audit) + on-hardware measurement on the RTX 2060 against `G:\TrueNAS\Users` (13,277 images). Engine headless-green throughout: `cargo clippy -D warnings` clean + **246** tests; pinned 1.90. No C# edits this pass (the dotnet gate is unaffected). NOT yet committed/merged.

- **Perf — two safe wins landed; the throughput ceiling is honestly characterized.** (1) **RAM++ CPU preprocess hoisted out of the model-session Mutex + GPU permit** (`tagging.rs`/`ram_plus.rs`: new `preprocess_tensor` + `tag_prepared`; the lock now wraps only the GPU forward). (2) **Pre-decoded RGB read-ahead byte-budgeted** (~256 MB) instead of a flat `worker×2` frame count (`tagging.rs`) — bounds the 5.7 GB RSS problem + the pathological-frame case. Both verified non-regressing (0 panics, all files tagged). **Measured on the 2060 (CUDA, cap 400): ~6–8 files/s with ~25 % run-to-run variance — RAM++ swung 517→671 ms/file on IDENTICAL code between runs (GPU-clock/thermal), so these <5 % wins sit BELOW the measurement-noise floor; they are architecturally sound hygiene, NOT a measured throughput win.** New repeatable harness `build/perf_bench.ps1` (isolated state, file-capped, GPU-sampled, `[STATS]`-parsed).
- **Perf research (cited, adversarially verified) — the real levers.** **INT8 is a dead end on this stack** (DirectML quantized conv ~10× *slower* per microsoft/DirectML#282; CUDA EP can't consume INT8 nodes; TensorRT auto-INT8 ≈1.0× for Swin — "FP16 recommended"). **The shipped model is genuinely fp16** — verified by inspecting the ONNX (924 MB FLOAT16 vs 0.4 MB FLOAT32; the 882 MB is the baked `[1,4585,51,512]`+`[512,233835]` tag-embedding constants), so the registry comment is right and fp16 conversion is already done (`build/inspect_onnx.py`). **The one real throughput lever is a lower-res 384→256 re-export (~1.8–2.7×, works on the shipped DirectML EP, relieves VRAM)** — toolchain prepared (torch 2.12 + checkpoint downloaded, `export_ram_plus_onnx.py` gained `--image-size`, A/B harness `build/ram_ab.py` ready) but **BLOCKED in this env**: Python 3.14 forces transformers 5.x, and `recognize-anything`'s vendored BERT needs the old `transformers.modeling_utils` symbols (`find_pruneable_heads_and_indices` is gone in 5.x). Needs a Python 3.11–3.13 env (transformers ~4.25 + timm<1.0). Spec in NEXT.md.
- **Bug-hunt (10-cell adversarial workflow) → 3 confirmed; 1 fixed.** **eng-ipc-0 (high) FIXED:** `spawn_blocking` JoinError now emits a terminal event in `planRestructure` / `applyRestructure` / `embedTextQuery` / `embedImageQuery` (was: Restructure plan/apply hangs forever, search stalls 5 s) — mirrors the `face_clustering` PAR-111 precedent. **eng-ipc-1/2 (medium/low) SPECCED, deferred:** IPC field-name casing drift (`queryID`/`personID`/`sourcePersonID`/`batchID`/… serialized lowercase-`d`, violating `ipc.schema.json` → breaks macOS round-trip). Full ~25-field both-sides inventory in NEXT.md; deferred as ONE atomic, test-guarded Rust+C# PR (a partial edit breaks the live Windows app; it is NOT a live Windows bug). Note: the bug-hunt under-reported (capacity blips zeroed several cells) — a fuller re-run is queued.
- **Lockstep audit (56-agent workflow) → 39 confirmed divergences → [`LOCKSTEP-2026-06-02.md`](LOCKSTEP-2026-06-02.md).** The cross-platform DB round-trip is broken on multiple axes, almost all **macOS-side** (needs a Mac to fix+verify): **CRITICAL** macOS writes timestamps as 2001-reference epoch vs Windows UNIX epoch (~31 yr silent corruption; the fix must reconcile several internally-inconsistent macOS read/write sites) and `startScan` uses `rootBookmark:Data` vs the schema's `rootPath`; **HIGH** macOS `FaceAlign.align112` has zero callsites (embeds unaligned crops) + Apple Vision extracts no landmarks, 9 reply events + `wipeLibrary`/`markPersonsDifferent` absent on macOS, source token `vision` vs `auto`, rule-cascade month/category token + VLM-rename divergences. Doc lists each with file:line on both sides + the byte-faithful fix + a `win_verifiable` flag.
- **Infra:** capacity blips repeatedly killed freshly-launched subagent bursts; the workflow scripts were hardened with a 4-try `ra()` retry wrapper (spreads attempts across wall-clock) which got bug-hunt + lockstep through. Reusable workflow scripts saved under the session `workflows/scripts/`.
- **Merged to `main` (5196252) + CI GREEN** (Windows engine ✓ + macOS app ✓; Windows app workflow correctly skipped — zero C# changes in that commit). Then **consolidated branches → only `main` remains**: deleted the 4 fully-merged local + 3 merged remote branches; no open PRs. The stale `fix/win-installs-liborder-cleanup-preview` (d9a0bf4) was **triaged not blind-merged** — its install-flow rewrite (delete CudaAutoInstaller/Llama) is SUPERSEDED by main's all-vendor auto-install (a merge breaks the build: `App.xaml.cs` calls the deleted `*.Hook()`), and its Cleanup keeper/delete-safety is superseded by the accuracy sweep's "likely duplicates — verify before deleting". **Salvaged the two still-good, install-independent parts onto main:** engine `tagging.rs` decoder-thread graceful spawn (no mid-scan panic if the OS refuses a thread under handle/RAM pressure) + `ReadStore` newest-first ordering (`scanned_at DESC, id DESC` — macOS parity); dropped the rest + deleted the branch.

## 2026-06-02 — Audit fixes merged to main (CI green) + RAM++ batching DISPROVEN + accuracy/residual sweep (28 fixes)

Three things landed since the audit:

- **`audit-fixes-2026-06-01` merged to `main` — all 3 GitHub workflows GREEN** (Windows engine 10m45s, Windows app 4m25s, macOS 3m41s). Notably the **macOS lockstep Swift compiled + passed on the real macOS runner** (the previously-unverifiable v13 `face_verification_anchors` migration, the `DBWriter` ON-CONFLICT cascade rewrite, the `timeIntervalSince1970` epoch fix, the canonical `vlm_model` tokens). The cross-platform DB round-trip (db-incompat) is now reconciled on the engine side.
- **Batched RAM++ MEASURED on the RTX 2060 → DISPROVEN.** Built the infra (dynamic-batch ONNX export + `RamPlusBatchCoordinator`, env-gated) then profiled the real wall. **GPU is compute+VRAM SATURATED at batch=1** (util mean 73% / p50 87% / p90 97%; VRAM 5348/5955 MB = 90% full) — the single-image *pool* already fills the GPU. A/B (same ONNX/corpus): single-pool **2.1 f/s** vs batched=4 **1.6 f/s** = **~23% SLOWER**. Production fp16+pool = **6.2 f/s**, near this card's ceiling for Swin-L @384. The "GPU <1% utilized / batching is the only win" premise was **wrong**. Coordinator kept **opt-in OFF** for high-SM/VRAM cards (re-validate per card); false "throughput fix" comment corrected. Real levers = TensorRT EP or a lighter tagger. See [`DECISIONS.md`](DECISIONS.md).
- **Accuracy + residual-bug sweep (10-dimension workflow, 45 agents, adversarially verified) → 30 confirmed; 28 fixed (branch `accuracy-residual-fixes-2026-06-01`).** Headless-green: engine `clippy -D warnings` clean + **246** tests; app build 0/0 + `format --verify` clean + tests green. Highlights — **accuracy:** CLIP nearest→bilinear resize (parity + de-aliasing #1), empty-RAM++→CLIP scene fallback (#7), YuNet landmark-clamp removed (#8), cluster name collision disambiguation + sanitize (#2/#9), c-TF-IDF per-file dedup (#18), dim-mismatch embedding skip (#15), OCR line-bbox union (#30). **Data-loss / correctness:** stale `face_prints`/`ocr`/`doc` cleared via stage-ran flags (#5/#11), Deep-Analyze single-file error now emits terminal `Complete` (no stranded card #6) + single-in-flight gate (#10) + VLM transaction (#23) + temp-file RAII (#24), Cleanup >16 MB "likely duplicates — verify before deleting" (no false byte-identical claim #3), `embed` `query_id`-on-failure (no 5s stall #12), scan coordinator pause/cancel (#20), long-path trash/rename (#28/#29), tags-sidecar follows rename/restructure (#27), path-redaction fallback leak closed (#26). **Schema:** `action` pattern reconciled with the 8 real discriminators (#13). **Deferred:** CLIP tokenizer reference-regex (#16 — needs scene-matrix regen + threshold retune); Cleanup phash parity (#4 — exact-content kept by design for delete safety, divergence documented).

## 2026-06-01 (later) — Full top-to-bottom Windows audit (4-stream, ~675 agents) + 7 verified fixes (branch `audit-fixes-2026-06-01`)

A multi-workflow audit of the entire Windows app across **four adversarially-verified streams** — engine static (18 units, all 75 Rust files × 6 dimensions), app static (15 units, all C#/XAML, threading/fast-fail first), macOS parity (24 Windows↔Swift pairs), and a live on-hardware run — synthesized into [`AUDIT-2026-06-01.md`](AUDIT-2026-06-01.md). **618 raw findings → 153 adversarially confirmed** (engine 48, app 39, parity 66) + on-hardware telemetry. Headless-green throughout: engine `cargo clippy -D warnings` clean + `cargo test` **243** (+1 new); app `dotnet build` 0/0, `dotnet format --verify` exit 0, IpcSchema **34/34**, App.Tests **108/108**.

- **On-hardware (RTX 2060, real `G:\TrueNAS\iMac Documents`, isolated temp DB via `build/audit_onhw.ps1` — the real 24k-file library was never touched).** CUDA EP **binds AND completes cleanly** (`executionProvider=cuda`, pack + cuDNN load) — the long-"unverified" 3-5× path actually works; the prior "DirectML" reports were stale-pack state, not a code defect. Scan: 311 files / **0 failures**, 2,639 content-accurate tags, faces all **128-d SFace** (no stale ArcFace), restructure + merge-suggestions functional. **Perf is the real problem:** **4.9 files/s even on CUDA** (target ≥140); CLIP barely batches (avg ~1.5 img/dispatch); per-file wall ~1.5 s vs ~0.36 s active → serialization stall; peak RSS **5.7 GB** (vs 1.5 GB cap); clustering over-splits (176 persons / 624 faces). A first DirectML attempt aborted at the model gate from a *test-harness* bug (Models junction one dir level too high) — the synthesis's HW-1 "DirectML never completes" was reclassified **UNVERIFIED** (re-measure separately).
- **8 fixes landed + headless-verified.** Engine (data-loss/crash): `wipe_all` FK-leak scope guard (**ENG-2** — was leaking `foreign_keys=OFF` on the persistent writer on any error path); `file_ref` lossless `u64→i64` bitcast at all 5 binds + high-bit regression test (**ENG-18** — a high-sequence NTFS ref `> i64::MAX` aborted the whole scan batch via rusqlite `ToSql`); restructure no-op-check-before-uniquify (**ENG-42** — was renaming already-correct files to ` (2)`); `SFace.embed` 128-d assert (**ENG-69**); per-file read-buffer pre-alloc clamp (**ENG-71** — a bogus/huge stat size aborted all decoder threads via `Vec::with_capacity`). App: `UndoStack` lock (**APP-1** — cross-thread `LinkedList` corruption); `AutoTriggerFaceClustering` re-entrancy gate (**PAR-111** — a rescan's 2nd `ScanComplete` re-fired clustering, racing the engine); model-install watchdog ctor-captured UI dispatcher (**APP-2** — was `null` post-`ConfigureAwait`, silently inert).
- **Perf wave — root cause PROFILED (RAM++), not guessed.** Two hypotheses were tested on the RTX 2060 and **disproven** (CLIP fill-window 20→75 ms: no gain → reverted; DBWriter back-pressure: `out_tx` buffers 256). Permanent `[STATS]` instrumentation (`ramplus_us`/`vision_wait_us`) then pinned it: **RAM++ Swin-L @384 ≈ 670 ms/file on `pool_size=2`** (VRAM-clamped on 6 GB) → workers wait ~680 ms for the RAM++ pool; CLIP (~190 ms) is starved downstream. A candidate fix was then TESTED on hardware — a CUDA pool=3 (EP-aware VRAM sizing) — and it **REGRESSED** to 3.9 files/s (RAM++ 670→812 ms, RSS→7.6 GB): 3 Swin-L sessions over-subscribe the one GPU and thrash, confirming RAM++ is **GPU-COMPUTE-bound, not concurrency-bound** (reverted). The only real win left is **batched RAM++** (offline dynamic-axis ONNX re-export + a batch coordinator) or a lighter tagger — specced in NEXT.md.
- **Second fix wave (headless-verified).** Engine: **ENG-59** per-EP crash-disable markers (two packs can now both stay disabled; was one overwritten `.ep_disabled` file); **ENG-88** zip-bomb ACTUAL-decompressed-bytes cap via `Read::take` (was trusting the attacker-declared header size); **ENG-91/92** rename keeps `path_hash` in sync + no longer reports false success on a failed DB write; **ENG-97** path-redaction anchored on the real app root + canonical app-dir (was leaking any user path containing a folder named "FileID", username and all); **PAR-69/96** restructure filename sanitizer ported byte-faithfully from macOS `componentSafe` (Windows reserved names / trailing dots / replace-not-delete — was emitting NTFS-invalid folder names + cross-platform tree drift). App: **PAR-116** kind-filter pushed into ReadStore SQL (was a post-LIMIT C# filter → under-filled grids); **PAR-117** `failed=0` in semantic search. Plus permanent RAM++/vision-wait `[STATS]` instrumentation + 2 new regression tests (engine **245** tests). All headless-green (engine clippy + tests + fmt; app build 0/0 + format + 108/108).
- **Adversarial self-review of every fix (17-agent workflow) → 5 gaps closed, 0 regressions.** The review returned 10 correct + 7 concerns. Closed: **ENG-88** cumulative zip cap now charges ACTUAL decompressed bytes (declared-size accounting let a many-entry bomb evade the 2 GiB total); **PAR-116** kind-filter now threads through the PRIMARY text-search path (`ClipSearchService`), not just browse/find-similar; **ENG-91** path_hash also synced at the restructure-apply move site; **ENG-97** redaction prefix now requires a separator boundary (was passing `…\Local\FileIDBackup\…` through) + the new test is Windows-gated; **PAR-111** face-clustering JoinError now emits an error event so the auto gate releases on a clustering panic. Left intentionally: ENG-59 reenable-all (bounded/safe), RAMPP-POOL (tested + reverted). Re-verified green after closures.
- **Most serious OPEN issue: the cross-platform DB does not round-trip** (db-incompat, needs a Mac) — macOS missing the v13 migration; SFace 128-d-vs-ArcFace-512-d face-embedding + alignment mismatch; `source=`/`vlm_model`/timestamp-epoch token drifts. Full prioritized backlog in the report + [`NEXT.md`](NEXT.md). Not committed/merged pending review.

## 2026-06-01 - Windows: Wipe = reset-to-clean + Restructure macOS-parity overhaul (branch `windows/wipe-restructure-overhaul`)

Two user-reported Windows issues. Headless-green: app `dotnet build` 0 warn / 0 err, `dotnet format --verify` exit 0, `FileID.App.Tests` 108 (+6 new) + `FileID.IpcSchema.Tests` 34 passed. On `windows/wipe-restructure-overhaul`; the WinUI runtime path needs the RTX 2060 (see NEXT.md).

1. **"Wipe + Rescan" -> "Wipe" (the button "couldn't wipe").** Root cause: `RunWipeAsync` always called `TriggerRescanAsync()` after a successful wipe, so the library repopulated on the spot - the wipe looked like a no-op. Removed both rescan calls (engine-side `wipeLibrary` truncate + the stop/delete/restart fallback are unchanged). On success the app now resets to first-run state - `AppViewModel.FolderPath = null` nulls `LastFolderPath`/`LastFolderDisplay` and returns the sidebar to the empty picker - and shows a "Library wiped" confirmation. Downloaded models under `Models/` are kept (per the user's "reset to a totally clean state, keep models"). `SidebarFolderHeader.xaml(.cs)`.
2. **Restructure tab - recommendation-first + file-first (macOS parity).** Replaced the analytics-first UI (Anchor/Mixed/Junk count strip, confidence-tier chips, flat category list) with a port of macOS `RestructureView.swift`: stat hero (Staying / Tidying / Reorganizing) + a reworked Deep-Analyze nudge (real "Run Deep Analyze" button gated on caption fraction < 0.4) + Flow/Tree toggle + unified surface (Sankey hero + Keep/Tidy/Reorganize recommendation cards) + Staying-put expander + nothing-to-move card. Cards expand in place to the actual files (checkbox + "from <folder>") with per-file + per-group selection; "See all" reuses `DrillDownSheet` via a new `SetOutcomeFilter`. Pure app-side - the engine plan already carried `Tier`/`Confidence`/`Reason`/`FolderClassifications`. New VMs `RestructureOutcome` / `RestructureFileRowVm` / `RestructureRecommendationVm` + a shared `RestructureGrouping` (Tier->outcome, unit-tested, replaces the duplicated mapping in the view + DrillDownSheet). All lists are ItemsRepeater + DataTemplate over observable VMs (V15.x fast-fail-safe); the stat hero + hover cross-highlight are inlined into the view (no separate control/bus) and one DataTemplate is tinted from the VM (no selector).

## 2026-05-31 (later) — Windows: Suggested-merges crash fixed + faces/merge audit (merged to `main`)

User report: opening **People → Suggested merges** hard-crashes the app. Root-caused + fixed, then audited the whole faces/merge path. Headless-green: solution `dotnet build` 0/0, `FileID.App.Tests` 102 + `FileID.IpcSchema.Tests` 34 passed, `dotnet format --verify` exit 0; engine `cargo clippy -D warnings` clean, `cargo test` 242 passed, `cargo fmt --check` clean. Merged to `main`; the win-installs work (`d9a0bf4`) is intentionally NOT included — it stays on its own branch, still gated on hardware verification. GUI runtime still needs the RTX 2060 (see NEXT.md).

1. **The crash (P0).** `SuggestedMergesSheet` built each row imperatively in `Render()` — which runs in a raw `DispatcherQueue.TryEnqueue` callback with no try/catch — and indexed *theme-dictionary* brushes via `Application.Current.Resources["TextFillColorSecondaryBrush"]`/`["SubtleFillColorTertiaryBrush"]` (throws `KeyNotFoundException`; the XAML correctly uses `{ThemeResource}` for exactly these), plus rebuilt full `UIElement` subtrees as ItemsRepeater items per engine event (the V15.4 layout-pass fast-fail shape). Replaced with a `DataTemplate` over a new `MergeSuggestionVm` (mirrors `PersonCluster.AnchorImage`: lazy/cached `BitmapImage`, `DecodePixelWidth=80`), `{ThemeResource}` resolved natively, `_unloaded` guard. Both crash mechanisms gone.
2. **Merge hardening (P1, engine).** `handle_merge_clusters` now guards `source==dest` (was: delete the person row while its faces still point at it → orphaned faces) and recomputes the destination `representative_face_id` (highest-quality embedded face) instead of leaving it stale.
3. **"Different people" via IPC + survives re-cluster (P1).** Was a direct app-side `ReadWrite` SQLite write (violated single-writer; `SQLITE_BUSY` risk) keyed on `person_id` — which churns every re-cluster, so the verdict silently stopped suppressing. New `markPersonsDifferent` IPC command routes the write through the engine's single writer; migration **v13** adds `face_a`/`face_b` to `face_verifications` and the verdict + `findMergeSuggestions` filter now key on the *stable* anchor `face_prints.id` pair (legacy person-pair rows still honored). **macOS must mirror v13.**
4. **Suggestion speed + freshness (P2).** `findMergeSuggestions` replaced two per-person correlated subqueries with a single rep-face JOIN. After a merge the sheet also resolves sibling rows referencing the merged-away person.

Known gap (flagged, deferred): `revertMerge` has no UI caller and `handle_merge_clusters` records no merge history, so merges are effectively un-undoable — true undo needs a history record (out of scope for the crash fix).

## 2026-05-31 (audit hardening) — ETA fix + data-loss/crash fixes + security + perf/quality (merged to main, CI-green)

A workflow audit (81 agents: parity + ETA design + adversarially-verified bug/security/perf hunt) drove a multi-phase pass. **All landed work is headless-verified** (engine `clippy -D warnings` + 242 tests incl. 11 new; app build + `dotnet format` + IpcSchema 34/34 + App 102/102). **Merged to `main` (PR #3 → `3b11713`); all three CI workflows green** — Windows engine (x64 + arm64-native + arm64-cross), Windows app (.NET, x64 + arm64), macOS app (SwiftPM build + test + smoke). The macOS edits (B8/S5/S8) thus got their first real verification on the macOS runner.

- **Phase 0 — critical data-loss + crash fixes (engine, 7 new tests).** B1 rename-heal no longer collapses coexisting byte-identical copies (heal only on `file_ref` move or hash-match-with-old-path-gone). B3 restructure drops `MOVEFILE_REPLACE_EXISTING` + uniquifies colliding dests. B2 clustering modal-dim filter (no panic on legacy/corrupt embeddings). B4/B5/S6/S7 restructure stale-plan revalidation + corrected atomicity comment + durable recovery sidecar + source containment. B6 `ep_guard` arms the override-aware EP (`runtime::armed_provider`). B7 removed `panic="abort"`. C1/C2/C4 doc-extract zip-bomb caps + trash-log 1024-cap.
- **Phase 1 — the broken ETA (engine + Windows app + macOS, 2 new tests).** Root cause fixed: ETA divides remaining by a rolling wall-clock EMA, not the per-batch DB-flush rate ("13s for an hour" gone). Windows UI shows the active-stage-labeled ETA ("Tagging — 48m left", "Counting files…"). macOS **B8** rolling-rate reset per session. *Decision:* no IPC `stages[]` array — a scan has 2 live stages; faces/captions are separate jobs with their own ETAs (see DECISIONS).
- **Phase 2 — security.** S9/S12 path redaction in logs; S4 bounded C# stdout framing (1 MiB + resync); S5 bounded Swift IPC buffer; S8 macOS `blobToEmbedding` empty-guards. S2 verify-or-bail is wired but inert (all `registry.rs` `sha256: None`) — activation = fetch+hash artifacts (network step). S1 macOS in-process unzip deferred to Mac.
- **Phase 3 — perf (engine).** P2 Deep Analyze CLI VLM now passes `-ngl 99` (was CPU-only → 5–20× on GPU runtimes, quality-neutral). P4 OpenVINO `AUTO:GPU,CPU` device pin. P16 sargable BINARY-range rescan/deep-analyze prefix seeks (was non-sargable `LIKE 'root%'`; +1 new test). P3 EP-aware vision/CLIP concurrency (rises to pool size on CUDA/TensorRT; no-op on 6 GB by design; DirectML keeps the TDR floor).
- **Phase 4 — quality.** P18 widen merge-suggestion band (dedicated `MERGE_SUGGEST_COS_HIGH=0.66`, additive). P17 mutual-kNN Pass-1 gated behind `FILEID_FACE_MUTUAL_KNN` (default off, on-hardware A/B). P22 already env-tunable.
- **Deferred (verification-gated), specced in NEXT.md:** S2 hash population + S1 macOS unzip; P1 batch RAM++ (ONNX re-export, Python/HF); CUDA-bind 3–5× verify on the RTX 2060; P19/P20/P21 quality tuning; macOS parity EG1–EG5 (RAM++ port, FaceAlign wiring, content-hash rebind, SFace contract cleanup, doc-text/BGE); Windows UI parity UG1–UG5 (Deep Analyze status card, RAM-fit gating, Settings); P12/P13 ANN search index.

## 2026-05-31 (later) — OpenVINO pack assembled + hosted on HF (merged, CI-green)

The B3 OpenVINO handoff is DONE. Assembled `ort-openvino-win-x64-1.22.0.zip` verbatim from the
official PyPI wheels `onnxruntime-openvino==1.22.0` + `openvino==2025.1.0` (ORT 1.22 + OpenVINO
provider + the matched OV 2025.1 runtime DLLs + a `plugins.xml` + bundled MIT/Apache-2.0 license
texts), uploaded to `huggingface.co/Web-World-Wide/OpenVINO` (model card documents provenance +
license). `registry.rs` `ort_openvino_x64` now points at the real repo (was the
`fileid-ort-openvino` placeholder), ~40 MB download. Verified the hosted zip round-trips and
`onnxruntime.dll` is a valid PE @ ProductVersion 1.22.0 with the OpenVINO provider + Intel GPU
plugin present. Commercial-clean (MIT + Apache-2.0; no proprietary bits). Merged to main
(`4d201bd`), both Windows workflows green. **Only remaining OpenVINO gap: bind + perf verification
on a real Intel GPU** (none in the dev env) — safe regardless via ep_guard.

## 2026-05-31 — All-vendor HW acceleration auto-install + vLLM decision (branch `windows-allvendor-accel`)

Builds on the merged CUDA pack. Headless-verified (engine clippy+tests; app build+format+tests). On-branch.

- **vLLM vs llama.cpp — researched, KEEP llama.cpp.** vLLM is a server throughput engine (pre-allocates ~90% VRAM, NVIDIA/Linux-first, no Metal); FileID is single-user on-device on consumer GPUs (6 GB 2060) across Windows+macOS — llama.cpp's exact lane. No backend change. Full rationale + sources in DECISIONS.
- **B1 — EP crash-safety gate (`models/ep_guard.rs`), the linchpin.** Arms a `packs/.ep_attempt` breadcrumb around the first ORT session bind (scan.rs), disarms on success; a stale breadcrumb at next startup (main.rs `resolve_poison_at_startup`) → the bind crashed → persistent `.ep_disabled`, fall back to DirectML until re-enable (Verify install / pack reinstall / override). `detect()` treats a disabled EP as absent. Bounds auto-enable risk to one crash → auto-revert.
- **B2 — CUDA auto-install on NVIDIA.** `CudaAutoInstaller.TryInstallOrtCudaPack` now auto-fetches cuDNN + `ort_cuda_x64` (gated by the now-wired `DisableAutoInstallCudnn`), independent of the llama-cuda sentinel. Stale `CudnnAutoInstaller` comment fixed.
- **B3 — OpenVINO framework (Intel), Apache-2.0.** `ort_openvino_x64` registry entry (HF `Web-World-Wide/fileid-ort-openvino`); `ORT_DYLIB_PATH` pin generalized to the detected vendor's pack via `runtime::active_pack_dir` (NVIDIA→cuda, Intel→openvino); `CudaAutoInstaller` Intel branch + `DisableAutoInstallOpenVino`; Accelerator sentinel/routing/size wired. **HANDOFF:** assemble + upload the OpenVINO ORT 1.22.0 artifact, then verify on Intel HW — until then the auto-install 404s gracefully and Intel stays on DirectML (B1-safe).
- **B4 — QNN/Snapdragon: no hosted pack (proprietary SDK).** DirectML baseline; QNN used only if the device provides it. Settings copy updated.
- **Still pending your RTX 2060:** confirm CUDA auto-installs + binds (`ExecutionProvider=="cuda"`, 3-5x), and that a forced bad bind reverts to DirectML via B1 instead of crash-looping.

## 2026-05-30 (later 5) — Crash + grid arrow keys + tag noise + CUDA pack (branch `windows-scan-fixes`)

Four user-reported issues from a real ~2h scan of a 24k+ library on `G:\TrueNAS`. All
headless-verified (engine clippy+232 tests; app build+format+102 tests). On-branch, not yet merged.

- **>1h crash — DIAGNOSED + MITIGATED.** Engine was innocent (`engine.jsonl`: clean shutdown
  after the app closed the pipe). The APP died by **native fast-fail** (`last-session.txt`
  clean_exit=false, ran 22:58→01:00 ≈ 2h; nothing logged despite full UnhandledException/AppDomain
  handlers). Died on the UI thread mid-burst extracting `.mp3` album art via the **in-process shell
  IThumbnailProvider** — shell providers run in-proc, so a flaky audio art handler fast-faults the
  whole app. Fix: `ThumbnailService` skips the shell provider for audio exts (after the L2 disk read,
  so cached covers still show). `build/enable-crash-dumps.ps1` arms WER full-dump capture for the
  next repro to confirm. Diverges from macOS (QLThumbnailGenerator is out-of-process).
- **Arrow keys — IMPLEMENTED.** The Library grid is an `ItemsRepeater` (no built-in keyboard nav;
  9dd7785 only fixed the preview sheet). Added a focus cursor over `ViewModel.Items`: arrows (±1 / ±row),
  Home/End, PageUp/Down, Shift+arrows extend, Enter opens preview, Space toggles select — wired on
  `GridScroller` tunneling PreviewKeyDown (handledEventsToo) so the ScrollViewer can't eat arrows first.
- **Tag accuracy — DIAGNOSED + duration noise removed.** `tag_report.py` on the real 32,899-file DB:
  RAM++ content tags are solid (child 0.95, cake 0.97, birthday cake 0.985). Noise was score-0.000
  enrichment. Removed audio/video **duration** tags (`3 sec`/`1 min` — metadata, not content). `iPhone`
  (camera) + `Year_*` KEPT per user (useful filter facets). Weak generics (huddle 0.70, floor 0.795)
  left for optional on-hardware floor tuning.
- **Perf 3-5x — ROOT-CAUSED + CUDA pack built (NOT yet on-hardware verified).** Real [STATS]:
  ~1,273 ms/file (~5 files/s); engine log: "NVIDIA … CUDA pack not installed; using DirectML
  (~3-5x slower)". Cause: pyke ort's binaries ship base onnxruntime.dll + providers_shared but NOT
  `onnxruntime_providers_cuda.dll`, so the EP chain falls through to DirectML. Built the **CUDA
  Performance Pack**: registry `ort_cuda_x64` = Microsoft's onnxruntime-win-x64-gpu-**1.22.0** zip
  (MIT, github.com, version matched to the shipped onnxruntime.dll), `ORT_DYLIB_PATH` pinned to the
  pack's matched runtime (inert until installed), provider-specific detection, Accelerator slot +
  Settings install the provider+cuDNN. cudart/cublas already present (llama.cpp-cuda pack); cuDNN
  auto-installs. **The RTX 2060 must confirm the EP binds + the 3-5x — see NEXT.md.** All-vendor:
  AMD/Intel/Snapdragon keep DirectML (production path); OpenVINO/QNN packs follow the same pattern.

## 2026-05-30 (later 4) — Processing-stat flicker + preview arrow/Space keys (Windows runtime bugs)

Three bugs the user hit in the running WinUI app:

- **Tagged / Memory / ETA erratic during a scan — FIXED.** The earlier "later 2" fix
  clamped only the phase *label*; the *stats* still flickered. Root cause: the engine
  emits `ScanProgress` from TWO concurrent sources during the discovery↔tagging pipeline
  overlap — the discovery ticker (`scan_session.rs:240`: processed=0, eta=None, fps=0,
  its own RSS read) and the tagging emitter (`:400`: live processed/eta/fps). `EngineClient.Apply`
  replaced `LastProgress` wholesale on each, so the sidebar bounced N→0→N / real→"computing"→real
  / two RSS readings. Fix: gate the WHOLE `ProgressEvent` on the monotonic phase rank —
  drop any event whose phase is below the latch, so `LastProgress` only holds one phase's
  stats at a time. Tagging events carry the LIVE `discovered` count (`scan_session.rs:404`),
  so "Discovered" keeps climbing through the overlap.
- **Arrow keys dead on the preview sheet — FIXED.** The sheet is hosted in a `ContentDialog`,
  which owns keyboard focus once shown, so the sheet's own `PreviewKeyDown` never fired.
  Fix: the host wires the handler on the DIALOG via `AddHandler(PreviewKeyDownEvent, …,
  handledEventsToo:true)` — tunneling reaches the dialog (ancestor of the focused element)
  before focus-nav or a focused button can eat the key.
- **Space starts/pauses video+audio — ADDED.** Files load paused (`AutoPlay=False`); Space
  now toggles `PreviewMedia.MediaPlayer` play/pause via the same handler (guarded so typing
  in the tag box still types a space).
- **macOS lockstep: nothing to port — verified, not assumed.** The macOS engine has exactly
  ONE `ScanProgress` construction site (`FileIDEngineMain.swift:606 emitProgress()`) built
  from a single `cur` session snapshot, so the Windows dual-emitter race structurally can't
  occur there. The arrow/Space fixes are WinUI-`ContentDialog`-focus-specific (SwiftUI has no
  analog). All three fixes are legitimately Windows-only.
- **Verified headless:** `dotnet build` x64 0/0, `dotnet format --verify-no-changes` 0,
  IpcSchema 34/34, App.Tests 102/102. (Live-GUI confirmation — flicker gone, keys live — is
  the user's to eyeball; the headless engine path can't drive the renderer.)

## 2026-05-30 (later 3) — On-hardware verify + macOS lockstep + RAM++ lock-in + consolidate to main (CI GREEN)

**Final: all three GitHub CI workflows green on `main`@784cc7b** — Windows engine ✓,
Windows app (.NET) ✓, macOS app ✓. The consolidation merge first tripped two real
failures, both fixed forward: (1) macOS `FaceAlign.swift` had a latent closure-arity bug
(a `(Float,Float,Float)->Float` closure called on a tuple — Swift dropped tuple-splat
years ago; never compiled, only surfaced when macOS CI built the lockstep branch for the
first time) → closure now takes the tuple; (2) `CleanupViewModel.cs` was written UTF-8
*without* BOM, failing the `dotnet format --verify-no-changes` CHARSET gate (my headless
gate ran build+test but not format) → re-encoded with BOM, format gate now passes locally
+ in CI. Lesson recorded: add `dotnet format` to the headless gate; macOS Swift only gets
real verification from macOS CI, so merge-then-watch is the loop.


Closed out the scan/cleanup batch: on-hardware test on the RTX 2060, ported the safe fixes to
macOS, tuned RAM++ to "locked in," and consolidated all work onto `main` (branches removed).

- **On-hardware (RTX 2060, 100-photo sample from `G:\TrueNAS\Users`, seed 42).** Built via
  `sample_corpus.ps1`; scanned with the release engine; the test **backed up and restored the
  user's 24,305-file working library** around the run (RESTORE_OK + independently re-confirmed
  24305 rows / 167.5 MB). Results: 100/100 tagged, 0 failed, 974 tags; **`content_hash` set on
  100/100** (Cleanup exact-dupes path is live); **restructure planner SQL ran with no
  DISTINCT error** (D1 verified on real data); tag set **clean — no "catch", no animal
  misclassification**, high-confidence content tags (boy 0.94, child 0.94, basketball 0.97).
- **RAM++ locked in.** The floor raise (0.5→0.62) cut weak tags; the remaining "too generic"
  offenders on the sample were posture/clothing fillers (stand 47×, pose 20×, wear, lay, sit),
  so those + `catch` are now in the built-in `SUPPRESSED_TAGS` (unit-tested, case-insensitive),
  on top of the no-rebuild `ram_plus_suppress.txt` sidecar. `cargo test --lib` green.
- **macOS lockstep (unverified-until-Mac, per apple/CLAUDE.md).** Ported the two mechanical,
  obviously-correct fixes: the identical `GROUP_CONCAT(DISTINCT …)` crash in `Restructure.swift`
  → deduped correlated subquery; Faces-badge removal in `LibraryView.swift` (tile + detail row).
  Consciously NOT ported (documented in DECISIONS): RAM++ tuning (macOS uses Apple Vision, no
  RAM++), Cleanup exact-dupes (macOS engine writes only phash; `content_hash` has no writer +
  BLAKE3 needs a dep), and the phase clamp (macOS `ScanCoordinator` is already one-way).
- **Consolidated to `main`.** Merged `windows-e2e-correctness` (this whole session) and the
  standing `macos-lockstep` branch (commercial-clean SFace/ViT-B/32 swap) into `main`, then
  removed every other branch so only `main` remains. Final headless build green (engine
  clippy+test, app build + both test projects). STATE/NEXT/DECISIONS updated to record the
  on-hardware verify, the macOS lockstep ports, and the consolidation.

## 2026-05-30 (later 2) — Scan/Cleanup UX pass: flicker + RAM++ tags + Faces badge + restructure SQL + exact dupes

Same branch `windows-e2e-correctness`. Second batch of reported Windows issues (Processing
sidebar, tag quality, the gold Faces badge, a DISTINCT crash, Cleanup semantics). All
headless-verified; on-hardware + macOS parity follow-ups remain.

- **A — Processing sidebar flicker — FIXED.** Discovery + tagging `ProgressEvent`s interleave,
  and `EngineClient.Apply` set `Phase` on every one, so the phase label / `PhaseIcon` /
  pipeline dot flipped Discovering<->Tagging several times a second. Added a monotonic
  phase-rank latch (`_shownPhaseRank` + `PhaseRank()`): a ProgressEvent may only ADVANCE the
  shown phase, never regress; `PhaseChangedEvent` / `ScanCompleteEvent` stay authoritative and
  re-sync the latch; reset on StartScan / ClearPhaseAndError / ResetForWipe /
  SetOptimisticScanningPhase. Fixes every consumer with one change.
- **B — RAM++ tag quality — knobs + tuning loop landed (empirical tuning is on-hardware).**
  `models/ram_plus.rs`: new `ram_plus_suppress.txt` sidecar (one tag/line, case-insensitive,
  merged with the built-in const — no rebuild to extend; `#` comments + blanks skipped); added
  `"catch"` to the built-in suppress set; raised the precision floor 0.5->0.62 and made it
  env-overridable (`FILEID_RAMPLUS_PRECISION_FLOOR`, mirrors `FILEID_RAMPLUS_THRESHOLD`). New
  harness: `build/sample_corpus.ps1` (fixed N-photo sample) + `build/tag_report.py` (frequency
  + mean-score histogram + lowest-confidence-accepted list). The "lock in until perfect" loop
  runs against `G:\TrueNAS` (on-hardware).
- **C — gold "Faces" badge removed.** FilePreviewSheet (the pill + its two code-behind refs +
  the "Faces: Detected" metadata row) and the LibraryView tile face overlay. Text/OCR badge
  kept. Diverges from macOS (still shows it) -> macOS follow-up (DECISIONS/NEXT).
- **D1 — restructure "DISTINCT aggregates must have exactly one argument" crash — FIXED.**
  `commands/restructure.rs` used `GROUP_CONCAT(DISTINCT p.name, char(31))`, which SQLite rejects
  at run (separator arg illegal under DISTINCT) -> the Restructure planner threw "Couldn't read
  files table" (a GLOBAL toast, so it read like a Cleanup error). Replaced with a deduped+ordered
  correlated subquery, extracted to a `PLAN_FILES_SQL` const + a unit test that prepares AND runs
  it (the old form prepared but failed at run).
- **D3/D2 — Cleanup = 1:1 bit-identical + previews — FIXED.** `CleanupViewModel` grouped by
  `phash` with Hamming<=4 fuzzy clustering (perceptual near-dupes, not byte-identical; empty
  groups -> nothing to preview). Switched to exact `content_hash` (BLAKE3/composite BLOB, hex)
  + `size_bytes` grouping, O(n) dictionary, dropped the union-find. `DuplicateGroup.PerceptualHash`
  -> `ContentHash`; CleanupView.xaml `Tag` + refresh tooltip updated. Real byte-dupes now populate
  groups, so the existing `ThumbnailService` previews render. Diverges from macOS (phash) ->
  macOS follow-up. Caveat: `content_hash` is full BLAKE3 only <=16 MB (else head+tail+size
  composite) — equality + matching size is "virtually certain identical"; a true byte-compare on
  collision is a possible future hardening.
- **Verified headless:** engine `cargo clippy --all-targets -- -D warnings` exit 0, `cargo test
  --lib` 232/232 (incl. new restructure-SQL + suppress-sidecar tests); app `dotnet build` x64
  GREEN (0 warn / 0 err), FileID.IpcSchema.Tests 34/34, FileID.App.Tests 102/102. On-hardware
  (flicker hold-steady, RAM++ tuning to clean tags, Cleanup byte-dupe groups + thumbnails,
  Restructure tab no-toast) still to run on the RTX 2060 / `G:\TrueNAS`.

## 2026-05-30 — Windows end-to-end correctness pass (P1–P5 landed; UI polish + on-hardware remain)

Branch `windows-e2e-correctness`. Fixing the reported Windows issues: `ram_plus`
startup toast, wrong download modal, out-of-date Deep Analyze, "Wipe partially
failed", Settings cleanup.

- **P1 — `ram_plus` "not registered" toast — FIXED (committed).** Root cause: a
  STALE `FileIDEngine.exe` (running engine predates commit 674da1d which added the
  ram_plus registry arm); the current app sends prewarm("ram_plus") and the old
  engine returns Unknown. Code: prewarm.rs emits user-facing text + a distinct
  `models_dir_unavailable` kind; ModelInstallerService routes `unknown_model` /
  `models_dir_unavailable` to the install slot as "engine out of date — reinstall/
  rebuild". The LIVE toast clears only after a clean engine rebuild
  (build-all.ps1 -Clean -Run). Leaner guard than a build-stamp handshake (DECISIONS).
- **P4 — "Wipe partially failed" DB lock — FIXED (committed).** Cross-process race:
  app deleted fileid.sqlite right after engine exit (3x200ms retry too short). Fix:
  new `wipeLibrary` IPC — engine (sole DB owner) truncates all tables in-process via
  db::wipe_all (sqlite_master-driven, FTS5-safe, preserves grdb_migrations) + clears
  face_crops/thumbs + WAL checkpoint, replies `libraryWiped`; no file deletion.
  SidebarFolderHeader prefers it + auto-rescans; legacy delete path kept as fallback
  with exponential backoff. Schema + Rust ipc/mod.rs + C# DTOs/converters updated.
- **P2 — wrong download modal — FIXED (committed).** WelcomeSheet showed the old
  non-commercial models (ArcFace MobileFace/~13MB/InsightFace, MobileCLIP-S2) and
  had NO RAM++ row (onboarding could never reach AllInstalled, which gates on
  RamPlus; RAM++ downloaded invisibly). Now Face="YuNet + SFace" (Apache-2.0),
  CLIP="ViT-B/32", + new RAM++ row bound to ModelInstallerService.RamPlus; sizes
  bound to the slots.
- **P3 — Deep Analyze naming gate — FIXED (committed).** The gate hard-disabled
  "Analyze All" whenever any face cluster was unnamed; now advisory (macOS two-path):
  the banner suggests naming for sharper captions, but the user can analyze now and
  name later. Optional deferred polish (NEXT): status card with per-model
  not-yet-analyzed counts + ETA, RAM-fit badge, "Smart names -> Review and apply" card.
- **P5 — Settings model cards — FIXED (committed).** Same stale strings as the welcome
  modal: "ArcFace + SCRFD / ~120 MB" -> "Face models (YuNet + SFace) / ~39 MB";
  "MobileCLIP-S2 / ~210 MB" -> "CLIP ViT-B/32 / ~220 MB"; + new RAM++ card bound to
  Svc.RamPlus (Tag="ram_plus" routed through SlotFor). Settings already had logs
  access, recent scans, engine info, performance/NVIDIA, storage, and About — a full
  macOS-style Advanced-disclosure reorg was scoped out as high-risk cosmetic churn
  for this correctness pass (NEXT).
- **Verified headless:** engine `cargo check` + `cargo clippy --all-targets -D
  warnings` + `cargo test` GREEN; app `dotnet build` (x64) GREEN; FileID.IpcSchema.Tests
  34/34 (incl. new WipeLibraryIpcTests for the wipeLibrary/libraryWiped round-trip);
  FileID.App.Tests 102/102. All on branch `windows-e2e-correctness`, working tree clean.

## 2026-05-30 (later) — Butler restructure built (P1–P4) + macOS mirror + docs rewrite + condense

On `butler-overhaul` (off the merged commercial-clean `main`). Implements the butler
redesign from [`RESTRUCTURE.md`](RESTRUCTURE.md) end-to-end and rewrites the dev/docs surface.

- **Butler engine (Windows; verified: clippy `-D` + 230 tests).** `pipeline/restructure_semantic.rs`
  (P1): CLIP+tags+time fusion → density cluster (reuses `identity_clustering`) →
  learn-your-style folder prototypes → proposed moves, wired into `commands/restructure.rs`
  with a rule-cascade fallback. **P2:** c-TF-IDF distinctive-term group naming (live
  local-VLM naming deferred to a background pass — a per-call llama subprocess is too slow
  for an interactive plan). **P3:** per-move confidence bands (auto/review/ask) from
  folder-match strength + top-1−top-2 margin + cohesion, plus a plain-language reason;
  surfaced over IPC + a "What to apply" tier strip (selective apply that holds "ask" back) +
  a drill-down confidence pill + reason. **P4:** Sankey gets the Okabe-Ito CVD-safe palette +
  an "Other" long-tail node (no silent drop).
- **macOS mirror — engine port CI-verified; app-side UI pending.** `RestructureSemantic.swift`
  ports the engine faithfully (reuses `IdentityClustering`); `proposeAll` runs it + stamps
  confidence/reason; IPC `RestructureMove` gains confidence/reason. The macOS CI
  (`swift build --product FileIDEngine/FileID` + `swift test`) compiled the port and passed the
  new parity tests. The app-side UI wiring (reason display, confidence→Keep/Tidy/Reorganize
  mapping, Okabe-Ito Sankey) remains — documented in `platforms/apple/MACOS_BUTLER_NOTES.md`.
- **Docs rewritten from scratch** against verified source: all three `CLAUDE.md` +
  SHIP/PRIVACY/SECURITY/CONTRIBUTING/TESTING/COVERAGE/SYMBOLS/VISUAL-LANGUAGE/BUGS. Honest
  findings surfaced: model-download SHA256 is wired but inert (every `registry.rs` entry is
  `sha256: None`) — now the top open hardening item; the old "Phase 8 coverage gate" was fictional.
- **Condense pass** (engine, behavior-preserving): match-arm merges, if/else→match,
  loop→iterator, push-loop→`extend`.
- **Verified**: clippy `-D warnings`, 230 engine tests, `dotnet build`/`test` (133), `dotnet
  format` (headless), **and all three GitHub workflows green on `main`** — Windows engine,
  Windows app, and macOS (which compiled the Swift port + ran the parity tests). **Not yet**
  verified on-hardware (butler plan quality on `G:\TrueNAS`); the macOS *app-side UI* wiring is
  the remaining Swift work.

## 2026-05-30 — Accuracy tightening + UI fixes + docs refresh + butler-restructure research/design

On `polish-docs-ui-tests` (off the merged commercial-clean `main`).

- **Accuracy (precision bias).** RAM++ `max_tags` 12→8 + a 0.5 precision floor under
  the per-class thresholds — validated on `G:\TrueNAS` (345→243 tags on 27 photos,
  cleaner, still accurate). Deep Analyze CAPTION/RENAME prompts sharpened for
  specificity (decoding already greedy). RAM++ generic-tag suppress-list
  ("face"/"image"/"photo"/…) in the engine + a read-side filter in `ReadStore`
  (legacy DBs need no re-scan).
- **UI.** Root-caused the spurious "faces" chip to RAM++'s 4585-vocab (not C#) and
  fixed it. Sidebar toggle is correct end-to-end on current main (V16.29 fix present +
  wired); added a null-guard for the startup/teardown race. Preview path diagnosed
  sound + bounded (the one full-file-read fallback documented, not blindly rewritten).
- **Docs.** README, `platforms/windows/CLAUDE.md`, `ARCHITECTURE.md` refreshed for the
  commercial-clean stack (RAM++ / ViT-B/32 / YuNet+SFace / Qwen-7B; v1–v12; Apache-2.0).
- **Cleanup + tests.** Removed the unused `DotProductScalar`; +7 engine tests (RAM++
  suppress, registry URL/alias/sentinel invariants, SFace normalize). clippy `-D
  warnings` + 224 engine tests + app tests green; dotnet format clean.
- **Butler restructure.** 5-angle cited deep-research synthesized into
  [`RESTRUCTURE.md`](RESTRUCTURE.md) — cluster-then-name, learn-your-style folder
  prototypes (Dropbox Smart Move pattern), 3-tier confidence, augmented Sankey. 4-phase
  build plan in `NEXT.md`; **P1 (semantic + style engine) is the next build.**
- **Deferred (documented, own pass):** the `Scrfd` reference removal (tested/silenced),
  a comprehensive comment-condense pass, the butler build (P1–P4), and the macOS mirror
  of the faces-tag fix + accuracy tuning.

## 2026-05-29 — Commercial-clean (Apache-2.0) model stack + RAM++ primary tagger (Windows; on-hardware verified)

Branch `windows-ramplus-adopt` (off `main`/V16.29). Adopts **RAM++** as the primary in-scan
tagger and replaces every non-commercial weight with an Apache/MIT one, so the app ships
license-clean under a new root **Apache-2.0 `LICENSE`**. See DECISIONS 2026-05-29 for the why.

**Engine (Rust)** — 6 commits:
- **RAM++** (`models/ram_plus.rs`): Swin-L @384, 4585-tag ONNX (fp16, self-hosted
  `Web-World-Wide/ram-plus-onnx`), per-class thresholds, `FILEID_RAMPLUS_THRESHOLD` override.
  Primary tagger in `pipeline/tagging.rs`; CLIP scene tags gated to fallback. VRAM pool budget
  1500→2000 MB.
- **Faces** (`models/{yunet,sface,face_align}.rs`): YuNet (MIT) detect + SFace (Apache, **128-d**)
  embed + 5-pt similarity alignment to the 112×112 template. `arcface.rs` removed; `scrfd.rs`
  kept as reference. v12 migration wipes face tables. Cluster bands calibrated on-hardware
  (pass1 0.66 / pass3_min_mean 0.60, set in the measured gap between genuine clusters ~0.85+ and
  chained blobs ~0.50) — largest cluster on a 1475-face set cut 90%→7%, known single identity (27
  studio portraits) stays one cluster at mean cohesion 0.93.
- **CLIP** → OpenAI ViT-B/32 (MIT), 512-d (schema unchanged); scene-embedding matrix regenerated.
- **VLM**: Qwen-3B (research-only) dropped → Qwen-7B (Apache) recommended + Mistral-Small-3.2.
- `registry.rs` arms repointed (ids/sentinels kept as stable keys → no install/gate churn).

**App (C#)**: AppSettings v5 migration (default 7B, allowed-VLM allowlist), RAM++ installer
slot, "Face models (YuNet + SFace)" label, display sizes. `dotnet build`/`test`/`format` clean.

**Verify**: clippy `-D warnings` clean; **217 engine tests + app tests green**. **On hardware
(RTX 2060, DirectML EP) against `G:\TrueNAS`** via the new `build/iterate.ps1` + `scan_assertions.py`
harness: faces detect+embed (128-d/512-byte prints), single-person (27/27→1) and multi-person
(11→4, recurring subject grouped) clustering correct, RAM++ tags specific + accurate, HEIC
decodes + tags, all models bind the GPU. Bounded stability soak (2000 files) run.

**Open**: macOS lockstep (WS-MAC, Swift not yet written); rename-heal collapses coexisting
exact-duplicate files (pre-existing, see NEXT.md); throughput re-baseline (DirectML ~6–7 files/s;
CUDA Pack = 3–5× path); SFace cluster-band calibration on labeled faces.

## 2026-05-27 — V16.29 SmolVLM removal, tag-quality diagnostic + threshold + audio duration, sidebar + Deep Analyze fixes

Targeted response to a user-reported triple: (1) tag chips on images/videos/audio "still
suck" — only the year shows; (2) "remove all SmolVLM stuff"; (3) navbar toggle doesn't
collapse + Deep Analyze tab doesn't show downloaded models.

**Engine (Rust)**:
- **SmolVLM dropped**: `VlmModelKind::SmolVlm` enum arm gone in `pipeline/deep_analyze.rs`;
  registry arm in `models/registry.rs` removed; `model_kinds_have_unique_ids` test updated to
  the three remaining kinds (Qwen 3B / 7B, Gemma 3 4B); `size_estimates_increase_with_capability`
  rewritten to compare without SmolVLM's tier. CLIP scene tags become the canonical auto-tagger
  (the comment in `scene_vocab.rs` that called CLIP a "placeholder" is now factually accurate;
  the const docstrings updated to reflect that).
- **Tag-quality diagnostic** (`pipeline/tagging.rs:1244-1290`): `[TAGGING] scene_summary` info
  line per image/video with `scene_emit_count` + `max_score`, and a separate `scene_skipped`
  line when either the labeler or embedding is missing. Gives the user a way to grep the log
  and diagnose why their image cards came back year-only.
- **CLIP scene threshold tuned** (`scene_vocab.rs:128`): `SCENE_COSINE_THRESHOLD` 0.18 → 0.15.
  History on this lever in the file: 0.24 filtered everything → 0.18 showed some chips →
  0.15 biases harder toward recall now that scene tags are the *canonical* auto-tagger.
- **Audio duration chip** (`pipeline/audio_meta.rs`): symphonia exposes `n_frames` +
  `sample_rate` on the default track; emit a "12 min" / "1 h 05 min" / "30 sec" chip even when
  there's no ID3 / Vorbis metadata. Voice memos (`Evernote 20130505 211937.wav` and the like)
  now have a useful chip beyond the year fallback.

**Windows app (C#)**:
- **SmolVLM removed end-to-end**: `ModelInstallerService.Vlm` slot deleted (single VLM concept
  now — `DeepVlm`); `VlmSentinelIds` deleted; `UpdateVlmRecommendation` deleted; `_vlmModelKind`
  field deleted; switch arms in `SlotFor` + `SlotForErrorPath` cleaned. CudaAutoInstaller drops
  the SmolVLM-gated CUDA-defer; downloads run when NVIDIA + engine ready (the 8-concurrent HTTP
  semaphore in the downloader handles contention). EngineClient's post-scan VLM auto-advance
  chain (`AutoTriggerDeepAnalyzeAsync`, `WireVlmInstallWatch`, `OnVlmSlotStatusChanged`,
  `SmolVlmWeightsPresent`) removed — CLIP scene tags are emitted inline during the scan, so no
  separate background tagging pass is needed.
- **DeepAnalyzeView**: SmolVLM card → Gemma 3 4B card (third slot was previously dead UI for
  users who installed Gemma; the model-kind sentinel was tracked but no card existed). All
  card subscriptions + tap routing switched to the `DeepVlm` slot (which already tracked
  Qwen / Gemma installs).
- **WelcomeSheet**: SmolVLM-tagger row removed; the 4-row layout is now CLIP · ArcFace ·
  Qwen Deep Analyze · GPU pack. CLIP comment updated to acknowledge it powers both semantic
  search AND scan-time scene tags.
- **AppSettings v3 → v4**: `DisableAutoInstallSmolVlm` property dropped; `AutoChainDeepAnalyze`
  property dropped (post-scan VLM auto-chain is gone). `AllowedVlmKinds` no longer contains
  `"smolvlm"`. Schema migration v3 → v4 flips any leftover `SelectedVlmModelKind = "smolvlm"`
  to `qwen2_5_vl_3b` with a log line. Tests in `AppSettingsTests` updated (schema 3 → 4, the
  `DisableAutoInstallSmolVlm` assertion removed).
- **Settings view**: "Tag automatically with AI after scans" toggle removed (the underlying
  AutoChainDeepAnalyze setting is gone). Sentinel-based VLM-installed migration switched to
  DeepVlm slot + drops smolvlm from the sentinel-id list.
- **Sidebar collapse fix** (`MainWindow.xaml.cs::ApplySidebarVisibility`): `SidebarColumn`
  XAML defines `MinWidth="240" MaxWidth="320"`; setting `Width = 0` to collapse was being
  silently clamped to 240px by MinWidth. Now clear `MinWidth = 0` BEFORE `Width = 0` on
  collapse, and restore `MinWidth = 240` BEFORE `Width = 260` on expand.

**macOS app**:
- `AIModelKind.smolvlm` enum case dropped from `apple/shared/.../AIModels.swift`; switch arms
  exhaustiveness preserved everywhere; `safeDefaultFor(ramGB:)` fallback now Qwen2.5-VL 3B.
  Engine-side `DeepAnalyze.swift::vlmConfig` + `gpuCacheBudgetMB` arms removed. Package.swift
  comment + CLAUDE.md model table + `wipe_local_state.sh` doc updated.

**Docs**:
- Current-state docs (ARCHITECTURE.md, MODELS.md, README.md, both CLAUDE.md, PHASES.md) lose
  SmolVLM from the model lineup tables and prose.
- Historical entries in DECISIONS.md, NEXT.md, STATE.md left intact — they document the V16.X
  architecture as it was at the time, per the append-only convention.

### Build/test (local, in-agent)
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test --lib` → **212 passed, 0 failed**.
- `dotnet build` → 0 warnings, 0 errors.
- `dotnet format FileID.sln --verify-no-changes` → clean (pre-push gate per V16.28 memory).
- `dotnet test FileID.App.Tests` → **101 passed, 0 failed** (V16.28 was 102; -1 for the
  SmolVLM InlineData entry in WelcomeSheetModelSizeTests).
- `dotnet test FileID.IpcSchema.Tests` → **31 passed, 0 failed**.

### On-hardware verify (gated on user)
- Rescan a folder of mixed kinds. Grep engine log for `[TAGGING] scene_summary` — every image
  should have `scene_emit_count >= 1` (with threshold 0.15 most photos clear it). Cards should
  show scene chips, not just year. If you still see year-only, check `scene_skipped` lines —
  they'll tell us whether the embedding or labeler is missing.
- Audio cards should show a duration chip (`12 min`, `1 h 05 min`) even on voice memos.
- Click the title-bar hamburger — the sidebar should collapse all the way to zero width.
- Deep Analyze tab now shows three cards: Qwen 3B (recommended), Qwen 7B, Gemma 3 4B. Install
  any of them; the card should flip to "Installed" once the download lands.

## 2026-05-26 — V16.28 hardening pass: OCR overflow, thumbnail-cache LRU, bulk-select batching, tile hover (Windows)

Targeted security/perf/parity pass on top of V16.27. No new features; the goal was to land concrete
fixes for issues surfaced by a code audit while pushing back on the audit items that turned out to
be wrong (`restructure_apply.rs` "unwraps" are test-only; `platform.rs:389` already uses
`unwrap_or`; `LibraryView.swift:506` is the kind-filter chip animation, not the tab switcher —
tab crossfade already matches at 0.22s).

**Engine (Rust)**:
- **OCR dimension overflow defense** (`engine/src/shell/ocr.rs`): `recognize` now caps each side
  at 16384 before any multiplication, so `width * height * 4` cannot overflow u32 and
  `SoftwareBitmap::CreateCopyFromBuffer`'s i32 dim parameters stay in range. Added 3 unit tests
  (zero dim, oversize dim, short buffer); all early-bail before any Windows API call.
- **Keyword extractor tidy** (`engine/src/util/keywords.rs:44`): replaced
  `u32::try_from(phrase.len()).unwrap_or(u32::MAX)` with `phrase.len() as u32`. Phrase length is
  bounded by the doc-extract 16 MB cap upstream; the saturating-cast defense was dead code.
- **OCR public-API comment** (`engine/src/shell/ocr.rs:13-25`): `OcrResult.lines` /
  `OcrResult.locale` / `OcrLine` are populated but not yet consumed. Replaced bare
  `#[allow(dead_code)]` with a one-line comment naming the future consumer (per-line OCR overlay)
  so the next maintainer knows why the surface is intentionally fat.

**Windows App (C#)**:
- **ThumbnailDiskCache: in-memory LRU index** (`FileID.App/Services/ThumbnailDiskCache.cs`): the
  previous sweep walked `EnumerateFiles("*.bin", SearchOption.AllDirectories)` on every cap trip
  — O(N) disk IO on libraries with 10K+ cached thumbnails. Replaced with a
  `ConcurrentDictionary<string, CacheEntry>` index seeded once at startup by `Prime()`. Reads
  touch `LastAccessTicks` in memory (no more `SetLastAccessTimeUtc` syscall per cache hit);
  writes update the index and recompute `_cachedBytes` by delta. On cap exceed, sort the
  in-memory index by ticks and delete oldest until under headroom — zero filesystem walks after
  startup. Eviction policy is factored into a pure `SelectEvictions(...)` helper covered by 4
  unit tests in `Tests/FileID.App.Tests/ThumbnailDiskCacheTests.cs`.
- **LibraryViewModel: bulk-selection batching** (`FileID.App/ViewModels/LibraryViewModel.cs` +
  `Views/Library/LibraryView.xaml.cs`): `OnTilePropertyChanged` was firing two PropertyChanged
  events + a `SelectionRegistry` republish on every per-tile `IsSelected` toggle. Ctrl+A on 10K
  tiles burned 20K notifications and 10K `_selected.ToList()` allocations (`SelectedItems`
  getter). New `BulkSelectionScope()` IDisposable wraps the three bulk-mutation sites in
  `LibraryView.xaml.cs` (Ctrl+A / `OnSelectAllClicked`, shift-click range select, plain-click
  clear-all). Per-tile handler still updates `_selected` but defers notifications under the
  scope; on dispose, fires one batch. `SelectedItems` now caches the list snapshot and
  invalidates on real change. `ClearSelection()` rewired through the same scope.
- **Tile hover stroke animation** (`Views/Library/LibraryView.xaml` + `.xaml.cs`): macOS tiles
  ramp their white stroke 0.08 → 0.18 opacity over `easeOut(0.18s)` alongside the existing
  scale (LibraryView.swift:676-680). Windows tiles were animating scale only. Replaced the
  Grid's themed `BorderBrush` with an inline `SolidColorBrush` per tile (so each instance owns
  an animatable opacity), and added `ApplyTileStrokeOpacity` — a `Storyboard` + `DoubleAnimation`
  with `CubicEase EaseOut` that runs alongside the scale spring. Shadow opacity animation
  (0.18 → 0.45, blur 5 → 14) is deferred since it needs per-tile `Composition.DropShadow`
  plumbing with cleanup on tile recycle.
- **ReadStore.cs: pre-existing Span-in-async fix** (`FileID.App/Services/ReadStore.cs:303`): the
  V16.27 in-flight work had introduced `MemoryMarshal.Cast<byte, float>` inside an `async`
  method, which is a C# 13 preview feature unsupported under .NET 8's stable language version
  (CS8652). Extracted the cast into a sync `BlobToFloats(byte[]) -> float[]` helper at the
  same level as `DotProduct`. Pre-existing blocker, not a regression from this session — the
  V16.27 build was broken on disk until this fix.

**ReadStore search query audit** (B3, audit-only): `SearchAsync` at `ReadStore.cs:144-166`
OR-joins six branches. The `ocr_fts` / `doc_fts` MATCH branches are fast (FTS5-backed). The four
`LIKE '%x%'` branches (`f.path_text`, `f.vlm_proposed_name`, `f.vlm_description`, `tags.tag`,
`persons.name/first_name/last_name`) are non-sargable — any one of them forces SQLite into a
files-table full scan, and indexes won't help leading-wildcard LIKE. The real fix is a migration
v8 that extends `doc_fts` (or adds a new `text_fts`) covering `path_text`,
`vlm_proposed_name`/`description`, `tag`, and `person_name` so the query becomes MATCH-only. Out
of scope this session — needs the user's real library to validate the migration. Surfaced as a
NEXT.md follow-up.

**Comment surgery** (D2, narrow): cleaned the LibraryViewModel header (was mangled with a stray
"The shape is the same:" run-on), trimmed the redundant "detach listeners" prose in `Dispose`,
and compressed the per-tile-PropertyChanged-forwarding comment to keep just the WHY (the "VM's
SelectedCount stayed silently stale" bug rationale). Other V16.27 files (`tagging.rs`,
`doc_extract.rs`, `audio_meta.rs`, `ReadStore.cs`) were inspected; no slop worth churning over —
the comments there are WHY-style technical notes (cross-references to SwiftUI line numbers,
performance pitfalls, invariant statements) that map cleanly to CLAUDE.md's keep-WHY rule.

### Build/test (local, in-agent)
- `cargo +1.90 check` clean. `cargo +1.90 clippy --all-targets -- -D warnings` clean.
- `cargo +1.90 test --lib` → **212 passed, 0 failed** (V16.27 was 209; +3 OCR overflow tests).
- `dotnet build src/FileID.App/FileID.App.csproj` → 0 warnings, 0 errors.
- `dotnet test Tests/FileID.App.Tests/` → **102 passed, 0 failed** (V16.27 was 98; +4
  `ThumbnailDiskCacheTests.SelectEvictions_*` tests).
- `dotnet test Tests/FileID.IpcSchema.Tests/` → **31 passed, 0 failed**.

### On-hardware verify (gated on user)
Same gates as V16.27 still pending. Additionally:
- Scroll a library with 10K+ thumbnails; the previous 30s "cache sweep" pause should be gone (no
  more directory walk after startup).
- Ctrl+A in a 10K-tile library: selection should land instantly, not over multiple seconds.
- Hover a Library tile: stroke should brighten from a faint 0.08 to a clear 0.18 over 0.18s,
  matching the macOS tile hover affordance.

## 2026-05-26 — V16.27 scan-pipeline single-read finalization + UI parity polish (Windows)

Pipeline I/O consolidation on top of V16.26, paired with two surgical UI-parity fixes the macOS
audit surfaced.

**Engine (Windows, `pipeline/tagging.rs` + `doc_extract.rs` + `audio_meta.rs`)**:
- **EXIF ghost-read fix**: `run_decoder_thread` now seeds `exif_data = Some((None, None, None))`
  on every successful image `read_to_end`, so the worker's `parse_exif_blocking` fallback is
  unreachable for images. Every non-EXIF format (PNG, GIF, screenshots, etc.) skips one wasted
  re-open + re-fail per file. `parse_exif_blocking` deleted as dead code.
- **Doc / PDF / Audio single-read**: extended the image-style pre-read pattern to Doc/Pdf/Audio
  kinds (files ≤ `FULL_HASH_MAX_BYTES` = 16 MB). The decoder thread reads once, hashes from the
  buffer, and threads `Option<&[u8]>` into the kind-specific extractor. `doc_extract::extract`
  and `audio_meta::extract` now accept `bytes: Option<&[u8]>` and dispatch internally:
    - `doc_extract`: zip helpers refactored to generic `<R: Read + Seek>` inner functions that
      take either a `File` or a `Cursor<&[u8]>`; plain-text path uses `String::from_utf8_lossy`
      on the buffer when supplied.
    - `audio_meta`: tiny `BytesMediaSource` adapter wraps `Cursor<Vec<u8>>` with symphonia's
      `MediaSource` trait (declares seekable + byte_len).
  Worker's `content_hash` fallback is unchanged — still fires correctly for video (codec API
  needs a path), unrecognized kinds, and the > 16 MB long-tail.

**Windows UI parity**:
- **ApplyBar hover spring** (`RestructureView.xaml.cs`): wired four `PointerEntered`/`PointerExited`
  handlers on `ApplySymlinkButton` + `ApplyMovesButton` via the existing `SpringEasing.AnimateScale`
  helper, mirroring macOS `RestructureApplyBar.swift:114-117` (response: 0.28, dampingFraction: 0.7,
  scale 1.02 on hover-while-enabled). The XAML comment had promised this; now it matches.
- **TagChip Kind brushes** (`Theme.xaml`): defined `TagChipKindForegroundBrush` (#FFFFFF) and
  `TagChipKindBackgroundBrush` (#808080 @ 0.30) so `TagChip.xaml.cs:74-75` no longer silently
  falls through to hardcoded values. Latent footgun closed.
- **TagChip.FormatTag macOS-parity fix** (`TagChip.xaml.cs:135`): the C# port used
  `ToTitleCase(ToLowerInvariant(...))`, which mangled internal capitals — `iPhone-14` → `Iphone 14`
  vs macOS `LibraryView.swift:646-652` `first.uppercased() + dropFirst()` → `IPhone 14`. Rewrote
  to match the Swift implementation exactly: pre-formatted space-bearing labels pass through, only
  the leading character of the final segment is uppercased, internal model-number casing is
  preserved. Adds an early `Contains(' ')` guard so `"Has TEXT"` stays as-is (previously it would
  have title-cased to `"Has Text"`). Test `FormatTag_MatchesMacParitySpec(iPhone-14, IPhone 14)`
  now passes — was failing on HEAD.

**Repo hygiene**:
- `.gitignore`: stray `onnxruntime.dll` / `onnxruntime_providers_shared.dll` under
  `src/engine/` (fetch-runtime-deps.ps1 sometimes drops them next to the binary for local dev).
- Staged `scene_embeddings_precomputed.rs` (real source — `scene_vocab.rs:35` includes it). The
  include is now wrapped in `mod scene_embeddings { ... } pub use scene_embeddings::SCENE_EMBEDDINGS;`
  with `#[allow(clippy::excessive_precision)]` so the precomputed CLIP rows stay byte-faithful with
  the source notebook without spamming 5 884 lint suggestions.
- `downloader.rs` SHA streaming: heap-allocated the 64 KB chunk buffer (`vec![0u8; 65536]`
  instead of `[0u8; 65536]`) so the async future doesn't balloon to ~67 KB and propagate
  `clippy::large_futures` errors through `prewarm.rs` callers. Pure quality fix; preserves the
  user's in-flight streaming-SHA logic exactly.
- `xml_text_runs` in `doc_extract.rs`: collapsed the nested-`if` into a match guard
  (`Ok(Event::Text(t)) if depth > 0 =>`) to silence the new `clippy::collapsible_match` lint.

### Build/test (local, in-agent)
- `cargo +1.90 check` clean. `cargo +1.90 clippy --all-targets -- -D warnings` clean against
  the full working tree (engine + user's in-flight edits). `cargo +1.90 test --lib` →
  **209 passed, 0 failed** (up from V16.26's 204 — added bytes-vs-path equivalence tests for
  `doc_extract` (txt + docx) and `audio_meta`, plus a sanity test for the new
  `BytesMediaSource` adapter). `dotnet build FileID.sln -c Debug` → 0 errors, 0 warnings.
  `dotnet test Tests/FileID.App.Tests/` → **98 passed, 0 failed** (the
  `FormatTag_MatchesMacParitySpec(iPhone-14, IPhone 14)` regression that was failing on HEAD
  is now green).  `dotnet test Tests/FileID.IpcSchema.Tests/` → **31 passed, 0 failed**.

### On-hardware verify
- Scan a library with PNG + GIF + JPG + docx + mp3 + pdf + a > 16 MB file. JPEGs surface
  camera/GPS in the preview metadata; PNGs scan without crash; docs/audio surface keyword chips
  / artist+album tags; > 16 MB file exercises the composite-hash fallback successfully.
- Restructure tab: hover the gold "Apply as shortcuts" and outlined "Convert to real moves" —
  both spring up to ~1.02× and settle. Disabled state stays at 1.0.
- Library Kind chips render visually identical to before (theme brushes match the previous
  hardcoded hex).

## 2026-05-22 — V16.26 no-self-host policy + hanging-feature sweep + PDF / HNSW / BGE unhang

Hardened-policy pass on top of V16.25: every artifact the engine downloads must already exist on
a public upstream (HuggingFace, ggml-org GitHub releases, NVIDIA developer CDN). No FileID-hosted
files. Plus a sweep that wires three previously-dormant modules.

**Removed (would require self-hosting; legal + sustainability exposure)**:
- **RAM++ integration** — `models::ramplus`, the scan-pipeline block, `ModelStack.ramplus`, the
  registry arm, `shared/scripts/convert_ramplus_onnx.py`, the `MODELS.md` entry. No public RAM++
  ONNX exists — only the official PyTorch `.pth` on `xinyu1205/recognize-anything-plus-model`.
  Image tagging stays on the V16.21 VLM tagger (SmolVLM / Qwen2.5-VL / Gemma 3) exactly as shipped.
- **Performance-Pack registry arms** (`cuda_pack_x64`, `openvino_pack_x64`, `qnn_pack_arm64`)
  plus the `LookupResult::NotYetAvailable` variant + `not_yet_available()` helper they used. The
  engine still picks up the matching execution providers when the user has the SDK DLLs on the
  loader path (system CUDA toolkit via `runtime::system_cuda_toolkit_dir`; user-installed Intel
  OpenVINO redist; Snapdragon's bundled QNN runtime). cuDNN + llama.cpp runtimes remain bundled
  (both publicly redistributable: NVIDIA developer CDN + ggml-org GitHub releases).
- **YAMNet (Phase 5b)** — same hosting blocker as RAM++ (no public general ONNX). Documentation
  removed.

**Unhung (modules previously gated behind `allow(dead_code)` now have real callers)**:
- **HNSW into `face_clustering`** above 5 k faces — turns O(n²) all-pairs cosine into O(log n)
  per query. Uses `instant-distance` (pure-Rust); the brute-force path still wins ≤ 5 k.
- **PDF text extraction** added to `doc_extract` via the gated `pdfium-render` binding (same
  binding `deep_analyze` already uses for rasterization).
- **BGE-small text embeddings** (`models::bge_text`) registered + loaded in `ModelStack` +
  invoked in `process_file_predecoded` for doc text + persisted into `text_embeddings` (new
  migration v11). The pure-Rust WordPiece tokenizer is now live via BGE.

**Tagging promise vs V16.21 — strictly better-or-equal, never worse**:
- Images: same (VLM tagger).
- Documents: strictly new (RAKE keyword chips + FTS5 + BGE semantic search; was zero before).
- Audio: strictly new (artist / album / title / genre / year chips; was zero before).
- Faces: same accuracy, faster above 5 k.
- Rename/move: tags preserved (was orphaned).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test --lib` → **204
  passed, 0 failed**. C# `dotnet build FileID.sln -c Debug` → 0 warnings, 0 errors.

### Documented follow-ups (in-policy; no self-hosting needed)
- USN reader (`FSCTL_READ_USN_JOURNAL`) + scan-skip-set integration.
- Whisper.cpp subprocess transcription (whisper.cpp binaries on ggml-org GitHub + GGUF Whisper
  models on HuggingFace — fully publicly downloadable).
- Florence-2 inference: 4 ORT sessions + Rust autoregressive generation loop + `tokenizers`
  crate + Deep Analyze backend `modelKind: "florence2_base"`.
- General image multi-label tagger: hold pending a public, clean-licensed, general-purpose ONNX
  (WD-Tagger family is anime-trained → bad for typical user photos; RAM++ has no public ONNX).

## 2026-05-22 — V16.25 research-implementation Phases 3–7: identity, docs, audio, variants, Florence-2

Five phases land on top of V16.24 (Phases 0–2 + content_hash brick from earlier today).

**Phase 3 — identity / USN / vector index.**
- **Rename/move heal**: BLAKE3 `content_hash` + Win32 MFT `file_ref` columns (migration v8),
  computed in discovery/tagging, dbwriter does a pre-INSERT lookup + `UPDATE OR REPLACE` so a
  renamed/moved file re-binds to its existing row instead of orphaning tags / embeddings / faces /
  OCR.
- **USN journal foundation**: `util::elevation::is_elevated` + `pipeline::usn::query_journal`
  (`FSCTL_QUERY_USN_JOURNAL`) + v9 `usn_state` cursor table. Scan-driver integration is Phase 3b;
  the default scan stays on the verified jwalk + timestamp-skip path.
- **Vector index**: pure-Rust HNSW via `instant-distance` — no C/C++ build dep (`usearch` rejected
  for that reason). `util::hnsw_index` build/search wrapper + tests; face_clustering integration
  above ~5 k faces is Phase 3c.

**Phase 4 — document content pipeline.**
- Pure-Rust text extraction (`pipeline::doc_extract`) for txt / md / docx / pptx / xlsx via the
  existing `zip` + new `quick-xml` 0.36. PDF text extraction is Phase 4b (re-uses the gated
  `pdfium-render` binding).
- RAKE-style keyword extraction (`util::keywords`) → `source='auto'` tag chips, no ML model.
- Migration v10: `doc_text` + `doc_fts` (FTS5) — same shape as `ocr_text` / `ocr_fts`.

**Phase 5 — audio metadata.**
- `pipeline::audio_meta` reads artist / album / title / genre / year via `symphonia` (pure-Rust,
  MPL-2.0, no system ffmpeg) → `source='auto'` chips. Audio libraries get real content-style tags
  today. YAMNet sound-event tagging + Whisper transcription are Phase 5b (both need offline ONNX
  conversion, same Python-3.14 constraint that gated RAM++).

**Phase 6 — per-vendor quantized variants.**
- Framework landed in Phase 1 (`models::variants` + pack-presence gating). This phase = explicit
  documentation that per-model accelerated variants (`_int8` for OpenVINO/Intel-NPU, `_qnn.bin` for
  Snapdragon HTP) ship alongside each model's base hosting; the resolver falls back to fp32 when
  the variant file is absent, so untested NPU hardware safely runs on DirectML/CPU.

**Phase 7 — Florence-2 foundation.**
- `models::florence2` skeleton + a real registry arm for `onnx-community/Florence-2-base` (4 ONNX
  files + tokenizer + config, ~440 MB total, MIT). Users can install today; the inference wiring (4
  ORT sessions + Rust autoregressive generation loop + `tokenizers` crate for the BART tokenizer +
  Deep Analyze backend `modelKind: "florence2_base"`) is Phase 7b — the plan ranked it last and
  defer-able since SmolVLM / Qwen / Gemma + RAM++ + Windows.Media.Ocr cover everything except
  phrase-grounded OD.

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` green across the full
  suite. 10 migrations applied (`v1`–`v10`); new tests: HNSW round-trip + composite hash edges +
  RAKE keywords + doc_extract OOXML + audio_meta dedup + florence2 paths + v8/v9/v10 schema spot-checks.
- **Needs user hardware:** Phase 0 long-path / OneDrive online-only / file-lock retry; CPU
  multi-threading uplift (Phase 1); rename-heal across a real move; doc/audio tag chips render.

### Documented follow-ups (foundation present; full integration deferred)
- **Phase 3b**: USN reader (`FSCTL_READ_USN_JOURNAL`) + scan-skip-set integration.
- **Phase 3c**: HNSW into `face_clustering` above ~5 k faces.
- **Phase 4b**: PDF text extraction (re-use existing pdfium binding); BGE-small text embeddings for
  semantic doc search; GLiNER NER for entity tags.
- **Phase 5b**: YAMNet sound-event tagging + Whisper transcription (both need offline ONNX hosting).
- **Phase 6 hosting**: per-model `_int8` (OpenVINO) + `_qnn` (Qualcomm AI Hub) variant files.
- **Phase 7b**: Florence-2 inference (4 ORT sessions + generation loop + `tokenizers` dep + Deep
  Analyze grounded-OD backend).
- **RAM++ activation**: run `shared/scripts/convert_ramplus_onnx.py` on **transformers 4.x / Python
  3.11–3.13** to produce + host the ONNX (Python 3.14 / transformers 5 blocked locally).

## 2026-05-22 — V16.24 research-implementation Phase 2: RAM++ tagging (+ Phase 3 kickoff)

- **RAM++ wrapper + pipeline** (`models/ramplus.rs`): 384px ImageNet-norm input → per-tag logits →
  sigmoid + per-tag calibrated threshold → `(tag, score)` (`source='auto'`). Wired into the scan
  fast pass right after the CLIP embed as the **primary scan-time tagger when installed**, gated
  behind the existing "model missing → stage skips" path — **zero regression**: the VLM tagger stays
  default until RAM++ is present. Single VRAM-bounded Session (batch-coordinator perf is a noted
  follow-up). I/O tensor names read from the session (robust to re-export). Supersedes the CLIP
  zero-shot scene labeler. Variant-aware load via `models::variants` (Phase 1).
- **Offline conversion**: RAM++ has no first-party ONNX. `shared/scripts/convert_ramplus_onnx.py`
  exports the `generate_tag` image→logits path (opset 17, einsum-vectorized) + copies the tag list +
  thresholds; `MODELS.md` + `DECISIONS.md` document hosting. Registry arm `"ramplus"` is
  `not_yet_available` until hosting lands; a locally-converted `ramplus.onnx` in `Models\ramplus\`
  is picked up directly.
- **Local conversion attempt — blocked (documented)**: the only local interpreter is Python 3.14,
  which forces transformers 5.x; the 2023 RAM++ stack targets transformers 4.x. The script's bundled
  compat shims clear all imports + reach model construction, but full v5 support isn't worth chasing.
  Run the script on **transformers 4.x / Python 3.11–3.13** for a clean export. App behavior is
  unchanged meanwhile (RAM++ gated off). Toolchain (torch/transformers/timm/scipy) was installed into
  the user Python; RAM++ source + weights are cached under `%TEMP%`.
- **Phase 3 kickoff**: `util::content_hash` — BLAKE3 content identity (full ≤ 16 MB; head+tail+size
  composite above) for rename/move rebind. `blake3` dep added (pure-Rust, no C/C++ build).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **184 passed, 0
  failed** (177 after Phase 1, +3 RAM++ wrapper, +4 content-hash incl. composite-path edge cases).

## 2026-05-22 — V16.23 research-implementation Phase 1: ML/hardware foundation

Shared plumbing every later phase builds on. Engine-only; no new dependencies.

- **`runtime::active_provider()`** — cached (`OnceLock`) single source of truth for which EP this
  process binds, driving the two helpers below.
- **`runtime::configure_session_builder()`** — replaces the hardcoded `.with_intra_threads(1)` in all
  four model wrappers (ArcFace / SCRFD / MobileCLIP / CLIP-text). Graph-opt Level3 everywhere except
  QNN (Level1/Basic — the HTP partitioner rejects ORT's aggressive fusion); intra-op threads =
  performance-core count on the **CPU EP** (CPU-only boxes were single-threaded before — a real
  throughput uplift) while staying 1 on GPU/NPU EPs.
- **`models::variants::resolve_model_path()`** — per-EP quantized-variant selection (`_int8` for
  OpenVINO/Intel-NPU, `_qnn.bin` for Snapdragon HTP) with **fp32 fallback when the variant file is
  absent**, so untested hardware always runs the universal graph (DirectML → CPU) rather than failing.
  Consumed by the Phase 2+ models.
- **`models::wordpiece_tokenizer`** — pure-Rust BERT WordPiece (no `tokenizers` crate) for the
  upcoming GLiNER + BGE text models.
- **QNN HTP backend** — `execution_providers_for_chain` now binds `QnnHtp.dll` for the Snapdragon NPU
  (falls through to DirectML/Adreno if the pack is absent). OpenVINO's NPU `device_type` hint + INT8
  variants are deferred to Phase 6 (need NPU detection; can't regress Intel-GPU users untested).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **177 passed, 0
  failed** (+10: 4 variant-resolution incl. fp32 fallback, 6 WordPiece).
- **Needs user hardware:** confirm CPU-only inference now uses multiple threads (faster scan where no
  usable GPU); QNN/OpenVINO NPU paths await Snapdragon/Intel hardware + the Phase 6 variants.

## 2026-05-22 — V16.22 research-implementation Phase 0: robustness + doc accuracy

First slice of the approved multi-phase plan to implement the "local high-accuracy file tagging"
research (`~/.claude/plans/i-want-to-implement-radiant-sunset.md`). Phase 0 is engine-side robustness
+ the report's pitfall fixes; no new dependencies.

- **Long paths (>260).** The engine `.exe` has no long-path manifest, so deep directories were
  invisible to the scan and deep files failed to open. `discovery` now walks a `\\?\`-verbatim root
  (children inherit it; jwalk traverses past MAX_PATH), stores normal-form paths (verbatim stripped on
  emit — DB / UI / cross-platform parity preserved), and reconverts to extended-length at the FS-access
  sites (image decode + EXIF). New `util::path_safety::{to_extended_length, strip_extended_length}`
  (+ 4 round-trip tests).
- **OneDrive / cloud placeholders.** Discovery flags `online_only` from the file attributes
  (`OFFLINE` | `RECALL_ON_OPEN` | `RECALL_ON_DATA_ACCESS`); the decoder skips content reads for those
  files (metadata-only row) so scanning never silently hydrates a multi-GB cloud download — both a perf
  and a no-telemetry-egress concern.
- **File-lock resilience + AV-friendliness.** Image opens go through `open_image_file`: 3-attempt
  retry-with-backoff on `ERROR_SHARING_VIOLATION` / `LOCK_VIOLATION`, opened with
  `FILE_FLAG_SEQUENTIAL_SCAN`.
- **Doc accuracy.** `platforms/windows/CLAUDE.md` no longer claims "Phase 0 ships only the engine"
  (everything it listed as deferred shipped by V16.21); MSRV corrected 1.78 → 1.90. Fixed a pre-existing
  `useless_conversion` clippy warning in `shell/tags.rs`.

### Build/test (local, in-agent)
- Engine: `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **167 passed,
  0 failed** (+4 long-path round-trip tests). App: `dotnet build FileID.sln -c Debug` → 0/0.
- **Needs user hardware:** a real scan over a >260-char path tree and a OneDrive online-only folder
  (confirm deep files get analyzed + stored with normal-form paths; online-only files get metadata-only
  rows and trigger no download).

## 2026-05-22 — V16.21 welcome models, discrete-GPU forcing, tag quality, progress flicker

Six Windows fixes spanning the WinUI app + Rust engine:

- **No more silent SmolVLM download.** Deleted `SmolVlmAutoInstaller` and its `App.xaml.cs` hook +
  `EngineClient` re-arm — model downloads are now strictly user-initiated (welcome screen / Deep
  Analyze tab). First-scan auto-tagging still resumes the moment SmolVLM is installed (the
  `WireVlmInstallWatch` path is unchanged).
- **Welcome screen offers a hardware-tiered Deep-Analyze model.** Split the single VLM row into two:
  the SmolVLM **tagger** row and a new **Qwen** Deep-Analyze row sized to the box
  (`ModelInstallerService.DeepVlm` slot + `UpdateDeepVlmRecommendation`: ≥16 GB RAM **or** ≥8 GB
  VRAM → Qwen 7B, else 3B). Installing it persists `AppSettings.SelectedVlmModelKind` so the Deep
  Analyze tab agrees. `Install all` now covers both VLM rows; `SlotFor`/sentinels split smolvlm→Vlm,
  qwen/gemma→DeepVlm.
- **Better image tags.** `"Has Location"`/`"Has Text"`/`"Has Faces"` capability tags are no longer
  emitted (`push_enriched_extras`) — they read as content but described a capability and crowded out
  real tags. `TAG_PROMPT` rewritten for 1–2 specific concrete tags; `parse_vlm_tags` caps at 2 and
  drops a generic-token stop-list (`photo`/`object`/`location`/…).
- **Discrete GPU forced.** `probe_gpu_vendor` now returns the DXGI adapter index of the highest-VRAM
  non-software adapter; `execution_providers_for_chain` pins DirectML to it via `with_device_id`
  (the scan path: CLIP/ArcFace/SCRFD). CUDA stays default (the iGPU isn't CUDA-visible). For
  llama.cpp (Deep Analyze) a best-effort `--list-devices` probe pins `--device VulkanN` only when a
  clearly-dominant (≥2 GiB) discrete device exists — no-op on CUDA builds / single-GPU boxes.
- **Download progress no longer flickers.** Welcome + Settings model rows now use one `ProgressBar`
  (indeterminate → determinate at first byte) instead of swapping a `ProgressBar`↔`ProgressRing` on
  every `Fraction`-crosses-0; the sidebar scan bar latches `IsIndeterminate=false` once the file
  total is known.

### Build/test (local, in-agent)
- Engine: `cargo +1.90 clippy --all-targets -D warnings` clean; `cargo +1.90 test` → **163 passed, 0
  failed** (new tests: `parse_vlm_tags` cap/stop-list, `parse_best_vulkan_device`). (Running clippy
  from the repo root picks `stable` 1.95 and surfaces unrelated toolchain-drift lints — use `+1.90`.)
- App: `dotnet build FileID.sln -c Debug` → **0 warnings, 0 errors**.
- **Needs user hardware:** discrete-GPU forcing (verify dGPU load in Task Manager during a scan +
  llama.cpp device log), the welcome flow end-to-end, and that tags read as 1–2 descriptive words.

## 2026-05-22 — V16.20 push V16.16–V16.19 + clear two pre-existing CI reds

Committed and pushed the session's work (CLIP split, crash fix, Deep Analyze gating, preview
nav/video, Restructure auto-gen, Cleanup thumbnails, docs trim) to `origin/main`. Two pipelines
were already red before this push and are fixed here:
- **Engine** `Privacy — source URL allowlist scan` (x64) had failed since `models/vlm_server.rs`
  landed — it formats `http://127.0.0.1:{port}` for the local llama-server and `127.0.0.1` wasn't
  allowlisted. Fixed by exempting loopback hosts in the scan (loopback is never egress; see
  DECISIONS V16.20). arm64 was always green (the scan is x64-only).
- **App** `Format check` (x64) had failed on `Add braces to 'if' statement` (IDE0011); the brace
  fix was already in this session's tree, so `dotnet format --verify-no-changes` is clean now.

### Build/test (local, pre-push)
- Engine: `cargo +1.90 fmt --check` + `clippy --all-targets -D warnings` + `test --all-targets`
  all green; URL-allowlist scan replicated locally → PASS.
- App: `dotnet build -c Release -p:Platform=x64` → 0 errors; `dotnet format --verify-no-changes` clean.

## 2026-05-21 — V16.19 macOS parity: Restructure auto-generates + Cleanup thumbnails

- **Restructure auto-generates** (macOS RestructureView.swift `.task`/`.onChange`): no manual
  "Generate plan" click. `RestructureView.OnLoaded` renders an already-computed plan (cached on
  the engine across tab switches) or, if none, auto-runs `PlanRestructureAsync` when a library
  folder is scanned; it also re-generates on `DeepAnalyzeComplete` so the People/<name> buckets
  reflect newly-named clusters. The Generate button stays as a manual re-gen.
- **Cleanup shows thumbnails** (macOS CopyTile parity): each duplicate group is now a
  horizontal strip of 132-px thumbnail tiles (thumbnail + filename + size + Keep radio) instead
  of text rows. Tiles load lazily through `ThumbnailService` via the members ItemsRepeater's
  `ElementPrepared`/`ElementClearing` (cancel + release on recycle) — the same
  virtualization-friendly pattern LibraryView uses. `DuplicateMember` gained `Thumbnail` +
  `ShowPlaceholder` + recycle guards.

### Build/test
- C# `dotnet build` 0/0, `dotnet format` clean, BOM intact.
- **User on hardware:** open Restructure with a scanned folder → a plan appears without
  clicking Generate; open Cleanup → each duplicate group shows file thumbnails.

## 2026-05-21 — V16.18 preview: arrow-key navigation + video player hardening

User-reported: arrow keys didn't move between items in the preview, and the video player was
buggy. `FilePreviewSheet`:
- **Arrow-key nav fixed.** The ←/→/Esc handler existed but only fired with keyboard focus
  inside the sheet — and the host ContentDialog (no default button) left focus on the dialog
  chrome, so keys never reached it. The sheet now grabs focus on `Loaded` and uses tunneling
  `PreviewKeyDown`, so ←/→ navigate siblings from anywhere in the sheet (overriding a focused
  video's seek), while the tag `TextBox` keeps ←/→ for its cursor. Esc closes.
- **Video player hardened.** Switched to `MediaSource.CreateFromStorageFile` (the StorageFile
  broker — same path the thumbnail loader uses) instead of a raw `file://` URI, which is more
  reliable for arbitrary local paths. The `MediaSource` is now disposed on navigation and the
  `MediaPlayer` is disposed on close — pause+null alone left audio playing and the file handle
  pinned. A generation guard drops stale async loads when arrow-navigating quickly through clips.

### Build/test
- C# `dotnet build` 0/0, `dotnet format` clean, BOM intact (UI behavior is the user's check).
- **User on hardware:** open a preview → ←/→ move between files (incl. over a video); play a
  video then close → audio stops + the file isn't locked; arrow through several clips → no glitch.

## 2026-05-21 — V16.17 CLIP scene-tagging OFF; CLIP kept for semantic search

SmolVLM is the sole tagger; CLIP must not emit tags — but free-text semantic search is kept
(user asked to keep it). CLIP (MobileCLIP-S2) did two independent jobs sharing the per-file
image embedding: scan-time scene tags (`source='auto'`) and the Library's semantic-search
embedding. Scene tags are now off; the search embedding stays. (SmolVLM is a generative VLM,
not a dual-encoder, so it can't do embedding search itself — CLIP runs alongside it for that.)

- **Engine.** `ENABLE_CLIP_SCENE_TAGS = false` → the `tagging.rs:954` scene-scoring block is
  skipped, so no `source='auto'` tags. `ENABLE_CLIP = true` keeps the MobileCLIP image encoder
  loading + the per-file embedding (stored in `clip_embeddings`) for semantic search.
  `load_default` builds the scene labeler ONLY when BOTH flags are on, so the ~21 s
  scene-matrix build is skipped (it's tags-only). SmolVLM (`source='vlm'`) is the sole tagger;
  `ReadStore` already orders vlm ahead of auto. The `commands/embed.rs` `!ENABLE_CLIP`
  short-circuit + the C# empty→null guards stay as harmless defense.
- **App.** Library semantic search works as before (MobileCLIP query embedding → cosine over
  `clip_embeddings`); the "install CLIP for search" banner, the MobileCLIP install card
  (Settings + Welcome), and CLIP in onboarding (`InstallAll`/`AllInstalled`) all stay. Settings
  diagnostic now reads "Tags: SmolVLM; Semantic search: MobileCLIP-S2."
- **Net:** no CLIP tags (SmolVLM only), semantic search preserved. To drop CLIP entirely
  (search → FTS5), flip `ENABLE_CLIP = false`.

### Build/test
- Engine on the pinned **1.90** toolchain: `clippy --all-targets -D warnings` clean, `test
  --lib` 158/0, `fmt --check` clean. C# `dotnet build` 0/0, `dotnet format` clean, UTF-8 BOM
  intact (incl. a BOM added to `WelcomeSheet.xaml` per `.editorconfig`).
- **User on hardware:** re-scan → tags are SmolVLM-only (`SELECT DISTINCT source FROM tags`
  has no `auto`); free-text search ("a dog at the beach") still returns semantic matches;
  `clip_embeddings` populates on new files; engine log shows no ~21 s scene-matrix build.

## 2026-05-21 — V16.16 mid-scan crash root-caused + fixed; Deep Analyze gating honest

The "click a page mid-scan → crash" bug was misattributed (the V16.5c DetailHostView
async-race theory). Three crash dumps from today (pid 19792, 12:03:21/23/32) were
identical: `NullReferenceException at RestructureView.OnVisualizationModeChanged` — a
`<ComboBox SelectedIndex="0" SelectionChanged=…>` raising SelectionChanged during
`InitializeComponent()`, before the `Sankey`/`TreeDiff` fields exist. It fired every time
the Restructure tab opened; `App.OnUnhandledException` (e.Handled=true) softened it to a
half-built tab, not a hard kill.

- **Crash fixed.** `RestructureView.OnVisualizationModeChanged` null-guards its siblings +
  wraps in `DebugLog.SafeRun`. Audited the init-fire pattern repo-wide — only this site crashed.
- **Settings EP-override clobber fixed (same pattern).** `SettingsView.OnProviderOverrideChanged`
  fired during `InitializeComponent` (before `HydrateToggles`/`_initializingToggles`),
  resetting the GPU EP override to "auto" on every Settings open. Now `!IsLoaded`-guarded.
- **ViewModel teardown race hardened.** People/Cleanup/Library `RefreshAsync` now create the
  linked `CancellationTokenSource` INSIDE the try, so a `Dispose()`-race
  `ObjectDisposedException` (from `_disposalCts.Token`) is caught as a clean no-op instead of
  escaping to the caller — that was the empty-message "OnLoaded refresh threw" log noise.
- **Deep Analyze gating honest** (`commands/deep_analyze.rs`): weights-gate FIRST →
  `vlm_model_missing` ("install it from the Deep Analyze tab") instead of a misleading
  runtime error / N silent per-file failures; one `llama_cpp_missing` when no backend can
  run the present weights. The engine source was already correct (registry pinned **b9254**,
  persistent llama-server is the default backend); the user's `llama_cpp_missing` is a STALE
  on-disk runtime (b4475, no llama-mtmd-cli.exe) + uninstalled Qwen weights — a rebuild +
  reinstall, not a code bug.
- **Audits + hygiene.** Dead code: all 32 engine `#[allow(dead_code)]` sites are deliberate
  (functional structs, a documented test fixture, non-Windows cfg-stubs, a parity primitive,
  future hooks) — nothing safe to remove; clippy confirms no *unmarked* dead code. Standards:
  `cargo fmt`/`clippy -D warnings`/`dotnet format`/analyzers all clean, BOM intact. Comments:
  conservative condensation of the verbose history blocks in the high-traffic views/services
  (ThumbnailService, sidebar controls, DeepAnalyzeView) — the load-bearing invariant/forensics
  comments CLAUDE.md flags are kept deliberately.
- **Docs.** STATE/NEXT/DECISIONS trimmed to a lean baseline; PACKS.md + DB-RESEARCH.md
  retired (refs fixed); PHASES checkbox/label + stale Phase-N notes corrected.

### Build/test
- C# `dotnet build FileID.sln -c Debug -p:Platform=x64` green (0/0) + `dotnet format` clean.
  Engine `cargo check`/`clippy --all-targets -D warnings`/`test --lib` (158/0)/`fmt --check`
  all green. (These gates run headlessly in the agent env now — see auto-memory.)
- **User, on hardware:** rebuild engine → relaunch (auto-reinstalls the b9254 runtime) →
  install Qwen2.5-VL-3B → open Restructure mid-scan (no crash) → scan → SmolVLM tags + Deep
  Analyze captions. Per NEXT.md V16.16.

## 2026-05-21 — V16.15 face crops fixed + 1-2 word tags + download jitter + dead code

- **Faces (root-caused + fixed).** SCRFD emits bbox as `[x1,y1,x2,y2]` corners
  (`scrfd.rs`, rescaled to original-image px by `detect()`), but `tagging.rs` fed it to
  `crop_and_resize_face` + stored it as `[x,y,w,h]` — so the crop ran from the face's
  top-left to the image's bottom-right ("not a face"/blank), and that smear was also fed
  to ArcFace (corrupting clustering). Now converted corners→xywh once at the
  detect→`DetectedFace` site → real face crops, meaningful embeddings, correct persisted
  bbox. (`validate_face_geometry` was already correct.) Follow-up: landmark-aligned
  ArcFace chips for better cluster accuracy.
- **Tags are 1-2 words.** `parse_vlm_tags` drops 3+-word fragments (was >3); the SmolVLM
  TAG_PROMPT already asks for 1-2 words.
- **Deep Analyze model reality (verified).** Qwen3-VL-4B has **no GGUF** (ggml-org has
  only Qwen3-VL 2B/30B; macOS uses MLX), and Qwen2.5-VL-7B (~4.7 GB) OOMs on the 4 GB
  card at `-ngl 99`. So Deep Analyze stays **Qwen2.5-VL-3B** (strongest Qwen that fits +
  already a card + full descriptive captions). Gemma-3-4B card swap + 7B-with-VRAM-aware
  `-ngl` flagged as follow-ups (need blind-unverifiable C# x:Name work / an engine change).
  See DECISIONS.
- **Download "freaking out" fixed.** `ModelSlot.UpdateRate` no longer zeroes rate/ETA at
  every per-file fraction reset in a multi-file bundle (carries the prior rate) — that was
  the 0-blip / "Stalled" flicker; sample interval 500→250 ms. `downloader.rs` progress
  throttle 100→50 ms + progress channel 256→512. (Already 12-way parallel range-GET; true
  throughput is near-capped.)
- **Dead code.** Removed the unused `run_ocr_blocking_arc` (live path is
  `run_ocr_blocking`). Remaining engine `#[allow(dead_code)]` are deliberate (test helper
  `ModelStack::empty`, non-Windows cfg-stubs, the pool-path CLIP `embed`). A broad
  slop-comment purge is **deferred** — much of the codebase's verbosity is the
  load-bearing institutional memory the CLAUDE.md says not to strip; touched code is
  WHY-focused.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean + `cargo test --lib` **158/0**
  (toolchain 1.90). C# (`ModelSlot`) `dotnet format` clean + BOM intact. WinUI compile is
  the user's VS build. Verify faces/tags/downloads on hardware per NEXT.md V16.15.

## 2026-05-21 — V16.14 small-screen / anti-clipping UI pass

User reported laptop UI content getting cut off. XAML audit (read-only — can't render
here) + conservative responsive fixes to the clear overflow patterns:
- **Deep Analyze action row** (7 controls: Whole library / Selected / Current / Skip
  toggle / Propose renames / Cancel) wrapped in a horizontal ScrollViewer (the
  PeopleView/CleanupView header pattern), so its right-hand controls can't clip on a
  narrow window — the most likely "cut off" culprit.
- **Oversized modal sheets shrunk to fit a laptop** (each already has an inner
  ScrollViewer for overflow): `FilePreviewSheet` 1080×720 → **880×520** (the worst —
  720-tall didn't fit a 768-px screen once title bar + taskbar are subtracted);
  `PersonDetailSheet` 480→440 H; `SuggestedMergesSheet` 520→440 H; `DrillDownSheet`
  700×520 → 640×440; `MainWindow` WelcomeOverlay MinWidth 660 → 580.
- Left as-is (degrades gracefully, doesn't hard-clip): Settings storage path
  (TextTrimming + tooltip), PersonDetail name fields (tight but fit), FilePreview
  toolbar (the `*` filename column absorbs the squeeze before buttons clip), sidebar
  (260 px with a Ctrl+Shift+S toggle).

All 6 edited `.xaml` parse as well-formed XML + BOM intact. **Not render-verified**
(no WinUI build/display here) — the user must eyeball on the laptop and report any
remaining clipping (which view + element).

## 2026-05-21 — V16.13 model-load timeout fix + tagging/Deep-Analyze split (first on-hardware run)

The build finally ran on the user's box (NVIDIA **~4 GB VRAM / DirectML**) after they
installed the VS WinUI PRI component (the CLI can't build WinUI here). First scan failed
with a false "models took >30 s / corrupted" — root-caused from the engine log to the
**21.5 s CLIP scene-matrix build** blowing the 30 s `load_default` timeout. Fixed, plus
the user's model-role ask.

- **Scene-label matrix is disk-cached** (`scene_vocab.rs`): build once (~21 s, first
  launch), reload ~instantly after (raw LE f32 + content-hash-keyed header under
  `Models/clip_scene_cache/`; the hit path also skips loading the 253 MB text session).
  **Model-load timeout 30 → 120 s** (`scan.rs`) so the one-time build can't false-fail.
  → first launch slow once, later launches <10 s. Immediate workaround for the user: a
  second "Start Scan" in the same session already worked (matrix cached process-static).
- **Tagging vs Deep Analyze split.** Auto-tag hardwired to **SmolVLM**
  (`EngineClient.AutoTriggerDeepAnalyzeAsync`, gated on SmolVLM weights present); **Deep
  Analyze defaults to Qwen 2.5-VL 3B** (`AppSettings.SelectedVlmModelKind` default → qwen
  + v2→v3 migration off the leaked smolvlm). SmolVLM auto-installs; Qwen installs
  on-demand from the Deep Analyze card.
- **Deep Analyze cards now honest** (V16.12.1): `DeepAnalyzeView.SyncCards` checks each
  model's gguf on disk instead of mirroring the shared "any VLM" slot — Qwen no longer
  falsely shows "Installed".
- **Hardware tailoring confirmed from logs:** DXGI vendor probe (NVIDIA), VRAM probe
  (3935 MB), EP chain cuda→tensorrt→directml→cpu, pool clamped to 1 to fit 4 GB, per-vendor
  runtime auto-install (Vulkan + SmolVLM + CUDA llama runtime + cuDNN all present). Open
  gap: ONNX runs on **DirectML** (the `cuda` ORT pack is `not_yet_available` → ~3-5×
  slower); the VLM path already uses CUDA. Sourcing the ORT CUDA EP DLLs is a follow-up.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean (toolchain 1.90, the CI pin).
- C# (`AppSettings`, `EngineClient`, `DeepAnalyzeView`, build-all.ps1 SDK fix) —
  `dotnet format` clean + UTF-8 BOM intact; full WinUI compile is the user's VS build (the
  dotnet CLI here lacks `Microsoft.Build.Packaging.Pri.Tasks.dll`).
- Verify on hardware per NEXT.md V16.13.

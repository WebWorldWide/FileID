# Windows port — full phase plan with parity checklist

This is the master parity tracker for the Windows version of FileID. Every checkbox below is a behavior the macOS app has, and the Windows port must reproduce. When a phase ships, every box in that phase must be checked. No "we'll come back to it" — that's how features go missing.

Phases are sequenced so each one builds on the previous one's primitives. Estimates are calendar-week ranges for a single dev working full-time; expect ±50% drift on the longer phases.

The macOS app is the source of truth. When this doc and `platforms/apple/` disagree, `platforms/apple/` wins; update this doc.

---

## Phase 0 — Foundation ✅ SHIPPED

Already in commit `6704a46`.

- [x] Repo restructure: macOS to `platforms/apple/`, Windows to `platforms/windows/`, docs to `shared/docs/`
- [x] Canonical IPC schema at `shared/ipc-schema/ipc.schema.json`
- [x] Rust engine scaffold (Cargo workspace, both Windows targets, locked deps, LTO release profile)
- [x] stdio IPC parse/emit loop with bounded mpsc sink
- [x] Parent-PID watchdog (OpenProcess + WaitForSingleObject)
- [x] Local-only structured tracing
- [x] DB connection mgmt + byte-faithful v1–v7 migrations + WAL checkpoint at shutdown
- [x] %LOCALAPPDATA%\FileID\ directory layout
- [x] Build scripts (x64 + ARM64 cross-compile)
- [x] GitHub Actions CI matrix with telemetry-string privacy gate
- [x] Cross-platform docs: PRIVACY, ARCHITECTURE, VISUAL-LANGUAGE, MODELS, DECISIONS, STATE, NEXT
- [x] Per-platform CLAUDE.md + READMEs

**Pending verifications before Phase 1 starts:**

- [ ] User runs `cd platforms/apple && bash run.sh` on a Mac → app builds, bundles, opens
- [ ] User runs `cd platforms/apple && swift test` on a Mac → 28/28 pass
- [ ] User runs `cd platforms/apple && bash scripts/iterate.sh` on a Mac → 11/11 pass
- [ ] User runs `pwsh platforms/windows/build/build.ps1 -RunTests` → engine compiles, all unit tests pass
- [ ] User applies the `startScan` IPC breaking change on the macOS side (separate clearly-labeled commit)

---

## Phase 1 — App shell + theme + sidebar + welcome sheet  ✅ CODE-COMPLETE (awaiting on-hardware verification)

The user opens `FileID.exe` for the first time. They see Mica/Acrylic, dark mode forced, the LavaLamp animating, the gold-accented sidebar, the welcome sheet. The engine is spawned and reports ready. They can pick a folder. **No scan logic yet** — that's Phase 2 — but the entire shell looks and feels like macOS.

> Code-complete as of V11 (2026-05-02). Everything below is ticked. The acceptance gate is the on-hardware verification described in `shared/docs/NEXT.md`.

### 1.1 WinUI 3 app project bootstrap
- [ ] `platforms/windows/FileID.sln` solution file
- [ ] `platforms/windows/src/FileID.App/FileID.App.csproj` — WinUI 3 unpackaged desktop app, .NET 8 (or 9), self-contained publish
- [ ] `platforms/windows/src/FileID.Theme/FileID.Theme.csproj` — class library for Theme + motion primitives
- [ ] `platforms/windows/src/FileID.IpcSchema/FileID.IpcSchema.csproj` — class library, hand-maintained C# DTOs mirroring `shared/ipc-schema/ipc.schema.json`
- [ ] WiX-friendly publish output: `dotnet publish -c Release -r win-x64 --self-contained true /p:PublishReadyToRun=true /p:PublishSingleFile=false`
- [ ] `App.xaml` + `App.xaml.cs` with single-instance gate (named mutex on app GUID)
- [ ] `MainWindow.xaml` shell

### 1.2 Window chrome
- [ ] Min size 1200×800
- [ ] `Window.SystemBackdrop = MicaController` on Win11; `DesktopAcrylicController` on Win10 22H2
- [ ] `Window.ExtendsContentIntoTitleBar = true`; custom drag region attached to the sidebar header
- [ ] `RequestedTheme = Dark` on the root content
- [ ] `dwmapi DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE, 1)` for the title-bar dark mode
- [ ] About dialog accessible from system menu; shows version + build (matches macOS About dialog at `FileIDApp.swift:47-57`)
- [ ] Window state (size, position, sidebar visibility, active tab) persisted to `%LOCALAPPDATA%\FileID\settings.json`

### 1.3 Theme port (`FileID.Theme/Theme.xaml`)
- [ ] Brushes: Gold `#FFCC00`, GoldDim `#CCA300`, AI lavender `#B19BCE`, Info cyan `#A0E2EA`, Delight pink `#F2A6C0`
- [ ] Surface tokens: SurfaceBase (Black @ 30%), SurfaceCard (White @ 6%), SurfaceBorder (White @ 8%)
- [ ] Spacing tokens: 4 / 8 / 16 / 24 / 40
- [ ] Radius tokens: 8 / 12 / 16
- [ ] `<AcrylicBrush x:Key="GlassBrush"/>` for glass cards

### 1.4 Reusable Theme components (1:1 with `Theme.swift`)
- [ ] `GlassCard` UserControl — Acrylic + 1px stroke + radius 12
- [ ] `BadgePill` UserControl — color-tinted capsule, 10pt semibold text
- [ ] `SettingToggleRow` UserControl — title + subtitle + right-aligned gold toggle
- [ ] `GoldButton` UserControl — bordered prominent, gold tint, black text, semibold
- [ ] `ThemedSegmentedControl` UserControl — gold pill on selected, white-8% on inactive, capsule wrapper
- [ ] `ThemedTogglePicker` UserControl — two-option pill picker

### 1.5 Motion primitives (`FileID.Theme/Motion/`)
- [ ] `LavaLampBackground.cs` — Win2D `CanvasControl`, three blurred ellipses (800/600/1000 px diameter, 120 px Gaussian, gold/red-orange/dark) at the exact macOS time multipliers (0.20/0.23/0.15/0.18/0.10/0.12). Vsync via `CompositionTarget.Rendering`. **Pause when occluded.** Tinted overlay rectangle on top with `#000000 @ 30%`. Match macOS visually at 1080p side-by-side.
- [ ] `ShimmerView.cs` — gold→lavender diagonal sweep, 1.6 s linear infinite, disabled under reduced-motion
- [ ] `CompletionRipple.cs` (attached behavior) — fires on any value change of a trigger; 0.4→2.6 scale + 0.85→0 opacity over 0.9 s easeOut; disabled under reduced-motion
- [ ] `IridescentBorder.cs` — Win2D `CanvasSweepGradient` (gold → delight → ai → info → gold) rotating 360° over 14 s linear infinite; static gold under reduced-motion
- [ ] `ReducedMotion.cs` — bridges `UISettings.AnimationsEnabled` to a global `IObservable<bool>`; every motion primitive subscribes
- [ ] Spring helper using `Microsoft.UI.Composition.SpringScalarNaturalMotionAnimation` — wrapper API `Spring(double response, double dampingFraction)` so call sites read like SwiftUI

### 1.6 EngineClient view-model (`FileID.App/ViewModels/EngineClient.cs`)
- [ ] Spawns `FileIDEngine.exe` via `ProcessStartInfo` with stdin/stdout/stderr redirected
- [ ] Reads stdout line-by-line on a background thread, dispatches each `IpcEvent` to the UI thread via `DispatcherQueue.TryEnqueue`
- [ ] `INotifyPropertyChanged` properties for direct XAML binding mirroring the macOS observable surface: `state`, `lastProgress`, `lastError`, `lastBatch`, `lastFaceClustering`, `deepAnalyzeProgress`, `deepAnalyzeLast` (2 Hz throttled), `deepAnalyzeComplete`, `modelDownloadProgress`, `queueState`, `autoPilotActive`, `autoPilotStage`
- [ ] Connection state: Starting / Ready / Crashed (Reason)
- [ ] Auto-respawn with exponential backoff (1 s / 4 s / 16 s within a 60 s window); after 3 misses go `.crashed`
- [ ] `WinVerifyTrust` Authenticode integrity check on `FileIDEngine.exe` before each spawn (Phase 1 ships the check; the EV signature check tightens at Phase 11). Refuse to spawn on signature mismatch.
- [ ] Engine path resolution: must live next to `FileID.exe` (matches macOS "must be inside Contents/MacOS/")
- [ ] Send commands via `WriteLineAsync` on stdin; serialize each `IpcCommand` with sorted keys
- [ ] Send `requestStatus` immediately after spawn to confirm bidirectional liveness

### 1.7 Sidebar (`FileID.App/Sidebar/`)
- [ ] Sidebar root: 260 DIP fixed width, AppStorage-equivalent visibility persistence, slide-in/out animation 0.20 s easeInOut
- [ ] Folder section header showing parent path in secondary, last component in gold (matches `Sidebar.swift:164-171`)
- [ ] Three folder actions: "Change folder…" (Ctrl+O), "Clear folder", "Wipe library + rescan" (destructive red, with confirmation dialog)
- [ ] Tab list (Library / People / Cleanup / Deep Analyze / Restructure / Settings) with active-tab gold @ 18% background + gold @ 55% stroke
- [ ] Tab disabled state with hint "Pick a folder above to enable tabs" until folder selected
- [ ] Folder picker via `IFileOpenDialog` + `FOS_PICKFOLDERS`; readability pre-validation; alert dialog on failure
- [ ] Drag-drop folder onto sidebar/main window: dashed gold border + "Drop Folder to Scan" overlay; readability check before accepting (matches `MainWindow.swift:130-161`)
- [ ] Sidebar collapse toggle button (top-right) with Ctrl+Shift+S keyboard shortcut
- [ ] Divider color white @ 8% between sidebar and detail
- [ ] Folder bookmark resolution off-thread (Windows: just persist absolute path; no bookmark)
- [ ] Tab auto-switch hooks: subscribe to engine events for face-clustering complete (→ People) and Deep Analyze complete (→ Library)

### 1.8 Sidebar processing control (`Sidebar/SidebarProcessingControl.xaml`)
- [ ] Pre-scan state: idle icon + status message + "Start Scan" gold button with Ctrl+R shortcut
- [ ] In-flight state: phase icon/badge + progress bar + 4-stat grid (discovered / tagged / memory MB / failures)
- [ ] Stat color coding: gold for discovered+tagged, orange if memory > 1200 MB, red if any failed
- [ ] Pause / Resume / Cancel buttons during scan; Rescan after completion
- [ ] Paused state banner: orange pause icon + "Workers idle"
- [ ] ETA computation + display
- [ ] Pipeline progress visualization (5 stages: Scan → Tag → People → Captions → Done) with filled / active / inactive dots and connecting segments; gold for filled+active; shadow on active dot

### 1.9 Sidebar engine status (`Sidebar/SidebarEngineStatus.xaml`)
- [ ] Three states: Starting (hourglass) / Ready (green check + tooltip showing version + workers + RAM + PID) / Crashed (red ×, surfaces last error)

### 1.10 Sidebar queue list (`Sidebar/SidebarQueueList.xaml`)
- [ ] Pending job rows: icon (magnifier scan / people cluster / sparkles deep-analyze) + title + ETA
- [ ] Job categories enum: Scan, FaceCluster, DeepAnalyze
- [ ] Total queue ETA at the top, formatted as "X minutes" / "X hours Y min"
- [ ] Truncation handling for long job titles

### 1.11 Welcome sheet (`FileID.App/Views/WelcomeSheet.xaml`)
- [ ] Three-model installer rows: CLIP (~210 MB), ArcFace (RAM-dependent, ~100–500 MB), Deep Analyze (recommended VLM ~1.5–4 GB)
- [ ] Model status icons: checkmark (installed), down-arrow (downloading), square-and-arrow-down (not started)
- [ ] Per-model progress bar + download speed + ETA
- [ ] RAM-based model recommendations via the engine (`FaceEmbedderKind.defaultFor(ramGB:)`, `AIModelKind.safeDefaultFor(ramGB:)`)
- [ ] "Install all" button + "Skip for now" button
- [ ] Auto-dismiss when all models installed
- [ ] Privacy disclosure: "100% on-device. No telemetry. No analytics." + link to PRIVACY.md
- [ ] Download rate smoothing via EMA (0.7 old + 0.3 new) per the macOS implementation
- [ ] 30-second timeout with fallback error message if engine doesn't report progress

### 1.12 Empty / loading / error state primitives
- [ ] `EmptyStateView` UserControl: 56 DIP icon (gold @ 55%), title (TitleLargeTextBlockStyle bold), body callout, optional secondary message, optional primary action button
- [ ] Onboarding splash for empty Detail (no folder picked, no files in DB): 6-step pipeline diagram (Pick a folder → Scan → Group people → Find dupes → Deep Analyze → Reorganize)
- [ ] Inline error banner component (red border, white-on-red icon, dismissible)

### 1.13 Keyboard shortcuts wired
- [ ] Ctrl+O — Pick folder
- [ ] Ctrl+R — Start Scan
- [ ] Ctrl+Shift+S — Toggle sidebar
- [ ] Ctrl+F — Focus search (no-op on tabs without search)
- [ ] Alt+1..6 — Jump to tab N (Library/People/Cleanup/DeepAnalyze/Restructure/Settings) — _macOS doesn't have these but they're idiomatic on Windows; confirm with user_

### 1.14 Acceptance for Phase 1
- [ ] Side-by-side video review at 1080p: LavaLamp visually indistinguishable from macOS
- [ ] Reduce-motion override (Settings → Accessibility → Visual effects) disables Shimmer + Ripple, freezes IridescentBorder gold, halves LavaLamp rate
- [ ] Engine spawn → ready handshake completes < 500 ms cold
- [ ] Engine kill (Task Manager) → app respawns within backoff window
- [ ] Welcome sheet completes a CLIP download end-to-end and dismisses
- [ ] Folder pick + drag-drop both accept readable folders, both reject unreadable folders with explanatory alert

---

## Phase 2 — Scan pipeline + Library tab end-to-end  (4–5 weeks)

The user picks a folder, hits Start Scan. Files start streaming into a thumbnail grid as they're tagged. They can search, filter by kind, multi-select, bulk-tag, bulk-rename, preview a file, edit its tags inline. Faces are detected, embedded, and stored — but the People tab still doesn't render them yet (Phase 3).

### 2.1 Engine — pipeline plumbing (`platforms/windows/src/engine/src/`)
- [ ] `coordinator.rs` — `ScanCoordinator` actor with pause/resume/cancel + sync mirrors via `AtomicBool`
- [ ] `job_queue.rs` — single FIFO job queue (Scan / FaceCluster / DeepAnalyze categories); emits `queueState` events
- [ ] `pipeline/discovery.rs` — `walkdir` enumerator, kind filters (image/video/PDF/doc/audio/other), size cap (skip >500 MB), sorted-by-path traversal for I/O locality
- [ ] `pipeline/dbwriter.rs` — bounded mpsc consumer; transactions of 100 files OR 200 ms; resume cursor in same transaction
- [ ] `pipeline/tagging.rs` — N tagging workers (`num_physical_cores * 1.7`, capped 2..32); per-file: kind detect → image decode → EXIF → phash → MobileCLIP embed → SCRFD detect + ArcFace embed → OCR fast tier → ship `TaggedFile` to dbwriter
- [ ] AsyncSemaphore primitive: 3-4 concurrent ORT inference, 2 concurrent CLIP — match macOS counts
- [ ] Hot-path cancellation via sync mirrors (`AtomicBool::load(Relaxed)`) — no actor hop per file
- [ ] Bounded channels: Discovery→Tagging cap 1024; Tagging→DBWriter cap 256
- [ ] Orphan sweep post-scan: `path_text LIKE rootPath/%` AND `scanned_at < scanStart`, capped at 5000 candidates
- [ ] Auto-enqueue face clustering job on scanComplete

### 2.2 Engine — image / EXIF / phash
- [ ] `image-rs` decode pipeline (JPEG, PNG, WebP, BMP, TIFF, GIF). HEIC via `libheif-rs` feature flag (gated; ships as a DLL with the engine)
- [ ] `fast-image-resize` thumbnail downscale with AVX-512 (x64) / NEON (arm64) auto-dispatch
- [ ] `kamadak-exif` for EXIF + GPS + camera model
- [ ] dHash port from Swift (~30 LoC; 9×8 grayscale → adjacent-pixel diff → 64-bit phash)
- [ ] Memory-mapped reads via `memmap2` for large image files

### 2.3 Engine — ML model wiring (Phase 2 covers everything except VLMs)
- [ ] `models/runtime.rs` — EP picker: NVML probe → CUDA EP, DXGI vendor probe → OpenVINO/DirectML, QNN probe → QNN EP, fallback CPU EP. Persist choice in settings.json. Manual override via IPC.
- [ ] `models/mobileclip.rs` — load `.onnx`, 256×256 RGB input, 512-d float32 output, L2-normalize. Pre-warm before workers spawn (matches macOS).
- [ ] `models/scrfd.rs` — Buffalo bundle SCRFD ONNX, output bounding boxes + 5-point landmarks
- [ ] `models/arcface.rs` — load `.onnx` (iResNet50 default ≥16 GB hardware, MobileFace default <16 GB), 112×112 RGB input, 512-d float32 output, L2-normalize
- [ ] PnP solve from SCRFD landmarks → roll/yaw/pitch (~50 LoC standard math) for face quality
- [ ] Face quality score: bbox confidence + Laplacian sharpness on the crop OR optional `face_quality_assessment.onnx`
- [ ] `models/clip_text.rs` — OpenAI CLIP text ONNX + BPE tokenizer port from `CLIPTokenizer.swift` (~150 LoC, deterministic, unit-tested against Swift output bytes)
- [ ] `shell/sleep.rs` — `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` RAII guard during scan
- [ ] Process priority elevation: `SetPriorityClass(ABOVE_NORMAL_PRIORITY_CLASS)` on scan start, reset on scan end
- [ ] Battery-aware throttle: `GetSystemPowerStatus` — if on battery + <20%, halve worker count (off by default on desktops)

### 2.4 Engine — OCR
- [ ] `shell/ocr.rs` — `Windows.Media.Ocr` WinRT bindings via windows-rs (default)
- [ ] PaddleOCR ONNX path (opt-in advanced setting; Phase 5 wires the toggle)
- [ ] Batched OCR per-file with timeout; OCR-failed paths recorded in DB with `failed=1`

### 2.5 Engine — shell helpers (`shell/`)
- [ ] `trash.rs` — `IFileOperation::DeleteItem` with `FOF_ALLOWUNDO` (used by Cleanup tab in Phase 4 but the primitive lives here)
- [ ] `reveal.rs` — `SHOpenFolderAndSelectItems`
- [ ] `thumbnail.rs` — `IThumbnailProvider` via `SHCreateItemFromParsingName` → `IShellItemImageFactory::GetImage` for `.docx` / `.pdf` / `.txt` / `.md` / .key / .numbers / etc.
- [ ] `tags.rs` — `IPropertyStore` writing `PKEY_Keywords` (`System.Keywords`); sidecar `.fileid-tags.json` fallback for files without a property handler
- [ ] Path redaction: `redact_path_for_log` (Rust mirror of `PathRedaction.swift`)

### 2.6 App — DB read layer (`FileID.App/Services/ReadStore.cs`)
- [ ] Read-only SQLite connection (`Microsoft.Data.Sqlite` or `sqlite-net` — pick one in DECISIONS.md before code)
- [ ] FTS5 keyword search across `ocr_fts`, filename, `vlm_proposed_name`, `vlm_description`, tags, person names (matches `LibraryView.swift:118-149`)
- [ ] CLIP semantic search: text via `CLIPTextEncoder.shared.embedText()` → cosine rank stored embeddings
- [ ] Visual similarity search: seed file embedding → cosine rank others
- [ ] Duplicate group aggregation (Phase 4 tab consumes; primitive lives here)
- [ ] File deletion (transactional, chunked, used by Cleanup)
- [ ] `version` counter increments on reload so XAML re-binds (matches Swift `ReadStore.version`)
- [ ] Recent scans list (Settings consumes in Phase 5)

### 2.7 App — services (`FileID.App/Services/`)
- [ ] `CLIPTextEncoder.cs` — `.Load()` async; `.IsReady`; `.EmbedText(query)` → `float[]` or null
- [ ] `CLIPTokenizer.cs` — BPE tokenizer port from `CLIPTokenizer.swift`; deterministic byte-for-byte against Swift output
- [ ] `CLIPModelInstaller.cs` — observable status, `.Install()` / `.Cancel()` / `.RefreshStatus()`
- [ ] `ArcFaceModelInstaller.cs` — per-kind status, `.Install(kind)` / `.Cancel(kind)` / `.RefreshStatus()`
- [ ] `ThumbnailService.cs` — async thumbnail generation via `IThumbnailProvider`-driven engine call; in-process LRU cache; shimmer placeholder while loading

### 2.8 Library tab UI (`FileID.App/Views/LibraryView.xaml`)
- [ ] Adaptive thumbnail grid (`ItemsRepeater` with `UniformGridLayout`, min/max tile 160–220 DIP, 12 DIP gap)
- [ ] Search bar focused via Ctrl+F; debounced 200 ms; clear-button
- [ ] Kind filter (image/video/PDF/document/audio) — `ThemedSegmentedControl`
- [ ] Multi-select mode: per-tile checkboxes, "Select all", "Clear", "Apply tags" bulk action, "Apply names" bulk action
- [ ] Similar-photos mode: seed a `FileRow`, banner with "Showing similar to <filename>" + Clear button
- [ ] Live re-query on each `lastBatch` engine event during scan, throttled to 1 Hz
- [ ] In-flight scan banner with progress bar inline
- [ ] Post-scan banners: "Grouping faces…" during clustering, "Writing captions…" during DA
- [ ] CLIP install hint banner when search active but encoder isn't installed
- [ ] Empty state when zero files match
- [ ] "X images" total count badge (top-right)
- [ ] Tag count batched per visible tile (single SQL query)

### 2.9 Library tab — file tile (`FileID.App/Views/Library/FileTile.xaml`)
- [ ] Thumbnail with shimmer placeholder
- [ ] Smart-name suggestion overlay (when `vlm_proposed_name IS NOT NULL`)
- [ ] Kind badge (image / video / PDF / doc / audio / other)
- [ ] Face indicator (small face icon if `has_faces`)
- [ ] OCR indicator (small text icon if `has_text`)
- [ ] Hover lift: `scaleEffect 1.012` + drop shadow elevation
- [ ] Selection checkbox overlay in multi-select mode
- [ ] Tile entrance animation: opacity + scale 0.96 over 0.25 s spring (response 0.35, dampingFraction 0.78)

### 2.10 Library tab — file preview sheet (`FileID.App/Views/Library/FilePreviewSheet.xaml`)
- [ ] Modal sheet overlay (custom presentation; WinUI 3 `ContentDialog` for the chrome OR a full-screen overlay)
- [ ] Per-kind preview: image (image-rs decode), video (Media Foundation poster + play CTA opens default app), PDF (pdfium-render with page navigation), audio (poster + metadata), generic (`IThumbnailProvider`)
- [ ] Toolbar: prev / next arrow nav (frozen sibling list at open time), close, reveal in Explorer, open in default app, "Analyze with VLM" button
- [ ] Metadata panel: kind, size, dimensions, EXIF camera + GPS, vlm_description, vlm_proposed_name
- [ ] Inline tag editor (Phase 2.11)
- [ ] Smart-name "Apply rename" inline action (renames via engine IPC)
- [ ] Aesthetic score badge (0–1 float) when present
- [ ] Person tags ("Adam, Beth, …") when faces are linked
- [ ] Keyboard nav: ←/→ for prev/next, Esc to close

### 2.11 Library tab — Finder-tags-equivalent inline editor
- [ ] Tag pill flow layout (port of `FlowLayout` from macOS)
- [ ] Add tag via text input + Enter
- [ ] Remove tag via × on pill
- [ ] Tags persisted to `IPropertyStore` `System.Keywords` AND mirrored in DB `tags` table
- [ ] Sidecar fallback for files without property handler

### 2.12 Bulk action sheets
- [ ] `BulkTagSheet.xaml` — comma-separated tag entry, "X files" count, Apply (Ctrl+Enter) gold button, status message (added/unchanged/failed counts)
- [ ] `BulkRenameSheet.xaml` — list of old → new filenames with checkboxes, "Select all" / "Clear" / "Rename X files" actions, ≥50 file confirmation dialog, status message, undo button (rename journal in DB)
- [ ] Bulk rename undo journal stored in DB (new migration `v8_rename_journal` if not already in v7)

### 2.13 Acceptance for Phase 2
- [ ] Cold scan of 50K-file corpus completes; thumbnail grid streams in real time
- [ ] Throughput hits per-tier targets (per `shared/docs/ARCHITECTURE.md`): NVIDIA + DirectML ≥ 140 files/s; CPU floor ≥ 40 files/s
- [ ] FTS5 search returns results < 100 ms
- [ ] CLIP semantic search returns results < 300 ms
- [ ] Tags written via `System.Keywords` round-trip via Explorer Details column
- [ ] Bulk rename of 200 files completes < 5 s
- [ ] Bulk tag of 1000 files completes < 5 s
- [ ] `iterate.ps1` (Phase 11) skeleton passes assertions 1–6 and 9 on the corpus
- [ ] No Library tab crashes through 1 hour of mixed scan + search + preview activity

---

## Phase 3 — People tab + face clustering  (2–3 weeks)

The user has scanned. Faces have been detected and embedded. Phase 3 surfaces the People tab and turns the embeddings into named clusters they can edit.

### 3.1 Engine — face clustering (`pipeline/face_clustering.rs`)
- [ ] Job triggered by IPC `runFaceClustering`; auto-enqueued at scanComplete
- [ ] Read all `face_prints` with `excluded=0` and `arcface_embedding IS NOT NULL`
- [ ] HNSW index build (use `hnsw` Rust crate or hand-roll if dep budget tight) — re-built each run, not persistent
- [ ] Cosine-similarity clustering with the same threshold the macOS pipeline uses (port `IdentityClustering.swift` constants)
- [ ] Identity anchors: each cluster's centroid + 90th-percentile cosine distance from members → `persons.centroid` + `anchor_radius` + `last_clustered_at` (v7 schema columns already exist)
- [ ] On re-cluster, new clusters whose centroid falls within an old anchor's radius inherit the old structured-name fields
- [ ] Emit `faceClusteringComplete(personCount, faceCount, unmatchedFaces, durationSeconds)`

### 3.2 Engine — VLM-verified merge suggestions (`pipeline/cluster_suggestions.rs`)
- [ ] Pairwise face crops in the L2-distance borderline band (0.45–0.70 cosine distance) sent to local VLM for "same person?" check (Phase 6 wires the actual VLM call; Phase 3 lays the table + IPC + UI surface)
- [ ] Results land in `face_verifications` table (v4 schema)
- [ ] Suggested-merges sheet ranks pairs by VLM confidence

### 3.3 People tab UI (`FileID.App/Views/PeopleView.xaml`)
- [ ] `ItemsRepeater` grid of cluster cards (`UniformGridLayout`, min 160 DIP)
- [ ] Cluster card: face crop (representative face), structured name (Title FirstName MiddleName LastName Suffix), file count, face count
- [ ] Card entrance: scale 0.92 + opacity over 0.35 s spring (response 0.35, dampingFraction 0.78); reduce-motion → opacity-only 0.15 s
- [ ] Multi-select merge mode: checkboxes on cards, "Merge selected" CTA
- [ ] Multi-select mark-unknown mode: checkboxes on cards, "Mark as unknown" CTA
- [ ] Hidden-unknowns toggle: collapses marked-unknown clusters
- [ ] Header status: "X people · Y still unnamed" or "X people · all named"
- [ ] Merge status banner after bulk merge (auto-dismiss after 5 s)
- [ ] Empty state: "No faces yet — run a scan to detect faces"
- [ ] Drag-merge: drag a card onto another card → drop-target border (gold @ 55%) → release merges

### 3.4 People tab — sheets
- [ ] `PersonDetailSheet.xaml` — full-page sheet with name editor (5 fields: title, first, middle, last, suffix), photo grid of every face for that person, "Tag all photos" batch action, "Mark as unknown" toggle, "Delete person" destructive button
- [ ] `MergeTargetPickerSheet.xaml` — when merging 2+ clusters, choose primary cluster (the one whose name + anchor are kept)
- [ ] `MovePhotosTargetPicker.xaml` — move photos from one cluster to another
- [ ] `SuggestedMergesSheet.xaml` — pairwise list with cosine similarity color (green ≥ 0.55, yellow ≥ 0.50, orange < 0.50), VLM confidence column when available, "Accept" / "Accept all" / "Skip" actions
- [ ] All sheets use enum-routed single-sheet driver (matches macOS pattern `PeopleView.swift:33-45`)

### 3.5 Person operations (engine + app)
- [ ] `mergePersons(target, sources)` IPC command + engine impl (matches macOS `Database.mergePersons`)
- [ ] Off-MainActor merge (Task.detached equivalent on .NET — `Task.Run` then DispatcherQueue.TryEnqueue back)
- [ ] Person-rename writes to `persons` table via IPC; UI re-binds via `ReadStore.version` increment
- [ ] "Tag all photos" batch action: writes a person tag to `System.Keywords` for every linked file

### 3.6 Acceptance for Phase 3
- [ ] Clustering on 50K-file corpus matches macOS person count ±5%
- [ ] Drag-merge produces visually identical "drop target" feedback to macOS
- [ ] Suggested merges sheet renders in <300 ms after cluster job completes
- [ ] Mark-unknown + hidden-toggle round-trips through DB and UI
- [ ] Auto-tab-switch from any tab to People when face clustering completes (matches `MainWindow.swift:95-110`)

---

## Phase 4 — Cleanup tab  (1 week)

Smallest tab. Duplicate groups via phash; user picks keepers; bulk trash via `IFileOperation`.

### 4.1 Engine — duplicate aggregation
- [ ] Already in DB schema (phash on `files`); `ReadStore` query groups by phash + counts
- [ ] Keeper detection rule (port from macOS `CleanupView` logic) — typically largest size or earliest mtime; **port the exact macOS rule** so DB built on Mac and reopened on Windows shows the same keeper

### 4.2 Engine — parallel trash
- [ ] `shell/trash.rs` — `IFileOperation::DeleteItem` with `FOF_ALLOWUNDO`
- [ ] **One COM apartment per worker** (`CoInitializeEx(COINIT_APARTMENTTHREADED)` per thread); 8 parallel
- [ ] IPC command `trashFiles(fileIDs)` → emits per-file done + final summary
- [ ] On success: DB rows deleted; tags rows cascade-deleted
- [ ] Auto-tag keeper if Settings → "Tag kept files after Cleanup" is on

### 4.3 Cleanup tab UI (`FileID.App/Views/CleanupView.xaml`)
- [ ] LazyVStack of `GroupCard`s, ScrollViewer for vertical
- [ ] First-timer green explainer banner ("FileID grouped these by visual similarity. Pick one to keep, trash the rest.")
- [ ] Empty state: "No duplicates found"
- [ ] All-skipped state: "All duplicate groups skipped — show hidden"
- [ ] Status banner after trash: "Moved X files to Trash" + "Open Trash" button + "Dismiss"

### 4.4 Cleanup — group card
- [ ] Header: folder icon + group ID + member count
- [ ] Horizontal scroll carousel of CopyTiles
- [ ] Per-tile checkbox overlay (red border when selected for delete, green border on keeper)
- [ ] Click a non-keeper tile to promote it to keeper
- [ ] Group-level menu: "Select all except keeper" / "Clear selection" / "Skip this group"
- [ ] "Delete X selected (Y MB)" red button at group level (and global at top of view)

### 4.5 Cleanup — global controls
- [ ] "Select all non-keepers across visible groups" button
- [ ] Global "Delete X selected (Y MB)" with confirmation dialog
- [ ] "Open Trash" via `ShellExecuteW("explorer.exe", "shell:RecycleBinFolder")`

### 4.6 Acceptance for Phase 4
- [ ] Cleanup of 1000 dup files completes < 10 s via 8-parallel `IFileOperation`
- [ ] Files moved to Recycle Bin (recoverable from Explorer)
- [ ] Auto-tag keeper writes `System.Keywords` when Settings toggle on
- [ ] Visual parity with macOS at the card + carousel level

---

## Phase 5 — Settings tab + privacy panel + Performance Pack UX  (1–2 weeks)

### 5.1 Settings tab UI (`FileID.App/Views/SettingsView.xaml`)
- [ ] `SettingToggleRow` for "Tag kept files after Cleanup"
- [ ] AI model picker cards: CLIP card, ArcFace card, DeepAnalyze model picker card (each with install state, RAM budget, manual override)
- [ ] Privacy disclosure card: "100% on-device" badge + 5 rows (Photos, Faces+names, Captions, Storage, Network) — copy from `shared/docs/PRIVACY.md`
- [ ] **Privacy "What we don't do" panel** — explicit list: no analytics SDK, no crash service, no update pings, no model-download instrumentation, no license server. Each row has a green checkmark.
- [ ] Advanced disclosure group (collapsed by default)

### 5.2 Settings — Advanced section
- [ ] Engine subsection: Status / Version / PID / Workers / RAM rows + "Restart Engine" / "Stop Engine" buttons
- [ ] Storage subsection: Total files / Images / Duplicate groups / Reclaimable MB / DB path + "Show database in Explorer" button
- [ ] Recent scans subsection: lazy-loaded on expand; status icons (running / completed / cancelled / crashed) + timestamps + paths
- [ ] Logs subsection: "Open scan log" / "Open app log" / "Show logs in Explorer" buttons (via `ShellExecuteW`)
- [ ] GPU EP picker dropdown: Auto / CUDA / OpenVINO / DirectML / QNN / CPU. Persist to settings.json.

### 5.3 Performance Pack download UX
- [ ] "Get faster on this hardware" subsection in Settings
- [ ] Hardware probe: detect NVIDIA (NVML) / Intel (DXGI) / Snapdragon NPU; auto-suggest matching pack
- [ ] Per-pack card: name, size, hardware target, install state (not installed / downloading / installed); "Get pack" button
- [ ] Download flow: same downloader pattern as model downloads (no telemetry, SHA256-pinned)
- [ ] On install: extract into `%LOCALAPPDATA%\FileID\runtimes\<pack>\`; engine adds to DLL search path on next spawn

### 5.4 Settings — copy buttons
- [ ] Info rows with monospace font + text-selection enabled (matches `SettingsView.swift:248`)

### 5.5 Acceptance for Phase 5
- [ ] Privacy panel reads as a contract, not boilerplate
- [ ] Engine restart from Settings reports a fresh PID
- [ ] Recent scans list refreshes on Advanced re-expand
- [ ] CUDA Pack download + install + engine pickup happens in <90 s on a 100 Mbps connection (network-bound; SHA256 verified after extract)

---

## Phase 6 — Deep Analyze (VLMs)  (3–4 weeks)

The user opens Deep Analyze, picks a model, hits "Analyze entire library." The engine downloads the model (if not already), loads it, processes every image with structured prompts, writes captions + proposed names back to the DB. Library tab now shows smart names. Per-file analysis works from the preview sheet.

### 6.1 Engine — VLM downloader (`engine/src/downloader.rs`)
- [ ] 12-way parallel HF range-GET downloader via reqwest + tokio
- [ ] Per-file progress stream → `modelDownloadProgress` IPC events
- [ ] Resume support: `Range: bytes=N-` on partial downloads
- [ ] SHA256 verification per file
- [ ] Write to `%LOCALAPPDATA%\FileID\Models\HuggingFace\<repo>\`
- [ ] EMA download-rate smoothing (0.7 old + 0.3 new)
- [ ] Cancel via tokio `CancellationToken`; lands at the next loop checkpoint within ~1 s
- [ ] **No telemetry** — only network code in the engine; documented in PRIVACY.md

### 6.2 Engine — llama.cpp wrapper (`models/vlm.rs`)
- [ ] Pin a llama.cpp commit; build as cdylib; add to engine's DLL search path
- [ ] `llama-cpp-2` Rust bindings (or hand-rolled via bindgen)
- [ ] Load GGUF + mmproj split for vision adapter
- [ ] Backend auto-pick: CUDA → Vulkan → DirectML → CPU
- [ ] RAM budget validator: `GlobalMemoryStatusEx` → estimate model RAM from quantization × params; refuse load if estimate > available - 4 GB
- [ ] Image preprocessing: 448×448 for description, 256×256 for face comparison (matches macOS DeepAnalyze sizes)
- [ ] Structured prompt with `DESCRIPTION:` / `FILENAME:` output parsing
- [ ] Face comparison prompt with `VERDICT: SAME/DIFFERENT` + `CONFIDENCE: 0.0–1.0` output (used by Phase 3 cluster_suggestions)
- [ ] GPU cache management: clear every 50 inferences (matches macOS `MLX.GPU.clearCache`)

### 6.3 Engine — DeepAnalyzeRunner (`pipeline/deep_analyze.rs`)
- [ ] Job dispatcher for `deepAnalyzeFile` / `deepAnalyzeFolder` / `deepAnalyzeAll`
- [ ] Phase events: `deepAnalyzeStarting(queued)` → `deepAnalyzeStarting(loadingModel)` → `deepAnalyzeStarting(resolvingTargets)` → per-file `deepAnalyzeProgress` + `deepAnalyzeFileDone` → `deepAnalyzeComplete`
- [ ] Resolve targets: skip files where `vlm_description IS NOT NULL AND vlm_model = currentModel` if `skipExisting=true`
- [ ] Folder targeting: `path_text LIKE prefix%`
- [ ] Per-file: load image (or PDF page render via pdfium / video keyframe via Media Foundation / doc thumbnail via `IThumbnailProvider`), run inference, parse output, write to DB
- [ ] Cancellation via IPC `deepAnalyzeCancel` — lands within ~1 s
- [ ] `prewarmModel` IPC → download-only path (no inference) for the welcome sheet flow

### 6.4 Engine — VLM lineup
Each model has its own cfg in `models/vlm_registry.rs`:
- [ ] Qwen 2.5-VL 3B (recommended for 8–16 GB and Snapdragon WoA)
- [ ] Qwen 2.5-VL 7B (recommended for ≥16 GB + dGPU)
- [ ] Gemma 3 4B vision (alt captioner)
- [ ] SmolVLM (tiny / battery-conscious / WoA fallback)
- [ ] MiniCPM-V 2.6 (PaliGemma substitute)
Each cfg: HF repo, file list, total bytes, RAM estimate, recommended GPU memory, prompt templates.

### 6.5 Deep Analyze tab UI (`FileID.App/Views/DeepAnalyzeView.xaml`)
- [ ] Header section + model picker (3 recommended + "Show all available" disclosure with `DisclosureGroup` equivalent)
- [ ] Per-model option row: radio-style selection, "Downloaded" / "Will download X GB" / "Needs Y GB RAM" badges
- [ ] Disabled + orange warning when model would OOM
- [ ] Per-model install progress (fraction bar + speed + ETA)
- [ ] Status card: in-progress / pending / completed (depending on state)
- [ ] Actions card: "Start Deep Analyze" gold CTA + "skip naming" escape hatch
- [ ] `skipExisting` toggle: reanalyze captioned files or skip them
- [ ] Smart names card: "X files ready" + "Review and apply…" → opens `BulkRenameSheet`
- [ ] Starting card: appears when DA is in-flight but progress hasn't landed yet (0.35 s spring entrance)
- [ ] Progress card: "X / Y files" counter + ETA + current path
- [ ] Last-file card: most recent file processed + its caption
- [ ] Completion card with summary
- [ ] Unavailable card: "Deep Analyze isn't available on this build" when llama.cpp DLL is missing (mirrors macOS `mlx.metallib` missing)
- [ ] Auto-tab-switch to Library when DA completes

### 6.6 Per-file Deep Analyze
- [ ] "Analyze with VLM" button in Library preview sheet toolbar
- [ ] Single-file `deepAnalyzeFile` IPC; shows inline starting / progress / done
- [ ] Result writes to DB and surfaces in preview metadata pane

### 6.7 Acceptance for Phase 6
- [ ] All 4 shippable VLMs load + caption a sample on a 12 GB GPU box
- [ ] VLM cold-load time within 2× of macOS MLX cold-load
- [ ] Captions populate the Library FTS5 index (search for caption keywords returns matching files)
- [ ] DA hint banner appears in Restructure when <40% of files captioned
- [ ] Cancellation works mid-batch within ~1 s
- [ ] CI privacy gate continues to pass with llama.cpp linked

---

## Phase 7 — Restructure tab  (3–4 weeks)

Largest single tab. Folder classification + Sankey + tree-diff + drill-down + apply-as-shortcut + convert-to-real-moves.

### 7.1 Engine — FolderClassifier port (`pipeline/folder_classifier.rs`)
- [ ] 1:1 port of `FolderClassifier.swift` (pure logic, no Apple deps); same anchor/mixed/junk classification
- [ ] Person-anchor folders, time-anchor folders (year detection), place-anchor folders
- [ ] Junk heuristics: "Untitled folder", "Camera Roll", "New Folder", etc.

### 7.2 Engine — Restructure proposal engine (`pipeline/restructure.rs`)
- [ ] Build proposals: per-file → destination bucket
- [ ] Sankey-source-by-destination aggregation
- [ ] Recommendation rows: per-outcome (Anchor / Mixed / Junk) with file lists
- [ ] **Path safety**: sanitize VLM-proposed names (strip `..`, leading dots, `/`, `\`, illegal Windows chars), containment check (target.canonicalize() startsWith root.canonicalize()) — port `RestructureEngine.sanitize*` and the containment guard from `RestructureView.swift`/`Restructure.swift`
- [ ] Apply mode 1 (default on Windows): real move via `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`
- [ ] Apply mode 2 (advanced, opt-in): `CreateSymbolicLinkW`. Surfaces Developer Mode requirement to the user when `SeCreateSymbolicLinkPrivilege` is missing.
- [ ] Convert-symlinks-to-real-moves command: read symlink destination → verify it matches the apply-time destination (TOCTOU guard) → move

### 7.3 Restructure tab UI (`FileID.App/Views/Restructure/RestructureView.xaml`)
- [ ] View mode toggle: Cards vs Tree (`ThemedSegmentedControl`); persist in settings.json
- [ ] Stat hero: 3 outcome tiles (Staying put / Tidying / Reorganizing) with hover cross-highlight (scale 1.012, 0.18 s easeInOut, shadow elevation)
- [ ] Hover bus: cursor-to-stat-tile or cursor-to-Sankey-ribbon updates a shared highlight state; recommendation rows + Sankey + stat hero all listen
- [ ] Recommendation rows: per-outcome cards with file counts + approval checkboxes
- [ ] "Staying put" disclosure: anchor folder breakdown
- [ ] "Nothing to move" card when classification is done but no proposals
- [ ] Empty state when no root selected or analysis not run
- [ ] Library root auto-defaults to most recent scan session
- [ ] Selection summary: "X of Y selected"
- [ ] Step chips at top: "1. Apply as shortcuts" → "2. Convert to real moves"
- [ ] DA hint banner when < 40% of files captioned
- [ ] Status banner after apply

### 7.4 SankeyFlowControl (`Restructure/SankeyFlowControl.cs`)
- [ ] Win2D `CanvasControl` with `Draw` override
- [ ] Top 8 source folders + "X more" rollup
- [ ] Cubic bezier ribbons via `CanvasPathBuilder`; column headers; entrance animation 0.55 s easeOut
- [ ] **Hover hit-testing**: cursor-to-bezier proximity check via `CanvasGeometry.StrokeContainsPoint` — port the macOS proximity math 1:1 (it's platform-independent)
- [ ] Cross-highlight via shared hover bus
- [ ] Tooltip near cursor showing "Source folder → Destination bucket" + file count
- [ ] Click source/dest node → drill down

### 7.5 TreeDiffControl (`Restructure/TreeDiffControl.cs`)
- [ ] Custom `Control` rendering side-by-side hierarchical diff
- [ ] Old structure on left, new structure on right
- [ ] Color-coded: green (new in destination), red (moved out), gray (unchanged)
- [ ] Indented stripe overlay matching macOS aesthetic

### 7.6 Restructure — apply bar (`Restructure/RestructureApplyBar.xaml`)
- [ ] Floating frosted-glass bar pinned to bottom (`ExperimentalAcrylicBorder` + 1px stroke + drop shadow)
- [ ] Selection summary: "X of Y selected"
- [ ] Step chips: "1. Apply as shortcuts" / "2. Convert to real moves"
- [ ] Primary button: "Apply as shortcuts (X)" with gold→goldDim gradient fill
- [ ] Secondary button: "Convert to real moves" with stroke only
- [ ] Hover scale on primary 1.02 (0.18 s)
- [ ] Disabled state opacity 0.45
- [ ] Confirmation dialog on convert-to-real-moves ("This is irreversible. Confirm?")
- [ ] Bar entrance animation: 0.4 s spring (response 0.4, dampingFraction 0.8) on selection becoming non-zero

### 7.7 Restructure — drill-down sheets
- [ ] Enum-routed scope: all / by outcome / by source folder / by destination bucket / long-tail rollup
- [ ] File-level detail per drill-down

### 7.8 Acceptance for Phase 7
- [ ] Side-by-side video review: Sankey hover behavior matches macOS exactly (cross-highlight latency, ribbon thickness, color)
- [ ] Folder classification on the corpus matches macOS exactly (path safety port verified by unit tests)
- [ ] Apply-as-shortcuts on 100 files completes < 5 s
- [ ] Convert-to-real-moves with TOCTOU verification rejects swapped symlinks
- [ ] Path traversal containment rejects malicious VLM names

---

## Phase 8 — AutoPilot mode + cross-cutting polish  (1–2 weeks)

Glue + audits. Features that depend on every previous phase being in place.

### 8.1 AutoPilot mode
- [ ] "Organize Everything" CTA in sidebar (or a new Auto tab — match macOS placement)
- [ ] Stage 1: Scan if needed
- [ ] Stage 2: Face clustering (auto-enqueued; engine-side)
- [ ] Stage 3: Deep Analyze entire library with recommended VLM
- [ ] Stage 4: Restructure proposals computed; tab auto-switches
- [ ] Persist `autoPilotActive` + `autoPilotStage` in `EngineClient` (matches macOS)
- [ ] Stage advancement watchdog timeouts (matches macOS, e.g. 6 s for face clustering settle)
- [ ] User can cancel at any stage

### 8.2 Wipe & rescan
- [ ] Sidebar "Wipe library + rescan" button → confirmation dialog
- [ ] Engine receives shutdown command, sets a pre-exit flag to wipe DB on next start
- [ ] App respawns engine, which deletes `fileid.sqlite` + WAL/SHM, re-creates schema, auto-starts scan against the persisted folder

### 8.3 6-step pipeline diagram (onboarding splash)
- [ ] Empty Detail view (no folder picked, no files): renders 6-step diagram (Pick folder → Scan → Group people → Find dupes → Deep Analyze → Reorganize) with gold-accented icons + connecting lines (matches `Detail.swift:104-199`)

### 8.4 Tab crossfade animation
- [ ] Switching tabs: 0.22 s easeInOut crossfade (matches `Detail.swift:98`)

### 8.5 Audit pass — animations
- [ ] Every animation duration in `shared/docs/VISUAL-LANGUAGE.md` matched on Windows
- [ ] Reduce-motion bridge gates every motion primitive
- [ ] Side-by-side recordings of every animated surface (LavaLamp, Shimmer, Ripple, IridescentBorder, springs) reviewed against macOS

### 8.6 Audit pass — empty / loading / error states
- [ ] Library: "No images yet" / loading shimmer / error inline banner
- [ ] People: "No faces yet" / loading shimmer
- [ ] Cleanup: "No duplicates found" / "All groups skipped"
- [ ] Deep Analyze: "Deep Analyze isn't available on this build" (when llama.cpp missing) / "No smart names yet"
- [ ] Restructure: "No proposals" (root not selected OR analysis not run)
- [ ] Onboarding splash on empty Detail
- [ ] Engine crashed pill in sidebar with last-error reason
- [ ] DB open failure bubbled to UI

### 8.7 Audit pass — keyboard nav + accessibility
- [ ] Every interactive control reachable via Tab key
- [ ] Screen reader labels on every actionable element
- [ ] AnnouncementHelper for status-banner-equivalents (banner appears → screen reader announces)
- [ ] Contrast: gold @ 80% against `#141414` background passes WCAG AA for large text; verify with the Accessibility Insights tool
- [ ] All keyboard shortcuts (Ctrl+O / Ctrl+R / Ctrl+Shift+S / Ctrl+F + any Phase 1 additions) confirmed working

### 8.8 Acceptance for Phase 8
- [ ] AutoPilot end-to-end on the corpus completes without manual intervention
- [ ] Wipe-and-rescan cleanly resets state without engine crash
- [ ] Accessibility Insights audit reports zero "Critical" issues

---

## Phase 9 — Shell integration polish  (1 week)

The tabs work. Now sand off the "feels Windows-native" rough edges.

### 9.1 Reveal in Explorer everywhere
- [ ] Library tile context menu: "Reveal in Explorer"
- [ ] Preview sheet toolbar: "Reveal in Explorer"
- [ ] People person-detail face: "Reveal in Explorer"
- [ ] Cleanup tile: "Reveal in Explorer"
- [ ] Sidebar folder row: "Reveal in Explorer"
- [ ] All wired through `shell/reveal.rs::SHOpenFolderAndSelectItems`

### 9.2 Open in default app everywhere
- [ ] Library tile context menu / preview sheet / video CTA → `shell/open.rs::ShellExecuteW`
- [ ] PDF preview "Open externally" CTA → ShellExecuteW

### 9.3 Drag-out of FileID
- [ ] Drag a Library tile out → produces a normal Windows drag drop with the file URL (Explorer accepts; user can drop into another app)
- [ ] Drag selection (multi-select) drags every selected file

### 9.4 Windows IPreviewHandler (optional, parity with macOS QuickLook plugin)
- [ ] DB-aware preview: when Explorer hovers a file FileID has tagged, the preview pane shows the FileID metadata pane (caption + tags + person names)
- [ ] Registered as a COM in-proc server during MSI install
- [ ] **If complexity exceeds 1 week, defer to v1.1** — the macOS QuickLook extension is also nice-to-have not core

### 9.5 Acceptance for Phase 9
- [ ] Right-click a tile → context menu has at least: Open, Reveal in Explorer, Apply tags, Apply name, Analyze with VLM, Trash
- [ ] Drag a Library tile to Notepad → Notepad opens with that file's content
- [ ] (Optional) Hover a FileID-tagged file in Explorer → preview pane shows FileID metadata

---

## Phase 10 — Performance + soak  (1–2 weeks)

### 10.1 Benchmarking
- [ ] Cold scan benchmark on each hardware tier (NVIDIA / Intel / AMD / Snapdragon / CPU-only)
- [ ] p95 latencies match `shared/docs/ARCHITECTURE.md` targets per EP
- [ ] Memory ceiling enforced (≤ 1.2 GB at 50K files)
- [ ] WAL checkpoint cadence tuned (don't checkpoint too aggressively under sustained writes)
- [ ] Hot-path concurrency: sync-mirror cancellation overhead < 1% throughput; semaphore counts (3-4 ORT, 2 CLIP) tuned per hardware

### 10.2 Soak test
- [ ] Run 10 back-to-back scans of the test corpus without restart; memory trends flat (no leaks)
- [ ] Run 24-hour soak with idle + occasional scan + DA + cluster
- [ ] No `WaitForSingleObject` watchdog false-positives during normal operation
- [ ] Engine respawn within 5 s of crash, state preserved (DB intact)

### 10.3 LavaLamp side-by-side review (final)
- [ ] Record 30 s of LavaLamp at 1080p on macOS; same on Windows; visually compare frame-by-frame
- [ ] Verify ProMotion-equivalent (high refresh rate display) drives at native rate

### 10.4 Tooling
- [ ] `iterate.ps1` — port of macOS `iterate.sh`, drives engine via stdin commands and runs the 11 assertions
- [ ] `assertions.py` — language-neutral; reads SQLite directly; same checks as macOS
- [ ] `iterate-arm64.ps1` — same as `iterate.ps1` but cross-runs on ARM64 hardware
- [ ] CI matrix runs all three on every PR

### 10.5 Acceptance for Phase 10
- [ ] All 11 assertions GREEN on x64 + ARM64 hardware tiers
- [ ] No memory growth in 24-h soak
- [ ] LavaLamp side-by-side review subjectively matches macOS

---

## Phase 11 — Installer, code-signing, ship  (2 weeks)

### 11.1 WiX Toolset v4 installer
- [ ] `installer/FileID.wixproj` builds `FileID-x64.msi` and `FileID-arm64.msi`
- [ ] Default install path `C:\Program Files\FileID\`
- [ ] Start Menu shortcut + (optional) Desktop shortcut
- [ ] Per-user data path is `%LOCALAPPDATA%\FileID\` (already engine convention; installer doesn't touch it)
- [ ] Uninstaller removes binaries; **does NOT auto-delete user data** (mirrors macOS posture)
- [ ] WiX bootstrapper that detects host arch and runs the matching MSI (optional; users can also download the right MSI directly)

### 11.2 Authenticode code-signing
- [ ] EV cert procured (user — gating; budget $300–500 + ~3 weeks lead time per ARCHITECTURE.md note)
- [ ] `signtool` integrated into `publish.ps1`: signs `FileIDEngine.exe`, `FileID.exe`, every shipped DLL, the MSI itself
- [ ] Timestamping via `http://timestamp.digicert.com` (or equivalent)
- [ ] Signature verified post-build via `signtool verify /pa`

### 11.3 WinVerifyTrust integrity check (tightened)
- [ ] Phase 1 wired the basic check; Phase 11 verifies it works against the EV-signed binary
- [ ] Refusal log path: when the engine signature fails, the app surfaces "Engine integrity check failed" + log path

### 11.4 Logs + crash handling (verify)
- [ ] All logs land in `%LOCALAPPDATA%\FileID\logs\` (already implemented in Phase 0)
- [ ] No crash-reporting service (already enforced by CI privacy gate)
- [ ] User can attach `engine.jsonl` to a GitHub issue manually

### 11.5 CI privacy gate (re-verify)
- [ ] Telemetry-string scan still passes on the final binaries
- [ ] Wireshark/Fiddler dry run: idle FileID = zero packets; scan = zero packets; Deep Analyze model fetch = HF traffic only

### 11.6 Final docs
- [ ] `platforms/windows/README.md` updated with install instructions + system requirements
- [ ] `shared/docs/PRIVACY.md` final review
- [ ] `shared/docs/MODELS.md` final SHA256 list per model
- [ ] `shared/docs/SHIP.md` Windows section appended; release checklist
- [ ] In-app About dialog shows version + signature chain summary

### 11.7 Acceptance for Phase 11
- [ ] `FileID-x64.msi` + `FileID-arm64.msi` install + uninstall on clean Windows 10 22H2 + Windows 11 + Windows 11 ARM64 VMs
- [ ] Authenticode chain verifies on a clean machine (no SmartScreen "unknown publisher" prompt with EV cert)
- [ ] All 11 corpus regression assertions pass on a clean install
- [ ] CI privacy gate is a hard gate on the release tag

---

## Phase 12 — Linux  (deferred — separate planning later)

Out of scope for the Windows ship. Cross-reference with `platforms/linux/` placeholder when the time comes.

---

## What this phase plan does NOT include (intentional omissions)

These are conscious cuts from the macOS feature set OR features the Windows port won't reproduce. Document each so it's a decision, not a miss.

- **Security-scoped bookmarks**. Windows desktop apps don't have the macOS sandbox; absolute paths are persisted directly.
- **`com.apple.metadata:_kMDItemUserTags` xattr**. Replaced by `IPropertyStore` `System.Keywords`. The semantics differ slightly: Windows tags surface in Explorer for files with property handlers (jpg/docx/mp3) but not for arbitrary file types. The DB-side tag store is canonical.
- **Spotlight indexing**. macOS auto-indexes paths; Windows Search may or may not index files without `System.Keywords`. We don't proactively register with Windows Search.
- **`run.sh`-style wipe-and-rebuild dev script**. Replaced by `build.ps1` which is more conservative (no DB wipe by default).
- **Sparkle auto-update channel**. Out of scope; manual updates only at v1.0 (matches macOS).
- **Localization**. English only at v1, matches macOS.
- **macOS-only DMG packaging**. WiX MSI is the Windows analog.

---

## Estimate summary

| Phase | Scope | Calendar weeks |
|---|---|---|
| 0 | Foundation | _shipped_ |
| 1 | App shell + theme + sidebar + welcome sheet | 3–4 |
| 2 | Scan pipeline + Library tab | 4–5 |
| 3 | People tab + face clustering | 2–3 |
| 4 | Cleanup tab | 1 |
| 5 | Settings tab + privacy panel + Performance Pack UX | 1–2 |
| 6 | Deep Analyze (VLMs) | 3–4 |
| 7 | Restructure tab | 3–4 |
| 8 | AutoPilot + cross-cutting polish | 1–2 |
| 9 | Shell integration polish | 1 |
| 10 | Performance + soak | 1–2 |
| 11 | Installer + ship | 2 |
| **Total (excluding Linux)** | | **22–30 weeks** |

This is a single-dev-full-time estimate with no scope creep. Realistic with normal life: **~8–12 months end-to-end**. Phases 1, 2, 6, and 7 are the largest individual investments and the highest-risk for slip.

If the calendar gets tight, the most defensible cuts (in priority order):
1. Phase 9.4 IPreviewHandler — defer to v1.1
2. Phase 8.1 AutoPilot — defer to v1.1 (tabs work standalone)
3. Phase 5.3 Performance Packs — defer to v1.1; ship with DirectML-only baseline
4. Phase 6 → ship with one VLM (Qwen 2.5-VL 3B) and add others in v1.1

Anything below those ("the privacy panel is too pretty", "we don't need both view modes in Restructure") is **not** an acceptable cut. They're load-bearing for the 1:1 parity claim.

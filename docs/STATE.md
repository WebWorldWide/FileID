# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.

## V9 (2026-04-30) — V1 deletion, organizational pass, security audit

**V1 cleanup**
- Deleted `docs/history/v1-app/` (43 MB, including a compiled V1 binary, V1 source tree, and V1 tests). Nothing in the live codebase referenced it.
- `scripts/iterate_truenas.sh`: replaced `--product FileIDv2` with `--product FileID`.
- `run.sh`: stripped stale "v1 launcher preserved" / "FileIDv2.app" comments.
- Path references in `docs/BUGS.md` and `docs/DECISIONS.md` updated from `app/Sources/FileIDv2/...` and `legacy/v1/...` to current paths.

**Organizational**
- Shared `bucketIconName(_:)` helper at `Views/Restructure/BucketIcon.swift`. Removed three duplicate copies (RestructureView, TreeDiffView, SankeyFlowView).
- `Views/ReviewSettingsViews.swift` → `Views/SettingsView.swift` (singular).
- `DeepAnalyzeSettings` (a service-level `@Observable`) extracted from `Views/DeepAnalyzeViews.swift` into `Services/DeepAnalyzeSettings.swift`.
- `AppSettings.swift` and `AppSupportPath.swift` moved into a new `Core/` subdirectory.
- `Sidebar.swift` (695 lines) split into a `Views/Sidebar/` subdirectory with four files: `Sidebar.swift` (composition root + nav rows), `SidebarProcessingControl.swift`, `SidebarPipelineProgress.swift`, `SidebarQueueList.swift`, `SidebarEngineStatus.swift`.
- `engine/Sources/FileIDEngine/` reorganized into subdirectories: `Pipeline/` (Discovery, Tagging, DeepAnalyze, DeepAnalyzeRunner, FaceClustering, IdentityClustering, Restructure), `Storage/` (Database, DBWriter), `Models/` (AIModelsEngine, MobileCLIPService, ArcFaceService, DeepAnalyzeCapability, HNSWIndex), `IPC/` (IPCSink, JSONLog). Top level keeps the entry point + cross-cutting helpers.

**Security audit fixes**
- **CRITICAL: Engine binary integrity check.** `EngineClient.start()` validates the engine binary against the app's designated requirement via `Security.framework` before spawning. Refuses to spawn if the binary isn't inside the app bundle's `Contents/MacOS/`, isn't signed, or doesn't satisfy the same requirement string as the app.
- **CRITICAL: Symlink TOCTOU.** Dropped the racy `fileExists` pre-check in `RestructureEngine.apply` — `createSymbolicLink` is now the atomic existence test, with `EEXIST` mapped to a conflict result. `convertSymlinksToMoves` reads the symlink's actual destination via `destinationOfSymbolicLink` and rejects the conversion if the target was swapped between apply and convert.
- **MEDIUM: Path traversal containment.** New `RestructureEngine.sanitizePathSegment` / `sanitizeFilename` strip `..`, leading dots, and `/` from VLM-proposed names + bucket components. After constructing the target URL, `RestructureEngine.compute` verifies `target.standardizedFileURL.path` starts with `root.standardizedFileURL.path + "/"` — drops the proposal otherwise.
- **MEDIUM: Zip-bomb defense.** `CLIPModelInstaller.runExtract` checks ≥1 GB free disk on the target volume before extraction and bounds the unzip with a 5-minute watchdog that calls `Process.terminate()`.
- **LOW: Logging redaction consistency.** `MobileCLIPService` model-load log calls now wrap their path argument in `redactPathForLog(_:)`.
- New `docs/SECURITY.md` documents the audit, what's fixed, and what's deferred to v1.0 (per-model SHA256, HuggingFace cert pinning, tokenizer DoS hardening).

**Verification:** debug build clean, release build clean, 28/28 tests GREEN, binaries rebundled into `FileID.app`.

---

## V8.5 (2026-04-30) — Restructure V3, Sankey perf + polish, V5 cleanup pass

Restructure tab landed in its production form. Major work:

**Restructure UI (`RestructureView.swift`, `Restructure/*.swift`)**
- One unified hero surface — Sankey + recommendation rows in a single GlassCard with hairline dividers. Stops the "stacked materials" overlap problem at the root.
- `RestructureStatHero` — three big-number tiles (Staying / Tidying / Reorganizing). Hover any tile to cross-highlight matching ribbons + cards.
- `RestructureRecommendationRow` — Settings-list-style rows with vertical gold accent strip on hover; no per-row materials.
- `RestructureApplyBar` — floating frosted bar pinned to bottom. Selection summary + numbered step chips + gold-gradient primary CTA.
- `RestructureHoverBus` — `@MainActor @Observable` shared between Sankey, cards, tree, and staysPut rows. Coalesced setter; reads via cached lookup tables (`destinationsForSource`, `sourcesForDestination`, `nodesByOutcome`) so cross-highlight is O(1) per node.

**Sankey (`Restructure/SankeyFlowView.swift`)**
- Single `Canvas` for all ribbons (was 70+ `Path` Views with per-ribbon `.onHover` and per-ribbon `.blur`). Massive perf win.
- Layout cached in `@State` and recomputed only on `proposals.count` or geometry change. `Dictionary(grouping:)` never runs on the render path.
- Source-tinted ribbons (gold for junk, orange for mixed) with two-layer gradient, `.compositingGroup` removed.
- Barycentric ordering (two-pass weighted-average) cuts ribbon crossings.
- Single `.onContinuousHover` walks the small flow list, hit-tests via cursor-to-bezier proximity.
- 14pt internal vertical buffer so focused-node 12pt halo never clips at the column edges.
- Rollups pinned to bottom of column, lighter visual treatment.
- Column headers (FROM → TO with monospaced counts and a center arrow).
- In-ribbon tooltip on hover near the cursor showing source → destination and file count.
- 0.55s easeOut entrance animation on first appearance / data change.
- Rollup tap fixed: `.sourceFolders([String])` / `.destBuckets([String])` drill-down scopes filter the long-tail folders, not a literal "+ N more folders" string that matches nothing.

**Deep Analyze ↔ Restructure**
- Hint banner ("Sharper proposals with Deep Analyze") shown when DA has captioned <40% of analyzable files. Lavender Theme.ai accent. Dismissable.
- `bucketForFile` reads `vlmDescription` and routes images of receipts/screenshots/forms/tickets/IDs/diagrams to specific Documents subcategories.
- `ReadStore.totalAnalyzableFiles()` powers the banner's coverage fraction.

**Skip → Deep Analyze instant feedback (V8 task 1)**
- New `IPCEvent.deepAnalyzeStarting(DeepAnalyzeStarting)` with phases `.queued` / `.loadingModel` / `.resolvingTargets`. Engine streams these the moment a DA command arrives + as the runner advances. App's `startingCard` binds its subtitle to the phase message + adds a gold `ShimmerView` bar so the ~10s VLM cold-load no longer feels frozen.

**Sidebar (`Sidebar.swift`)**
- Section spacing 16 → 22pt; horizontal padding 12 → 14pt.
- Nav rows: 20pt-wide icon column, 13pt label, gold stroke when active (was just background fill).
- Stats grouped on a recessed card so the row of monospaced numbers reads as a unit.
- "System sleep blocked while scan runs (lid-closed safe on AC)" line removed (duplicate of Settings).

**Cleanup (V5 push)**
- Dead RestructureView helpers removed: `actionsBar`, `stepBadge`, `sankeyCard`, `recommendationsStack`, `assistantSummaryCard`, `outcomeRow`, `beforeAfterCard`, `flowRows`/`FlowRow`/`flowRowView`, `flowSubtitle`, `legendStrip`, `legendChip`, `BeforeKind`, `beforeRowStyle`, `destinationChip`, `proposalsPreviewCard`. Old `RecommendationCard.swift` deleted (replaced by `RestructureRecommendationRow`).
- Verbose AI-style multi-paragraph comments replaced with terse single-line WHY notes throughout the Restructure subsystem and Sidebar.
- `LavaLampBackground` 60Hz cap dropped (`TimelineView(.animation(...))` is now system-paced — picks up ProMotion 120Hz on supported displays).

**Verification:** `swift build` clean (debug + release). 28/28 tests GREEN. Binaries rebundled into `FileID.app`.

---

## V7 (2026-04-30 evening) — Restructure redesign + Deep Analyze coverage

Replaced the single-column flow card with a Sankey diagram + dual-pane Tree view toggleable from a header pill. Deep Analyze coverage extended: SQL filter `kind IN ('image', 'pdf')` → `kind IN ('image', 'pdf', 'video', 'doc')`. Videos use AVAssetImageGenerator (keyframe at 25%); office docs fall back to QLThumbnailGenerator (8s timeout). BulkRenameSheet renders Quick Look thumbnails per row.

Audit fixes carried forward: Sankey overlap fix (third-pass clamp + `availableHeight`-respecting layout). `AppSupportPath` helper replaced every `.first!` force-unwrap. CLIPTokenizer caps `vocab.json` / `merges.txt`. `redactPathForLog(_:)` applied to every log call. GROUP_CONCAT separator → `\u{1F}` ASCII unit-separator.

## V2 (2026-04-29) — Face clustering V2 + split-process rewrite

Replaced Chinese Whispers with `IdentityClustering` — two-pass density + Pass 3 quality validation. ArcFace required (Vision-feature-print fallback deleted). Identity persistence via centroid + 90th-percentile anchor radius on the `persons` row.

V2 split-process landed earlier this day: engine CLI as child of app, IPC over stdin/stdout newline-delimited JSON, GRDB.swift on SQLite WAL, MobileCLIP-S2 image embeddings.

11/11 iterate.sh assertions GREEN, no mega-cluster on the test corpus.

---

Earlier history is in `~/.claude/plans/in-media-library-i-temporal-acorn.md`.

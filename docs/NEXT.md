# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## V8 — v1.0 release readiness (queued, not started)

The V7 audit surfaced 54 gaps for an open-source v1.0 release. **15 must-haves**, ~6 working days total. See `~/.claude/plans/in-media-library-i-temporal-acorn.md` for the full inventory + the 39 nice-to-haves.

Top of the queue, in priority order:

1. **`.photoslibrary` / `.aplibrary` exclude rules** in `engine/Sources/FileIDEngine/Discovery.swift:80-126` — Discovery currently descends into Apple Photos packages, which is a corruption risk. Treat them as opaque files.
2. **Crash recovery completion** — `last_file_index` is written to `scan_sessions` but never read at startup, so a resumed scan re-scans from zero. `engine/Sources/FileIDEngine/FileIDEngineMain.swift:560-585` is where the read should land.
3. **DB backup / export** — users spend hours naming faces; a single corruption = trust loss. Add a "Back up library" button in Settings + an "Import" path. `~/Library/Application Support/FileID/fileid.sqlite` + the WAL/SHM files.
4. **First-run privacy/consent panel** — backs the "100 % on-device" pitch. Should show what gets read (filenames, EXIF, image bytes for AI), what doesn't (network, telemetry), and where data lives.
5. **Distribution basics** — `LICENSE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `.github/` issue templates, README screenshots + GIFs. Currently absent.
6. **DMG packaging + codesign + notarize** workflow.
7. **Sparkle auto-update** integration.
8. **Help menu** with keyboard shortcut cheat sheet (currently only About dialog).
9. **Diagnostic bundle export** (logs + DB schema, no PII) for OSS support.
10. **Per-folder exclude rules** (Time-Machine-style).
11. **Quick Look extension** — currently source-only scaffolding; either ship as Xcode subproject or remove the README claim.
12. **Spotlight incremental indexing** — currently re-indexes everything on every launch (wastes battery + I/O).
13. **RAW + JPEG pair handling** — don't dedupe siblings. Common photographer use case.

Items 1–4 are critical safety/correctness. Items 5–7 are required for any "trustable open-source release." Items 8–13 are polish.

---

## Recently shipped (history)

## Done in the late-evening hardening pass (2026-04-25)

Continuation of the autonomous loop from the user's "keep working till it's perfect" directive. **All M4-cut features that were deferred in the original plan are now landed.** v2 is daily-usable end-to-end.

- **People tab end-to-end** — face detection in the per-file scan emits bbox + quality + landmarks. Stage D's lazy ArcFace embedder writes 512-d L2-normalized face IDs into `face_prints.arcface_embedding`. The `runFaceClustering` IPC command then runs IdentityClustering (two-pass density + Pass 3 quality validation; cosine ≥ 0.55 cores, margin-rule outlier assignment, 2-means split for mixed clusters) and persists `persons` + `face_prints.person_id` idempotently. PeopleView shows person cards with face-cropped representative thumbnails, tap → sheet with all photos for that person + rename field. `engine.lastFaceClustering` summary surfaces in the header.
- **Engine auto-respawn** — `EngineClient.handleEngineExit` schedules up to 3 attempts with 1s/4s/16s backoff over a 60s window. Successful `.ready` resets the budget. Manual "Restart Engine" button in Settings recovers from budget exhaustion.
- **Post-scan orphan sweep** — `FileIDEngineMain.sweepOrphans` deletes up to 5000 rows under the scan root whose file no longer exists on disk (rows with `scanned_at < scanStart`). ON DELETE CASCADE handles the joined tables. Skipped on cancelled scans.
- **MediaPreviewOverlay parity** — added prev/next sibling navigation with ← → keyboard shortcuts and AVKit `VideoPlayer` for `kind == "video"` files in `LibraryView.FilePreviewSheet`. Uses the current `rows` array as the navigable list.
- **Sidebar engine-error dismiss + Settings engine controls** — sidebar's last-error pill has a × dismiss; Settings adds Restart/Stop Engine + Open app log + Show logs in Finder.
- **Latent-crash audit** — clean. The earlier `ThumbnailService` cross-queue resume bug was the only one of its class; engine continuations are all single-resume.

**Verified:** `bash scripts/iterate.sh 60` against TrueNAS — 144.7 files/s, 100% util, 162 MB resident, no new crashes. All build clean under Swift 6 strict concurrency.

---

## Deferred from `~/.claude/plans/i-need-you-to-refactored-cherny.md`

The original "FileID — Scan Performance & Tag Accuracy Overhaul" plan promised three sessions (A/B/C). Session A shipped in full. Session B and C were **planned but never implemented** — confirmed via grep on 2026-04-24 (no `TagTaxonomy`, `GeocodeQueue`, `CLIPTokenizer`, `ocrFast`, `setPersonTag`, or `renamePerson` symbols exist outside a single commented-out reference in `VisionWorker.swift`).

### Session B — Tag richness (not shipped)
- [ ] **`Sources/Services/TagTaxonomy.swift`** — hierarchy collapse (drop ancestors when descendants present), typed tags (`"kind:label"` storage with `TagKind` enum). Drops `"Blue_Sky" + "Sky" + "Outdoor"` duplication.
- [ ] **Always-on fast OCR.** Add `worker.ocrFast(_:)` returning `.fast`-level OCR with no language correction. Drop the document-tag gate in `MediaProcessor.swift:545` so whiteboard/sign/menu photos get OCR text tags.
- [ ] **EXIF camera model + `Year_<yyyy>` as tags.** Already read in `MediaProcessor.swift:575`, just never appended to `aiTags`. One-liner per field.
- [ ] **`Sources/Services/GeocodeQueue.swift`** — reverse-geocode GPS coords to `"location:City, State"` tags. `reverseGeocode` helper already exists at `MediaProcessor.swift:720` but is never called. Post-scan phase, not inline (CLGeocoder rate-limits).
- [ ] **Face-name propagation.** When user names a `PersonRecord` in PeopleView, fan out `"person:<name>"` tags to every `FileRecord.id` in `person.fileIDs`. New `FaceClusteringService.renamePerson(id:newName:)` + `FileIDDataStore.setPersonTag(recordIDs:name:oldName:)`.
- [ ] **Composition tags from saliency/horizon/barcode.** Requires extending `VisionWorker.runPrimaryPass` to include the extra requests, then typing the output.

### Session C — Open-vocabulary CLIP (not shipped)
- [ ] **`Sources/Services/CLIPTokenizer.swift`** — port OpenAI CLIP BPE tokenizer in pure Swift (~300 LOC, no deps). Bundle `clip_vocab.json` + `clip_merges.txt` as `Resources/`. The text encoder at `MobileCLIPService.swift:160` currently has a TODO and silently returns `nil` — so zero-shot CLIP labels never fire.
- [ ] **`Sources/Services/CLIPSelfTest.swift`** — 20-prompt golden self-test on first launch. Disable zero-shot if fails.
- [ ] **`Sources/Services/CLIPVocabulary.swift`** — expand the hardcoded 54-label list in `MobileCLIPService` to ~400 labels. Precompute text embeddings once, cache to `Application Support`. User-extendable via SettingsView.
- [ ] **CLIP-conditioned OCR escalation.** Trust CLIP top-1 to decide on `.accurate` re-OCR (whiteboards / handwritten notes get slow pass, misclassified-as-doc photos skip it).

---

## 0. Done this session — v2 Skunkworks Rewrite (M1→M5 first-pass) + autonomous-hour polish

The whole v2 stack — engine binary, IPC, schema, pipeline, UI shell, Library/Cleanup/Review/Settings tabs, brand integration. See `docs/STATE.md` for the full breakdown.

**Plus tonight's perf iteration loop (autonomous, against the user's TrueNAS library):**

Built a perf harness (`/tmp/perfharness.swift`-style — temporary scratch) that spawns the engine as a subprocess, runs scans against `/Volumes/Adlon/TrueNAS` (~60K files), captures structured batch events, computes throughput. 7 iterations:

| Iter | Change | files/s | Notes |
|---|---|---|---|
| 0 | Baseline (v1 best) | **13.8** | Per the v1 docs. v1 collapsed to 0.1 once. |
| 1 | LineReader stdin fix (engine could finally receive commands when launched from .app) | **97.3** | Same root cause as the stdout pipe bug — sync `read(upToCount:)` doesn't wake on parent writes in .app context. Switched to `readabilityHandler`. **7× over v1 just from architecture.** |
| 2 | CLIP semaphore 2 → 4, DB batch wait 50ms → 200ms | 102 (last10 avg) | CLIP ANE was underutilized; bigger batches reduced commit overhead. |
| 3 | Skip CLIP for files <30 KB | 108 (last10 avg) | Modest — most TrueNAS files are >30 KB. |
| 4 | (Diagnostic) CLIP entirely off | regressed | Confirmed CLIP isn't the bottleneck — load (NAS I/O) is. |
| 5 | Added per-stage timing breakdown to batch JSONL (load / vision / clip / OCR p50+p95) | — | Diagnostic only. **Found**: load p95 = 252ms; vision = 100ms; clip = 7ms. Load dominates. |
| 6 | `kCGImageSourceCreateThumbnailFromImageIfAbsent` (was `Always`) + thumb size 1024→512 | **123** (+27%) | Embedded JPEG thumbnails used when present. **Load dropped 70-90ms → 2-3ms.** Failures dropped from 21 → 10 too. |
| 7 | Trimmed Vision bundle: dropped `VNGenerateImageFeaturePrintRequest` + `VNGenerateAttentionBasedSaliencyImageRequest` (unused — scene-print was mislabeled as facePrints; saliency had no consumer) | **149** (+21% more) | Vision dropped 100ms → 82ms per file. |
| 8 | (Reverted) workerCap 14 → 18 | regressed to 27 | NAS saturated with parallel reads (load times spiked to 1500ms). 14 is the sweet spot for this storage tier. |

**Final: ~149 files/s on the user's 60K-file TrueNAS library.** That's **10.8× the v1 baseline** of 13.8/s. Per-stage breakdown (steady state, last 10 batches): load 2ms, Vision 82ms, CLIP 7ms, util 100%. Vision-on-ANE is the residual ceiling — would need either more aggressive request bundling or smaller input sizes to push further, both of which trade accuracy for speed.

**Plus this evening's autonomous-hour polish (after the user reported crashes/UX gaps):**

- **Stdout-pipe-doesn't-deliver bug.** When the SwiftUI .app launched the engine via Process(), engine writes to fd 1 silently dropped (verified via `Darwin.write(1, ...)` direct syscalls). Stderr (fd 2) worked. **Fix**: route IPC events over stderr instead. `IPCSink.emit` writes via `FileHandle.standardError.write(contentsOf:)`. Pump replaced sync `read(upToCount:)` with `readabilityHandler` (GCD/kqueue) — only pattern that reliably wakes in the .app context.
- **`@Observable` with `@unchecked Sendable` broke change tracking.** `EngineClient.state = .ready(info)` ran on MainActor but SwiftUI never re-rendered; UI stuck on "Starting…". **Fix**: dropped `@unchecked Sendable`, made `EngineClient` `@MainActor @Observable final class` — now observation triggers re-renders correctly.
- **Spam-clicking Start triggered `SQLITE_BUSY`.** Engine opened a NEW `DatabasePool` per `runScan`. GRDB explicitly forbids multiple pools to the same file. **Fix**: open the Database ONCE at engine startup; pass the same instance into every `runScan`. Dispatcher also rejects re-entrant `startScan` if a scan is already in progress.
- **Start Scan button could be spam-clicked.** **Fix**: optimistic `@State var startRequested` flips on click, button disables until the engine emits a phase change.
- **`engine.startScan` silently failed because of `.withSecurityScope` on bookmarks.** App isn't sandboxed — scope-required bookmarks fail. **Fix**: dropped `.withSecurityScope` from create; engine's resolve already had a fallback.
- **No explicit Start button.** Folder pick auto-fired the scan silently. **Fix**: explicit "Start Scan" button in the Processing Control panel; both NSOpenPanel and drag-drop just set `pickedURL` now.
- **Big v1-style progress card in Library tab during scans** (mirrors v1's headline progress feel).
- **`ThumbnailService` MainActor crash** (`EXC_BREAKPOINT` from QL completion crossing MainActor boundary). **Fix**: `generate(url:size:scale:)` is now `nonisolated`; cache + inflight bookkeeping stay on MainActor.
- **Cancel during discovery now works** via a `nonisolated(unsafe) static var` cancel mirror that the sync enumerator polls.
- **Crash recovery on engine startup**: any `scan_sessions WHERE status = 'running'` rows from a prior run get marked `crashed` with the last_file_index preserved. JSONL emits `crash_recovery_detected` event. Verified end-to-end with a fake crashed session.
- **Cleanup tab now prunes DB rows** for trashed files (cascades to tags/face_prints/ocr/clip_embeddings via FK).
- **Settings tab** shows MobileCLIP-S2 model status + "Open Models folder" button.

## 0a. Top deferred items (v2 — pick up here next session)

The whole v2 stack — engine binary, IPC, schema, pipeline, UI shell, Library/Cleanup/Review/Settings tabs, brand integration. See `docs/STATE.md` for the full breakdown. Headline: v2 is functional today for the basic flow (scan → tag → browse Library → delete duplicates) on a folder of any size, with structured JSONL telemetry and a per-batch resume cursor in the DB.

## 0a. Top deferred items (v2 — pick up here next session)

These are intentional cuts from the M3→M5 first pass, ranked by user-visible value:

1. ~~**People tab**~~ — ✅ DONE (V2 face-clustering rewrite, 2026-04-29). ArcFace 512-d L2-normalized embeddings in `face_prints.arcface_embedding`; pure-Swift `HNSWIndex` builds the kNN graph; `IdentityClustering.swift` (two-pass density + Pass 3 quality validation) replaces Chinese Whispers. Identity persistence via centroid + anchor radius on the persons row.
2. **AI Models picker UI** — Settings tab acknowledges it but currently swap is manual (replace `~/Library/Application Support/FileID/Models/mobileclip_image/` contents). Need: per-model download flow, license-acceptance sheet, ANE warmup on swap. Reuse v1's `AIModelDownloadService` pattern. ~3 hours.
3. **SigLIP 2 SO400M (accuracy embedder)** — accuracy-tier image embedding for "find me photos of dogs at the beach" semantic search. Needs ONNX Runtime SwiftPM dep, ~1.5 GB model download, lazy-on-viewed embed path. ~4 hours, ~10 GB disk for the model + cache.
4. **vectorlite (HNSW SQLite extension)** — sub-50ms k-NN queries at ≥500K vectors. Compile from source as a `.dylib`, vendor in `engine/Resources/`, load via `sqlite3_load_extension`. ~2 hours.
5. **Crash-resume on engine startup** — ✅ DETECTION DONE (logged, marked `crashed`); actual SKIP-EXISTING-FILES on resume still pending. Lookup `WHERE status='crashed'` at scan start, skip already-tagged paths from discovery. ~1 hour.
6. **Restructure tab** — proposal engine for folder hierarchy with diff preview. UX-heavy. ~10+ hours.
7. **Cancel during discovery** — ✅ DONE (sync cancel mirror polled by the enumerator loop).
8. **MediaPreviewOverlay full port** — v2's `FilePreviewSheet` covers the core (preview + metadata + Reveal in Finder). v1's overlay is fancier (full-screen, video player, EXIF slide-in, Deep Analyze button). ~3 hours.
9. **Migration UI** — first launch detects v1's `default.store*`, offers "Start fresh" or "Import old tags." Currently v2 just writes to a new path; v1 store is untouched. ~2 hours.
10. **Soak test + CI perf bench** — infrastructure work. Decide CI host (GitHub Actions / local), build the synthetic 100K-file fixture, gate merges on perf regression. ~6 hours including CI setup.
11. **Notarization + signing** — deployment, defer until ship-ready.
12. **swift-test wrapped E2E IPC test** — the standalone test passes but the swift-testing wrapper hung in M1. Now that the engine is more mature, retry; the failure mode may have resolved.
13. **Prune trashed files from DB** — ✅ DONE for the Cleanup → Trash action. Background sweep for orphan rows (e.g. files deleted via Finder while v2 is running) still pending; could be a periodic check or a FolderWatcherService follow-up.
14. **v1 deletion / cutover** — when the user signals "v2 is the one," delete `Sources/`, rename `engine`/`app` to top-level layout, drop `run.sh`, archive v1 docs.

## 0b. Closed — Batch 12 "Stall investigation + perf instrumentation + VisionWorkerPool deactor + Reveal-in-Finder" (closed)

Build clean (9.15 s, only the two documented `@Model` Sendable warnings). User reported on the prior build that the 58 K TrueNAS scan stalls halfway and "CPU/GPU is underutilized by almost 50%." Per-file logged Vision work is ~140 ms median → 14 workers × 140 ms = ~100 files/s theoretical, but observed 13.8 files/s. We're at ~14% of theoretical capacity. The "13.8 is fine" claim from the prior batch was wrong; this batch corrects course by **measuring instead of guessing** and lands one mechanical fix.

- **VisionWorkerPool: actor → lock-guarded class.** `Sources/Services/VisionWorker.swift:263` — was an `actor` that funneled every per-file `pool.with` through its executor. Replaced with `final class @unchecked Sendable` + `NSLock` over the worker array. Same `with { ... }` API; both call sites in `MediaProcessor.startDirectoryScan` unchanged.
- **PHASE-PROFILE per-batch instrumentation.** `Sources/Services/MediaProcessor.swift` — new profiler statics (`nonisolated(unsafe) static` + `NSLock`, same pattern as Batch 11's scan-log buffer). Records three timing spans per file: `workerWith` (time inside `pool.with { ... }`), `storeInsert` (time on `await store.insertScanResult(...)`), `resultLoopIter` (time per `for await` body iteration). Snapshot flushed in `commitBatchSave` after the existing batch line as `PHASE-PROFILE batch=N processedTotal=M availMB=X residentMB=Y` + four lines (per-stage p50/p95/total + workerWall utilization). User can `tail -f scan.log | grep -A 5 PHASE-PROFILE` and the output pinpoints which stage funnels the 14 workers.
- **Reveal in Finder in main preview toolbar.** `Sources/MediaPreviewOverlay.swift` — added "Show in Finder" button (folder SF symbol) between Deep Analyze and Close. Calls `NSWorkspace.shared.activateFileViewerSelecting`. No keyboard shortcut (Cmd-R is bound globally to Rescan). Existing EXIF-panel button kept as a secondary surface.

**Verification for the next user run:**
- [ ] Open the preview overlay → toolbar shows Info / Deep Analyze / Show in Finder / Close. Click "Show in Finder" → Finder activates with file selected.
- [ ] Start a fresh scan; after the first batch (~400 files), `~/Library/Logs/FileID/scan.log` contains a `PHASE-PROFILE` block with `workerWith`, `storeInsert`, `resultLoopIter`, `workerWall` lines.
- [ ] Paste the first 3 batches' PHASE-PROFILE blocks → that's the data the next batch needs. If `storeInsert.total` ≈ `batchDur` then FileIDDataStore is the funnel; if `workerWith.total / (batchDur × 14) < 0.4` then the worker pool is starved (look upstream); if `resultLoopIter.total ≈ batchDur` then the result loop itself is the bottleneck.
- [ ] No regression in scan stability: scan completes; no new `~/Library/Logs/DiagnosticReports/FileID-*.ips`.

## 0a. Closed — Batch 16 "P-core saturation" (closed)

User reported P-cores at 30-50% during scan. Two fixes:

- **Result loop atomic-only polling (`Sources/Services/MediaProcessor.swift`).** Same root cause as the Discovery fix: per-file `await viewModel.isCancelled` + `await viewModel.isPaused` were stalling the loop on MainActor wake-ups, blocking new task spawns. Replaced with the nonisolated atomic mirrors. Pause check now per 64 files instead of per file. Seed cap bumped `cap*2 → cap*4` so workers have cushion when the loop briefly stalls.
- **Worker cap formula change (`Sources/Services/Hardware.swift`).** Each worker spends ~half its wall time in ANE/GPU. To keep P-cores busy during CPU stages, need more workers than P-cores. New formula: `P + E + max(1, P/2)`. M1 Pro: 9 → 14 workers. Mac Studio Ultra: still capped at 32.

**Verification:**
- [ ] `./run.sh` — clean build.
- [ ] Console: `workers=14` on M1 Pro (was 9).
- [ ] CPU History during Tagging: P-cores noticeably more pinned. Throughput climbs from 21 → ~30 files/s.

## 0a. Closed — Batch 15 "Discovery fix + cleanup"

User reported Discovery taking 15+ minutes; also asked to complete remaining queued items, clean dead code, remove worthless comments.

- **🚨 Discovery 15min → seconds (`MediaProcessor.swift` + `AppViewModel.swift`).** Per-file MainActor await + per-file `resourceValues` syscall + UTType prefetch via `.contentTypeKey` were adding ~10-15 minutes of overhead to the discovery enumeration. Fix: FileStream is now a class not actor; new batched `nextBatch(count: 1024)` API; discovery runs in `Task.detached`; cancellation/pause polled via nonisolated atomic mirrors of @Published state (no MainActor hop); no per-file stat (FileRecord.init does it lazily); `includingPropertiesForKeys: nil`. >500 MB skip moved into processFile.
- **FileRecord / PersonRecord externalStorage** — bookmarkData, clipEmbedding, deepAnalysis, representativeFaceCropData, featurePrintsData all moved to `@Attribute(.externalStorage)`. Combined with the WAL checkpoint, keeps per-save fsync time bounded as scan progresses.
- **Dead code purged** — `applyFolderStructure` chain (AppViewModel + MediaProcessor + FileIDDataStore + MovePlan struct + updateURLAfterMove), `FileRecord.scenePrintData` + `facePrintsRawData`, duplicate `FolderOrganizationView.categoryName` (replaced with the canonical `fileIDCategory`).
- **Comment cleanup** — historical "WAS X. NOW Y" prose stripped, redundant MARK headers removed, inflated narrative blocks shortened. Kept comments that explain non-obvious decisions.
- **Tooltip hit-testing fix** — `.contentShape(Rectangle())` added to the icon-button pattern across 5 sites.
- **MediaPreviewOverlay nav resilience** — `currentIndex` falls back to 0 when current file is missing from list.
- **PeopleView search debounce** — 200 ms debounce on search-text changes.
- **FaceClusteringService threshold lower bound** — rejects `< 0.30` as invalid.
- **filesPerSec flicker fix** — 0.1 s floor on elapsed.

**Verification:**
- [ ] Discovery on 50K local folder is **seconds**, not minutes.
- [ ] Window has traffic lights; tooltips appear on hover; tab switching is fast.
- [ ] PeopleView search smooth at 5K+ identities.
- [ ] Long scan: `grep "WAL checkpoint" scan.log` shows clean entries; throughput steady through hour 2.

## 0a. Closed — Batch 14 "Stability + responsiveness"

User reported the Batch 13 fixes weren't enough and asked for "every line of code under critical scrutiny." Three audit subagents identified the structural causes; six surgical changes plus one new ~110 LOC service.

- **Traffic-light buttons** (`Sources/MainWindowView.swift`, `Sources/FileIDApp.swift`) — Batch 11 left a `.toolbar(.hidden, for: .windowToolbar)` modifier on the NavigationSplitView that on macOS 26 hides the entire titlebar layer (buttons included). Removed; the existing `.underWindowBackground` material handles the white-bar case alone. AppDelegate window setup hardened: factored into `configureMainWindow()`, called twice (sync + DispatchQueue.main.async), filters out NSPanel auxiliaries.
- **Tab switching unfrozen** (`Sources/MainWindowView.swift`, `Sources/PeopleView.swift`, `Sources/AcceptChangesView.swift`) — reverted Batch 5's scan-time tab unmount. Audit math: 4 fresh @Query fetches on first-mount cost 1-3 s vs +1.8% scan-time overhead from keeping all six mounted. Trade was worth it. Bounded the previously-unbounded queries in PeopleView (5 000) and AcceptChangesView (`Hardware.gridFetchLimit`).
- **Tooltips work on action buttons** (`Sources/MainWindowView.swift`, `Sources/SettingsView.swift`, `Sources/PeopleView.swift`) — Pause/Cancel/Export/Reset/etc. were missing `.contentShape(Rectangle())` between `.buttonStyle(.plain)` and `.help(...)`. Without it, hover hit-testing follows intrinsic Label size, not the `frame(maxWidth:)` expansion. Added the modifier to all five sites the audit flagged.
- **SQLite WAL checkpoint** (`Sources/Services/SQLiteCheckpoint.swift` new + integration in `Sources/Services/MediaProcessor.swift`) — fixes the "incredibly long wait time after running for a while" cliff. Separate sqlite3 connection runs `PRAGMA wal_checkpoint(TRUNCATE)` every 8 batch saves. SQLITE_BUSY treated as "try next round." WAL size before/after logged to scan.log. SLOW SAVE warning added (>1.5 s).
- **HNSW thrash gate** (`Sources/Services/FaceClusteringService.swift`) — drift-floor bumped 50 → 200 + added 8-second wall-clock cooldown between rebuilds. Each rebuild logs identities/nodes/duration to scan.log.

**Verification:**
- [ ] `./run.sh` — clean build.
- [ ] Window has traffic lights in the top-left.
- [ ] Tab switching during scan is fast (under 200 ms).
- [ ] Hover Pause/Cancel/Export — tooltips appear.
- [ ] After ≥30-minute scan: `grep "WAL checkpoint" ~/Library/Logs/FileID/scan.log` shows entries with `walMB <5` after each. `grep "SLOW SAVE"` is empty. Throughput stays steady through hour 2.

## 0a. Closed — Batch 13 "Scaling pass"

User explicit ask: scale to 100K+ files on top-tier Macs while keeping a 16 GB Mac stable, restore traffic-light buttons, make face recognition useful, make folder restructuring work. Seven changes plus one new ~330 LOC service.

- **HNSW face-clustering index (`Sources/Services/HNSWIndex.swift` + `Sources/Services/FaceClusteringService.swift`).** Pure-Swift HNSW with Accelerate vDSP for L2. Wired as phase-1 candidate filter in `clusterSync` — flat scan below 500 identities, HNSW above. Phase-2 sample fallback unchanged so a stale index can't produce a wrong assignment. Rebuild on >50% drift. Mutation paths (`merge`, `rebuildIndex`, `rebuildPeopleFromStoredPrints`) explicitly invalidate. Tests in `Tests/FileIDTests/HNSWIndexTests.swift` cover insert/search exact match, dim-mismatch rejection, recall ≥ 90% vs flat scan on synthetic 64-d data, tombstone semantics, compact rebuild. **5 K identities: O(N) → O(log N), search drops from ~5 000 distance comparisons to ~13.**
- **Window traffic-light buttons restored (`Sources/FileIDApp.swift`).** Removed `.windowStyle(.hiddenTitleBar)`, kept transparent titlebar, explicitly unhid close / minimize / zoom buttons. macOS standard back in place.
- **Hardware caps — high-end tiers (`Sources/Services/Hardware.swift`).** Added 96 GB / 192 GB tiers across `thumbnailCacheMB`, `thumbnailCountLimit`, `saveEvery`, `visionCeilingMB`. New `gridFetchLimit` so view code can scale @Query limits per machine class. `workerCap` capped at 32 to avoid pathological per-worker overhead on future 64-P-core machines. 16 GB tier unchanged.
- **Face-name propagation as `person:<name>` tag (`Sources/Services/FaceClusteringService.swift` + `Sources/PeopleView.swift` + `Sources/PersonDetailView.swift`).** New `renamePerson(id:newName:)` writes `person.name` and fans out `"person:<name>"` to every FileRecord in the cluster. Both rename UI sites route through it. Library search/filter on the tag works immediately. Was Session B's queued item; landed.
- **FolderOrganizationView Apply / Undo hardening (`Sources/FolderOrganizationView.swift`).** Was eating every move failure with `catch {}`. Now collects per-file failures, logs a summary + per-file detail line, disambiguates same-name conflicts with numeric suffixes, recomputes categorization snapshot at apply time, and `undoChanges` creates parent dirs + reports successes/failures separately.
- **`AppViewModel.applyFolderStructure` marked orphan (`Sources/AppViewModel.swift`).** Dead method that ran a *different* categorization than the view; `@available(*, deprecated)` + `fatalError`.
- **PeopleView filter cache (`Sources/PeopleView.swift`).** `filteredIdentities` cached into `@State var cachedFiltered`, invalidated on input change. Same pattern CleanupView uses.
- **AppViewModel.treeAccumulator hard-cap (`Sources/AppViewModel.swift`).** 10K-key safety cap so a million-folder library can't blow up the sidebar OutlineGroup diff.

**Verification:**
- [ ] `./run.sh` — clean build, two existing `@Model` Sendable warnings.
- [ ] `swift test` — 5 test files all pass (HNSW recall test is randomized but deterministic via SplitMix64 seed).
- [ ] Window — close / min / zoom buttons visible in top-left.
- [ ] Name a person in PeopleView → search `person:<name>` in Library → every clustered photo appears.
- [ ] Folder Restructure → Apply → log shows summary + any failures explicitly.
- [ ] Folder Restructure → Undo → log shows restoration count + any failures.
- [ ] Open PeopleView on a library with 5K+ identities — first-paint is fast and Suggested Merges populates in ~2 s.

## 0a. Closed — Batch 12 "Production hardening pass"

User asked to make the app "production level" after intermittent crashes; chose "Full production hardening pass" scope. No fresh `.ips` on disk, so the work targets structural risks rather than a single repro.

- **Bounded `pendingFaces` with hard cap of 10 K.** `Sources/Services/MediaProcessor.swift` — new `pendingFacesHardCap = 10_000` constant. The result loop force-flushes mid-batch when the buffer crosses the cap, in addition to the existing `liveClusterThreshold = 2_000` flush at batch-save boundaries (every `saveEvery = 400` files). `flushFacesIfReady(_:force:)` gained a `force: Bool = false` argument; `force=true` bypasses the soft threshold. Defends against face-dense photo runs (wedding albums, group shots) that push the in-flight buffer well past 2 K between commits, the most likely Jetsam mode on 16 GB Macs.
- **Sentinel-aware Hardware queries.** `Sources/Services/Hardware.swift` — `residentMB()` and `availableMemoryMB()` now return `-1` on `task_info`/`host_statistics64` failure (was `0`, indistinguishable from "no memory used"). `canSafelyLoadLargeModel()` treats the sentinel as "don't risk the VLM load." Other callers are diagnostic logs where -1 surfaces as a visible "memory query failed" instead of a misleading "0 MB". HardwareTests exercises the contract.
- **Cooperative yields in long FaceClusteringService loops.** `Sources/Services/FaceClusteringService.swift` — `rebuildPeopleFromStoredPrints()` yields every 64 blobs in both the unarchive and re-cluster passes (was blocking the actor for ~20 s on a 9 K-print library). `suggestedMerges()` got a 2 s wall-clock deadline (checked every 16 outer iterations), a critical-pressure abort, and a 256-pair `break outer` cap — partial answer in 2 s instead of stalled UI for 30+ s.
- **Defensive guards in pure helpers.** `MobileCLIPService.embedImage` / `runTextEncoder` return `nil` on zero-length `MLMultiArray` outputs (was returning `[Float]()` and silently disabling zero-shot CLIP). `MediaProcessor.computeDHashStatic` early-returns 0 on `cgImage.width == 0 || height == 0`.
- **Visible scan.log write errors.** `flushPerFileScanLog()` and `writeScanLogLine(_:)` no longer swallow disk-full / permission-denied / volume-gone via `try?`. Now `NSLog("FileID scan.log write failed: %@", error.localizedDescription)` so Console.app surfaces the failure.
- **First test target.** `Package.swift` gained a `.testTarget`. `Tests/FileIDTests/` has four files covering TagTaxonomy mappings, Hardware contract, JunkScorer threshold behaviour, and `computeDHashStatic` / `lightweightAestheticStatic` math. Run via `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test`. No Vision/MLX/SwiftData coverage — those remain untestable without integration scaffolding.

**Verification on the next user run:**
- [ ] `./run.sh` compiles clean (two existing `@Model` Sendable warnings).
- [ ] `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test` — all four test files pass.
- [ ] Full TrueNAS scan: throughput unchanged (Batch 12 doesn't touch the scan engine). Resident memory should be flatter through face-dense subfolders — the hard cap prevents the pendingFaces spike between commits.
- [ ] During scan, tap Settings → Deep Analyze → "Rebuild People" — Library tab stays scrollable instead of freezing for ~20 s.
- [ ] PeopleView "Suggested Merges" returns within ~2 s on a 5 K-identity library.
- [ ] If scan.log ever stops mid-scan: Console.app shows `FileID scan.log write failed` lines instead of silence.

## 0a. Closed — Batch 11 "Full-screen chrome + scan-log buffer + Best/date copy + tooltip pass"

Build clean (14.40 s, only the two documented `@Model` Sendable warnings). User bundled four UX/perf asks into one message after running Batch 10 on a 58 K TrueNAS library.

- **Full-screen white bar.** `Sources/FileIDApp.swift` — `VisualEffectView` material swapped from `.hudWindow` → `.underWindowBackground` (the dark opaque surface that extends under toolbar strips). `Sources/MainWindowView.swift` — `.toolbar(.hidden, for: .windowToolbar)` + `.toolbarBackground(.hidden, for: .windowToolbar)` on the `NavigationSplitView` so the split view's internal toolbar chrome can't render a system-default light strip.
- **Scan-log fsync batching.** `Sources/Services/MediaProcessor.swift` — `nonisolated(unsafe) static var perFileBuffer: [String]` + `NSLock`. `appendScanLogPerFile(_:)` pushes to buffer; `flushPerFileScanLog()` drains in one open + write + fsync + close and is called from `commitBatchSave` (every 400 files) and at scan end. Phase-boundary / discovery / Deep Analyze headline lines still go through direct-writing `appendScanLog`. Expected steady-state win 2–5% — documented honestly; 13.8 files/s on M1 Pro with CLIP + Vision + face archive is within the expected band.
- **"Best" and date UX.** `Sources/CleanupView.swift:192` + `:202` — duplicate-delete tooltip and confirmation dialog rewritten as `"Keeps the sharpest, largest copy of each duplicate group and trashes the others."` and long form with the earliest-date tiebreaker explained (no more subjective "best"). `CleanupView.swift:537` creation date format `.abbreviated` → `.numeric` (year shows). `Sources/MainWindowView.swift` — file-card `creationDate` wrapped with `.help` explaining filesystem-vs-capture date; Library Date/Best sort picker got a `.help` explaining the criterion. **Ranking logic unchanged** — preserves original-EXIF copies; was a copy problem, not a logic problem.
- **Tooltip pass.** Added `.help` to: throughput chip, `elapsedCell`, `etaCell`, Library sort picker, PeopleView sort picker. Verified Pause / Cancel / Export / Reset / memory chip / search-clear already had `.help` from earlier batches.

**Verification for the next user run:**
- [ ] Enter full-screen (⌃⌘F) — top strip renders dark, no white band. Windowed mode unchanged.
- [ ] Start a fresh TrueNAS scan: `grep "file type=" ~/Library/Logs/FileID/scan.log | wc -l` still tracks the scanned-file count (batching doesn't lose lines); throughput ~0.5–1.0 files/s higher.
- [ ] Cleanup → Duplicates → hover "Delete Duplicates (keep 1)": tooltip reads "Keeps the sharpest, largest copy …" with no word "best". Click; confirmation dialog reads same.
- [ ] Hover throughput / memory / ETA / elapsed chips, Library Date/Best picker, PeopleView sort picker: every one shows a 1-line tooltip.

## 0a. Closed — Batch 10 "Crash fix + scale + human labels + PDF perf + Deep Analyze throttle"

Build clean (11.07 s, only the two documented `@Model` Sendable warnings). Covered the user's four-part Batch 10 ask in one pass.

- **Crash fix: no live tree during scan.** `Sources/AppViewModel.swift` — `rebuildTreeFromAccumulator()` call in `drainAtomicState` is now gated on `!isProcessing`; one-shot rebuild added to `finishNamingPhase` right before `stopDrainTimer()`. `recordTreeProgress` caps `parts` at the first 6 path components. `Sources/MainWindowView.swift:414` matching `&& !viewModel.isProcessing` guard so the `Section("File Hierarchy")` doesn't render at all during Tagging. Root cause is SwiftUI's AttributeGraph internal dynamic-attribute table saturating on thousands of `OutlineGroup`-inside-`List`-inside-`TransitionBox` rebuilds (evidence: `AG::precondition_failure → grow_region` in `~/Library/Logs/DiagnosticReports/FileID-2026-04-24-163532.ips`). Not Jetsam, not OOM — a SwiftUI structural anti-pattern.
- **`Sources/Services/TagTaxonomy.swift`** (new, ~125 LOC). Static `[String: String]` map of ~40 common Vision taxonomy labels to everyday words. `key(for:)` normalizes lowercased + underscore-collapsed; unknown labels pass through unchanged to preserve internal tag contracts (`Tax_Document`, date tags like `2024_12`, Session-A markers). Wired into `MediaProcessor.processFile` replacing the terminal `Array(Set(tags))` — humanize dedups and preserves first-occurrence order.
- **PDF perf.** `Sources/Services/VisionWorker.swift` gains `ocrFast(_:)` (fast Vision OCR, no language correction — ~200 ms/page vs ~3 s/page). `MediaProcessor.processPDF`: 3-page cap (was 10), switched to `ocrFast`, skip + tag as `["PDF", "Large_Document"]` when `sizeMB > 20`. Per-PDF time: 28–38 s → ~500 ms–1 s.
- **Deep Analyze intensity throttle.** New `@AppStorage("deepAnalyzeThrottle")` — `performance` / `balanced` (default) / `gentle`. `SettingsView.DeepAnalyzeSettingsPanel` exposes a segmented picker with a help tooltip. `runDeepAnalyzePassIfEnabled` reads the setting and maps to `(chunkSize, interChunkSleepMs)`: 64/50, 32/250, 16/1000. Gentle tier also checks `Hardware.canSafelyLoadLargeModel()` between chunks and waits 5 s if memory is tight. Existing `Hardware.isUnderMemoryPressure` backoff preserved (additional, not replacement).

**Verification for the next user run:**
- [ ] Full TrueNAS scan: no new `FileID-*.ips` in `~/Library/Logs/DiagnosticReports/`.
- [ ] During Tagging: the File Hierarchy section is **not visible** in the sidebar. Counter + ETA + memory chip are the only live elements.
- [ ] Throughput visibly higher past PDF-heavy subfolders — no more 30 s stalls.
- [ ] Post-scan: land on Review → switch to Library; sidebar Hierarchy section appears, fully populated, static. Expand/collapse is smooth.
- [ ] 10 random Library thumbnails: tags show `"Glasses"` / `"Packaged Food"` / `"Pet"`, not `"Optical Equipment"` / `"Bottled And Jarred Packaged Foods"` / `"Domesticated Animal"`.
- [ ] `grep "type=pdf" ~/Library/Logs/FileID/scan.log | sort -k6 -t= -r | head -5` — P95 PDF time ≤ 2 s; PDFs > 20 MB log `tags=[PDF, Large_Document]` with no OCR.
- [ ] Settings → Deep Analyze shows a new "Intensity" segmented picker defaulting to Balanced. Set to Gentle, click Run on a 5 K-row library while Safari has 20 tabs open — system stays responsive.

## 0b. Closed — Batch 9 "Sequential scan + no-resume + simplified ETA"

Build clean (12.91 s, only the two documented `@Model` Sendable warnings). Covered the user's direct pushback on Batch 8's interleaved design.

- **Sequential scan.** `Sources/Services/MediaProcessor.swift` — `startDirectoryScan(url:)` drains `FileStream` fully into `var allFiles: [DiscoveredFile]` before spawning any worker. Once drained, `viewModel.totalCount = allFiles.count` is set once and the phase transitions to `.tagging`. Tagging loop feeds workers from the array by index. `phaseTotal` is now a true constant throughout Tagging — no denominator drift.
- **No resume — every Start is fresh.** `Sources/AppViewModel.swift` + `Sources/Services/FileIDDataStore.swift` + `Sources/Services/MediaProcessor.swift` — deleted `hasIncompleteScanSession(forFolder:)`, `existingFilePaths()`, the `resuming: Bool` param threaded through `runScan` and `MediaProcessor.startDirectoryScan`, and the `FaceClusteringService.rebuildIndex()` call that was only needed on the resume path. Every Start now unconditionally wipes via `wipeForNewScan` + `FacePrintCache.removeAllAsync`.
- **ETA simplified.** `updateETA` in `Sources/AppViewModel.swift` now emits a single `… left` string. Dropped the `(avg Xm Ys)` dual-display. Live rolling 60 s rate, falls back to cumulative with < 2 samples.
- **Discovery shows an indeterminate spinner.** `Sources/MainWindowView.swift` — `.discovering` branch of the phaseDone/phaseTotal switch returns `(0, 0)`, making `showDeterminate` false.
- **DiscoveredQueue actor removed.** No longer needed — array suffices.

**Verification for the next user run:**
- [ ] Sidebar shows a "Discovering" phase with an indeterminate spinner and a live "N found" label for however long enumeration takes.
- [ ] When it transitions to Tagging, the progress bar starts at 0 / `finalTotal` and climbs monotonically to the same `finalTotal`.
- [ ] ETA reads as `1m 42s left` — no `(avg …)` suffix.
- [ ] Cancel mid-scan and Start again on the same folder: full wipe and re-scan from zero. No "Resuming previous scan…" status anywhere.
- [ ] Quit and relaunch directly from Finder (skipping `run.sh`): first scan is still fresh (code-level: resume branch is gone).

## 0a. Next — perf pass on honest counters

Unchanged from Batch 8 — now actually meaningful since `phaseTotal` is locked at the start of Tagging:

- [ ] Run a full scan on the 58 K TrueNAS corpus with `~/Library/Logs/FileID/scan.log` open. Capture per-batch files/s across the full Tagging phase (not counting Discovery).
- [ ] From scan.log, compute P50 and P95 tag-per-file times. Look for a rate cliff vs. a steady plateau.
- [ ] If P95 is > 2× P50, the bottleneck is variance (probably thumbnails or large videos). If P50 itself is below target, the bottleneck is steady-state throughput (revisit `workerCap`, `saveEvery`, or VisionWorker parallelism).
- [ ] Separately, time the Discovery phase on TrueNAS — if enumeration alone takes > 60 s, consider async-parallel directory walks (out of scope for perf batch, but worth noting).
- [ ] Only **then** decide whether to touch `Sources/Services/Hardware.swift` caps or MediaProcessor concurrency.

**Acceptance:** a written diagnosis in DECISIONS.md naming the specific knob to turn (and why), rather than another speculative concurrency change.

## 0b. Closed — Batch 8 "Pipeline tidy-up + honest progress counter + fresh-on-compile"

Five landable threads, one batch, build clean (6.14 s, only the two documented `@Model` Sendable warnings). Covered all four items from the user's screenshot-driven feedback.

- **Progress counter on a single clock.** `Sources/AppViewModel.swift` — folded tree rebuild + ETA refresh into `drainAtomicState` via a `drainTickCounter` that fires the heavy pass every 6th tick (~500 ms). Consolidated the old `bumpProcessedAtomic` + `enqueueTreeProgress` pair into one `recordFileCompleted(fileURL:)` that bumps the processed count and enqueues the tree-progress entry under a single `NSLock`. Removed the standalone 5-second tree-rebuild `Task`. The old ≥5 s gap between the "N / M" counter and the File Hierarchy pane is gone.
- **Per-file MainActor hop removed + batch-save extracted.** `Sources/Services/MediaProcessor.swift` — deleted the `await MainActor.run { vm.totalCount = vm.discoveredCount }` that fired 58 K times per scan (drain timer owns the denominator). Per-file path now calls `viewModel.recordFileCompleted(fileURL:)` (one nonisolated call) instead of two separate calls. Main scan loop's 44-line inline batch-save collapsed into `commitBatchSave(batchSize:batchStart:processedTotal:)`, `flushFacesIfReady(_:)` (throttled live clustering), `flushPendingFaces(_:)` (tail flush).
- **Fresh-on-compile.** `run.sh` now wipes SwiftData store (`default.store*`), `app_running.json`, `FacePrintCache`, `ScanCache`, `~/Library/Caches/com.adamnolle.FileID`, and `~/Library/Logs/FileID` on every launch. Preserves model weights under `~/Library/Application Support/FileID/Models/` and `~/Documents/huggingface/models/` — per user: "just anything that can't be redownloaded."
- **Gitignore — downloadable weights.** `.gitignore` extended with `*.safetensors`, `*.gguf`, `*.mlmodel*`, `*.mlpackage`, `*.onnx`, `*.pt`, `*.pth`, `*.bin`, `*.ckpt`, `*.tflite`, `*.weights`, plus `Resources/Models/` and `Resources/**/weights/`.
- **Style sweep on touched files.** Trimmed two history-talk comments in `AppViewModel.swift` (`drainAtomicState` + `startTreeUpdateLoop` headers). Fixed an unused-variable warning in `Sources/Services/FaceClusteringService.swift` (`let attempt = ...` → `_ = ...`).

**Verification for the next user run:**
- [ ] During Tagging: sidebar "N / M" counter and the File Hierarchy pane advance in lockstep; no ≥1-tick gap.
- [ ] `M` is monotonically non-decreasing; progress bar never jumps backward.
- [ ] On every `./run.sh`: SwiftData store gone, `FacePrintCache` / `ScanCache` empty, model weights under `Application Support/FileID/Models/` and `~/Documents/huggingface/models/` still present.
- [ ] `git status` after a recompile shows no accidentally staged `.safetensors` / `.gguf` / `.mlmodel*` files.
- [ ] Batch 7 behaviour preserved: scan finish lands on Review tab; UI does not revert to folder-picker after completion.

## 0c. Closed — Batch 7 "One-shot scan" UI state fix (shipped)

Four line-level edits. Fixed (a) `finishNamingPhase` reverting the UI to the folder-picker after scan completion, and (b) `totalCount` freezing at 1 while `processedCount` climbed — the "remaining files grows" feel. Predicates at `Sources/MainWindowView.swift:143` / `:500` switched to `currentFolderURL != nil`; `drainAtomicState` phase gate dropped; `activeTab = "Review"` set on scan completion. Details in `docs/STATE.md` "Previous: Batch 7".

## 0b. Done earlier this session — Batch 6.5 Jetsam + People rebuild (closed)

Shipped the plan at `~/.claude/plans/in-media-library-i-temporal-acorn.md` (Batch 6.5 revision, overwritten by Batch 7). Build clean (5.59 s).

- **`Sources/Services/Hardware.swift`** — replaced `vm_kernel_page_size` (shared-mutable global, Swift 6 concurrency warning) with `UInt64(getpagesize())` inside `availableMemoryMB()`. Same value, concurrency-clean.
- **`Sources/Services/MediaProcessor.swift`** — `runDeepAnalyzePassSafely` gains a free-RAM gate using `Hardware.canSafelyLoadLargeModel()` before it calls through to the Qwen VLM load. Line 553 default for `"deepAnalyzeEnabled"` key now falls back to `Hardware.deepAnalyzeAutoDefaultOn` (`true` on 24 GB+, `false` on 16 GB) instead of a blanket `true`.
- **`Sources/FileIDApp.swift`** — `applicationDidFinishLaunching` seeds the `"deepAnalyzeEnabled"` UserDefaults key with `Hardware.deepAnalyzeAutoDefaultOn` on first launch, so `@AppStorage` reads a RAM-aware default without changing the declaration.
- **`Sources/Services/FaceClusteringService.swift`** — new `rebuildPeopleFromStoredPrints() async` (~130 LOC). Extracts every blob from `PersonRecord.featurePrintsData` inside `autoreleasepool` + `CrashSentinel` per-blob marker (matches Batch 6 SIGABRT-hardened path), deletes old `PersonRecord` rows, re-clusters at the current 0.55 threshold, writes `rebuildPeople: persons=A→B prints=N dropped=K threshold=…` to scan.log.
- **`Sources/SettingsView.swift`** — new "Rebuild People" button in `DeepAnalyzeSettingsPanel` next to "Reset skip-list", disabled while `isProcessing`.

Closed the "instant crash after clustering" the user reported on 2026-04-24 13:49 CDT (Jetsam SIGKILL, no `.ips`, confirmed via `app_running.json` marker showing `phase=vision subject=TrueNAS pid=95399`).

## 0c. Done earlier — Batch 5 Scan throughput rescue (closed)

Shipped the six-section plan at `~/.claude/plans/in-media-library-i-temporal-acorn.md`. Build clean (17.81 s, pre-existing warnings only). Targets the two regressions diagnosed from the last scan.log: (a) throughput cliff 80 → 6.7 files/s at ~17 K files; (b) 27-minute stall between Cancel and the next Discovery.

- **Unmount inactive tabs during scan.** `TabHost` (`Sources/MainWindowView.swift`) gains `mounted: Bool` → renders `Color.clear` when false. Idle keeps the Batch 4 six-tab ZStack keep-alive; `viewModel.isProcessing` mounts only Library + active tab. SwiftData notification fan-out during scan drops from 6× to ≤2× per batch save.
- **Bounded `FileGrid` query + cached `filtered`.** `FetchDescriptor.fetchLimit = 2_000` (was unbounded); `@State var cachedFiltered` replaces the per-body-eval O(N) filter — recomputed only on `files.count` / `query` / `tab` changes.
- **Off-main wipe with a splash.** New `@Published var isWiping` on `AppViewModel`. `WipingSplash` in `MainWindowView` (ProgressView + "Clearing previous scan…") covers the window while the six-tab ZStack is torn down, so the 17 K-row `modelContext.delete(model:)` fires with zero `@Query` observers. `FacePrintCache.removeAllAsync()` added — enqueues directory delete onto the existing `writeQueue` instead of blocking the main actor. Redundant `FaceClusteringService.rebuildIndex()` call after wipe dropped.
- **Resume detection.** New `FileIDDataStore.hasIncompleteScanSession(forFolder:)`; `startProcessing` branches to `runScan(..., resuming: true)` without wiping if a `ScanSession` for the same folder has `completedAt == nil`. Cancel-then-Start on the same folder preserves tagged files.
- **People-backfill one-shot gate.** `FaceClusteringService.rebuildIndex` now guards the expensive `urlToFileID` fetch behind `UserDefaults "peopleFileIDsBackfill_v1_done"`. Was re-running on every launch for any library with legacy identities. Matches the 2026-04-23 DECISIONS claim that was missing from the code.
- **Throttled live clustering.** `MediaProcessor` accumulates prints across batches; detached `clusterBatch` fires only when `pendingFaces.count >= 2_000` (new fileprivate `liveClusterThreshold`). Previously fired every batch (250 files × ~10 faces × 500 identities). Post-scan tail flush still catches remainder.

**Verification for the next user run:**
- [ ] Open the 59 K TrueNAS folder. Watch `~/Library/Logs/FileID/scan.log` — rate stays ≥ 30 files/s sustained past 20 K, 30 K, 40 K files. No batch > 10 s after the first. Resident memory ≤ 500 MB through the scan.
- [ ] Cancel at ~15 K files, immediately click Start on the same folder. **Expected:** "Resuming previous scan…" status, Discovery starts within 5 s, and existing tagged files are preserved (count does not reset to 0). If instead the user clicks "Start Fresh" (clean slate), the "Clearing previous scan…" splash appears and Discovery begins within ~5 s (NOT 20+ min).
- [ ] Switch to Cleanup during a scan: mounts fresh (~100 ms first time), Library switch-back feels instant. Post-scan: all tabs warm, switches < 50 ms.
- [ ] PeopleView: faces still appear during scan but the pulse is every ~20 batches (~4× the print threshold / face-per-file ratio), not every batch.
- [ ] Regression: People detail + "Not this person" still works (Batch 4). Deep Analyze Full Sweep still streams without OOM (Batch 4).
- [ ] Relaunch on a library that has already been clustered once: launch time doesn't stall on a redundant `FileRecord` fetch. (Check `~/Library/Logs/FileID/scan.log` or Console for backfill traces — should not see any.)

## 0a. Done earlier — Batch 4 People detail + toggles + tab perf + streaming Deep Analyze (closed)

Shipped the four-section plan at `~/.claude/plans/in-media-library-i-temporal-acorn.md`. Build clean (14.8 s, pre-existing warnings only).

- **People detail view.** Click any person card → full overlay with every photo in that cluster. Multi-select + **Not this person (N)** re-clusters the selected files against other identities; falls back to orphan if no match passes threshold. Inline rename via pencil; Delete Person wipes the record. New `fileIDs: [UUID]` on `PersonRecord` is the authoritative list (previously only ≤8 sample URLs existed); `FaceClusteringService.rebuildIndex` runs a one-shot backfill from `sampleFileURLs` for libraries clustered before this change. `FaceClusteringService.reassignFiles(from:fileIDs:)` does the re-cluster with `clusterSync(skip: personID, allowCreate: false)`.
- **Gold right-aligned toggles.** New `SettingToggleRow` in `Theme.swift`. Both Deep Analyze toggles in `SettingsView` and both Restructure toggles (Dry Run, Shortcuts) in `FolderOrganizationView` migrated. No more stock blue.
- **Tab-switch perf.** `MainWindowView` replaced `Group + .id(activeTab)` with a `ZStack` of six `TabHost` wrappers — inactive tabs stay mounted (opacity 0 + `allowsHitTesting(false)`), so `@Query` subscriptions and SwiftData caches persist across switches. Cleanup ⇄ Library is now instant after first visit. `CleanupView.screenshotDescriptor.fetchLimit` dropped 2000 → 500.
- **Streaming Deep Analyze (full-library crash fix).** `FileIDDataStore.deepAnalyzeTargetIDs(fullSweep:limit:)` + `deepAnalyzeTargetCount(fullSweep:)` let `MediaProcessor.runDeepAnalyzePassIfEnabled()` stream 64-file chunks: per-file `autoreleasepool` around CG decode, `Task.yield()` between files, `DeepAnalyzeService.trimCaches()` + sleep between chunks, 500 ms backoff when `Hardware.isUnderMemoryPressure`. Qwen is unloaded at end of pass via new `DeepAnalyzeService.unload()`. The `deepAnalysis == nil` predicate shrinks naturally, so an offset-0 fetch each loop gives a resumable cursor after force-quit.
- **Hardware pressure API.** `MemoryPressureLogger` promoted from `VisionWorker.swift` into `Hardware.swift` as `installMemoryPressureMonitor()` / `isUnderMemoryPressure` / `isUnderCriticalMemoryPressure` / `residentMB()`. All call sites migrated.
- **Cancellable Deep Analyze.** `AppViewModel.runDeepAnalyzeNow()` stores the `Task`; `cancelDeepAnalyze()` cancels it. `SettingsView` shows a **Cancel** button while running; Run is disabled during scans with an explanatory tooltip.

**Verification for the next user run:**
- [ ] Click a person card → overlay opens with every photo in the cluster (not just 8 samples). Multi-select 3 files that aren't this person → **Not this person (N)** → they vanish; open a likely-correct person → they appear there (or stay orphaned if no cluster passes threshold). Delete Person wipes the record.
- [ ] Settings → Deep Analyze: both toggles are gold and right-aligned. Restructure → Dry Run / Shortcuts toggles are also gold. No blue remains.
- [ ] Cleanup ⇄ Library switching feels instant (< 100 ms) after first visit to each.
- [ ] Deep Analyze → Full Sweep → Run on a 25 K+ library, walk away 30 min. Process memory stays < 4 GB. No crash. Progress count rises monotonically. Cancel stops the loop within 1 s.
- [ ] Force-quit mid-Deep-Analyze → relaunch → Run again → picks up where it left off (rows with `deepAnalysis != nil` skipped).
- [ ] Simulated memory pressure (Safari with many tabs during Deep Analyze) → chunk loop slows visibly rather than crashing.

## 0a. Done earlier — Batch 3 critical perf + correctness pass (closed)

Shipped the 10-section plan from `~/.claude/plans/in-media-library-i-temporal-acorn.md`. Build clean (15.7 s, pre-existing warnings only).

- **UI lockup root cause fixed.** `scanBatchCount` demoted from `@Published` to plain `var` — every batch save was forcing a rebuild of every SwiftUI view observing `AppViewModel`. `uiRefreshTick` (1 s debounce) is now the only publish path during scan.
- **Scan throughput.** Batch save 50/100 → 250/500. `FacePrintCache.store` now writes on a utility queue. `FileStream` drops files > 500 MB before queueing. Rolling 60 s window ETA (ring buffer per phase, shows rolling + cumulative when they drift > 20 %).
- **Label quality.** Vision threshold 0.30 → 0.50; CLIP 0.22 → 0.28. Post-tag generic-term filter removes `Outdoor/Indoor/Object/Item/Thing/Other/Background/Image/Photo`.
- **Live face clustering.** Face prints accumulate per batch and fire `FaceClusteringService.shared.clusterBatch(prints:)` on a detached Task after each save. `PeopleView` shows a `liveScanClusteringCard` while the first identities materialize. Final tail is flushed synchronously after last save.
- **Media preview.** Dropped `@Query allFiles` fallback — overlay opens instantly on 50 K libraries.
- **Deep Analyze safety.** `DeepAnalyzeService` converted from `@MainActor @Observable` to `actor`. MLX cache 20 MB → 3 GB. Button disables while `isProcessing` with explanatory tooltip.
- **Tooltip hover.** `.contentShape(Rectangle())` added after `.buttonStyle(.plain)` across all icon-only buttons where `.help()` was hit-miss (sidebar tabs, grid hover buttons, preview overlay nav, Cleanup trash, PeopleView search/merge).
- **Folder Restructure shortcuts mode.** New `useShortcuts` toggle — creates POSIX symlinks via `FileManager.createSymbolicLink(at:withDestinationURL:)`, records targets in new `FileRecord.shortcutPaths: [String]`, leaves `FileRecord.url` untouched. Deleted the duplicate-move bug (view layer is now authoritative).
- **Qwen justification.** Info tooltip next to "Qwen2.5-VL 3B (4-bit)" in Settings → Credits and `AIModelSetupView` card — Apache 2.0, fully local, MLX offline, why 3B beats LLaVA 1.6 / Moondream / Phi-3.5-Vision.

**Plan-vs-reality adjustments (documented here so future Claude doesn't redo the research):** VisionWorker requests were already reused at init; FaceClusteringService was already full-dim comparing; drag overlay was already conditional; MediaProcessor never called thumbnails during scan. The plan was wrong on those four — left them alone. No `ScenePrompts.swift` (MobileCLIP vocabulary already has 50+ prompts). No `releaseInferenceCaches()` (the `isProcessing` button gate obviates it).

**Verification for the next user scan:**
- [ ] Library grid stays scrollable at >50 fps during a 59 K scan (Instruments → Core Animation FPS).
- [ ] Throughput ≥ 12 files/s sustained (was 6.8 /s). Batch logs in `~/Library/Logs/FileID/scan.log` should show consistent batch times without the 30 % mid-scan dip the previous run had.
- [ ] PeopleView populates within ~60 s of scan start; face count climbs while scan is still running.
- [ ] Media preview overlay opens in < 200 ms on the full library.
- [ ] Deep Analyze disabled with "Pause scanning to run Deep Analyze…" tooltip while a scan is active. When run idle, no crash — caption + category returned.
- [ ] Folder Restructure → shortcuts mode: apply on a small folder → `ls -la` target shows `l` symlink entries; originals untouched in source.
- [ ] Hover every interactive control — tooltips appear within ~0.5 s.
- [ ] Settings → Credits → Qwen info button reveals the local-only justification text on hover.

## 0a. Done earlier — Batch 2 UI fix pass (closed)

- **Uninstaller.** `Sources/Services/UninstallService.swift` (new) wipes `Application Support/FileID`, both HF model caches, `Library/Logs/FileID`, `Library/Caches/<bid>`, and `UserDefaults` persistent-domain. New Settings section (between System and Credits) with preview paths + byte total + destructive `.confirmationDialog` → `NSApp.terminate`. App bundle stays on disk.
- **Processing Control sidebar.** Counter rows aligned via two-column `Grid` with `.gridColumnAlignment(.trailing)` + `.monospacedDigit()`. Button palette: Pause amber, Cancel red, Export soft blue, Undo/new-scan neutral gray. `.help(...)` on every control.
- **Sankey layout.** Source/target columns rewritten from `VStack + .offset` to `ZStack + .position(x:y:)`. `SankeyLayout.heights(for:total:)` now two-pass-rescales to fit exactly (no canvas clipping). Target row `HStack { label; Spacer(8); icon }` + 12pt padding prevents label/count overlap.
- **Cleanup badge redesign.** `CleanupReasonBadge` (top-left, reason-specific symbol + color) + `CleanupFileKind` indicator (bottom-right, file-type capsule) replace the blanket amber `doc.on.doc.fill`. Both carry `.help(...)` with full junk-reason list.
- **Scan-time UI lockup mitigated.** FileGrid `@Query(animation:)` and per-card stagger both gate on `isProcessing`. Sidebar `TimelineView` downgrades `.animation` → `.periodic(by: 0.25)` during scans, `.periodic(by: 10)` idle. New `AppViewModel.uiRefreshTick` (1 s trailing-edge debounce off `scanBatchCount`) drives CleanupView + FolderOrganizationView filter recomputes instead of every save batch.
- **Project-wide tooltip sweep.** `.help(...)` on every interactive control across MainWindowView, SettingsView, FolderOrganizationView, CleanupView, PeopleView, AcceptChangesView, MediaPreviewOverlay, AIModelSetupView, OnboardingView.
- Updated stale `"Qwen2-VL 2B"` strings → `"Qwen2.5-VL 3B"` in SettingsView + MediaPreviewOverlay so help text matches `AIModelRegistry`.
- Build clean (10.81 s, 2 expected `@Model` Sendable warnings).

**Verification next user run:**
- [ ] Start a scan → Processing Control sidebar shows colored buttons with aligned counter columns.
- [ ] Hover sweep: every sidebar tab / Settings toggle / Cleanup card control / Restructure button / People action reveals a tooltip within ~0.5 s.
- [ ] Folder Restructure tab: no targets clipped at the canvas bottom; no category label overlapping the file-count chip.
- [ ] Cleanup → Junk: badge symbol matches first junk reason; tooltip shows full reason list; type indicator at thumbnail bottom-right.
- [ ] During a scan on the 58 K folder: grid doesn't stutter on batch saves; scroll stays smooth (>50 fps); sidebar counter still visibly updates.
- [ ] Settings → Uninstall → confirm → app quits → relaunch goes through onboarding and `~/Library/Application Support/FileID/` is absent; both HF model caches are gone.

## 0a. Done earlier this session — scan.log-driven fixes + Qwen 3B upgrade + JunkScorer rework (closed)

Five fixes landed off the 4134-line scan.log from the cancelled TrueNAS run:

- **MobileCLIP path bug** (`MobileCLIPService.swift:87`): `locateModel` was stripping 3 path components off `primaryFileURL` instead of returning it. Every image scanned without CLIP embedding. Fix: return `primaryFileURL` directly.
- **SwiftData context growth** (`FileIDDataStore.swift` + `MediaProcessor.swift:220`): new `resetAfterSave()` actor method clears `recordByID`, flags `pHashIndexDirty`, and calls `modelContext.rollback()`. Called after every `store.save()`. Expected: batch rate stays in 25–35/s band past batch 60 instead of halving to 14/s.
- **Qwen download crash** (`AIModelDownloadService.swift:148`): MLX's HF-hub downloader on @MainActor was crashing. Now uses our `performDetachedDownload` path (new `overrideDestDir: URL?` param) writing into `DeepAnalyzeService.modelCacheDirectory()`. MLX's `loadContainer` skips download on first Deep Analyze.
- **Qwen2.5-VL 3B upgrade**: `VLMRegistry.qwen2_5VL3BInstruct4Bit` is present in the pinned mlx-swift-examples 2.29.1. Swapped `modelConfig`, cache paths, and registry descriptor (displayName `"Qwen2.5-VL 3B (4-bit)"`, `sourceRepo "mlx-community/Qwen2.5-VL-3B-Instruct-4bit"`, `approxBytes ~3.07 GB`). Kept the `.qwen2VL2B` enum case for simplicity; old 2B caches on disk show as "not installed" and re-download into the new path.
- **JunkScorer rework** (`JunkScorer.swift` + `CleanupView.swift:23`): removed the `hasFaces` hard-zero (killed 60–90% of phone-photo corpora); now `score *= 0.65` soft penalty. Added `aestheticScore < 0.25 → +0.15` (half credit at <0.4) and `fileSizeMB == 0 → +0.50`. Dropped `junkThreshold` 0.6→0.45; Cleanup predicate literal updated inline.

Build clean (15.02s). **Verification next scan:** (a) every image log line shows `clip=…ms`; (b) batch throughput stays in 25–35/s band past batch 60; (c) Qwen download no crash, Deep Analyze button enabled on completion; (d) Cleanup → Junk tab populates with 3–10% of corpus.

## 0a. Done earlier this session — LavaLamp restore + durable scan log (closed)

- `MainWindowView.swift:26` — removed `paused:` arg on `LavaLampBackground(...)`; animation now runs during scans (user preference reversal).
- `MediaProcessor.swift` — added nonisolated `appendScanLog(_:)` helper mirroring the existing NSLog sites. Emits begin / per-batch / tagging-total lines to `~/Library/Logs/FileID/scan.log` so the data survives beyond the 5 min unified-log buffer and can be `cat`ed post-scan. NSLog sites kept.

Prior pass (same date) had already landed the status-label fix (`"Tagging files…"`), elapsed-time display (`Xs elapsed · Xm Ys left`), and the per-batch NSLogs that the file-sink now mirrors.

**Next perf action is blocked on data:** rerun one scan, `cat ~/Library/Logs/FileID/scan.log`, share the contents. The numbers pick the lever (save-debounce vs per-file Vision vs scale-degradation).

## 0a. Done in prior session — Part 5 polish sweep (closed)

Every Swift file except `LavaLampAesthetics.swift` had its AI-tell comment prose, `(Phase N)` / `(Fix X.X)` banners, `// Note:` / `// IMPORTANT:` preambles, and step-by-step narration stripped. Single-line *why* notes retained where genuinely non-obvious. Build green (7.16 s), app launches. No logic touched.

## 1. Done — Session A: bundled Vision pass + interleaved discovery + dropped "Unclassified"

Shipped against the perf+accuracy overhaul plan (`~/.claude/plans/i-need-you-to-refactored-cherny.md` — Sessions A/B/C). Build clean.

- **Single VNImageRequestHandler per image.** New `VisionWorker.runPrimaryPass(_:) -> VisionPass` bundles `[classifyReq, animalReq, faceRectReq]` into one `perform()`, then runs all face feature-print requests via `regionOfInterest` on the same handler in a second `perform()`. Old code created 3+N handlers per file (one per request kind, plus one per detected face for cropping-based feature prints) — handler construction decodes the image and allocates GPU textures, so this was the dominant per-file cost.
- **Eliminated double CLIP image-encoder pass.** New `MobileCLIPService.classify(usingEmbedding:topK:)` overload accepts a precomputed vector. `MediaProcessor` now embeds once for `clipEmbedding` storage and reuses the vector for label scoring (was running the image encoder twice per file when CLIP was loaded).
- **Interleaved discovery + tagging.** New `DiscoveredQueue` actor (continuation-pool, mirrors `VisionWorkerPool`) is fed by a detached discovery `Task` and consumed by the existing `withTaskGroup`. The `.discovering → .tagging` transition fires on the first file received instead of waiting for full enumeration. `viewModel.totalCount` updates live with the discovery count.
- **Removed the literal `["Unclassified"]` fallback** in `VisionWorker.classify`. Empty results stay empty; downstream filters already drop generic tags.

**Files touched:** `Sources/Services/VisionWorker.swift` (rewrote `classify`/`facePrints` into `runPrimaryPass`, kept lightweight `classify` for video), `Sources/Services/MediaProcessor.swift` (replaced `classify`/`facePrints` calls with `runPrimaryPass`, replaced `discovered[]` materialization with `DiscoveredQueue`+detached discovery task, added `StreamPuller`/`DiscoveredQueue` actors, fixed CLIP double-embed), `Sources/Services/MobileCLIPService.swift` (added `classify(usingEmbedding:topK:)` overload). DECISIONS.md entry: 2026-04-24 — "Session A".

**Acceptance for the next user run:**
- [ ] On a NAS folder with 30 s discovery time: tagging CPU history starts ≤1 s after scan click, not ≤30 s.
- [ ] `~/Library/Logs/FileID/scan.log`: `classify=` per file drops from ~150 ms to ~60 ms baseline; `faces=` drops to near-zero on face-less images (only NSKeyedArchiver serialization remains there now).
- [ ] `batch:` files/s line shows ≥ 1.8× the prior baseline.
- [ ] SQL spot-check 50 random `FileRecord` rows — zero `"Unclassified"`.
- [ ] PeopleView still populates with face clusters mid-scan; first re-scan after this change may produce slightly different cluster IDs (face-print vectors shift from the cropped-image path to the regionOfInterest path — see DECISIONS.md).
- [ ] No regressions on local-SSD scans (discovery is near-instant there; behavior should match prior).
- [ ] Cancel-then-Start on the same folder still resumes (the `hasIncompleteScanSession` path is unchanged).

## 1a. Next — Session B: tag richness without model changes

Same overhaul plan. Drops `Blue_sky / Outdoor / Sky` duplication, surfaces metadata that's already being read.

- New `Sources/Services/TagTaxonomy.swift` — collapse Apple's hierarchy (drop ancestors when descendants present), drop generic terms expanded to include `outdoor / indoor / natural_scene / environment / photo / image`, lower confidence threshold 0.50 → 0.30 (hierarchy collapse handles the noise). Introduce `TagKind` enum + `"kind:label"` storage so `aiTags: [String]` index stays untouched and old rows parse as `.scene`.
- Always-on fast OCR (drop the document-only gate at `MediaProcessor.swift:545`). New `VisionWorker.ocrFast(_:)` runs `.fast` level no language correction (~60 ms vs ~250 ms). Escalate to `.accurate` only for documents/screenshots.
- `NLTagger` `.lexicalClass` pass on OCR output → top-5 proper nouns as `ocrEntity:` tags.
- EXIF camera model + `Year_<yyyy>` → typed tags directly (currently stored separately and never joined).
- New `Sources/Services/GeocodeQueue.swift` — post-scan reverse-geocoding phase; in-memory + disk cache keyed at 3-decimal coord precision so a vacation's worth of photos shares one geocode result. Wire into the post-cluster pipeline before Deep Analyze. Wires up the existing `reverseGeocode` helper at `MediaProcessor.swift:720` which is currently never called.
- `FaceClusteringService.renamePerson(id:newName:) async` — when user names a cluster in PeopleView, fan out `person:<name>` tags to every file in that cluster.
- `VisionPass` extension: emit horizon-tilt (`composition:tilted`), saliency placement (`composition:subject_centered` / `rule_of_thirds`), and barcode payloads (`barcode:<payload>`) as typed tags. Requires extending `runPrimaryPass` to also include `VNDetectHorizonRequest`, `VNDetectBarcodesRequest`, `VNGenerateObjectnessBasedSaliencyImageRequest` in the bundled `perform()` call.

**Acceptance:** median ≥ 8 useful tags per file; zero `Unclassified`; no `outdoor + nature + sky + blue_sky` duplication; `camera:` tag on photos with EXIF; `location:City, State` after geocode phase; named-person tags propagate within 5 s of rename.

## 1b. Next — Session C: open-vocabulary CLIP (the real ceiling raise)

Phase 3 of the seven-phase plan, brought forward.

- Port OpenAI's BPE tokenizer in pure Swift (~300 LOC, no third-party deps; bundle `clip_vocab.json` + `clip_merges.txt` as `Resources/`).
- Wire `MobileCLIPService.runTextEncoder` against the new tokenizer; ship a 20-prompt golden self-test to verify on first launch.
- Expand the hardcoded 54-label vocabulary in `MobileCLIPService` to ~400 labels; precompute embeddings to `Application Support/FileID/clip_text_embeddings.bin`. SettingsView gets a textfield for user-added labels.
- CLIP-conditioned OCR escalation in `MediaProcessor`: trust CLIP top-1 to decide on `.accurate` re-OCR (handwriting/whiteboards get the slow pass even when Vision said photo; landscapes flagged as docs by Vision skip it).

**Acceptance:** `clip:` prefixed tags on ≥ 60 % of photos in a diverse test folder; specific labels like `clip:hiking_trail` appear where today only `Outdoor` does.

## 2. Verify no-caps + repo-sweep on the 58K-file library

**Why this is next:** The 2026-04-23 no-caps pass removed every remaining app-side performance throttle and the repo-sweep pass landed 20 accuracy/perf/feature fixes on top. Build is green. User needs to run a scan and confirm (a) throughput in the 30–40 files/sec band, (b) zero crashes, (c) no accuracy regressions after the `l2()` dim-mismatch fix or the MobileCLIP top-K change, (d) feature smoke-tests below pass.

**Acceptance criteria:**
- [ ] `./run.sh` launches cleanly; only the 4 expected `@Model` Sendable warnings.
- [ ] Console at launch: `FileID hardware: RAM=16GB cores=10 Pcores=8 workers=8 visionCeiling=3500MB thumbCache=400MB saveEvery=50`.
- [ ] Sidebar memory chip's hover-help reads `P-cores: 8  Workers: 8  Vision ceiling: 3500 MB  Thumbs: 400 MB  Save every: 50`.
- [ ] Drop the 58K-file folder. **Throughput target: 30–40 files/sec**.
- [ ] Activity Monitor → CPU History: all 8 P-cores pinned >70 % during the tagging phase.
- [ ] **Zero crashes.** If the app crashes, check Console for `FileID memory pressure: CRITICAL` entries just before the crash.
- [ ] **Zero mid-scan stalls.** Throughput meter should not drop to zero for multiple seconds.
- [ ] UI responsive with all P-cores pinned: scroll grid, switch tabs, pause/resume smooth.
- [ ] Thermal test: run a scan on battery for 10+ minutes. Fans audible (expected). Machine should NOT panic or shut down.
- [ ] **Cleanup**: Junk + Duplicates populate within 30 s.
- [ ] **People**: "Clustering faces… X / Y" progress card during post-scan clustering; identity merges don't accidentally cross people after a corpus upgrade (l2() dim-mismatch fix).
- [ ] **Restructure**: loads flow once on tab-open; does NOT re-layout while hidden.
- [ ] LavaLamp pauses at scan start, resumes at end.

**Feature smoke-tests from the repo sweep (should all still pass):**
- [ ] Cleanup → Trash All triggers the confirmation dialog with count + MB preview.
- [ ] Single-file recycle on a file whose permissions deny trash → record does NOT flip to `isTrashed`.
- [ ] Delete a file on disk → corresponding `PersonRecord.sampleFileURLs` entry removed (observe in PeopleView).
- [ ] About FileID Professional menu → alert shows version + build.
- [ ] Deep Analyze button with Qwen2-VL not installed → disabled/helpful sheet, not silent no-op.
- [ ] Sidebar shows scan phase chip (Discovering / Tagging / Clustering / Naming / Scoring) alongside free-form status.

Rollback order (most → least conservative): re-add memCap → re-add `.critical`-only thermal guard → reduce `maxInFlight` → reduce `saveEvery`.

## 3. MobileCLIP tokenizer (makes zero-shot labels actually work)

The text encoder loads and caches per-label embeddings, but we currently pass raw strings through a placeholder since Apple's BPE tokenizer isn't yet ported to Swift.

**Without this:** image embeddings + "more like this" similarity work. Zero-shot tag lookup returns no tags.

**Steps:**
- [ ] Either port Apple's MobileCLIP tokenizer (~200 LOC) or bundle HF's `swift-transformers` tokenizer and point it at MobileCLIP's vocab file.
- [ ] Replace `runTextEncoder` in `MobileCLIPService.swift` to tokenize → int32 array → pass as input.
- [ ] Verify: scan a photo of a sunset; "Sunset" appears in `aiTags` alongside Apple Vision labels.

## 4. Wire opt-in "Deep Dedupe"

`scenePrint` is removed from the hot loop; restore it behind a user-triggered pass:

- [ ] Add a "Deep Dedupe" button in Settings that:
  - Fetches all FileRecords with duplicate pHash values.
  - Runs `VisionWorker.scenePrint(_, enabled: true)` only on bucket members.
  - Refines duplicate groupings via `VNFeaturePrintObservation.computeDistance`.
- [ ] Results save as `duplicateGroupUUID` on matching records.

## 5. FSEventStream deprecation

`FSEventStreamScheduleWithRunLoop` → `FSEventStreamSetDispatchQueue` in `FolderWatcherService`. Cosmetic — not blocking anything. Callback-safety assert already landed in the 2026-04-23 repo-sweep pass; the deprecation itself remains.

## 6. Tuning

- [ ] Face cluster threshold (0.80 default) needs empirical validation on the user's corpus. Bump up → more identities. Bump down → more aggressive merging.
- [ ] `VisionWorker.acquire()` memory ceiling may be too conservative on 32 GB machines. Could scale with physical RAM.
- [ ] Scene/animal classification threshold now unified and UserDefaults-backed (repo-sweep pass) — the default 0.30 should be validated on the corpus; expose as a Settings slider if it needs frequent tuning.

## 7. Still-open items from the 2026-04-22 bug crawl

- [ ] **FaceClusteringService O(N²) at large person counts.** `clusterSync` compares every new face against every cached centroid, and if no close match, every raw sample of every identity. Fine at 100 identities, bad at 5000. Consider an ANN index (HNSW / Annoy) or a coarse centroid-only first pass before falling back to raw samples.
- [ ] **ThumbnailService cache cost accounting.** `NSCache.setObject(_:forKey:cost:)` cost is advisory; verify the 400 MB `totalCostLimit` actually caps resident memory at 50 K-file scale by instrumenting `ProcessInfo` resident size during a heavy scroll.

## 8. Feature completeness — deferrals from the 2026-04-23 pass

These are polish items flagged during the feature-completeness audit but not shipped in the current pass; revisit as a dedicated UX sprint.

### Library
- Multi-select across the grid (cmd-click, shift-range).
- Favorites flag + filter.
- Inline quick-tag editor on a file card (add/remove a tag without opening the preview).

### People
- Dedicated detail view for a single identity (all faces, merge history, confidence).
- Split-identity tool for a cluster that erroneously merged two people.
- Merge-confidence score rendered in the suggestion pair UI.

### Restructure
- Dry-run file-list detail (expand a ribbon to see which files move where).
- Custom category mapping (drag a source folder to a different target).

### Settings
- Cache management (clear thumbnails, clear face-print cache, clear CLIP embeddings).
- Inline log viewer.
- Reset-to-defaults button per section.

### MediaPreviewOverlay
- Share sheet + "Copy path" / "Copy as Markdown" quick actions.
- Before/after rename panel (visualise the proposed change without leaving the preview).

### UX
- Typography normalisation (settle on one size/weight ramp — currently ad-hoc).
- File-card shadow consistency (a few inline `.shadow` calls still bypass `Theme`).
- Multi-tab motion hierarchy (only content-swap is animated today — consider transitions inside tabs too).
- Slider-control styling (Settings sliders use system chrome; could match the gold theme).

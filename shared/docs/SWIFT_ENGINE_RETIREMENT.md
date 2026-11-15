# Retiring the Swift engine: what would be lost

Audit of 2026-11-15 at `626d2ca0` (branch `agent/swift-audit`). Changes no code. Purpose: before `platforms/apple/engine/Sources/FileIDEngine` (101 files, 35,720 lines) is deleted, list what it does that the Mac-hosted shared Rust engine (`fileidd`, `--features apple-adapters`) does not do, does differently, or does worse.

How each statement was established is marked: **[read]** from source, **[run]** from a run of both engines on this Mac, **[doc]** from STATE/DECISIONS/CONVERGENCE and not re-proved. Runs used debug builds of both engines (Swift: `swift build --product FileIDEngine`; Rust: `cargo build --locked --offline --features apple-adapters`, 1.98, existing `libFileIDAppleKit.dylib` of the same sources), generated files only, no models, no ONNX Runtime, no network (a `sandbox-exec` profile denying network wrapped every build and run), state, models, HOME and CFFIXED_USER_HOME in scratch, no Trash. The Swift engine had no `mlx.metallib` beside it, so it sent `deep_analyze_unavailable` at start; a bundled app does not. Scripts used: `events.py`, `cancel2.py`, `misc.py`, `stub.py`, `crossopen.py` (scratch `~/FileIDDev/scratch/swift-audit`, not committed).

## 1. Summary

- 101 Swift engine files inventoried (section 2); 30 numbered behaviours compared in section 3 (plus the environment, event, state-file and error-kind lists).
- Conformance, run here (no test-Trash-root builds, no models): hosted Rust leading with Swift as peer: **105 pass, 0 fail, 0 known, 21 skip**; Swift leading with hosted Rust as peer: **106 pass, 0 fail, 1 known (shortcut restructure), 19 skip** [run]. Skips: Trash cases (need test-trash-root builds), 2 timeline cases (need a model), and cases that run only when the other engine leads.
- Catalog schema: both engines produce the same 131 objects and the same 36 migration identifiers; after removing whitespace and quotes every table and index SQL is equal, except two index names (`files_on_kind`/`files_on_path_hash` in Swift, `index_files_on_kind`/`index_files_on_path_hash` in Rust) [run].
- **Gaps to close before retirement, most serious first** (details and sizes in 3 and 6): B14 empty-root hides every row (medium); B2/B1 vanished and zero-byte rows counted as "Unreadable" (small); B9/B6 scan notices shown as "Something went wrong" (small); B4 no automatic "Group faces" after a scan (small to medium, needs a run with faces); B8 Recent scans shows no file count (small); B17 log viewer points at a file only Swift writes (small); B16 relative scan root accepted (small); the app has no engine to fall back to (decision, section 4).
- Not assessed: see section 7.

## 2. Inventory by file

All 101 files of `platforms/apple/engine/Sources/FileIDEngine` (35,720 lines, counted at `626d2ca0`). "Hosted Rust" is the shared Rust engine as the Mac app hosts it (`platforms/windows/src/engine`, `--features apple-adapters`, `Contents/MacOS/fileidd`); paths under it are relative to `src/`. Adapter paths are under `platforms/apple/adapters/Sources/`. "Shared" means the code already lives in a module both engines link (`platforms/apple/shared/Sources/<module>`), so deleting the Swift engine does not remove it. Last column: **moved** (the Swift file is a thin user of shared or adapter code, or its work moved there), **ported** (a separate Rust implementation exists and CONVERGENCE or a conformance case says they agree), **by design** (the hosted engine does the job differently), **nothing** (no hosted equivalent; the row says whether that matters).

Every "ported" claim was checked against the Rust file's existence and its header comment; behaviour was compared by the conformance cases and STATE measurements the row names, and by the runs in section 3 where noted. It was not re-proved line by line.

### 2.1 Startup, shutdown, IPC, jobs, logging, power

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `FileIDEngineMain.swift` (2808) | `main`, the command dispatcher (46 commands), bulk actions (rename, trash, merge, reassign, rename person, mark unknown/different, wipe), `runScan` (walk, orphan sweep, FSEvents follow-up, post-scan phases, catch-up), purge of excluded folders, crash detection, auto face clustering | `server.rs` (dispatch), `commands/{scan,bulk,trash,person_merge,roots,wipe,...}.rs`, `scan_session.rs`, `apple/mod.rs` (`run_hosted`) | ported; differences in section 3 (rows B2, B3, B4, B12, B13) |
| `IPC/IPCSink.swift` (342) | Non-blocking writer for IPC events on fd 2; drops/coalesces progress under pressure; 64 MiB frame guard | `ipc/sink.rs` (`route_events_to_stderr`, same frame cap) | ported |
| `IPC/IPCTransport.swift` (54) | Moves events to fd 2 and points stdout at a log so C++ chatter cannot splice into a line | `ipc/sink.rs::route_events_to_stderr` (fd 2 duplicated, then pointed at `logs/engine-stderr.log`) | ported |
| `IPC/JSONLog.swift` (111) | Structured JSONL log `logs/scan.jsonl` (+ `.1`), 1 file, rotated | `logging.rs` writes `logs/engine.jsonl` (daily roll, tracing JSON) | by design; the app's Settings points at `scan.jsonl` (row B17) |
| `JobQueue.swift` (156) | Serializes scan, face clustering and Describe jobs; emits `queueState` | `job_queue.rs`, `server.rs` (`QueueState` on every change) | ported |
| `ResourceScheduler.swift` (311) | Memory/CPU admission for models and jobs | `resource_scheduler.rs` | ported |
| `ScanTaggingAdmission.swift` (20) | Background-CPU reservation for the tagging pass | `resource_scheduler.rs`, `pipeline/catch_up.rs` | ported |
| `AsyncSemaphore.swift` (60) | Bounds in-flight Vision requests | tokio semaphores in `pipeline/tagging.rs` | nothing (language idiom) |
| `PlainError.swift` (39) | Errors whose text is already user wording | none | nothing |
| `SleepGuard.swift` (65) | IOPM assertion during jobs | `apple/mod.rs::SleepAssertion` + `FileIDAppleKit/SleepAssertions.swift` | moved |
| `VLMCoreHostSetup.swift` (50) | Gives `FileIDVLMCore` the engine's log, scheduler, downloader | `FileIDAppleVLM/VLMHost.swift`, `VLMHostHooks.swift` | moved |
| `SecurityScopedAccessRegistry.swift` (134) | Holds bookmark leases for chosen folders | `commands/folder_access.rs` + `FileIDAppleKit/BookmarkLeases.swift` | ported |
| `ScanRootGuard.swift` (31) | Canonical root path; "is this root reachable and non-empty" (unplugged `/Volumes` stub) | `commands/folder_access.rs` (`root_is_listable`), `scan_session.rs` (`clean_walk` gate before hiding rows); the unplugged-stub case is not the same code (row B14) | ported; check |
| `QueryCommands.swift` (90) | Merge suggestions, CLIP text vector, video thumbnail replies | `commands/embed.rs`, `commands/thumbnail.rs`, `commands/person_merge.rs` | ported |
| `Models/DeepAnalyzeCapability.swift` (25) | Startup check for `mlx.metallib`; engine sends `deep_analyze_unavailable` | none (the hosted engine fails a model load with a message instead) | nothing; row B7 |

### 2.2 Scan, discovery, decode, Vision, tagging, text reading

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `ScanCoordinator.swift` (388) | One scan's id, phase, counters, pause/cancel | `coordinator.rs` | ported |
| `Pipeline/Discovery.swift` (591) | Streaming walk, kind classification, skips, exclusions, notices | `pipeline/discovery.rs` | ported (zero-byte difference, row B1) |
| `Pipeline/FileSystemChanges.swift` (84) | FSEvents "did anything change since the scan began" for the follow-up pass | none; `scan_session.rs` compares directory modification times (`dirs_changed_since`) | by design (row B13) |
| `Pipeline/Tagging.swift` (1195) | Per-file processing: decode, Vision, tags, faces, CLIP, OCR, rows | `pipeline/tagging.rs` + `pipeline/tagging_mac.rs` + adapters `ScanImages/ScanVision/ScanModels.swift` over `FileIDImaging` | ported/moved |
| `Pipeline/DocText.swift` (272) | Document text (PDFKit, textutil, zip names) | `FileIDAppleKit/ScanDocuments.swift` + `apple/scan.rs`; portable `pipeline/doc_extract.rs` | moved |
| `Pipeline/ArchiveNames.swift` (65) | File names inside archives | `pipeline/archive_names.rs` | ported |
| `Pipeline/ContentHash.swift` (85) | Persisted content hash (sampled above 16 MB) | `util/content_hash.rs` | ported |
| `Pipeline/ScanCatchUp.swift` (578) | After-scan RAM++ and careful-read catch-up, owed rows | `pipeline/catch_up.rs`, catalog v34/v36 shared SQL | ported |
| `Pipeline/ScanDecodeAhead.swift` (441) | Decode-ahead queue; off by default since 2026-11-13 | `pipeline/tagging.rs` has its own bounded pre-decode | nothing (dormant in Swift; DECISIONS 2026-11-13) |
| `Pipeline/TwinReuse.swift` (484), `Storage/TwinCatalogStore.swift` (153) | Byte-identical images analysed once (in-memory registry, seeded from finished rows) | `pipeline/twin_reuse.rs`, `pipeline/dbwriter.rs` (writer copies the twin's rows) | by design (DECISIONS 2026-11-12) |
| `Pipeline/TextTranscription.swift` (73) | VLM transcription into `ocr_text` | `commands/deep_analyze*.rs`, `pipeline/text_reading.rs` | ported |
| `Pipeline/SpeechPermission.swift` (20) | Speech-recognition permission state | `FileIDAppleKit/MediaSpeech.swift` | moved |
| `Storage/CaptureDateBackfill.swift` (65) | One-time capture-date backfill per root | `pipeline/capture_date_backfill.rs` | ported |
| `Storage/DBWriter.swift` (1156) | The single catalog writer for scan rows, rename healing, vectors | `pipeline/dbwriter.rs` | ported (row B2: missing files) |
| `Models/MobileCLIPService.swift` (307) | CLIP image via ONNX Runtime (fallback when MLX is not chosen) | MLX tower in `FileIDImaging/CLIPVisionMLX.swift` through `ScanModels.swift`; ORT path `models/mobileclip.rs` | moved |
| `Models/RamPlusService.swift` (481) | RAM++ tags via ORT (fallback) | MLX in `FileIDImaging/RamPlusMLX.swift`; `models/ram_plus.rs` | moved |
| `Models/BGETextService.swift` (183), `Models/WordPieceTokenizer.swift` (148), `Pipeline/BGEDocumentMatrix.swift` (107) | BGE document vectors; flat in-memory matrix for Chat scoring (5 idle minutes, 50,000 rows) | `models/bge_text.rs`, `models/wordpiece_tokenizer.rs`; no matrix cache (row B18) | ported; cache by design |
| `Models/ORTSessions.swift` (82) | Process-wide ORT env, CoreML EP session builder | `ort_runtime.rs`, `models/runtime.rs` (CoreML EP configured as `ArcFaceService`) | ported |

### 2.3 Faces, people, clustering

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Pipeline/FaceClustering.swift` (2125) | Extraction of pending faces, clustering, review holdout | `commands/face_clustering_mac.rs`, `commands/face_extract_mac.rs`, `pipeline/face_clustering.rs` | ported (STATE 2026-11-10/11: identical metrics) |
| `Pipeline/IdentityClustering.swift` (483) | Shared identity passes (k 10, single linkage) | `pipeline/identity_clustering.rs` | ported |
| `Pipeline/ExactCosineJoin.swift` (194) | Exact centroid auto-merge | `commands/face_clustering_mac.rs` | ported |
| `Models/ArcFaceService.swift` (328) | SFace embedder through ORT + CoreML EP | `models/sface.rs` (same EP config, bit-identical vectors) | ported |
| `Models/FaceAlign.swift` (209), `Models/FaceLandmarkMatch.swift` (28) | 112×112 alignment, landmark match | `models/face_align.rs`, `models/face_landmark_match.rs` + adapters `Faces*.swift` | ported/moved |
| `Models/FaceAnalysisCache.swift` (115), `Models/FaceClusterPersistence.swift` (68), `Models/StablePersonAssignments.swift` (68) | Face vector cache rules, non-destructive persistence, stable person ids | `commands/face_extract_mac.rs`, `commands/face_clustering_mac.rs` | ported |
| `Models/FaceMeshService.swift` (248) | 478-point face mesh for eye openness (Best Takes) | `models/face_mesh.rs` | ported |
| `Storage/FaceReview.swift` (814), `Storage/FaceQueues.swift` (848) | Review, merge and split queues | `commands/face_review.rs`, `commands/face_queues.rs` | ported |
| `Storage/FaceRefreshJob.swift` (349) | Face-space refresh job | `commands/face_refresh.rs` (Mac backend under `apple-adapters`) | ported |
| `Storage/PersonCounts.swift` (30) | `persons.file_count` | `db/person_counts.rs` | ported |
| Auto face clustering after a scan (inside `FileIDEngineMain.runScan`) | Enqueues "Group faces" when unassigned faces exist | none (row B4) | **nothing: gap** |

### 2.4 Search, vectors, catalog actions, Best Takes, names

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Models/HNSWIndex.swift` (666) | HNSW graph over vectors | `util/hnsw_index.rs` | ported |
| `Storage/CatalogVectorIndex.swift` (351), `Storage/CatalogIndexJob.swift` (65) | Visual search index, snapshot, durable job | `catalog_index.rs`, `catalog_index_snapshot.rs` (a different on-disk snapshot; rebuilds from the v23 change log) | by design |
| `Storage/CatalogSearch.swift` (174), `Storage/SearchStrength.swift` (124) | Keyword/semantic/hybrid search, "N good matches" | `commands/catalog_search.rs`, `models/search_strength.rs` | ported (identical lists on 72 queries) |
| `Storage/CatalogStore.swift` (454), `CatalogCast.swift` (45), `CatalogTakes.swift` (266), `CatalogTakeEvidence.swift` (97), `CatalogTakeSuggestions.swift` (86), `CatalogNamingContext.swift` (87), `CatalogNameProposals.swift` (322) | Catalog actions: chapters, events, takes, evidence, suggestions, naming evidence, name proposals | `commands/catalog.rs`, `catalog_takes.rs`, `catalog_take_suggestions.rs`, `catalog_names.rs`, `pipeline/naming_context.rs` | ported |
| `Storage/CatalogTakeGrouping.swift` (519), `Storage/CatalogTakeAuto.swift` (379), `Pipeline/NearDuplicateGrouping.swift` (217), `Pipeline/TakeScoring.swift` (606), `Pipeline/TakeMediaAnalysis.swift` (316) | Best Takes grouping, auto scoring, near-duplicate grouping, scoring, media signals | `commands/take_grouping.rs`, `catalog_take_auto.rs`, `pipeline/near_duplicate_grouping.rs`, `pipeline/take_scoring.rs`, `apple/faces.rs` (Vision face judgement) | ported |
| `Storage/LibraryRoots.swift` (97) | Roots list with reachability and counts | `commands/roots.rs` | ported |
| `Pipeline/ChatService.swift` (563), `ChatIntent.swift` (829), `ChatFacts.swift` (422), `ChatSearchPlan.swift` (263), `ChatActions.swift` (529), `ChatSelection.swift` (614) | Chat: intents, facts, retrieval plan, action cards, selection checks | `commands/chat*.rs` (`chat.rs`, `chat_intent.rs`, `chat_facts.rs`, `chat_search.rs`, `chat_actions.rs`, `chat_selection.rs`) + MLX through `FileIDAppleVLM` | ported |
| `Pipeline/ChatYield.swift` (31) | A Chat answer runs between Describe files | `commands/deep_analyze_mlx.rs` + `models/resident_service.rs` (`has_waiters`) | ported |
| `Pipeline/MLXErrorScope.swift` (27) | MLX errors fail one file/request, not the engine | `FileIDAppleVLM/VLMHost.swift` (`withError` per request) | moved; scan-time CLIP/RAM++ MLX calls are not under a scope in either engine |

### 2.5 Describe Files, VLM, models, benchmarks

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Pipeline/DeepAnalyzeRunner.swift` (819), `Pipeline/DeepAnalyzeNaming.swift` (515) | Describe Files batch, naming | `commands/deep_analyze.rs`, `commands/deep_analyze_mlx.rs`, `pipeline/deep_analyze*.rs`, `pipeline/deep_analyze_naming.rs` (model calls in shared `FileIDVLMCore` through `apple/vlm.rs`) | ported |
| `Pipeline/VLMDownloader.swift` (535) | Pinned-revision MLX downloads, verification, sentinels | `downloader.rs` (+ `models/mlx.rs` reading and writing the same sentinels) | ported (pinned by `mlx-verified-install.json`) |
| `FileIDEngineMain.swift` prewarm/benchmark paths | `prewarmModel`, `benchmarkModel` | `commands/prewarm.rs`, `commands/benchmark.rs`, `models/benchmark.rs` | ported |

### 2.6 Timeline, video, media tools

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Pipeline/TimelineAnalysis.swift` (851), `TimelineChapterSuggestions.swift` (81), `TimelineSpeechTranscription.swift` (269) | Timeline jobs, chapters, speech passes | `commands/timeline.rs`, `timeline_media.rs`, `timeline_speech.rs`, `pipeline/timeline_analysis.rs`, `pipeline/speech_passages.rs` | ported (same version and coverage ids) |
| `Pipeline/VideoFramePool.swift` (233), `VideoFrameWorker.swift` (421), `VideoSceneSelection.swift` (154) | Frame pool, isolated frame worker, scene selection | `FileIDAppleKit/MediaFrames.swift` + `apple/media.rs` (`--sample-video`, `--sample-video-pool`), `pipeline/video_scene_selection.rs` | moved/ported |
| `Pipeline/MediaTools.swift` (375), `CommonMediaTools.swift` (316) | Trim, remux, audio, proxy, rotate, photo recipes | `commands/tools.rs`, `commands/media_recipes.rs` (`AppleRuntime`) + `FileIDAppleKit/Media{Export,Inspect}.swift` | ported/moved |
| `Pipeline/VideoConversionWorker.swift` (208), `VideoReframing.swift` (195), `VideoStabilization.swift` (225) | Video export worker, follow-people reframing, stabilization | `commands/video_export.rs`, `apple/media.rs` (`--convert-video`), `FileIDAppleKit/Media{Reframing,Stabilization}.swift` | moved |

### 2.7 Organize Folders (Restructure)

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Pipeline/Restructure.swift` (2120), `RestructureSemantic.swift` (932), `RestructureRespect.swift` (290), `RestructureFeedback.swift` (100) | Plan, semantic passes, respect stage, feedback, apply and undo | `pipeline/restructure.rs`, `restructure_semantic.rs`, `restructure_respect.rs`, `restructure_feedback.rs`, `restructure_apply.rs`, `commands/restructure.rs` | ported (plans identical move for move, 8 of 8 comparisons, STATE 2026-11-15) |

### 2.8 Trash, rename, undo journals

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Storage/TrashJournal.swift` (269) | Write-ahead Trash journal `trash-journal.json`, restore, startup reconcile | `commands/trash_log.rs` (`trash_log.json`, HMAC NDJSON, canonical), `commands/journal_import.rs` (one-way import of the Swift file) | ported; import must outlive the Swift engine |
| `Storage/RustTrashLog.swift` (167) | Swift dual-write to the Rust log and read-only recovery from it | none needed after retirement | nothing (deleted with the engine) |
| `Storage/PathMoveJournal.swift` (97) | Rename write-ahead journal `path-moves.ndjson` | `commands/path_moves.rs` (same file and format) | ported |
| `Storage/EngineTrash.swift` (66) | Test-only Trash root (`-DFILEID_TEST_TRASH_ROOT`) | Rust `test-trash-root` feature | ported; the Swift half goes with the engine |
| `FileIDEngineMain.swift` `trashFiles`/`restoreFromTrash` | `FileManager.trashItem`, identity checks | `shell/macos.rs` + `FileIDAppleKit/TrashAndTags.swift` | ported |

### 2.9 Catalog storage, migrations, state folder

| Swift file (lines) | What it does | Hosted Rust equivalent | Status |
|---|---|---|---|
| `Storage/Database.swift` (758) | GRDB pool, migrations v1–v36, open errors (`db_newer_than_engine`) | `db/mod.rs`, `db/migrations.rs` (v1–v20 hand mirrors, v21+ include the shared `shared/catalog/v*.sql`) | ported |
| `Storage/CatalogSchema.swift` (378) | Generated copy of `shared/catalog/v21–v36.sql` (checked by `check_catalog_schema.py`) | `db/migrations.rs` `include_str!` of the same files | nothing (generated file) |
| `Storage/EngineStateRoot.swift` (36) | State folder beside `FILEID_DATABASE_PATH` or Application Support | `paths.rs` (`APP_HOSTED`) | ported |

## 3. Behaviour differences

Verdicts: **same**, **Rust better**, **by design** (different and fine), **GAP** (close before retirement; size small/medium/large), **UNKNOWN** (needs the run named).

### 3.1 Observable differences

| ID | Behaviour | Swift engine | Hosted Rust engine | Evidence | Verdict |
|---|---|---|---|---|---|
| B1 | Zero-byte files | Catalogued (`zero_byte_rows` 1) | Skipped; a zero-byte row from a Swift library becomes `failed=1`, "File is zero bytes and has no indexable content." on the first hosted scan of that root; the app counts it under Unreadable (`ReadStore.unreadableSQL` only excludes "will retry") | [run] `x1.json`, `events.py`; `db/zero_byte.rs`; `ReadStore.swift:317` | by design + GAP small (app: leave these two messages out of Unreadable) |
| B2 | A file removed from disk | Row deleted at the next scan (`sweepOrphans`, up to 5,000 per scan) | Row kept, `failed=1`, "File is no longer present under the completed scan root.", deleted after 30 days (`purge_expired_hidden_rows`). After removing 4 files: Swift 66 rows / 0 failed; Rust 69 rows / 4 failed; the app's Unreadable count is 4 and Settings "Total files" counts them | [run] `events.py`; `scan_session.rs:1475`; DECISIONS 2026-11-14 | GAP small (app SQL excludes the message from Unreadable and totals, or the engine deletes at once as Swift does) |
| B3 | End of a scan | Terminal `progress` (phase completed/cancelled) then `scanComplete` | `phaseChanged(completed/cancelled)` then `scanComplete`, no terminal `progress`; progress ticks are more frequent and carry `residentMB`/`availableMB` 0 (the app never reads them) | [run] `events.py`, `cancel2.py`; `EngineClient.swift:~746` (`ScanProgress.settled`) | same (fixed in the app 2026-11-15) |
| B4 | After a scan, face grouping | Enqueues a "Group faces" job when unassigned faces exist (`autoEnqueueFaceClusteringIfNeeded`) | Nothing: no caller of `handle_run_face_clustering` after a scan; the only callers are the button and `RunFaceClustering`. The Mac app has no auto-run either (`cancelAutoPilot` is an empty stub; buttons only in `PeopleView.swift`) | [read] `FileIDEngineMain.swift:2234`, `server.rs:1626`, `PeopleView.swift:373,734,1895` | GAP small to medium; UNKNOWN until run with real faces, SFace and ORT (none here) |
| B5 | Group faces with no SFace model | `face_cluster_no_model` error ("Open Settings to download it") then `faceClusteringComplete` | `faceClusteringComplete` only, no hint | [run] `events.py` | by design; lost hint, GAP small if the card is wanted |
| B6 | Second `startScan` while one runs | `scan_already_queued` (app lists it as benign) | `scan_already_running` (not in the app's benign list, so the sidebar card shows with title "Something went wrong") | [run] `misc.py`; `EngineClient.swift:789`, `Sidebar.swift` | GAP small (app: add the kind) |
| B7 | `deep_analyze_unavailable` at start when `mlx.metallib` is missing | Sent | Not sent; a load fails later with "mlx.metallib is missing" | [run] `events.py`; `DeepAnalyzeCapability.swift`; `apple/vlm.rs:617` | by design (bundles always carry the metallib) |
| B8 | Settings ▸ Recent scans, file count | Writes `last_file_index` (the app shows "N files"); `total_files` NULL | Writes `total_files`; `last_file_index` NULL, so no count shows | [run] scan_sessions rows in both state folders; `SettingsView.swift:sessionRow` | GAP small |
| B9 | Rescan of an unchanged folder | Silent | `error{rescan_no_changes}`; not in `Sidebar.errorTitle`, so a card "Something went wrong" with the notice text | [run]; [read] `Sidebar.swift:116,222` | GAP small (map the kind to a quiet notice) |
| B10 | Empty or missing folder | Empty: silent. Missing: `discovery_partial` "1 item couldn't be opened" | `empty_folder` ("No files to scan") in both cases | [run] | Rust better |
| B11 | Follow-up pass for files changed during a scan | FSEvents (content, rename, delete) and a retry of Vision timeouts | Directory modification times only; an in-place edit is caught by the next scan | [read] `FileSystemChanges.swift`, `scan_session.rs:89` | by design |
| B12 | Rename or Trash sent during a scan | Not measured | Held by the mutation gate until the scan ends (STATE 2026-11-15) | [doc] | UNKNOWN: run `renameFiles` during a long scan on both |
| B13 | Face crops and crash repair | Swift writes crops, never repairs | Rust repairs crops after a kill (soak 12 of 12) | [doc] | by design |
| B14 | Scan root that exists but is empty (unplugged drive's stub) | `ScanRootGuard.isReachable` skips the sweep: rows kept (42 rows, 0 failed after emptying the root) | Hides every row under the root (41 of 41 `failed=1`, tags kept); they return when files return (`failed=0` after the rescan). Hidden rows are deleted after 30 days | [run] `stub.py` | **GAP medium**: add the same reachability guard before `soft_hide_missing_rows` |
| B15 | Cancelled scan | Terminal progress snapshot "cancelled" + `scanComplete` | `phaseChanged(cancelled)` + `scanComplete`; app handles both | [run] `cancel2.py` | same |
| B16 | Relative scan root | `folder_access_denied` | Accepted when no bookmark is sent; resolved against the engine's folder (`empty_folder`) although STATE 2026-11-15 says Rust already refused it. The app always sends absolute paths | [run] `misc.py`; `folder_access.rs` | GAP small |
| B17 | Log files | `logs/scan.jsonl` (+`.1`), `engine-stderr.log` | `logs/engine.jsonl.<date>`, `engine-stderr.log`. Settings ▸ "Show scan log in Finder" and "Start over" name `scan.jsonl` | [run] state listing; `SettingsView.swift:279`, `EngineClient.swift:1304` | GAP small |
| B18 | BGE document matrix cache for Chat | In memory, 5 idle minutes, 50,000 rows | None | [read] | UNKNOWN: run `benchmark_chat.py` with BGE installed |
| B19 | Twin reuse | In-memory registry | Writer copies the twin's rows | [doc] | by design |
| B20 | Visual index snapshot | Rebuilds | Binary snapshot on disk (`FIDGRAPH`); Swift state is not read | [doc]/[read] | by design (rebuilds from the v23 change log) |
| B21 | `ready`, shutdown | 0.26 s, shutdown 0.04 s | 0.25 s, 0.07 s (debug, loaded machine) | [run] `misc.py` | same |
| B22 | Rename/Trash journals and plan files | `trash-journal.json`, `path-moves.ndjson`, `restructure_undo.ndjson` | Reads all three (`journal_import.rs`, `path_moves.rs`, `restructure_apply.rs`); Rust also writes `trash_log.json` (HMAC, `log-hmac.key`) | [doc]; cross cases pass [run] | same; keep the importers after retirement |
| B23 | RTF, WAV INFO, FLAC titles | Swift engine and hosted engine read RTF (textutil, `ScanDocuments.swift`); neither reads WAV INFO tags or FLAC titles (plain Rust does the audio tags) | same on the Mac; the plain Rust engine has no RTF reader | [doc], [read] | same |
| B24 | Sandbox, folder outside container without bookmark | Refuses | Refuses since 2026-11-15 (`folder_access_denied`) | [doc] | same; TestFlight run still owed |
| B25 | Careful text reads, "Faster text reading" | `FILEID_CAREFUL_READ` read by shared `CarefulTextRead` | Same code through the adapter; set by the app for both | [read] | same |
| B26 | Status pill / first search after a scan | Ranked list at once | Fixed 2026-11-15 (brute-force up to 50,000 vectors) | [doc] | same |
| B27 | Face clustering parameters | k 10 single linkage | Mac port, same; the portable path uses k 32 | [doc] | same on the Mac |
| B28 | Describe Files, no model | `unknown_model` + `deepAnalyzeComplete` | Same two events, plus `queueState` | [run] | same |
| B29 | Organize Folders plan/apply/undo events; missing folder | Same sequences; `plan_restructure_root` | Same | [run] | same |
| B30 | Catalog migrations from older libraries | GRDB v1–v36 | v1–v20 hand-copied SQL, v21+ shared files; same identifiers | [run] schema compare; older-version libraries not run | same; UNKNOWN for libraries older than v36 |

### 3.2 Event sequences (runs)

Both engines were driven with the same generated library. Sequences: `ready`; scan; rescan; cancelled scan; Describe Files with no model; face clustering; plan, apply and undo; `listRoots`; `healthCheck`; `embedTextQuery`; `requestStatus`; idle pause/cancel. **Identical event names and order:** `ready`, plan/apply/undo (`restructurePlan`, `restructureApplyResult` twice), `rootsList`, `healthCheckResult`, `clipTextEmbedding`, idle controls (no reply). **Different:** rows B3, B5, B6, B9, B10, B15, B16; Describe Files adds `queueState` on Rust; `requestStatus` answers `progress` on Swift and `ready` on Rust (the app never sends it). Not run: Describe Files batch with a model, timeline job, model install, Trash and put back (all need a model, network, or the Trash); their sequences were compared only by the conformance cases.

### 3.3 Environment variables and defaults

The app sets, for either engine: `CI_DISABLE_NETWORK_MONITOR=1`, `ORT_DISABLE_TELEMETRY=1`, `FILEID_RESTRUCTURE_GRANULARITY` (non-default only), `FILEID_CAREFUL_READ` (only with the opt-in), plus whatever the user's shell has (`EngineClient.swift:488`). Neither engine reads UserDefaults. Both honour `FILEID_RESTRUCTURE_GRANULARITY` and `FILEID_CAREFUL_READ`.
Read only by Swift-engine code (no hosted reader): `FILEID_DECODE_AHEAD`, `FILEID_DECODE_AHEAD_MB` (dormant; default off), `FILEID_INFERENCE_CONCURRENCY`, `FILEID_VIDEO_SCENES` (hosted always uses scenes), `FILEID_APP_STORE` is only a build flag. Read by shared modules and so by both engines through the adapters: `FILEID_MLX_CACHE_MB`, `FILEID_MODEL_IDLE_SECONDS`, `FILEID_VLM_*` (15 variables), `FILEID_TIMELINE_FRAME_LABELS`, `FILEID_VISION_CLASSIFY_DEVICE`, `FILEID_TEST_VISION_TIMEOUT`, `FILEID_CLIP_BACKEND`, `FILEID_RAMPLUS_BACKEND`, `FILEID_SCAN_MAX_PIXELS`, `FILEID_HF_CACHE_ROOT`. Read by both: `FILEID_DATABASE_PATH`, `FILEID_TEST_TRASH_ROOT`, `FILEID_TWIN_REUSE`, `FILEID_FACE_EP`, `FILEID_FACE_ALIGN`, `FILEID_DEEP_GROUPING`, `FILEID_FACE_SPLIT_MIN_FACES`, `FILEID_RESTRUCTURE_*`, `FILEID_OCR_WARMUP`. The Swift engine ignores `FILEID_MODELS_DIR` (AGENT_RULES lists it for Swift; only `FILEID_HF_CACHE_ROOT` and the real Application Support path matter), the hosted engine honours it. Rust-only variables (`FILEID_DB`, `FILEID_LOG`, `FILEID_SFACE_SESSIONS`, `FILEID_TAG_CAP`, `FILEID_HIDDEN_RETENTION_DAYS`, ...) do not matter to the app. Lists: scratch `env_*.txt`.

### 3.4 Files in the state folder (`~/Library/Application Support/FileID` or the container)

Written by both, same name and format: `fileid.sqlite*`, `restructure_undo.ndjson`, `restructure_plans/`, `face_crops/`, `logs/engine-stderr.log` (+`.1`), `model-benchmarks.json`, `capture-dates-v1/`. Swift only: `logs/scan.jsonl(.1)`, `trash-journal.json` (read by Rust once), `path-moves.ndjson` (Rust reads and writes the same file), `restructure_recover.ndjson`, `ExportRecovery`. Rust only: `trash_log.json` and `log-hmac.key` (the Swift engine dual-writes and reads them), `thumbs.cache/`, `fileid.face-clustering.json`, `sha256-content-hash-since`, `restructure_shortcut_undo/`, `logs/engine.jsonl.<date>`, visual index snapshot files, `runtime/`. Rust created none of these in a Swift state folder that the Swift engine then failed to open: the 13 hosted-led and 11 Swift-led cross cases pass [run]. A Swift-written library (432 files) opened under Rust: no errors at open, `listRoots`/`healthCheck` answered, rescan reported `rescan_no_changes` [run `crossopen.py`]. The app's "Start over" deletes only `fileid.sqlite*`, `logs/scan.jsonl` and `logs/app.log`, so Rust-only side files survive it; the Swift engine had the same behaviour for its own journals (UNKNOWN: whether a stale `fileid.face-clustering.json` can skip clustering on a fresh catalog).

### 3.5 Memory and model residency
Swift frees MLX CLIP and RAM++ 120 s after a scan and before a description model loads (`ScanModelResidency`); hosted does the same through `scan_models.rs` (footprint 193–253 MB after idle, STATE 2026-11-14). Describe Files model parking about 145 MB in both [doc]. Decode-ahead is off in Swift.

### 3.6 Error kinds the app matches (`grep` of the app, each checked in the Rust source)
Sent by both: `face_clustering_busy`, `deep_analyze_already_running`, `prewarm_cancelled`, `model_license_not_accepted`, `find_merge_suggestions_failed`, `plan_restructure_failed`, `plan_restructure_root`, `db_unavailable`, `ipc_frame_too_large`, `discovery_partial`, `db_newer_than_engine`, `folder_access_denied`, `command_decode_failed`, `unknown_model`, `prewarm_failed`. Rust only (the app has titles for the first five): `empty_folder`, `scan_failed`, `model_load_failed`, `restructure_already_running`, `plan_restructure_cancelled`, `plan_restructure_db|store|library_busy|db_unavailable|invalid`; and kinds with no title: `rescan_no_changes`, `scan_already_running` (B6, B9). Swift only: `deep_analyze_unavailable`, `scan_already_queued`, `db_failed`, `orphan_sweep_failed`, `purge_excluded_failed`, `restructure_busy`, `face_cluster_no_model`; the app handles them, none is needed once Swift is gone, except that the first two should be matched to their Rust equivalents (B6, B7). `bookmark_unavailable` is sent by the app itself.

## 4. What the app, scripts, workflows and gates would lose

### 4.1 App (`platforms/apple/app/Sources/FileID`)

| Item | Where | What happens when the Swift engine is gone |
|---|---|---|
| Backend policy, `FILEID_ENGINE_BACKEND`, `engine.backend` default, `EngineFallback` | `EngineBackend.swift` (120 lines) | Delete. `explicitChoice`, `preferred`, `candidates` and the demotion counters exist only to choose between two engines. |
| Launch candidates, demotion calls, `runningBackend`, `fallBackAfterExit`, `switchBackend`, `rustEngineBundled`, `preferredBackend` | `EngineClient.swift` lines 255–360, 384, 481, 630, 721, 969–1051 | Reduce to one binary. `engine_fallback` error kind disappears. The integrity check (`engineIntegrityFailure`) stays for `fileidd`. **A Rust engine that cannot start, or exits twice before `ready`, then leaves the app in `.crashed` with no engine** (today it falls back to Swift for the session). This is the main behavioural loss; there is no replacement short of the respawn budget. |
| Settings ▸ Advanced ▸ Engine choice, fallback note, "Running" row | `Views/SettingsView.swift` 88–104, 243–247 | Remove the segmented bar and the note; keep a "Status" row. The `info.engine == "rust"` test in the "Running" row goes. |
| `EngineModelInstall.active` branch and the app's own download path | `Services/ModelsEngineInstall.swift`, `ModelInstaller.swift` (`runInstall`, `parallelStreamingDownload`, lines 198–250), `CLIPModelInstaller.swift` (lines ~300–360, `installFromLocalZip`) | The `active` test is always true. `runInstall` and the CLIP installer's own downloader become dead; `StreamingDownload.swift` (1,283 lines) and `TLSPinning.swift` (234) in `FileIDShared` then have no user outside their tests. Both, and `CLIPModelInstaller.swift`, are digest-locked in `check_runtime_egress.py`: deleting them is a reviewed refresh, and the shipped binary loses its only Swift HTTP client. |
| `FILEID_APP_STORE` gate `EngineBackendPolicy.isAppStoreBuild` | `EngineBackend.swift`, `SettingsView.swift:88` | The gate exists so TestFlight builds run Swift. With one engine it becomes "the Rust engine in the sandbox", which has been checked only locally (NEXT, Stage 10 step 6). The other `#if FILEID_APP_STORE` sites (`SecurityScopedBookmark.swift`, `MainWindow.swift:308`, `ToolsWorkbench.swift:115,696`) are sandbox behaviour, not engine choice; they stay. |
| `Settings.scanLogURL` = `logs/scan.jsonl` | `Views/SettingsView.swift:279`; `EngineClient.deleteLibraryFiles` | The file is written only by the Swift engine. Under the Rust engine "Show scan log in Finder" points at a file that does not exist (row B17). Point both at `logs/engine.jsonl` or write a `scan.jsonl`. |
| `deep_analyze_unavailable` handling | `EngineClient.swift:772` | Only the Swift engine sends it (row B7). Dead code once Swift is gone, unless the Rust side is given the same startup check. |
| Event handling | `EngineClient.handleEvent` | Nothing is lost: the app handles every event the Rust engine sends. The `scan_already_queued` notice is the only error kind it treats as benign that only Swift sends (row B6). |
| Tests | `FileIDAppTests/EngineBackendSwitchTests.swift` (192), `HostedGapsTests.swift` (35), `SigpipeTests.swift` (13), `EngineResetRecoveryTests.swift` (89), `ModelsEngineInstallTests.swift` (46), `EngineQueryRepliesTests.swift` (spawns a real engine via `FILEID_TEST_ENGINE_PATH`) | The switch and fallback tests go; `SigpipeTests` and `EngineResetRecoveryTests` stay; `EngineQueryRepliesTests` must be pointed at `fileidd` (it needs the adapters dylib beside it). |

### 4.2 Scripts (`platforms/apple`)

| Script | Swift-engine dependence |
|---|---|
| `run.sh` | Builds `--product FileIDEngine`, `pkill -x FileIDEngine` (by name; also forbidden to agents). Needs a rewrite to build `fileidd`. |
| `scripts/assemble_app.sh` | Requires `.build/release/FileIDEngine` (`[ -x … ] || exit`), copies and signs it, refuses a test build by grepping `FILEID_TEST_TRASH_ROOT`; the Rust half (`FILEID_WITH_RUST_ENGINE`, default 1) stays. |
| `scripts/release.sh` | Builds, strips, signs, Team-ID-checks and privacy-scans `FileIDEngine`; uses `Resources/FileIDEngine.entitlements`. The Rust half stays. |
| `scripts/testflight.sh` | Builds the Swift engine with `-DFILEID_APP_STORE`, signs it with `FileIDEngineAppStore.entitlements`, asserts `fileidd` is absent. **Cannot be edited until the owner accepts the Rust engine in the sandbox** (NEXT, Stage 10 step 6). |
| `scripts/iterate.sh` | Corpus regression harness; builds and runs `.build/debug/FileIDEngine` against a FIFO. Must be rewritten for `fileidd`, or dropped (the conformance harness covers the same ground). |
| `scripts/build_dmg.sh` | Builds and bundles `FileIDEngine` (older path than `release.sh`). Rewrite or drop. |
| `scripts/wipe_local_state.sh` | `pkill -x FileIDEngine`, `pkill -9 -x FileIDEngine`; add `fileidd`. |
| `Resources/FileIDEngine*.entitlements`, `FileIDEngine-Info.plist` | The Rust binary has its own entitlements today (`release.sh` signs `fileidd` with the engine entitlements); the file name and the Info.plist keep the old name. |

### 4.3 Workflows

| Workflow | What breaks |
|---|---|
| `.github/workflows/macos.yml` | Builds the Swift engine four ways (release, `-DFILEID_TEST_TRASH_ROOT`, `-DFILEID_APP_STORE`, tests); runs **Swift conformance** with the Rust engine as peer and the **Rust conformance with the Swift engine as peer**; runs `check_catalog_index_jobs.py` against the Swift engine; the "Smoke — engine startup" step launches `.build/release/FileIDEngine` and greps its stderr for `ready`; the privacy scan lists `FileIDEngine`. The 12-minute `swift test` cap is dominated by `EngineTests` (118 files). Every one of these has to be re-pointed or removed. |
| Other workflows | `windows-engine.yml`, `linux.yml`, `release.yml`, `packaging.yml`, `tools-verify.yml` mention `FileIDEngine`, but that is the **Rust** binary's name on those platforms; they do not use the Swift engine. Nothing to change. |

Note the name clash: the hosted Rust binary is built as `FileIDEngine` (and renamed `fileidd` inside the bundle). A script that greps for "FileIDEngine" cannot tell the two apart; after retirement the Cargo bin name can stay but the Swift product name is free.

### 4.4 Policy and evaluation scripts under `shared/scripts`

| Script | Swift dependence |
|---|---|
| `check_runtime_egress.py` | Locks 8 Swift-engine files by path and digest (`VideoConversionWorker`, `VideoFrameWorker`, `VLMDownloader`, `WordPieceTokenizer`, `RamPlusService`, `DocText`, plus shared `StreamingDownload`, `TLSPinning`). Deleting the engine means removing them from the lists in the same commit. `test_check_runtime_egress.py` (46 "swift" mentions) pins several. |
| `check_catalog_schema.py` | Requires `Storage/CatalogSchema.swift` to equal the concatenated `shared/catalog/v21–v36.sql`. Fails the moment the file is deleted; drop the Swift half of the check. The Rust half (the `include_str!` check) stays. `migrations.rs` keeps v1–v20 by hand; nothing else checks them against the Swift `Database.swift` once it is gone (`MigrationParityTests` is the only guard and moves with the engine). |
| `check_app_command_coverage.py` | Reads app sources and the Rust dispatcher; its conformance-case lookup honours `engines: ["swift", …]` (line 157). No Swift engine source is read; it keeps working. |
| `check_binary_privacy.py` | The macOS workflow passes it `.build/release/FileIDEngine`; the script itself is binary-agnostic. |
| `conformance.py` + 126 cases | 17 cases run only when the Swift engine leads (all cross-engine), 32 need both engines. After retirement the cross-engine cases lose their Swift side: they must be turned into single-engine cases against fixtures that the Swift engine wrote once (see the plan). `$engine` expectations remain in `media-tools.json` only. |
| `check_catalog_index_jobs.py` | Starts the **Swift** engine (`swift=True`) to prove index-job recovery. No Rust equivalent runs on CI; `catalog_index_tests.rs` covers recovery by unit test only. |
| `bench_model_load.py`, `bench_vlm.py`, `benchmark_catalog_search.py`, `eval/{people_eval,people_parity,search_eval,selection_eval}.py`, `e2e/{soak,crash_safety}.py` | Take `--flavor swift|rust` or `--runtime swift|rust`. `people_eval.py` defaults to `--flavor swift` and `people_parity.py` compares the two engines (retire it). The soak and crash harnesses lose their Swift variant. |
| `gen_model_catalog_swift.py` | Generates the Swift model catalog; it feeds `FileIDModelInfo` (stays). |
| `run_local_audit_gate.sh` | Runs `swift test` with strict concurrency on the whole package. |
| The soak (`e2e/soak.py --flavor swift`) | The Swift flavour is the only soak of the Swift engine; the hosted Rust flavour exists (`--flavor hosted`). |

## 5. What must stay

### 5.1 Shared Swift modules (`platforms/apple/shared/Sources`)
Built from a name-reference closure over app + adapters sources (`liveness.py`, `symdead.py` in scratch), then checked by `grep`; name-based, so each deletion needs a compile to confirm. Everything below is **[read]**.

| Module | Stays (app or adapters use it) | Becomes dead with the engine |
|---|---|---|
| `FileIDShared` | IPC DTOs and `IPCCoder`, `CatalogProtocol`, `ChatProtocol`, `ToolProtocol`, `DBTypes`, `FloatBlob`, `TagWriter`, `SmartName*`, `FTSQuery`, `ExactFileDigest`, `CLIPTextEncoder` (only `defaultModelURL` is used by the app), `CLIPTokenizer`, `FaceBBox`, `PerceptualGrouping`, `LocalArchiveSafety`, `ORTPrivacy` | `LineReader.swift` (182), `PathContainment.swift` (38), `StablePathHash.swift` (54), `PersonMergeSuggestions.swift` (334, only `QueryCommands`), `SmartFileName.swift` (48), `FaceCacheProvenance.swift` (53, engine only), `FilesystemNameSafe.swift` (53, engine and shared only), `StreamingDownload.swift` (1,283) and `TLSPinning.swift` (234) once the app installers drop their own download path |
| `FileIDImaging` | all, through the adapters; `BoundedImageDecode.isHEIF`, `FaceQualityGate.Reason`, `VisionWorkerPool` look unused outside the engine | single symbols only |
| `FileIDVLMCore` | all, through `FileIDAppleVLM` | `AnalysisResultFactory.swift` (9) |
| `FileIDModelInfo` | all | `AIModelsInstalled.swift` (18), `PathRedaction.swift` only if no app/adapter caller remains |
| `FileIDORTC` + the `onnxruntime` SwiftPM package | nothing outside `ORTPrivacy`/`CLIPTextEncoder` (the app's `import` list has no ORT) | The package dependency of the `FileID` and `FileIDShared` targets can be removed once `CLIPTextEncoder` is reduced to its path. Also removable with the engine: `swift-async-algorithms` (engine only). `GRDB` stays (app `ReadStore`). MLX, swift-transformers stay (adapters). Removing a dependency changes `Package.resolved`; it adds none. |

### 5.2 Tests (`Tests/EngineTests`, 118 files)
- **Test shared code that stays** (move to `SharedTests` or `AppleKitTests`; reference no engine type): `CarefulTextReadTests`, `HardwareTests`, `JPEGPreviewTests`, `ModelBenchmarkTests`, `ModelResidencyTests`, `ScanModelResidencyTests`, `TimelineSignalAnalysisTests`.
- **Mostly shared code with one engine call** (decide per file when moving): `HEIFDecoderTests` and `TaggingLoadSizeTests` (use `Tagging.loadImageAndEXIF`), `VLMSceneCaptionTests`, `MLXErrorScopeTests`, `FaceOrientationRefreshTests`, `FaceAlignTests`, `FaceLandmarkMatchTests`, `ORTSessionsTests`, `VLMSentinelIdentityTests`/`VLMSentinelInteropTests` (the Rust side pins the same sentinels by `mlx-verified-install.json`).
- **Engine only** (delete with it): the other ~100 files (catalog, DBWriter, restructure, chat, takes, faces, timeline, media, IPC sink, job queue). Their coverage on the Rust side is Rust unit tests plus conformance; no per-test mapping was made, so a list of Swift-only invariants (for example `CrashSafetyTests`, `MigrationParityTests`, `DiscoveryTests`) should be walked against `cargo test` names before the target is deleted.
- Also needing a new home: `FileIDAppTests/EngineQueryRepliesTests` (spawns an engine), `ScanCancellationTests` (spawns an engine; its replacement is a conformance cancel case), `FileIDAppTests/EngineBackendSwitchTests` (delete).

## 6. Plan

### 6.1 Gaps to close first, in order
1. B14 empty-root guard in the hosted scan before `soft_hide_missing_rows` (refuse to hide when the root is missing, empty or an unmounted `/Volumes` stub, as `ScanRootGuard.isReachable`); add a conformance case (empty root keeps rows, both engines). **Medium** (one function, one case, one soak round).
2. B2/B1 app counts: exclude "no longer present" and "zero bytes" rows from Unreadable and totals (`ReadStore.swift:228,234,317`); test in `ReadStoreUnreadableTests`. **Small.**
3. B9/B6/B16 scan notices: map `rescan_no_changes` and `scan_already_running` to quiet notices in `Sidebar.swift` and `EngineClient.swift:789`; refuse a relative root in `folder_access::scan_access`. **Small** each.
4. B8 write `last_file_index` (or read `total_files`) so Recent scans shows a count. **Small.**
5. B17 point Settings and Start over at `logs/engine.jsonl*`. **Small.**
6. B4 auto "Group faces" after a scan on the hosted engine, and B5's model hint. **Small to medium**; first run it with real faces, SFace and the bundled ORT (not possible here).
7. B12, B18, B30: three runs (rename during a long scan; Chat with BGE installed; a library from an older Swift version) before deciding. **Small each, run-only.**
8. Replace `check_catalog_index_jobs.py` (Swift-only) with a Rust/hosted run. **Small.**
9. Decide what the app does when `fileidd` fails to start (today: Swift fallback). Options: keep a "repair/reinstall" card, no engine. Owner decision.

### 6.2 Removal, as mergeable steps (each leaves `main` green)
1. **Stop shipping the Swift engine in Developer ID bundles** (`assemble_app.sh`, `release.sh`, `build_dmg.sh`; keep the target). Risk: loses the fallback; undo: revert the script change. Needs owner confirmations below.
2. **Remove fallback, Settings choice, `engine.backend`, `FILEID_ENGINE_BACKEND`** (`EngineBackend.swift`, `EngineClient`, `SettingsView`, tests). Keep `isAppStoreBuild` until step 6. Undo: revert.
3. **Turn cross-engine conformance cases into one-engine cases** using fixtures once written by the Swift engine (committed under `shared/test-corpus`), drop `--peer-engine` from `macos.yml`, `engines:["swift"]` cases removed. Risk: loses the live cross-check; keep the fixtures so the Swift-written catalog, journals and plans stay covered.
4. **Move the surviving tests** (section 5.2); remove the Swift flavour from `bench_*`, `eval/*`, `soak`, `crash_safety`; port or drop `iterate.sh`, `run.sh`.
5. **Delete the Swift engine target**, `CatalogSchema.swift` and the Swift half of `check_catalog_schema.py`, the `swift-async-algorithms` dependency, the Swift jobs in `macos.yml` (release engine, test-trash-root, App Store engine, smoke), and the egress-lock entries (reviewed digest refresh). Keep the importers for `trash-journal.json`, `path-moves.ndjson` and headerless `restructure_undo.ndjson` for at least one release after step 1.
6. **App Store**: only after the owner's TestFlight sandbox run, flip the gate, change `testflight.sh` to bundle `fileidd`, delete `FileIDEngineAppStore.entitlements` use of the Swift binary.
7. **Prune dead shared code** (5.1), remove the `onnxruntime` package, then delete `StreamingDownload`/`TLSPinning` with the app's own installers (digest-locked; reviewed refresh).
Each step: run the policy scripts, Swift tests, Rust tests, conformance, and the three conformance suites after merging `main`.

### 6.3 What the owner must confirm first
The TestFlight sandbox run (NEXT Stage 10 step 6, including Trash and Put back inside the sandbox); the quiet-machine timing (step 7); notarization and the on-screen walk with the Rust engine (steps 2–4); a real-catalog copy opened, changed and undone by Rust and reopened by Swift (step 8); a decision on item 9 above.

## 7. Could not be assessed
Faces on real photos, ORT and models (none installed here): B4, B5, face clustering sequence; Describe Files, timeline, chat and benchmark event sequences with models; model install events (network); Trash and put-back (needs test-trash-root builds; the real Trash is forbidden); sandbox/TestFlight behaviour; libraries written by Swift versions older than the current v36; the app's actual rendering of each notice (read only, not seen on screen); performance beyond the debug-build timings above; memory residency measurements; symbol-level dead code (name-based only; a compile is needed); soak behaviour of either engine.

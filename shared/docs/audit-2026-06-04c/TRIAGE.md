# Audit 2026-06-04c — Triage & fix plan

Five-workflow deep audit (engine correctness · app correctness · perf/mem · security/integrity),
each with refute-by-default 3-skeptic verification (confirmed = >=2/3 "real"). 44 confirmed + ~9 contested.
Baseline before any change: engine clippy -D + tests green; app build + tests green.

Disposition: **FIX** = fix this pass · **DEFER** = real but needs schema/hardware/product call · **RECHECK** = contested, verify before deciding.

## Convergences (one fix covers several)
- BGE pooling OOB: engine[9] + sec[1] -> single clamp+validate fix in bge_text.rs.
- Person-writer interlock family: engine[17] (wipe vs bulk), sec[0] (clustering phase-3 vs bulk), sec-contested (StartScan vs clustering) -> one shared interlock.
- Restructure-plan path: engine[10] (outbound 1MB cap) + app[0] (O(n^2) reader) + app[1]/[3] (Keep moves) -> related cluster.
- Off-UI-thread PropertyChanged: app[2] + app[12] -> marshal via captured _ui.

## ENGINE (correctness)
- [E0] FIX  HIGH  hnsw_index.rs:37 — entropy-seeded HNSW -> nondeterministic clustering. Add fixed .seed(). +test.
- [E1] FIX  HIGH  tagging.rs:1726 — mid-scan GPU-removal leaves image row failed=false (stranded in skip-set). Mark failed.
- [E2] FIX  MED   tagging.rs:1289 — GPU death drops audio metadata tags + strands row. Mark evaluated/failed.
- [E3] FIX  MED   scan_session.rs:368 — empty/rescan notice racily dropped when discovery <250ms. Emit independent of tick.
- [E4] FIX  MED   deep_analyze.rs:403 — cancel can't interrupt in-flight VLM request (300s). select! on cancel + shorter timeout.
- [E5] DEFER MED  commands/deep_analyze.rs:530 — server death mid-batch fails all remaining (no fallback). Needs liveness probe + CLI fallback; moderate, evaluate.
- [E6] FIX  MED   variants.rs:39 — EP-variant chosen from override-blind active_provider() vs bound EP. Use priority_chain first EP.
- [E7] FIX  MED   ep_guard.rs:54 — concurrent EmbedTextQuery+StartScan race on shared .ep_attempt breadcrumb. Per-owner key or serialize.
- [E8] FIX  MED   wordpiece_tokenizer.rs:96 — ASCII-only lowercase -> non-ASCII -> [UNK]. Use to_lowercase().
- [E9] FIX  MED   bge_text.rs:140 — mean-pool indexes attention_mask by model seq dim -> panic. Clamp+validate. (== sec[1])
- [E10]FIX  MED   EngineClient.Commands.cs:21 / main.rs:330 — applyRestructure 1MB OUTBOUND cap rejects large plan. App-side chunk.
- [E11]FIX  MED   downloader.rs:90 — redirect allowlist checks host not scheme -> http downgrade. Reject non-https.
- [E12]FIX  LOW   restructure_semantic.rs:176 — existing-folder routing can emit dest outside library_root. Drop out-of-root prototypes.
- [E13]FIX  LOW   vlm_server.rs:123 — 120s readiness defeated by stalled /health. Per-request timeout on probe.
- [E14]FIX  LOW   clip_tokenizer.rs:113 — no punctuation pre-tokenization, diverges from reference CLIP. Isolate punctuation.
- [E15]DEFER LOW  shell/trash.rs:40 — long-path probe verbatim but delete non-extended. Needs longPathAware manifest (build change). Document.
- [E16]FIX  LOW   downloader.rs:475 — parallel->simple fallback orphans .part files. Best-effort remove.
- [E17]FIX  LOW   wipe.rs:47 — wipe doesn't interlock vs bulk person/tag handlers. (folded into interlock family)

## APP (correctness)
- [A0] FIX  HIGH  EngineClient.cs:592 — O(n^2) frame reader (4MiB=132s). Incremental scan offset. +test.
- [A1/A3]FIX HIGH RestructureGrouping.cs:11 / RestructureView.xaml.cs:616 — Anchor/"Keep" moves silently applied despite "untouched" UI. **Engine-side fix: drop Anchor-folder moves from plan after counting** (matches macOS "no proposals for anchor folders"). +test. Document in DECISIONS.
- [A2] FIX  HIGH  ModelInstallerService.cs:949 — Reset() raises PropertyChanged off UI thread -> RPC_E_WRONG_THREAD. Marshal via _ui.
- [A4] FIX  MED   LibraryViewModel.cs:410 — refresh entry points clobber each other. Monotonic generation guard.
- [A5] FIX  MED   LibraryViewModel.cs:324 — IsLoading cleared while another refresh in flight. Interlocked counter.
- [A6] FIX  MED   ThumbnailService.cs:173 — per-request CT ignored after dequeue. Linked CTS.
- [A7] FIX  MED   LibraryView.xaml.cs:60 — ReadStore never disposed (SQLite+semaphore leak/tab nav). Dispose in OnUnloaded.
- [A8] FIX  MED   AppSettings.cs:244 — shared static debounce CTS -> lost-update. Merge-on-write under gate.
- [A9] FIX  MED   FilePreviewSheet.xaml.cs:361 — no nav generation guard -> stale async overwrites. _navGen.
- [A10]RECHECK MED DrillDownSheet.xaml.cs:54 — Sankey "Other" bucket drilldown wrong list. Reproduce bucketing (verify first).
- [A11]FIX  MED   SpringEasing.cs:89 — AnimateScale anchors at top-left (visual.Size 0,0). Use ActualWidth/Height.
- [A12]FIX  LOW   EngineClient.Commands.cs:433 — WaitFor/DeepAnalyze continuations write observable state off UI thread. Marshal.
- [A13]FIX  LOW   DeepAnalyzeView.xaml.cs:418 — _proposedNameCount never reset on new run. Reset.
- [A14]FIX  LOW   PeopleView.xaml.cs:94 — SqliteConnection leaked in hidden-unknowns footer. using.
- [A15]FIX  LOW   MainWindow.xaml.cs:652 — OnDragOver null-derefs DragUIOverride. Null guard.

## PERF
- [P0] FIX  HIGH  bulk.rs:157 — applyTags COM/sidecar per file INSIDE writer tx. Collect then write after commit.
- [P1] FIX  HIGH  dbwriter.rs:388 — face-crop JPEG encode+write inside SQLite tx/lock. Move crops out of lock/after commit.
- [P2] FIX  HIGH  DebugLog.cs:75 — sync disk write per IPC event on UI thread. Buffered background sink (KEEP tracing).
- [P3] FIX  MED   dbwriter.rs:247 — eager redact_path_for_log per file. Inline into error closures.
- [P4] FIX  MED   discovery.rs:232 — per-entry String alloc in noise filter. Use Cow.
- [P5] FIX  MED   tagging.rs:1674 — OCR clones full RGB frame. Move instead of clone.
- [P6] FIX  LOW   FilePreviewSheet.xaml.cs:359 — preview decodes at native res. Set DecodePixelWidth=1024.
- [Pc] RECHECK HIGH EngineClient.cs:428 — Authenticode revocation + SHA-256 on UI thread before first frame. Verify.

## SECURITY / INTEGRITY
- [S0] FIX  HIGH  face_clustering.rs:293 — phase-3 DELETE+reINSERT discards concurrent People edits. (interlock family)
- [S1] FIX  HIGH  bge_text.rs:140 — == E9.
- [S2] DEFER/FIX MED trash.rs:153 — restoreFromTrash restores arbitrary item on dup original path. Disambiguator (timestamp). Evaluate.

## CONTESTED (recheck before deciding)
- dbwriter.rs:655 — rename-heal UPDATE OR REPLACE orphans ocr_fts/doc_fts (FTS5 desync). RECHECK (data-integrity).
- shell/ocr.rs:55 — OCR no COM/WinRT init on blocking-pool thread -> CO_E_NOTINITIALIZED swallowed. RECHECK.
- trash.rs:219 — revertMerge sets dest person file_count to SOURCE count. RECHECK (wrong counter).
- main.rs:396 — panic in inline stdio dispatch hangs engine. RECHECK (robustness).
- ram_plus_batch.rs:109 — batch coordinator panics on thread-spawn failure. RECHECK (robustness).
- trash.rs:179 — restoreFromTrash claims success when dest occupied. RECHECK.

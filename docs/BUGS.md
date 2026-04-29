# FileID — Bug & Performance Checklist

Full codebase audit completed 2026-04-21. 16 issues identified. All implemented and verified.

---

## 🔴 Critical — Broken core features

- [x] **C1** Grid empty during scan — `scanBatchCount` was defined but never incremented; grid never refreshed while scanning
  - Fixed: MediaProcessor now saves every 100 results (was 1000) and increments `scanBatchCount` after each save; `ProcessingGridView` observes `scanBatchCount` and calls `resetPagination`

- [x] **C2** Blank flash on search/tab change — `resetPagination` cleared `visibleFiles = []` synchronously before async fetch completed
  - Fixed: `resetPagination` no longer clears `visibleFiles`; `loadNextPage` replaces on page 0, appends on subsequent pages

- [x] **C3** Duplicate detection non-functional — `scenePrint(enabled:)` defaulted to `false`; `runDuplicateDetection` always found zero matches
  - Fixed: `processFile` now calls `worker.scenePrint(cgImage, enabled: true)`

- [x] **C4** Resume scan never works — `ScanSession.completedAt` was never written after scan finished
  - Fixed: `finishNamingPhase()` now fetches the active ScanSession and sets `completedAt = Date()`

---

## 🟠 High — Significant UX degradation

- [x] **H1** Batch save at 1000 — grid saw no new files for first ~20s of scan
  - Fixed: batch reduced to 100 (see C1)

- [x] **H2** `updateTreeIfNeeded` loaded all 58K records from DB every 5s — O(N) query on every tree tick
  - Fixed: Replaced DB-polling tree with an in-memory `treeAccumulator` dictionary. `MediaProcessor` calls `recordTreeProgress(fileURL:done:)` per file (O(1) per update). Tree rebuilds from accumulator in O(unique-folders), never touches the DB.

- [x] **H3** Preview navigation capped at 2000 files — arrow keys stopped working after file 2000
  - Fixed: `MediaPreviewOverlay.allFiles` fetch limit raised from 2000 → 10,000

- [x] **H4** `FileCard` never showed processing state — status jumped `.pending → .namingRequired` silently
  - Fixed: `enqueue()` in `MediaProcessor` now inserts records with `.processing` status

---

## 🟡 Medium — Noticeable but recoverable

- [x] **M1** `CleanupView` loads all files at once via `@Query` — scroll lags with 10K+ files
  - Fixed: Replaced `@Query` with per-tab `FetchDescriptor` queries with predicates and `fetchLimit = 500`. Screenshots (which need array filtering) fetch 2000 non-trashed records and filter in-memory. Categories reload on tab change and on `scanBatchCount` change.

- [x] **M2** `FaceClusteringService.suggestedMerges` is O(N²) — blocks UI when People tab opens with many identities
  - Fixed: Result cached after first computation; invalidated only when `merge()` is called

- [x] **M3** `loadNextPage` auto-retry race — recursive call can hit same offset twice on sparse pages
  - Verified safe: recursive call runs synchronously on MainActor after `currentPage` is already incremented, so offset is always correct. No fix needed.

- [x] **M4** `pageContexts` cleared while `visibleFiles` still holds SwiftData object refs
  - Fixed: Removed `pageContexts = []` from `resetPagination`. `loadNextPage` already atomically replaces `pageContexts = [context]` and `visibleFiles = filtered` together on the first page.

---

## 🟢 Low — Polish and dead code

- [x] **L1** `VisionProcessor.evaluateAesthetics` dead code — never called by MediaProcessor
  - Fixed: Deleted. MediaProcessor uses `lightweightAesthetic()` instead.

- [x] **L2** `aestheticScore` computed but never exposed as sort option in grid
  - Fixed: Added "Date / Best" segmented Picker to `ProcessingGridView`. `AppViewModel.sortByAesthetic` drives sort order in `loadNextPage` FetchDescriptor. Picker change triggers `resetPagination`.

- [x] **L3** Face crop JPEG stored in `PersonRecord.representativeFaceCropData` but not rendered in PeopleView
  - Fixed: `PersonCard` now falls back to `ThumbnailView(url: identity.sampleFileURLs.first)` when crop data is nil, with the same gold/blue ring overlay. Only falls back to gray circle when no sample URLs exist either.

- [x] **L4** File sizes shown in grid can be stale if files moved/resized since scan
  - Fixed: `FileCard.task` reads actual file size from disk after thumbnail loads and updates `file.fileSizeMB` if delta > 0.1 MB.

---

## Summary

| Priority | Total | Done | Remaining |
|----------|-------|------|-----------|
| 🔴 Critical | 4 | 4 | 0 |
| 🟠 High | 4 | 4 | 0 |
| 🟡 Medium | 4 | 4 | 0 |
| 🟢 Low | 4 | 4 | 0 |
| **Total** | **16** | **16** | **0** |

---

# v2 Audit Pass — 2026-04-27

Six concrete findings turned up while diagnosing the persistent "blank sidebar + missing PeopleView header" symptom. Three fixed in this pass, three left explicitly deferred (with rationale).

## 🔴 Critical — Fixed in this pass

- [x] **V1** Sidebar Section regression — `app/Sources/FileIDv2/Views/Sidebar.swift:29` rebuilt the body as `List { headerRow ... }` (Text labels, no `Section { ... }` wrappers) under `.listStyle(.sidebar)`. macOS 26 NavigationSplitView's sidebar list does not properly inflate row contents without Section grouping; the symptom was a visually empty sidebar after engine respawn / sheet-dismiss / heavy `queueState` event flow. **Fixed**: restored v1's `List { Section("…") { row } }` pattern, which ships fine on the same macOS 26.4.1. v1 reference: `legacy/v1/Sources/MainWindowView.swift:359-459`.

- [x] **V2** PeopleView header collapse — `app/Sources/FileIDv2/Views/PeopleView.swift:35-58` wrapped the header in `VStack(spacing: 0) { … }.layoutPriority(1).fixedSize(horizontal: false, vertical: true)` to "anchor" it. `fixedSize(vertical:)` on a VStack containing an HStack with `Spacer()` is ambiguous — the resolver collapses to height 0 in a flexible parent. **Fixed**: removed the wrapper; plain VStack lets `header` take its intrinsic height while `content` (which already has `.frame(maxHeight: .infinity)` baked into each branch) expands without pushing the header out.

- [x] **V3** Orphan engine after force-quit — engine relied solely on stdin EOF detection (`for try await cmd in commands` loop break). Confirmed in this session: prior app run's engine (pid 69790) was still alive while a fresh app+engine pair (pid 70489 + 70502) ran on top. **Fixed**: added a parent-death watchdog in `engine/Sources/FileIDEngine/FileIDEngineMain.swift:25-35` that polls `getppid() == 1` every 5 s and `Darwin._exit(0)` when the parent dies. Belt-and-suspenders complement to stdin EOF.

## 🟡 Medium — Deferred (documented for follow-up)

- [ ] **V4** `VisionWorkerPool.acquire` continuation leak on Task cancellation — `engine/Sources/FileIDEngine/VisionWorker.swift:184-187`. If a task is cancelled while waiting for a worker, the appended waiter is never resumed; the actor's `waiters` array grows indefinitely. Severity: medium. Not currently triggered (the codebase doesn't cancel tasks at this layer), but a future feature that does will leak. **Fix when needed**: wrap in `withTaskCancellationHandler` and remove from `waiters` on cancel.

- [ ] **V5** `IPCSink.parkUntilWoken` orphan timer tasks — `engine/Sources/FileIDEngine/IPCSink.swift:142-145`. Each park spins a Task that sleeps `timeoutMs` then tries to wake the drainer. Overlapping parks accumulate short-lived tasks (~250 ms lifetime each). Severity: low (memory cost negligible, no correctness issue — `wakeDrainerIfWaiting` no-ops when nothing is parked). **Fix when noticed**: cancel the prior timer Task in the next park call.

- [ ] **V6** Conditional whole-Section in Sidebar (Processing Control + Queue) — `app/Sources/FileIDv2/Views/Sidebar.swift:39-49` still uses `if let url = pickedURL { Section("Processing Control") { … } }` and `if !engine.queueState.pending.isEmpty { Section("Queue") { … } }`. Same pattern that may have caused the original elision concern. Risk: low for these two — Folder + Navigation + Engine are always rendered, so even worst-case elision keeps the sidebar non-empty. **Fix if elision recurs**: convert to always-rendered Section with conditional inner content (the same mitigation V1 applied to Navigation).

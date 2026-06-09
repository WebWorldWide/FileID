# FileID — Known Issues & Bug Tracker

> Open defects and known-issue notes across both platforms. Closed bugs are not
> kept here — once fixed, the detail lives in `git log` and (for non-obvious
> calls) [`DECISIONS.md`](DECISIONS.md). For *next work* with acceptance
> criteria see [`NEXT.md`](NEXT.md); for *what's working now* see
> [`STATE.md`](STATE.md). This file is for "we know it's wrong and haven't fixed
> it yet."

Tiers: 🔴 Critical (broken core feature / data risk) · 🟠 High (significant UX
degradation) · 🟡 Medium (recoverable, latent, or large-library only) · 🟢 Low
(polish / dead code).

---

## 🔴 Critical

_None open._

## 🟠 High

- [ ] **Rename-heal collapses coexisting exact-duplicate files** (cross-platform;
  Windows `pipeline/dbwriter.rs`, macOS `Database`/dbwriter). The rename/move heal
  (v8 identity) re-binds an existing row to a new path whenever the file's
  `file_ref` *or* `content_hash` matches — without checking the old path still
  exists on disk. For a real move that is correct, but when two byte-identical
  files coexist (`IMG_1558.HEIC` + `IMG_1558(1).HEIC`), the second steals the
  first's row, so only one appears in the library and the Cleanup tab can't
  surface the exact-dup group. Not data loss — both files stay on disk.
  **Fix:** only heal when the prior path no longer exists (stat it) or the USN
  journal recorded a rename; otherwise insert a distinct row and let phash dedup
  handle it. Mirror in macOS for byte-faithful parity. *Acceptance:* scanning N
  byte-identical pairs yields 2N rows and Cleanup shows the dup group.

## 🟡 Medium

- [ ] **SFace identity clustering — Pass 1 is single-linkage** (cross-platform;
  Windows `pipeline/identity_clustering.rs`, the source of truth for the shared
  algorithm). Pass 1 is connected-components on a kNN graph, which still chains
  different people through bridge faces on very large libraries. The current
  bands (`pass1_cosine 0.66`, calibrated on-hardware) exploit the gap between
  genuine clusters (~0.85+ mean cohesion) and chained blobs (~0.50) and fail
  *safe* toward over-split, so the symptom is extra singletons (mergeable in the
  UI), not silent identity merges. The structural fix is mutual-kNN / density-
  gated edges (a higher threshold would start over-splitting genuine identities);
  the bands themselves want a hand-labeled `G:\TrueNAS` subset to find the
  precision/recall optimum. Marked PROVISIONAL in the code.

## 🟢 Low

_None open._

---

## Deferred macOS audit notes (latent, not currently triggered)

Two items surfaced in the 2026-04 macOS audit are real but not reachable on the
current call paths; fix when a feature starts exercising them.

- [ ] **`VisionWorkerPool` continuation leak on task cancellation** —
  `platforms/apple/engine/Sources/FileIDEngine/VisionWorker.swift`. A task
  cancelled while waiting to acquire a worker is never resumed and never removed
  from the waiter set, so the actor's waiter list can grow unbounded. The engine
  doesn't cancel tasks at this layer today. **Fix when needed:** wrap the wait in
  `withTaskCancellationHandler` and drop the waiter on cancel.

- [ ] **`IPCSink` backstop-timer task accumulation** —
  `platforms/apple/engine/Sources/FileIDEngine/IPC/IPCSink.swift`. Each
  `parkUntilWoken` spins a short-lived backstop `Task` that sleeps the timeout
  then tries to wake the drainer; overlapping parks accumulate these tasks. The
  single-waiter invariant means there's no correctness issue and per-task memory
  is negligible. **Fix if noticed:** cancel the prior backstop task in the next
  park.

---

## History

A full macOS codebase audit (16 issues across the four tiers) was completed and
closed 2026-04-21, with a follow-up pass on the sidebar / PeopleView layout and
the orphan-engine-after-force-quit watchdog 2026-04-27. All fixes shipped; the
per-issue detail is in `git log shared/docs/BUGS.md` and the surrounding engine /
view commits.

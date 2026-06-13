# Audit 2026-06-10 — TRIAGE

Synthesis of four adversarial audit workflows (WF-1 unit-audit, WF-1b lean unit-audit, WF-2 parity, WF-3 perf-adaptive). 252 raw findings -> 131 canonical fixable + 15 rejected after dedup. Source of truth for the fix waves. Machine-readable mirror: `findings.json`.

## Summary — counts by wave x severity

| Wave | crit | high | med | low | total |
|---|---|---|---|---|---|
| C1 | 0 | 6 | 12 | 7 | 25 |
| C2 | 0 | 1 | 5 | 2 | 8 |
| C3 | 1 | 19 | 19 | 6 | 45 |
| C4 | 1 | 6 | 7 | 7 | 21 |
| C5 | 0 | 4 | 7 | 1 | 12 |
| C6 | 0 | 3 | 11 | 4 | 18 |
| C7 | 0 | 1 | 1 | 0 | 2 |
| **TOTAL (fixable)** | 2 | 40 | 62 | 27 | 131 |

## Summary — by disposition

| Disposition | count |
|---|---|
| FIX-LOCAL | 30 |
| FIX-CI | 96 |
| HARDWARE | 5 |
| REJECT | 15 |
| **TOTAL** | 146 |

Disposition legend: **FIX-LOCAL** = Rust engine, verifiable here (clippy/test). **FIX-CI** = macOS Swift (swift build local, tests CI-only) or C#/WinUI (CI-only). **HARDWARE** = ML threshold values / GPU-EP / WinUI-runtime / RTX-2060 / Mac-needed -> NEXT.md recipe, not a code fix now. **REJECT** = deliberate guard or ruled divergence (cited).

## C1 — Rust engine (Windows) correctness/perf/security  (25)

- **[F-C1-001]** HIGH `FIX-LOCAL` — Graceful shutdown during model-load leaves .ep_attempt breadcrumb on disk — false-poisons a healthy CUDA/OpenVINO EP next launch
    - file: `platforms/windows/src/engine/src/ep_guard.rs`
    - sources: WF1b:W13, WF1b:W12, WF3:ep-cuda-readiness
- **[F-C1-002]** HIGH `FIX-LOCAL` — Terminal PhaseChanged(Cancelled/Failed) emitted via droppable try_send — a drop renders a cancelled scan as Completed and auto-fires face clustering
    - file: `platforms/windows/src/engine/src/scan_session.rs`
    - sources: WF1b:W11, WF3:backpressure-e2e
- **[F-C1-003]** HIGH `FIX-LOCAL` — restoreFromTrash reports success when the original path is already occupied — trashed bytes silently never restored
    - file: `platforms/windows/src/engine/src/commands/trash.rs`
    - sources: WF1b:W22
- **[F-C1-004]** HIGH `FIX-LOCAL` — Anchor-strip silently drops the semantic butler's highest-confidence moves when a source folder's files all route to one destination group
    - file: `platforms/windows/src/engine/src/commands/restructure.rs`
    - sources: WF1b:W5
- **[F-C1-005]** HIGH `FIX-LOCAL` — Deep Analyze batch silently excludes PDFs despite a shipped default-on PDF render path (kind IN ('image','video'))
    - file: `platforms/windows/src/engine/src/pipeline/deep_analyze.rs`  [ruling]
    - sources: WF1b:W3, WF2:P4-deep-analyze
- **[F-C1-006]** HIGH `FIX-LOCAL` — Deep Analyze ignores gpuExecutionProviderOverride=cpu — the documented GPU-TDR recovery path still forces full GPU offload (-ngl 99)
    - file: `platforms/windows/src/engine/src/pipeline/deep_analyze.rs`
    - sources: WF1b:W4, WF3:ep-cuda-readiness
- **[F-C1-007]** MEDIUM `FIX-LOCAL` — restoreFromTrash shells out a fresh PowerShell (full Recycle Bin enumeration) per item serially — large undo batches blow the 30s app waiter
    - file: `platforms/windows/src/engine/src/commands/trash.rs`
    - sources: WF1b:W22
- **[F-C1-008]** MEDIUM `FIX-LOCAL` — SleepGuard acquire/Drop run on different Tokio worker threads — thread-scoped SetThreadExecutionState never cleared, machine never sleeps after a scan
    - file: `platforms/windows/src/engine/src/sleep_guard.rs`
    - sources: WF1b:W19
- **[F-C1-009]** MEDIUM `FIX-LOCAL` — Engine outbound IPC sink has no frame-size cap — a large restructurePlan exceeds the app's 32 MiB inbound cap and is silently dropped, hanging the Restructure UI
    - file: `platforms/windows/src/engine/src/ipc/sink.rs`
    - sources: WF1b:W18, WF3:scale-extremes
- **[F-C1-011]** MEDIUM `FIX-LOCAL` — Video keyframe extraction permanently MTA-initializes shared tokio blocking-pool threads — contaminates later STA-expecting shell ops (trash/tags/thumbnail)
    - file: `platforms/windows/src/engine/src/pipeline/video.rs`
    - sources: WF1b:W21a
- **[F-C1-012]** MEDIUM `FIX-LOCAL` — Bulk rename on-disk moves are not atomic with the single end-of-batch DB commit and have no recovery sidecar — a commit failure desyncs the whole batch
    - file: `platforms/windows/src/engine/src/commands/bulk.rs`
    - sources: WF1b:W20
- **[F-C1-013]** MEDIUM `FIX-LOCAL` — OneDrive online-only files dehydrated at scan time are stranded in the incremental skip-set after hydration — never embedded/OCR'd/face-scanned without a forced rescan
    - file: `platforms/windows/src/engine/src/scan_session.rs`
    - sources: WF1b:W10
- **[F-C1-017]** MEDIUM `FIX-LOCAL` — Parallel resume reports progress from 0, ignoring bytes already on disk — progress bar jumps backward on Retry; no 416 recovery
    - file: `platforms/windows/src/engine/src/download.rs`
    - sources: WF1b:W16a
- **[F-C1-019]** MEDIUM `FIX-LOCAL` — Year_ tag derived from modification time (UTC, >=1990) — diverges from macOS creation-time/local/>1990
    - file: `platforms/windows/src/engine/src/pipeline/tagging.rs`  [ruling]
    - sources: WF1b:W1b, WF2:P3-tagging
- **[F-C1-020]** MEDIUM `FIX-LOCAL` — skip_existing on the full (non-tags-only) Deep Analyze pass is not model-aware — switching VLM model + re-running skips every prior file
    - file: `platforms/windows/src/engine/src/pipeline/deep_analyze.rs`
    - sources: WF1b:W3, WF2:P4-deep-analyze
- **[F-C1-021]** MEDIUM `FIX-LOCAL` — Batch Deep Analyze permanently abandons the persistent llama server on the FIRST per-file error of any kind, not just on server death
    - file: `platforms/windows/src/engine/src/pipeline/deep_analyze.rs`
    - sources: WF1b:W4
- **[F-C1-024]** MEDIUM `FIX-LOCAL` — Windows install sentinel is id-keyed and never invalidates on a pin bump — stale artifact persists across a hash change (b4404->b9254 episode)
    - file: `platforms/windows/src/engine/src/model_install.rs`
    - sources: WF2:P9-model-supply
- **[F-C1-025]** MEDIUM `FIX-LOCAL` — Bulk rename: a video/over-cap-doc content hash re-opens a file the decoder already read, as blocking IO on a tokio worker (inside the writer path)
    - file: `platforms/windows/src/engine/src/pipeline/dbwriter.rs`
    - sources: WF1b:W2a, WF3:io-patterns, WF3:scale-extremes
- **[F-C1-010]** LOW `FIX-LOCAL` — Oversized inbound IPC frame error reports wrong cap ('1 MB') while the actual cap is 32 MiB
    - file: `platforms/windows/src/engine/src/main.rs`
    - sources: WF1b:W18
- **[F-C1-014]** LOW `FIX-LOCAL` — pptx slide-glob extraction has no member-count cap — a crafted .pptx can burn unbounded CPU on a decoder thread
    - file: `platforms/windows/src/engine/src/pipeline/office.rs`
    - sources: WF1b:W21b
- **[F-C1-015]** LOW `FIX-LOCAL` — Parent-watchdog PID-reuse TOCTOU between get_parent_pid() snapshot and OpenProcess — can watch an unrelated reused-PID process
    - file: `platforms/windows/src/engine/src/watchdog.rs`
    - sources: WF1b:W12
- **[F-C1-016]** LOW `FIX-LOCAL` — RAM++ batch coordinator thread-spawn panics (.expect) where every sibling spawn site degrades gracefully
    - file: `platforms/windows/src/engine/src/pipeline/tagging.rs`
    - sources: WF1b:W1a
- **[F-C1-018]** LOW `FIX-LOCAL` — Bulk trash: a trash_log append failure after commit leaves trashed files with no undo-journal entry — UndoStack restore is a silent no-op
    - file: `platforms/windows/src/engine/src/commands/bulk.rs`
    - sources: WF1b:W20
- **[F-C1-022]** LOW `FIX-LOCAL` — Deep Analyze targets ignore files.failed — re-attempts GPU-death-marked rows the macOS reference excludes
    - file: `platforms/windows/src/engine/src/pipeline/deep_analyze.rs`
    - sources: WF1b:W3, WF2:P4-deep-analyze
- **[F-C1-023]** LOW `FIX-LOCAL` — Name-based auto-merge guard built from the phase-1 snapshot, not re-read under the phase-3 lock — a rename in the lock-free window can unblock a wrong cluster auto-merge
    - file: `platforms/windows/src/engine/src/commands/face_clustering.rs`
    - sources: WF1b:W8a

## C2 — IPC schema cluster (schema-first: ipc.schema.json -> Rust -> C# -> Swift)  (8)

- **[F-C2-005]** HIGH `FIX-LOCAL` — v16 path_search NFC asymmetry: Windows stores verbatim bytes while its own app assumes NFC — NFD filenames unsearchable on Windows only
    - file: `platforms/windows/src/engine/src/db/migrations.rs`  [ruling]
    - sources: WF2:P5-db-schema
- **[F-C2-001]** MEDIUM `FIX-CI` — Swift IPC mirror drops deepAnalyzeAll.proposeRenames and hard-requires tagsOnly
    - file: `shared/ipc-schema/ipc.schema.json`
    - sources: WF2:P6-ipc, WF2:P4-deep-analyze, WF1:M3
- **[F-C2-002]** MEDIUM `FIX-CI` — Swift cancelPrewarm cannot carry modelKind — targeted prewarm cancel degrades to cancel-all on macOS
    - file: `shared/ipc-schema/ipc.schema.json`
    - sources: WF2:P6-ipc
- **[F-C2-003]** MEDIUM `FIX-CI` — Error-kind vocabulary drift in shared flows: face_cluster_failed (macOS) vs face_clustering_failed (Windows), plus unknown-model/scan-notice kinds
    - file: `shared/ipc-schema/ipc.schema.json`  [ruling]
    - sources: WF2:P6-ipc
- **[F-C2-004]** MEDIUM `FIX-LOCAL` — Windows engine path redaction leaks the username for files directly under a home directory
    - file: `platforms/windows/src/engine/src/platform.rs`
    - sources: WF2:P6-ipc
- **[F-C2-008]** MEDIUM `FIX-CI` — deepAnalyzeProgress.etaSeconds never populated by the Windows engine; caption frames omit currentPath
    - file: `shared/ipc-schema/ipc.schema.json`
    - sources: WF2:P4-deep-analyze
- **[F-C2-006]** LOW `FIX-CI` — discoveryComplete emission may diverge on cancel-during-discovery / sub-250ms scans
    - file: `platforms/windows/src/engine/src/scan_session.rs`
    - sources: WF2:P6-ipc
- **[F-C2-007]** LOW `HARDWARE` — IPC identifier-field casing drift (schema fileID/personIDs vs Rust/C# ...Id) — contract-conformance gap
    - file: `shared/ipc-schema/ipc.schema.json`  [ruling]
    - sources: WF2:P6-ipc

## C3 — macOS engine (Swift)  (45)

- **[F-C3-001]** CRITICAL `FIX-CI` — C1 gate trio: macOS DBWriter ungated DELETE wipes tags + face_prints(person_id) + ocr_text on a Vision/OCR/face timeout or zero-result rescan
    - file: `platforms/apple/engine/Sources/FileIDEngine/Storage/DBWriter.swift`  (port: platforms/windows/src/engine/src/pipeline/dbwriter.rs)
    - sources: WF1b:M1, WF2:P3-tagging
- **[F-C3-002]** HIGH `FIX-CI` — People edits committed during a clustering pass are silently destroyed by the stale Phase-0 snapshot persist (Windows S0 fix never mirrored to macOS)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`  (port: platforms/windows/src/engine/src/pipeline/face_clustering.rs)
    - sources: WF1:M5a, WF1:M5b, WF1:M6
- **[F-C3-003]** HIGH `FIX-CI` — macOS tightPairAutoMerge treats is_unknown persons as mergeable — destroys the user's 'don't identify' verdict
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`
    - sources: WF1:M5b, WF2:P7-faces-identity
- **[F-C3-004]** HIGH `FIX-CI` — macOS automerge ignores face_verifications 'different people' verdicts — a Windows-authored library gets user-refused merges force-applied
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`  (port: platforms/windows/src/engine/src/pipeline/face_clustering.rs)
    - sources: WF1:M5b, WF2:P7-faces-identity
- **[F-C3-005]** HIGH `FIX-CI` — Union-find transitive chaining in tightPairAutoMerge defeats the named-named guard and Pass-2 margin rule — bridge singletons merge distinct identities and can delete a user-assigned name
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`
    - sources: WF1:M5b
- **[F-C3-006]** HIGH `FIX-CI` — macOS HNSW level-draw uses entropy RNG — face clustering nondeterministic across runs (Windows E0 fixed-seed fix never mirrored)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/HNSWIndex.swift`  (port: platforms/windows/src/engine/src/pipeline/hnsw_index.rs)
    - sources: WF1:M5b, WF1:M6, WF1b:M14, WF2:P7-faces-identity
- **[F-C3-009]** HIGH `FIX-CI` — macOS restructure move leaves files.path_hash stale (Windows ENG-91 fix not ported)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/commands/restructure_apply.rs)
    - sources: WF1:M4, WF2:P2-restructure-apply
- **[F-C3-010]** HIGH `FIX-CI` — macOS restructure apply has no B4 stale-plan identity guard — proposal's oldPath is moved without re-reading the live files row
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/commands/restructure_apply.rs)
    - sources: WF1:M4, WF2:P2-restructure-apply
- **[F-C3-011]** HIGH `FIX-CI` — F-1: macOS restructure collision policy skips+reports vs Windows uniquify — converge on auto-rename 'name (2).ext' on both
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/commands/restructure_apply.rs)  [ruling]
    - sources: WF1:M4, WF2:P2-restructure-apply
- **[F-C3-013]** HIGH `FIX-CI` — macOS semantic new-group folder names are not sanitized (Windows #2 fix not ported)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/RestructureSemantic.swift`  (port: platforms/windows/src/engine/src/pipeline/restructure_semantic.rs)
    - sources: WF1:M4, WF2:P1-restructure-plan
- **[F-C3-018]** HIGH `FIX-CI` — Month folder names diverge: macOS '01-Jan'..'12-Dec' vs Windows 'January'..'December' — macOS converges
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/pipeline/restructure.rs)  [ruling]
    - sources: WF2:P1-restructure-plan
- **[F-C3-021]** HIGH `FIX-CI` — macOS engine Restructure butler is dead code: planRestructure/applyRestructure return not_implemented_yet and proposeAll has zero callers — wire it and route the app through it
    - file: `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift`  (port: platforms/windows/src/engine/src/commands/restructure.rs + restructure_apply.rs + pipeline/restructure*.rs)  [ruling]
    - sources: WF1:M4, WF2:P1-restructure-plan, WF2:P2-restructure-apply
- **[F-C3-023]** HIGH `FIX-CI` — ensureLoaded has no single-flight/reentrancy guard — overlapping prewarm + Deep Analyze double-load containers, mis-attribute a batch's vlm_model, double model RAM
    - file: `platforms/apple/engine/Sources/FileIDEngine/Models/.../ModelLoader.swift`
    - sources: WF1:M2
- **[F-C3-024]** HIGH `FIX-CI` — deepAnalyzeCancel cannot abort an in-run model download — single-lane JobQueue wedged for the full multi-GB fetch the user cancelled
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M2
- **[F-C3-028]** HIGH `FIX-CI` — deep_targets_failed exit path emits no terminal deepAnalyzeComplete — the only run() exit without one (UI stranded)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M3, WF2:P4-deep-analyze
- **[F-C3-029]** HIGH `FIX-CI` — deepAnalyzeComplete terminal event is NOT pinned — full-buffer coalescing can evict it, stranding the Deep Analyze UI forever
    - file: `platforms/apple/engine/Sources/FileIDEngine/IPC/IPCSink.swift`
    - sources: WF1b:M10, WF3:backpressure-e2e
- **[F-C3-030]** HIGH `FIX-CI` — macOS IPCSink full-buffer eviction removeFirst() ignores criticality and the critical set omits non-scan completions
    - file: `platforms/apple/engine/Sources/FileIDEngine/IPC/IPCSink.swift`
    - sources: WF3:backpressure-e2e, WF1b:M10
- **[F-C3-034]** HIGH `FIX-CI` — Tag-undo journal goes stale on an all-unchanged batch, then 'Undo last tags' strips tags from a different earlier batch; no age/identity guard
    - file: `platforms/apple/engine/Sources/FileIDEngine/.../TagUndo.swift`
    - sources: WF1b:M12
- **[F-C3-037]** HIGH `FIX-CI` — VLM download path (3.3-13.5 GB models) has no free-disk-space preflight, unlike the small-artifact installers
    - file: `platforms/apple/engine/Sources/FileIDEngine/Models/.../Downloader.swift`
    - sources: WF1b:M13a
- **[F-C3-038]** HIGH `FIX-CI` — macOS download redirects followed across scheme downgrade (https->http) and cross-host loses CA pinning — no https-only/host-allowlist (Windows E11 parity)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Models/.../Downloader.swift`  (port: platforms/windows/src/engine/src/download.rs)
    - sources: WF1b:M13b, WF2:P9-model-supply
- **[F-C3-007]** MEDIUM `FIX-CI` — macOS clustering iterates unordered Dictionaries / dropped sorted-root iteration — output nondeterministic across launches
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`  (port: platforms/windows/src/engine/src/pipeline/identity_clustering.rs)
    - sources: WF1:M6, WF2:P7-faces-identity
- **[F-C3-012]** MEDIUM `FIX-CI` — apply(): successful moveItem followed by failed DB UPDATE leaves disk/DB divergent, double-counts moved+failed, no recovery record
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/commands/restructure_apply.rs)
    - sources: WF1:M4, WF2:P2-restructure-apply
- **[F-C3-014]** MEDIUM `FIX-CI` — macOS semantic classify lacks used_group_names dedup + bounded numeric suffix (Windows #9 fix not ported) — distinct clusters with identical top tags silently merge into one folder
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/RestructureSemantic.swift`  (port: platforms/windows/src/engine/src/pipeline/restructure_semantic.rs)
    - sources: WF1:M4, WF2:P1-restructure-plan
- **[F-C3-015]** MEDIUM `FIX-CI` — macOS nearestTwoFolders / existing-folder routing not constrained to prototypes inside libraryRoot (Windows E12 guard not ported)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/RestructureSemantic.swift`  (port: platforms/windows/src/engine/src/pipeline/restructure_semantic.rs)
    - sources: WF1:M4, WF2:P1-restructure-plan
- **[F-C3-016]** MEDIUM `FIX-CI` — proposeAll has no Anchor/Mixed/Junk tiering or anchor-move strip (Windows A1/A3 guard absent from the engine plan)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/commands/restructure.rs)
    - sources: WF1:M4
- **[F-C3-017]** MEDIUM `FIX-CI` — Rule cascade routes videos/audio/other-kind into Photos/<Year>/<Month> — diverges from Windows Videos/<Year> and Audio/ buckets
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  (port: platforms/windows/src/engine/src/pipeline/restructure.rs)  [ruling]
    - sources: WF1:M4, WF2:P1-restructure-plan
- **[F-C3-019]** MEDIUM `FIX-CI` — Wire category label vocabulary differs: macOS 'Documents'/'Photos'/'Misc' vs Windows 'document'/'photo'/'misc'
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`  [ruling]
    - sources: WF2:P1-restructure-plan
- **[F-C3-020]** MEDIUM `FIX-CI` — Missing-timestamp year handling diverges: macOS omits the year folder, Windows coerces to 1970
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`
    - sources: WF2:P1-restructure-plan
- **[F-C3-022]** MEDIUM `FIX-CI` — parseFaceComparison inverts natural-language negatives: 'not the same person' parses as SAME at defaulted 0.80, above the 0.75 auto-merge threshold
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M2
- **[F-C3-025]** MEDIUM `FIX-CI` — clearCancel() at job start erases a cancel issued while the Deep Analyze job was queued — the cancelled run proceeds anyway
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M3, WF1:M2
- **[F-C3-026]** MEDIUM `FIX-CI` — Synchronous image decode on the DeepAnalyze actor executor stalls deepAnalyzeCancel (and every IPC command) while probing a file on an unreachable volume
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M2
- **[F-C3-027]** MEDIUM `FIX-CI` — Folder-scoped Deep Analyze uses unescaped LIKE — folders containing '_' or '%' over-match sibling paths outside the chosen folder
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`
    - sources: WF1:M3
- **[F-C3-031]** MEDIUM `FIX-CI` — Cancelled/aborted scans can never write final status: GRDB 7 task cancellation makes markSessionFinal's pool.write always throw — scan_sessions stuck 'running', mislabeled 'crashed' next startup
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/ScanCoordinator.swift`
    - sources: WF1:M7a, WF1b:M8
- **[F-C3-032]** MEDIUM `FIX-CI` — startScan rejected for db-unavailable emits an error but no scan-terminal event — app auto-pilot sticks on 'Scanning…' indefinitely
    - file: `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift`
    - sources: WF1b:M8
- **[F-C3-033]** MEDIUM `FIX-CI` — Pending-extraction window starves: permanently-failing rows occupy ORDER BY id ASC LIMIT 5000 forever, blocking newer faces from embedding; maxFacesPerRun overflow never picked up
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`
    - sources: WF1:M5b
- **[F-C3-035]** MEDIUM `FIX-CI` — namedPerson Mixed gate measures the WRONG person's homogeneity — flags most of a folder as outliers when a different person dominates
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`
    - sources: WF1b:M12
- **[F-C3-036]** MEDIUM `FIX-CI` — Vision-timeout image is persisted failed=false then permanently skipped on incremental rescans (never re-tagged)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift`
    - sources: WF1b:M1
- **[F-C3-044]** MEDIUM `FIX-CI` — 50MP decode cap absent from the macOS deep-analyze path; vlm_proposed_name/vlm_description NULL-clobber (vs Windows COALESCE)
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift`  (port: platforms/windows/src/engine/src/pipeline/deep_analyze.rs)
    - sources: WF2:P4-deep-analyze
- **[F-C3-045]** MEDIUM `FIX-CI` — Hardware.workerCap Intel fallback counts SMT logical threads as P-cores and lacks Windows' logical-core clamp — 1.5x oversubscription on every Intel Mac
    - file: `platforms/apple/engine/Sources/FileIDEngine/Hardware.swift`  (port: platforms/windows/src/engine/src/platform.rs)
    - sources: WF3:mac-memory-adaptation
- **[F-C3-008]** LOW `FIX-CI` — Pass-2 recomputes the full cluster centroid on every outlier assignment — quadratic work the Windows mirror replaced with O(dim) running sums
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/IdentityClustering.swift`  (port: platforms/windows/src/engine/src/pipeline/identity_clustering.rs)
    - sources: WF1:M6, WF2:P7-faces-identity
- **[F-C3-039]** LOW `FIX-CI` — macOS writer omits PRAGMA cache_spill=0 — dirty pages can spill to a temp file mid-transaction under memory pressure
    - file: `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift`  (port: platforms/windows/src/engine/src/db/mod.rs)
    - sources: WF1b:M7b, WF2:P5-db-schema
- **[F-C3-040]** LOW `FIX-CI` — IPCSink is never drained/closed before Darwin._exit(0) — a buffered terminal event can be lost on graceful shutdown
    - file: `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift`
    - sources: WF1b:M10
- **[F-C3-041]** LOW `FIX-CI` — PHASE 4 re-creates persons whose representative_face_id references faces cascade-deleted mid-pass by app-side Cleanup — re-introduces the dangle reconcilePersons repairs
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`
    - sources: WF1:M5a
- **[F-C3-042]** LOW `FIX-CI` — No cancellation path for face clustering: shutdown _exit(0)s a mid-flight cluster job, discarding the pass and delaying exit behind its persist transaction
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`
    - sources: WF1:M5a
- **[F-C3-043]** LOW `FIX-CI` — Latent: HF tree listing is non-recursive yet .fileid-verified sentinel is written as if all repo files were fetched — a pinned repo with any subfolder installs incomplete and wedges offline
    - file: `platforms/apple/engine/Sources/FileIDEngine/Models/.../HFDownloader.swift`
    - sources: WF1:M3

## C4 — macOS app (Swift)  (21)

- **[F-C4-001]** CRITICAL `FIX-CI` — 'Restart Engine' spawns a second engine without terminating the first — dual SQLite writers + orphaned errPipe handler corrupts lifecycle
    - file: `platforms/apple/app/Sources/FileID/EngineClient.swift`  (port: platforms/windows app EngineClient RestartAsync)
    - sources: WF1b:MA1
- **[F-C4-002]** HIGH `FIX-CI` — ReadStore.Dispose() / store teardown can free the live SqliteConnection out from under an in-flight thread-pool read (use-after-free crash)
    - file: `platforms/apple/app/Sources/FileID/ReadStore.swift`
    - sources: WF1b:WA6
- **[F-C4-003]** HIGH `FIX-CI` — Double-click on 'Convert to real moves' can permanently destroy a user file (no single-flight / button-disable on the irreversible apply path)
    - file: `platforms/apple/app/Sources/FileID/.../RestructureView.swift`
    - sources: WF1b:MA2a
- **[F-C4-004]** HIGH `FIX-CI` — Person merge/move/mark-unknown and Cleanup deleteFiles app-side writes have no busy timeout — collide with the engine writer under WAL, report false success or silently drop the edit
    - file: `platforms/apple/app/Sources/FileID/Database/People.swift`
    - sources: WF1b:MA4, WF1b:MA6, WF1b:MA5a, WF1b:MA7b, WF1:M7a
- **[F-C4-006]** HIGH `FIX-CI` — Semantic/similarity search runs the full clip_embeddings cosine scan synchronously on the MainActor — multi-second UI hang on a 50k library
    - file: `platforms/apple/app/Sources/FileID/.../SearchStore.swift`
    - sources: WF1b:MA5b
- **[F-C4-008]** HIGH `FIX-CI` — Bulk-tag 'Replace existing' permanently wipes user tags with no undo and no confirmation
    - file: `platforms/apple/app/Sources/FileID/.../BulkTagView.swift`
    - sources: WF1b:WA7b
- **[F-C4-009]** HIGH `FIX-CI` — TreeDiffView re-groups all proposals and eagerly materializes every row into AnyView per render — defeats LazyVStack virtualization, storms on hover
    - file: `platforms/apple/app/Sources/FileID/.../TreeDiffView.swift`
    - sources: WF1b:MA2b
- **[F-C4-005]** MEDIUM `FIX-CI` — Bulk-tag undo / total-failure bulk rename clobber a valid prior undo journal with an empty batch; raw-path undo with no identity guard strips an unrelated same-named file's tag
    - file: `platforms/apple/app/Sources/FileID/.../BulkOps.swift`
    - sources: WF1b:MA5a, WF1b:MA8
- **[F-C4-007]** MEDIUM `FIX-CI` — Keyword reload() runs the multi-table search query on the MainActor on every throttled batch event during a live scan
    - file: `platforms/apple/app/Sources/FileID/.../SearchStore.swift`
    - sources: WF1b:MA5b
- **[F-C4-012]** MEDIUM `FIX-CI` — Per-file deselections wiped (everything re-selected) on every regenerate, including a Deep Analyze finishing mid-review
    - file: `platforms/apple/app/Sources/FileID/.../RestructureView.swift`
    - sources: WF1b:MA2a
- **[F-C4-013]** MEDIUM `FIX-CI` — Drag-to-merge runs the full merge transaction synchronously on the main thread — freezes the UI on large libraries; ghost cluster after moving all photos
    - file: `platforms/apple/app/Sources/FileID/.../PeopleView.swift`
    - sources: WF1b:MA6
- **[F-C4-014]** MEDIUM `FIX-CI` — DeepAnalyze run-start staleness: deepAnalyzeLast not cleared (stale caption beside 'Starting…'); Cancel button non-functional during model download
    - file: `platforms/apple/app/Sources/FileID/.../DeepAnalyzeView.swift`
    - sources: WF1b:MA3
- **[F-C4-016]** MEDIUM `FIX-CI` — Engine crash mid-run strands DeepAnalyze in a frozen 'running' UI until the user tab-switches; persisted _activeModel not re-validated against the RAM gate
    - file: `platforms/apple/app/Sources/FileID/.../DeepAnalyzeView.swift`
    - sources: WF1b:WA4
- **[F-C4-018]** MEDIUM `FIX-CI` — Whole-library 'Select all non-keepers' persists by file id across reloads — a mid-scan keeper re-rank leaves the now-keeper selected and trashed on next Delete
    - file: `platforms/apple/app/Sources/FileID/.../CleanupView.swift`
    - sources: WF1b:MA7b
- **[F-C4-010]** LOW `FIX-CI` — Auto-pilot no-faces watchdog auto-runs deepAnalyzeAll on the whole library — contradicts the 'Deep Analyze waits for a named person' chaining rule
    - file: `platforms/apple/app/Sources/FileID/.../AutoPilot.swift`
    - sources: WF1b:MA1
- **[F-C4-011]** LOW `FIX-CI` — shutdown() latches expectedExit=true even when .shutdown was never sent — masks the next genuine crash
    - file: `platforms/apple/app/Sources/FileID/EngineClient.swift`
    - sources: WF1b:MA1
- **[F-C4-015]** LOW `FIX-CI` — modelDownloadProgress never reset to nil — Settings model picker shows a stale 'Downloading 100%' bar forever after any download
    - file: `platforms/apple/app/Sources/FileID/.../SettingsView.swift`
    - sources: WF1b:MA3
- **[F-C4-017]** LOW `FIX-CI` — CLIP Uninstall leaves the text encoder loaded — semantic search keeps running against the deleted model; 'Install all' permanently disabled after one click
    - file: `platforms/apple/app/Sources/FileID/.../SettingsView.swift`
    - sources: WF1b:MA7a
- **[F-C4-019]** LOW `FIX-CI` — Anchor ('staying put') source folder in the Tree-diff opens an empty 'Nothing to show' drill-down (display-name vs full-path key mismatch)
    - file: `platforms/apple/app/Sources/FileID/.../TreeDiffView.swift`
    - sources: WF1b:MA8
- **[F-C4-020]** LOW `FIX-CI` — hasActiveScan dead code / unordered discovery-progress Tasks can overwrite the final count with a stale lower value
    - file: `platforms/apple/app/Sources/FileID/.../ScanDispatcher.swift`
    - sources: WF1b:M9
- **[F-C4-021]** LOW `FIX-CI` — Person name-match uses bare substring containment — misclassifies common folders as person folders
    - file: `platforms/apple/app/Sources/FileID/.../Restructure*.swift`
    - sources: WF1b:M12

## C5 — Windows C# app  (12)

- **[F-C5-001]** HIGH `FIX-CI` — ReadStore.Dispose() can free the live SqliteConnection out from under an in-flight thread-pool read (use-after-dispose / native crash)
    - file: `platforms/windows/src/FileID.App/.../ReadStore.cs`
    - sources: WF1b:WA6
- **[F-C5-002]** HIGH `FIX-CI` — DrillDownSheet eagerly builds one UIElement + fires one shell thumbnail per move, uncapped — 'See all' on a large group freezes the UI and floods the shell
    - file: `platforms/windows/src/FileID.App/.../DrillDownSheet.xaml.cs`
    - sources: WF1b:WA3b
- **[F-C5-003]** HIGH `FIX-CI` — No double-apply prevention on Apply — re-clicking after a successful real-move apply produces a false 'Some changes couldn't be applied' alarm; stale plan re-rendered as pending
    - file: `platforms/windows/src/FileID.App/.../RestructureViewModel.cs`
    - sources: WF1b:WA3a
- **[F-C5-004]** HIGH `FIX-CI` — Bulk-tag 'Replace existing' permanently wipes user tags with no undo and no confirmation; grid shows stale filenames/tags after bulk rename/tag
    - file: `platforms/windows/src/FileID.App/.../BulkTagViewModel.cs`
    - sources: WF1b:WA7b
- **[F-C5-005]** MEDIUM `FIX-CI` — hardwareReprobed.execution_provider reports a fresh probe, not the engine's actually-bound EP — Settings claims 'CUDA active, no restart needed' while scans run DirectML
    - file: `platforms/windows/src/FileID.App/.../SettingsViewModel.cs`
    - sources: WF1b:W22, WF3:win-tier-transitions
- **[F-C5-006]** MEDIUM `FIX-CI` — PersonDetailSheet rename can write the name onto a different person (or no-op) after a background re-cluster — stale _personId, no generation guard
    - file: `platforms/windows/src/FileID.App/.../PersonDetailSheet.xaml.cs`
    - sources: WF1b:WA8a
- **[F-C5-007]** MEDIUM `FIX-CI` — DebugLog truncation check + truncating rewrite run OUTSIDE s_writeLock — concurrent log writes silently dropped once app.log exceeds 10 MB
    - file: `platforms/windows/src/FileID.App/.../DebugLog.cs`
    - sources: WF1b:WA10
- **[F-C5-008]** MEDIUM `FIX-CI` — 'Auto-install CUDA acceleration' toggle is a dead control — DisableAutoInstallCuda is read nowhere
    - file: `platforms/windows/src/FileID.App/.../SettingsViewModel.cs`
    - sources: WF1b:WA11
- **[F-C5-009]** MEDIUM `FIX-CI` — Pipeline progress strip blanks to grey on every app launch of an existing library (no DB-derived stage; macOS reference parity break)
    - file: `platforms/windows/src/FileID.App/.../PipelineProgressView.xaml.cs`
    - sources: WF1b:WA9b
- **[F-C5-010]** MEDIUM `FIX-CI` — Sankey Render() makes ~4 full O(Moves) passes with a String.Split+Substring per move on the UI thread on every plan arrival
    - file: `platforms/windows/src/FileID.App/.../SankeyView.xaml.cs`
    - sources: WF1b:WA3b
- **[F-C5-011]** MEDIUM `FIX-CI` — Multi-select selection silently dropped when a refresh replaces a selected cluster's instance; Cleanup header text claims perceptual-hash grouping
    - file: `platforms/windows/src/FileID.App/.../PeopleView.xaml.cs`
    - sources: WF1b:WA8b
- **[F-C5-012]** LOW `FIX-CI` — Selection toolbar goes stale when a background refresh prunes selected tiles (VM emits SelectedCount; View never re-reads it)
    - file: `platforms/windows/src/FileID.App/.../LibraryView.xaml.cs`
    - sources: WF1b:WA7a

## C6 — Perf batch (cross-cutting)  (18)

- **[F-C6-001]** HIGH `FIX-CI` — macOS has no discovery-time incremental skip set — every rescan re-runs the full ANE/Vision/CLIP pipeline + NAS IO on unchanged files
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Discovery.swift`  (port: platforms/windows/src/engine/src/scan_session.rs)
    - sources: WF3:backpressure-e2e, WF3:io-patterns
- **[F-C6-002]** HIGH `FIX-CI` — macOS semantic search materializes every CLIP embedding via fetchAll — ~1 GB transient per query at 500k files
    - file: `platforms/apple/app/Sources/FileID/ReadStore.swift`
    - sources: WF3:scale-extremes
- **[F-C6-003]** HIGH `FIX-CI` — macOS ReadStore.refreshCounters runs an O(N log N) duplicate-group window query synchronously on the main thread up to once per second during a scan
    - file: `platforms/apple/app/Sources/FileID/ReadStore.swift`
    - sources: WF3:scale-extremes
- **[F-C6-004]** MEDIUM `FIX-CI` — macOS DBWriter commits inline on the rendezvous channel — every DB transaction stalls all tagging workers; WAL 10000-page autocheckpoint does one ~40 MB sync copy mid-commit
    - file: `platforms/apple/engine/Sources/FileIDEngine/Storage/DBWriter.swift`
    - sources: WF3:backpressure-e2e, WF3:io-patterns
- **[F-C6-005]** MEDIUM `FIX-CI` — macOS Discovery materializes and sorts the full corpus before any tagging starts — O(N) memory held for the whole scan + dead-air phase at 500k
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Discovery.swift`
    - sources: WF3:scale-extremes
- **[F-C6-006]** MEDIUM `HARDWARE` — macOS CLIP preprocessing allocates ~1.4MB and redundantly memsets/copies ~800KB per image, partly inside the ANE semaphore
    - file: `platforms/apple/engine/Sources/FileIDEngine/Models/.../MobileCLIPService.swift`
    - sources: WF3:hot-loops-macos
- **[F-C6-007]** MEDIUM `FIX-CI` — runVisionWithTimeout executes Vision on a global queue with NO autoreleasepool — intermediates accumulate on never-draining root-queue threads
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/VisionWorker.swift`
    - sources: WF3:hot-loops-macos
- **[F-C6-008]** MEDIUM `FIX-CI` — loadImageAndEXIF re-stats every image via attributesOfItem when DiscoveredFile.sizeBytes already holds the answer — an extra NAS round-trip per file
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift`
    - sources: WF3:hot-loops-macos, WF3:io-patterns
- **[F-C6-009]** MEDIUM `HARDWARE` — PDF pages render into RGBA at up to 200MB/page for OCR that needs no color, under a single whole-document autoreleasepool
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift`
    - sources: WF3:hot-loops-macos
- **[F-C6-012]** MEDIUM `FIX-CI` — macOS Restructure compute pages the full files table with LIMIT/OFFSET over an unindexed ORDER BY — O(N^2/page) full re-sort per regenerate
    - file: `platforms/apple/app/Sources/FileID/.../RestructureEngine.swift`
    - sources: WF1b:MA2b, WF3:scale-extremes
- **[F-C6-013]** MEDIUM `FIX-CI` — Restructure apply is an uncancellable, progress-less serial loop with a per-move DB transaction — at 100k+ moves the user gets no feedback and no stop
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift`
    - sources: WF3:backpressure-e2e
- **[F-C6-016]** MEDIUM `FIX-LOCAL` — Windows planRestructure loads the entire clip_embeddings table into a HashMap with no memory-tier gating
    - file: `platforms/windows/src/engine/src/commands/restructure.rs`
    - sources: WF3:scale-extremes
- **[F-C6-017]** MEDIUM `FIX-LOCAL` — Windows discovery serializes per-file metadata + file_ref syscalls on the single consumer thread — the 'parallel walk' only parallelizes read_dir
    - file: `platforms/windows/src/engine/src/scan_session.rs`
    - sources: WF3:scale-extremes, WF1b:W10
- **[F-C6-018]** MEDIUM `HARDWARE` — Windows predecode byte budget acquired AFTER decode — up to decoder_count × 150 MB of decoded frames live outside the 256 MB budget when it saturates
    - file: `platforms/windows/src/engine/src/pipeline/tagging.rs`
    - sources: WF3:semaphore-rationale
- **[F-C6-010]** LOW `FIX-CI` — macOS embedding output copy chain is 3 copies + a scalar loop per 512-d vector; insertOne re-prepares every statement and pays a per-file SELECT
    - file: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift`
    - sources: WF3:hot-loops-macos, WF3:io-patterns
- **[F-C6-011]** LOW `FIX-CI` — macOS app inbound IPC frame cap is 1 MiB — raise to 32 MiB with a visible oversize error (the exact Windows bug shape)
    - file: `platforms/apple/app/Sources/FileID/EngineClient.swift`  (port: platforms/windows app EngineClient.cs)
    - sources: WF3:scale-extremes
- **[F-C6-014]** LOW `FIX-CI` — Per-second store.notifyChanged() during a live scan re-fires every visible tile's .task — re-reads Finder xattrs for unchanged files
    - file: `platforms/apple/app/Sources/FileID/.../LibraryView.swift`
    - sources: WF1b:MA5b
- **[F-C6-015]** LOW `FIX-LOCAL` — Windows IPC sink flushes stdout per event — 2 syscalls per frame during the bursts the 16384 cap was sized for
    - file: `platforms/windows/src/engine/src/ipc/sink.rs`
    - sources: WF3:backpressure-e2e

## C7 — Roadmap features (F-2, F-4)  (2)

- **[F-C7-001]** HIGH `FIX-CI` — F-2: macOS rename-heal — renamed/moved files lose tags/faces on rescan (no file_ref/content_hash heal on macOS); Windows heal already fixed, BUGS.md stale
    - file: `platforms/apple/engine/Sources/FileIDEngine/Storage/DBWriter.swift`  (port: platforms/windows/src/engine/src/pipeline/dbwriter.rs heal_candidate_moved)
    - sources: CAMPAIGN-NOTES:F-2
- **[F-C7-002]** MEDIUM `HARDWARE` — F-4: face-clustering pass-1 mutual-kNN structure — single-linkage over-chains; adopt mutual-kNN (thresholds stay provisional, recipe -> NEXT.md)
    - file: `platforms/windows/src/engine/src/pipeline/identity_clustering.rs`
    - sources: CAMPAIGN-NOTES:F-4

## REJECTED (15)

Not carried as fixable. Each cites the deliberate guard or ruled-divergence rationale. Where a finding bundles a real bug with a rejected framing, the real bug is split out (cross-referenced).

- **[R-001]** LOW — Windows hard-codes cancelled:true on non-cancel Deep-Analyze error arms
    - claim: WF-2 P4 flagged Windows reporting cancelled:true on llama_cpp_missing / query-failure arms as a parity bug.
    - citation: DECISIONS.md:117 (2026-06-04) — deliberately LEFT the llama_cpp_missing arm at cancelled:true; it pairs with a specific actionable toast and suppresses a redundant generic warning. 'Not an oversight.' The folder/all query-failure arms are the same #6 convention.
    - sources: WF2:P4-deep-analyze
- **[R-002]** MEDIUM — C7: CLIP-installed-later backfill exists on macOS but not Windows
    - claim: WF-2 P3 C7 claimed a Windows scan that completed without embeddings then gains them on macOS but not Windows.
    - citation: Refuted (WF-2 adjudication): the precondition is unreachable on Windows — scan pre-flight (scan.rs:59-105) hard-requires the mobileclip_s2 sentinel and aborts with models_not_installed if CLIP is missing; the post-load guard aborts if installed CLIP fails to load. A completed Windows scan never produced failed=0 rows without embeddings, so the divergence cannot manifest.
    - sources: WF2:P3-tagging
- **[R-003]** LOW — Both engines stamp OpenCLIP ViT-B/32 embeddings with the legacy 'mobileclip_s2' identifier
    - claim: WF-2 P3 C9 flagged the mobileclip_s2 model identifier on OpenCLIP ViT-B/32 embeddings as a hazard.
    - citation: Refuted (WF-2 adjudication): a deliberate, identical choice on BOTH engines (DBWriter.swift:543-548 / dbwriter.rs:360-364); no migration wipes clip_embeddings; the residual concern is already owned by a verified prior finding. WS-MAC model-identity divergence is explicitly REJECT-listed in the guard registry (model identity only).
    - sources: WF2:P3-tagging
- **[R-004]** LOW — macOS DBWriter batch ceilings (100/200ms) are not memory-adaptive / tier-polled like Windows
    - claim: WF-3 raised adopting Windows' tier-polled 64/250/500 DBWriter batch sizing on macOS as a perf win across several checks.
    - citation: DBWriter 100-file/200ms batch bound is in the deliberate-guard registry. Refuted on materiality (WF-3 adjudications x4): the 200ms time ceiling binds first (~7 files/60ms measured, DBWriter.swift:112-116), so the 100-count cap only fires above ~500 files/s — ~3.5x the >=140 files/s target. The High-tier amortization win is arithmetically zero. Batch size as a *commit-overhead* knob (not memory) is folded into F-C6-004; the memory-tier batch knob itself is part of the F-3 design (see HARDWARE).
    - sources: WF3:scale-extremes, WF3:hot-loops-macos, WF3:semaphore-rationale, WF3:win-tier-transitions
- **[R-005]** HIGH — macOS RestructureEngine.compute does ~100 full-table sorts of 500k rows (no scanned_at index)
    - claim: WF-3 scale-extremes claimed regenerate() forces ~100 full sorts of 500k rows because there is no standalone scanned_at index.
    - citation: Refuted (WF-3 adjudication x2): the load-bearing premise is false — idx_files_scanned ON files(scanned_at) IS created at Database.swift:106-107 via GRDB's db.create(index:) builder (the candidate's raw-SQL grep missed it). SQLite walks the index in reverse for ORDER BY scanned_at DESC. The residual OFFSET-paging cost (immaterial at scale, and retired by the engine-butler port) is captured separately as F-C6-012.
    - sources: WF3:scale-extremes, WF1b:MA2b
- **[R-006]** LOW — macOS engine Restructure.apply / proposeAll is dead code (should be deleted as unused)
    - claim: WF-2 P2 flagged the engine-side Restructure.apply as dead code (live apply is app-side).
    - citation: DECISIONS.md:61-64 (L1, 2026-06-10) — engine-side Restructure.apply is 'dead code by design'; it is the future vehicle for moving apply into the engine for full Windows parity; 'do not delete it as unused.' Per user ruling 2, it is being WIRED (F-C3-021), not deleted — so the 'delete dead code' framing is rejected, the port is the C3 work.
    - sources: WF2:P2-restructure-apply, WF2:P1-restructure-plan
- **[R-007]** LOW — Stale 'ipc_unknown_command' kind in the Rust EngineError doc — neither engine emits it
    - claim: WF-2 P6 flagged that ipc_unknown_command appears in a doc comment but the engines emit command_decode_failed.
    - citation: Exact-match failure-kind strings are in the deliberate-guard registry; the emitted kind (command_decode_failed) is correct and identical on both engines. This is a stale doc-comment only with zero runtime effect; not carried as a fixable behavioral finding (a doc tidy may ride along with C2 but is not a triaged finding).
    - sources: WF2:P6-ipc
- **[R-008]** MEDIUM — macOS auto-downloads an uninstalled VLM mid-run; Windows refuses with vlm_model_missing
    - claim: WF-2 P4 flagged that macOS auto-downloads multi-GB VLM weights inside a Deep Analyze run while Windows refuses.
    - citation: WS-MAC model-stack identity divergence / model-supply behavior is a known-open, explicitly RULED divergence (DECISIONS.md 2026-05-13/05-17 not_implemented_yet + macOS-lockstep-lag rulings; WF-2 adjudication cites it as 'documented, explicitly ruled expected divergence'). The SECURITY half of the macOS download path (redirects/pinning/SHA256) is NOT rejected — it is F-C3-038. The mid-run auto-download UX choice itself is the ruled divergence.
    - sources: WF2:P4-deep-analyze
- **[R-009]** LOW — DeepAnalyzeStarting 'queued' phase is macOS-only; Windows never emits it
    - claim: WF-2 P4 flagged the missing 'queued' phase on Windows as a parity gap.
    - citation: Refuted (WF-2 adjudication): no consumer makes it observable — the Windows app acknowledges the click locally/optimistically before any engine event (DeepAnalyzeView.xaml.cs:731-739), so the missing queued phase has zero user-visible impact. Wire-shape symmetry is already locked by round-trip tests.
    - sources: WF2:P4-deep-analyze
- **[R-010]** MEDIUM — tagsOnly/proposeRenames are wire-accepted but semantically inert on macOS (mode plumbing)
    - claim: WF-2 P4 flagged that macOS accepts tagsOnly/proposeRenames on the wire but does nothing with them.
    - citation: The SEMANTIC inertness of the Deep Analyze MODE plumbing is the explicitly ruled macOS-lockstep lag (DECISIONS.md 2026-05-13/05-17; WF-2 adjudication: 'documented, explicitly ruled expected divergence'). NOTE: the DECODE bug (Swift hard-requires tagsOnly, omits proposeRenames) is a real contract gap and is NOT rejected — it is F-C2-001. Only the 'mode does nothing yet' behavior is the ruled item.
    - sources: WF2:P4-deep-analyze, WF2:P6-ipc
- **[R-011]** LOW — Windows tag dedup/cap (case-insensitive first-wins + 16-cap) vs macOS exact/last-wins/unbounded
    - claim: WF-2 P3 C2 flagged divergent tag dedup/cap semantics.
    - citation: Refuted (WF-2 adjudication): impact is unreachable — every macOS visionTag producer emits at most ~10 inherently-unique identifiers (prefix(8) VNClassify + Year_ + one camera family), with no possible case-variant or exact duplicate, so the Windows dedup/cap has nothing to collapse. No user-visible divergence.
    - sources: WF2:P3-tagging
- **[R-012]** MEDIUM — Windows persists source='vlm' tag rows from Deep Analyze; macOS persists none
    - claim: WF-2 P3 C8 flagged that Windows writes source='vlm' tag rows where macOS writes none.
    - citation: Refuted (WF-2 adjudication): this is the same ruled macOS Deep-Analyze mode-plumbing lag (no tag write-out on macOS yet). The load-bearing impact assertion is owned by the ruled D-2 VLM-runtime divergence; not a new fixable finding. (If macOS later wires VLM tag write-out, it rides the butler/Deep-Analyze parity work, not this finding.)
    - sources: WF2:P3-tagging
- **[R-013]** LOW — Cross-platform IPC schema asymmetric: macOS engine returns not_implemented_yet for restructure/plan
    - claim: WF-2 flagged the macOS engine returning not_implemented_yet for planRestructure/applyRestructure as a parity bug.
    - citation: DECISIONS.md 2026-05-13 (option 2 on record) + 2026-05-17 (restructurePlan event family is a future macOS task, not a Windows blocker) — the wire is deliberately symmetric with macOS stubs, round-trip tests lock the shape. Per user ruling 2 the stubs are being WIRED (F-C3-021); the 'asymmetry is a bug' framing is rejected, the port is the C3 work.
    - sources: WF2:P1-restructure-plan, WF2:P2-restructure-apply
- **[R-014]** MEDIUM — IPC event backpressure model differs between platforms (macOS coalesce vs Windows blocking channel)
    - claim: WF-3 / WF-2 noted the two engines handle event backpressure differently.
    - citation: DECISIONS.md:10-23 (2026-06-10) — the asymmetry is by design for v1.0 and both designs preserve terminal events; recorded explicitly 'so the asymmetry isn't re-flagged as a parity bug.' NOTE: the specific droppable-terminal-event bugs (try_send on terminal PhaseChanged; IPCSink not pinning non-scan terminals) are REAL and NOT rejected — they are F-C1-002 and F-C3-029/030.
    - sources: WF3:backpressure-e2e
- **[R-015]** LOW — macOS Face HNSW build holds three full copies of all face embeddings
    - claim: WF-3 scale-extremes flagged 3 coexisting copies of face embeddings during HNSW build.
    - citation: Refuted on materiality (WF-3 adjudication): the candidate self-classifies as bounded low-severity waste with no OOM cliff (SFace 128-d, ~51 MB/copy at 100k). Arithmetic confirmed but immaterial at realistic scale; the actionable landHere core is refuted. Not carried.
    - sources: WF3:scale-extremes

## Unpinned invariants (tests to add) (7)

WF-2 MATCH invariants with `pinnedByTest == null` — the platforms agree today but no regression test locks it. Add a pinning test so a future edit can't silently fork.

- **[P7-faces-identity]** Excluded-face flag population and respect
    - Constants and respect sites are identical; the quality-score scale difference (Vision faceCaptureQuality vs YuNet score) is D-1 expected divergence, acknowledged in the Windows comment. Unpinned on both sides — reported, not silently passed.
- **[P8-tag-writeout]** Failure isolation per file
    - Both sides isolate per file and surface per-file results, but no test on either side exercises a failing file mid-batch (macOS TagWriterBatchTests asserts failed==0 on an all-success batch; Windows commands/bulk.rs has no #[cfg(test)] module). One reporting nuance recorded as candidate 7: Windows counts a file succeeded once the DB row commits even if its disk write-out fails (warn-only), whereas the macOS disk write failure increments failed.
- **[P5-db-schema]** Unix-epoch timestamps everywhere incl. persons.*
    - No test on either side pins the epoch convention itself (DBWriterUpsertTests.swift:15,26,79 uses epoch fixtures only incidentally). Unpinned — reported, not silently passed.
- **[P5-db-schema]** PRAGMA set parity (WAL, synchronous, mmap, cache_size, wal_autocheckpoint)
    - All five invariant-scoped pragmas (plus temp_store) match exactly. The Windows-only cache_spill=0 is recorded as a low candidate; no test on either side pins the pragma set.
- **[P4-deep-analyze]** sentinel-gated offline model load (no network when installed) both sides
    - The strict invariant (no network when installed) holds on both sides. The NOT-installed behavior diverges (macOS auto-downloads mid-run, Windows errors) — recorded as a medium candidate.
- **[P6-ipc]** Unknown-command rejection error code identical (ipc_unknown_command)
    - The two engines emit the IDENTICAL code, but it is command_decode_failed, not ipc_unknown_command as the brief states. The string ipc_unknown_command exists only in a stale Rust doc comment (ipc/mod.rs:765) and nowhere in the schema or either engine's emit paths — filed as a low candidate. No test on either platform asserts the rejection kind string.
- **[P5-db-schema]** 16-identifier migration parity list
    - Pinned by MigrationParityTests.swift + migrations.rs (already pinned — kept here only as a reminder that the list is the cross-platform fork guard).

## HARDWARE-deferred (-> NEXT.md) (12)

ML threshold values, GPU/EP runtime behavior, WinUI runtime/visual, or anything needing the RTX 2060 / a Mac. Each carries a measurement recipe; these are NEXT.md specs, not code fixes in this campaign.

### H-F3 — F-3 — macOS runtime memory-adaptation design (MemoryPressureMonitor in the engine)
- recipe: Land the STRUCTURE in code (CI-testable pure tier-transition function with injected memory values + injectable DispatchSource.makeMemoryPressureSource; worker admission parks indexed workers at file boundaries on tier drop, in-flight files always complete; tier-clamp maxPDFRenderPixels; tier-polled DBWriter batch at batch boundaries; asymmetric fast-down/slow-up hysteresis with dwell because availableMemoryMB overstates usable RAM). Calibrate the TIER THRESHOLDS and dwell on hardware: on a Mac, run a 50k+ NAS scan while inducing memory pressure (memory_pressure -S -l warn/critical or a ballast allocator), record tier transitions vs residentMB/availableMB, and tune so the box never swaps and never parks workers spuriously. Acceptance: >=140 files/s sustained at Balanced; no swap at Critical; tier flaps < N/min.
- sources: WF3:mac-memory-adaptation, WF3:hot-loops-macos, WF3:semaphore-rationale, WF3:backpressure-e2e, WF3:io-patterns, WF3:scale-extremes, CAMPAIGN-NOTES:F-3

### H-C6-006 — CLIP preprocess allocation reduction (output-identical)
- recipe: On a Mac, A/B the current vs reduced-copy CLIP preprocess over a fixed 5k-image corpus: assert byte-identical embeddings (hash the blobs) and measure files/s + peak RSS. Land only if embeddings are bit-identical and throughput improves. (CLAUDE.md: tune ML against real data.)
- sources: WF3:hot-loops-macos

### H-C6-009 — Grayscale PDF render + per-page autoreleasepool for OCR
- recipe: On a Mac, render a representative PDF set both RGBA (current) and grayscale, run OCR on both, and diff the recognized text + has_text classification. Land only if OCR accuracy is unchanged; measure peak memory reduction at high page counts.
- sources: WF3:hot-loops-macos

### H-C6-018 — Windows predecode byte budget acquired before decode
- recipe: On the RTX 2060 against the G:\TrueNAS corpus, change the predecode budget acquisition to before decode (with a size estimate), then measure peak decoded-frame memory + throughput vs baseline. Acceptance: peak bounded by the 256 MB budget with no throughput regression; verify via iterate.ps1 + scan_assertions.py.
- sources: WF3:semaphore-rationale

### H-C2-007 — IPC identifier casing rename (...Id -> ...ID) — Windows app<->engine round-trip
- recipe: Requires a Windows box: perform the coordinated #[serde(rename)] (Rust) + [JsonPropertyName] (C#) rename to the schema's capitalized identifier suffixes, then run a real Windows app<->engine session exercising every command/event carrying an id field. Acceptance: no decode failures across the live wire (the only validation neither side's same-language unit tests cover).
- sources: WF2:P6-ipc

### H-F4-thresholds — F-4 — face-clustering mutual-kNN k + cosine thresholds
- recipe: After landing the mutual-kNN graph structure (F-C7-002), calibrate k and the edge cosine against a labeled face set on hardware: sweep k and threshold, measure over-merge (distinct identities chained) vs over-split (one identity fragmented) against ground truth, pick the knee. Thresholds stay provisional until measured (DECISIONS.md butler/clustering precedent).
- sources: CAMPAIGN-NOTES:F-4

### H-DirectML — Windows DirectML throughput ceiling (known-open)
- recipe: Known-open per the guard registry; not a new finding. On the RTX 2060, characterize the DirectML EP throughput ceiling vs CUDA on the standard corpus. NEXT.md tracking item only.
- sources: KNOWN-OPEN

### H-VRAM-clamp — VRAM clamp budgets against TOTAL DedicatedVideoMemory, not free/budget (landHere=false)
- recipe: On a box where another app holds VRAM, confirm the pool over-admits and recreates the DirectML wedge; then budget against current free/budget (DXGI QueryVideoMemoryInfo) instead of total. Hardware-only because the wedge only manifests under real VRAM contention. Pairs with the scan-pool vs llama-server mutual-exclusion design (see F-C1-006 / NEXT).
- sources: WF3:ep-cuda-readiness

### H-EP-CUDA-activate — Mid-process CUDA pack install never activates CUDA (ORT_DYLIB_PATH startup-only)
- recipe: On the RTX 2060: install the CUDA pack mid-session, confirm the chain+UI advertise CUDA but scans still bind DirectML (ORT_DYLIB_PATH is read only at startup), and that the bind can crash-poison. Decide restart-required UX vs a safe re-init path; verify the bound EP via the (now-fixed, F-C5-005) reprobe. Hardware-only (GPU EP runtime behavior).
- sources: WF3:ep-cuda-readiness

### H-VLM-server-mutex — Scan ORT pool vs Deep-Analyze llama-server VRAM mutual exclusion
- recipe: On the RTX 2060 / small card: run a scan (ORT pool VRAM-resident) concurrently with Deep Analyze (-ngl 99) and confirm over-commit; design+verify a shared VRAM budget or mutual-exclusion gate so the two never co-reside on a saturated card. Hardware-only (VRAM pressure).
- sources: WF1b:W12, WF3:ep-cuda-readiness

### H-ANE-permits — macOS ANE inference permits (CLIP=4, SFace=4) drifted from recorded decision (=2)
- recipe: On a Mac (post ORT-CoreML-EP swap): sweep ANE permit width (2 vs 4) over a fixed corpus, measure files/s and thermal/throttle behavior, and re-record the decision. landHere=false — runtime-measured ANE behavior.
- sources: WF3:semaphore-rationale

### H-WIN-TIER — Windows mid-scan tier transitions only reach DBWriter batch (real knobs frozen at start)
- recipe: On the RTX 2060: confirm pool/semaphore/predecode are frozen at scan start while only the DBWriter batch target re-polls; route the real memory knobs through the (hysteresis-guarded) 30s poll and measure that a mid-scan pressure event actually reduces resident set without wedging the GPU. Gated so the 6 GB reference box stays byte-identical (per the 2026-06-04 perf-gating decision). Hardware-only.
- sources: WF3:win-tier-transitions

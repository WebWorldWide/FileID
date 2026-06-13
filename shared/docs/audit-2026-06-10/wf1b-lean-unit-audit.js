export const meta = {
  name: 'fileid-unit-audit-lean',
  description: 'WF-1b: review-only deep audit of the 66 units WF-1 never reached (rate-limit-safe, no verify fan-out)',
  phases: [{ title: 'Review', detail: 'one reviewer per remaining unit+lens assignment' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const AE = 'platforms/apple/engine/Sources/FileIDEngine/'
const AS = 'platforms/apple/shared/Sources/FileIDShared/'
const AA = 'platforms/apple/app/Sources/FileID/'
const WE = 'platforms/windows/src/engine/src/'
const WA = 'platforms/windows/src/FileID.App/'

const COMMON = `
REPO ROOT: ${ROOT}. All file paths below are relative to it. If a listed file does not exist at its path, find it by basename with Glob under the same platform subtree before concluding it is missing; list truly-missing files in missingFiles and audit what you found instead.

CONTEXT: FileID is an on-device AI file organizer at production-readiness. A prior campaign confirmed and fixed 73 findings, so obvious bugs are gone and the historical false-positive rate of new claims is 40-50%. Report only what you can defend with code-level evidence (file:line, mechanism, concrete trigger scenario, user impact). No style nits, no 'could be' speculation without a reachable trigger path. Before claiming a design choice is a bug, grep shared/docs/DECISIONS.md and the prior audit records under shared/docs/audit-*/ — settled choices have written rationale.

DELIBERATE-GUARD REGISTRY — these exist on purpose; do NOT flag them or their consequences:
- tags_evaluated gate in tagging (prevents GPU-death tag wipes)
- id-preserving UPSERT + v15 FTS sync triggers
- 'sender != _process' stale-exit guards in EngineClient.cs
- face_cluster_active single-flight guard
- exact-match failure-kind string comparisons (the Contains() version was the bug)
- strip_extended_length (NON-canonicalizing) junction/containment walk in path_safety.rs — canonicalize would FOLLOW the junction; current form IS the SEC-5 fix
- synchronous DebugLog writes (durability is load-bearing; async version reverted on record)
- labeled break in the engine command loop (shutdown fix, commit d1cf0b9)
- NFC normalization in v16_path_search; StablePathHash SipHash-1-3
- monotonic ModelSlot progress Fraction
- armed-EP-set breadcrumb in ep_guard (over-disabling is intentional crash safety) — EXCEPTION: a breadcrumb never cleared on GRACEFUL shutdown (poisoning a healthy EP next launch) IS a real bug, report it
- anchor-folder 'Keep' moves dropped engine-side with preserved Keep count
- cache-only WinVerifyTrust revocation (no network egress; DECISIONS 2026-06-09)
- macOS engine IPC rides fd 2 (stderr) with real stderr redirected to a log — intentional wire design
- DBWriter 100-file/200ms batch bound (it IS the backpressure bound)
- db_newer_than_engine downgrade refusal; suffix-room-reserved sanitize dedup loops
- any code near a B3 / B4 / SEC-3 / SEC-5 / SafeRun / [APPLY:N] marker is a deliberate guard

KNOWN-OPEN ISSUES — already documented, do NOT re-report (you MAY report distinct interactions):
- rename-heal collapses coexisting byte-identical files (Windows half already fixed via heal_candidate_moved old-path-gone gate; macOS has no heal at all — that gap IS reportable)
- face clustering pass-1 single-linkage over-chains on large libraries
- Windows DirectML throughput 6-7 files/s vs 140 target (CUDA pack pending)
- macOS WS-MAC model-stack lockstep in progress (RAM++/ViT-B/32/SFace mirror) — model identity divergence is expected; LOGIC/persistence bugs are not
- deferred: VLM server-death CLI fallback; CLIP tokenizer punctuation A/B; long-path trash manifest; single-file DeepAnalyze waiter ambiguity; C# unknown-enum event drops

Read every listed file COMPLETELY (use offset/limit for files over 2000 lines — read all of them in chunks). Trace actual data/control flow; cross-reference callers with Grep when a claim depends on usage. Severity: critical = data loss / security breach / crash-on-common-path; high = wrong results or hang on realistic path; medium = degraded correctness/UX on edge path; low = minor but real defect. If the unit is genuinely clean under your lens, return findings: [] and verifiedClean: true.`

const FINDINGS = {
  type: 'object',
  required: ['unit', 'findings'],
  properties: {
    unit: { type: 'string' },
    verifiedClean: { type: 'boolean' },
    missingFiles: { type: 'array', items: { type: 'string' } },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        required: ['title', 'file', 'severity', 'category', 'claim', 'evidence', 'trigger'],
        properties: {
          title: { type: 'string' },
          file: { type: 'string' },
          line: { type: 'integer' },
          severity: { enum: ['critical', 'high', 'medium', 'low'] },
          category: { enum: ['logic', 'concurrency', 'data-loss', 'security', 'resource', 'silent-failure', 'ipc', 'ui-state', 'perf', 'other'] },
          claim: { type: 'string' },
          evidence: { type: 'string' },
          trigger: { type: 'string' },
          suggested_fix: { type: 'string' },
        },
      },
    },
  },
}

// The 66 units WF-1 never reached (7 done: M2,M3,M4,M5a,M5b,M6,M7a).
const UNITS = [
  { id: 'M1', files: [AE+'Pipeline/Tagging.swift', AE+'Models/MobileCLIPService.swift'], lens: 'logic correctness + concurrency', focus: 'Per-file tagging pipeline: Vision request reuse safety, ANE semaphore discipline, OCR/dHash/CLIP stage error isolation (one stage failing must not corrupt/skip-persist others), PDF gates, video metadata-only path, tag source/sanitizer contract, embedding BLOB shape (2048B)' },
  { id: 'M7b', files: [AE+'Storage/Database.swift'], lens: 'schema correctness', focus: 'All 16 migrations DDL, index coverage vs the app queries (grep ReadStore), v15 FTS triggers, v16 path_search NFC population for pre-existing rows, pragma safety (synchronous=NORMAL + WAL durability window, cache_spill)' },
  { id: 'M8', files: [AE+'FileIDEngineMain.swift'], lens: 'logic + silent failure', focus: 'Command routing completeness vs IPCCommand enum, shutdown path, terminal-event emission for every command, error surfacing (no swallowed throws), watchdog/parent-death. Glob the real main entry file if the name is off.' },
  { id: 'M9', files: [AE+'ScanCoordinator.swift', AE+'JobQueue.swift', AE+'Pipeline/Discovery.swift'], lens: 'concurrency', focus: 'NSLock cancel/pause mirrors vs actor-state desync, JobQueue drainer advance on EVERY terminal path, hasActive races, Discovery channel capacity + cancellation while blocked on send, worker-cap math, EMA/ETA math' },
  { id: 'M10', files: [AE+'IPC/IPCSink.swift', AE+'IPC/IPCTransport.swift', AE+'IPC/JSONLog.swift', AS+'LineReader.swift'], lens: 'ipc + silent failure', focus: 'Coalescing must never delay/clobber terminal events (scanComplete, deepAnalyzeComplete, restructureApplyResult, faceClusteringComplete, error), fd-2 wire purity, write-failure handling, log rotation, frame bounds, full-buffer eviction criticality' },
  { id: 'M11', files: [AS+'IPCProtocol.swift'], lens: 'ipc conformance', focus: 'Every command/event vs shared/ipc-schema/ipc.schema.json: field names, optionality, casing, payload shapes. Read the schema. Unknown-variant decode; optional encode (null vs absent)' },
  { id: 'M12', files: [AS+'TagWriter.swift', AA+'Database/FolderClassifier.swift', AS+'CLIPTokenizer.swift'], lens: 'logic + data-loss', focus: 'Finder-tag xattr writes + undo journal (T4), batch tag failure isolation, folder tier rules, tokenizer bounds (DoS), BPE merge correctness' },
  { id: 'M13a', files: [AS+'StreamingDownload.swift', AS+'AIModels.swift'], lens: 'logic + resource', focus: 'Download resume/range handling, partial-file integrity, disk-space handling, concurrent download of same model, manifest SHA256 enforcement for every artifact' },
  { id: 'M13b', files: [AS+'TLSPinning.swift', AS+'StreamingDownload.swift'], lens: 'security', focus: 'CA-allowlist pinning: bypass paths, redirect handling (must stay https + pinned through redirects), host allowlist scope, downgrade paths, error masking' },
  { id: 'M14', files: [AE+'Models/'], lens: 'logic (ML preprocessing)', focus: 'Glob all Swift under this dir; audit face/CLIP preprocessing: channel order, normalization, alignment, resize, L2-norm, CoreML EP error paths, HNSW determinism (entropy vs fixed seed). Flag real defects, not the documented WS-MAC mirror.' },
  { id: 'MA1', files: [AA+'EngineClient.swift'], lens: 'concurrency + silent failure', focus: 'Spawn/respawn backoff window math, binary integrity checks, state machine, event dispatch completeness, waiter timeouts (every awaited reply needs timeout or process-exit hook?), in-flight UI flag reset on crash, inbound frame cap' },
  { id: 'MA2a', files: [AA+'Views/RestructureView.swift'], lens: 'ui-state + data-loss', focus: 'Plan/apply state machine: stale plan vs re-scan, selection across refresh, convert-symlinks irreversibility guard, apply error surfacing, partial-apply reconciliation, double-apply prevention, the live app-side classifier (bucketForFile) vs engine' },
  { id: 'MA2b', files: [AA+'Views/RestructureView.swift', AA+'Views/Restructure/SankeyFlowView.swift'], lens: 'perf + ui-state', focus: 'Sankey/drill-down with 10k+ proposals: O(N^2) layout, per-frame alloc, main-thread query work (OFFSET paging full-table sorts), hover storms' },
  { id: 'MA3', files: [AA+'Views/DeepAnalyzeViews.swift', AA+'Services/CLIPTextEncoder.swift'], lens: 'ui-state', focus: 'DA UI: stale terminal state cleared on run-start, streaming vs cancel, model picker truth, prewarm flow, CLIPTextEncoder usage (dead/off-main?)' },
  { id: 'MA4', files: [AA+'Database/ReadStore.swift'], lens: 'security + logic + perf', focus: 'EVERY query-building site: FTS5/SQL injection beyond the fixed search path, version-bump consistency, deleteFiles write-path safety + busy-timeout, main-thread IO, refreshCounters O(N log N), fetchAll materialization of embeddings' },
  { id: 'MA5a', files: [AA+'Views/LibraryView.swift'], lens: 'data-loss + logic', focus: 'Bulk tag/rename/undo: journal correctness, partial-failure handling, undo after re-scan, name collision on bulk rename' },
  { id: 'MA5b', files: [AA+'Views/LibraryView.swift'], lens: 'ui-state + perf', focus: 'Search/filter/refresh races: generation guards, grid render with 50k files, context menu state, thumbnail churn' },
  { id: 'MA6', files: [AA+'Views/PeopleView.swift'], lens: 'ui-state + logic', focus: 'Face cluster UI: edit-during-cluster, merge/rename vs engine events, unknown-person handling, revertMerge integrity' },
  { id: 'MA7a', files: [AA+'Views/SettingsView.swift'], lens: 'ui-state + silent failure', focus: 'Model install flows: cancel/reset slots, progress truth, error surfacing, prewarm wiring. Glob Services/*Installer*.swift and include them. Glob ReviewSettingsViews.swift too.' },
  { id: 'MA7b', files: [AA+'Views/CleanupView.swift', AA+'Views/WelcomeSheet.swift'], lens: 'data-loss', focus: 'Cleanup DELETES user files: confirmation, trash-vs-permanent, dedupe group selection (which copy kept), ReadStore.deleteFiles coupling, onboarding first-scan' },
  { id: 'MA8', files: [AA+'Views/Restructure/', AA+'Views/BulkRenameSheet.swift'], lens: 'ui-state + logic', focus: 'Glob all under Views/Restructure/ except SankeyFlowView: apply-bar state machine, recommendation rows, tree diff, bulk rename validation (illegal chars, collisions, empty)' },
  { id: 'W1a', files: [WE+'pipeline/tagging.rs'], lens: 'concurrency + resource', focus: 'Worker orchestration (cores x 1.7), vision_sem/clip_sem acquisition order (deadlock?), decode-before-or-after-permit (memory held while queued?), predecode byte budget vs decode ordering, bounded channel backpressure, cancellation between batches, memory-tier batch resize mid-scan, VRAM clamp (free vs total)' },
  { id: 'W1b', files: [WE+'pipeline/tagging.rs'], lens: 'logic (per-kind correctness)', focus: 'Per-kind paths (image/video/document/audio): failed-marking semantics, tags_evaluated gate placement, detector wiring (scrfd vs yunet — which is live?), EXIF/dhash/aesthetic, OCR language fallback, Year_ tag timestamp/timezone, CLIP backfill' },
  { id: 'W2a', files: [WE+'pipeline/dbwriter.rs'], lens: 'data-loss + concurrency', focus: 'Batch txn atomicity, rename-heal blast radius + heal_candidate_moved gate correctness, UPSERT id preservation, FTS coupling, crash between move and path_text update, recovery sidecar, BLOCKING IO inside the writer txn (legacy-hash re-read, stat probes)' },
  { id: 'W2b', files: [WE+'pipeline/dbwriter.rs'], lens: 'perf', focus: 'Batch sizing 64/250/500 per tier, NEW clone hotspots (512B/face known-deferred), statement caching, write-path index usage, WAL checkpoint pressure' },
  { id: 'W3', files: [WE+'pipeline/deep_analyze.rs'], lens: 'logic + resource', focus: 'VLM caption/rename: 50MP cap on every entry, cancel via select!, VLM error paths (server death mid-gen), CaptionOnly/RenameOnly/TagsOnly routing, keyframe/PDF-first-page bounds, skipExisting model-awareness, batch scope (image/video vs +pdf/doc)' },
  { id: 'W4', files: [WE+'commands/deep_analyze.rs', WE+'models/vlm.rs', WE+'models/vlm_server.rs'], lens: 'concurrency + silent failure', focus: 'Command lifecycle: duplicate-command bounce, token assembly, 4Hz throttle, terminal events on every path, llama.cpp server spawn/health/stderr drain (blocked pipe?), port/handle leaks on respawn, server-death detection, Vulkan ignoring gpu-override=cpu' },
  { id: 'W5', files: [WE+'pipeline/restructure.rs', WE+'commands/restructure.rs'], lens: 'logic + resource', focus: 'Plan generation: classification fusion, confidence enum bands, anchor-drop with Keep counts, plan paging/size cap (200k moves vs 32MB frame), person-name sanitization in dest, clip_embeddings HashMap memory-tier gating' },
  { id: 'W6', files: [WE+'pipeline/restructure_apply.rs', WE+'util/path_safety.rs'], lens: 'data-loss + security', focus: 'Apply: unique_destination edge cases (case-only, in-batch vs on-disk, suffix overflow, ~200-char truncation + ext preservation), MoveFileExW cross-volume window, B4 stale-plan TOCTOU, SEC-5 containment vs post-plan symlink/junction, long-path, DB-update failure after move, cancellability + progress' },
  { id: 'W7', files: [WE+'pipeline/restructure_semantic.rs'], lens: 'logic', focus: 'Butler semantic grouping P2-P4: cluster naming determinism, constrained decoding output validation (VLM output in folder names — injection/illegal chars?), group hierarchy, partial-completion state' },
  { id: 'W8a', files: [WE+'pipeline/face_clustering.rs', WE+'commands/face_clustering.rs'], lens: 'concurrency', focus: 'Phase-2 lock-free window vs concurrent wipe/bulk/restructure-apply, single-flight guard release on ALL error paths, phase-3 under-lock snapshot re-read (the S0/concurrent-edit-safety fix), cancellation' },
  { id: 'W8b', files: [WE+'pipeline/face_clustering.rs'], lens: 'logic (cluster math)', focus: 'Consolidate/automerge math (0.75 cosine), suggestion band 0.55..0.97, anchor handling, excluded faces, verdict guards (markPersonsDifferent honored by automerge?), determinism (fixed seed)' },
  { id: 'W9', files: [WE+'pipeline/identity_clustering.rs', WE+'pipeline/cluster_suggestions.rs', WE+'util/hnsw_index.rs'], lens: 'logic + determinism', focus: 'BLAKE3 content identity, HNSW determinism (insertion order, seeds), suggestion generation, mixed-dimension embeddings (512 ArcFace legacy vs 128 SFace), triple-copy memory in HNSW build' },
  { id: 'W10', files: [WE+'pipeline/discovery.rs', WE+'commands/scan.rs', WE+'pipeline/usn.rs'], lens: 'logic + perf', focus: 'jwalk parallel discovery: prune rules, FileKind inference, symlink/junction cycles, USN journal state (wrap, volume change, reset), scan gate models, incremental skip-set, per-file metadata serialization on single consumer thread, model-load serialized ahead of discovery' },
  { id: 'W11', files: [WE+'scan_session.rs'], lens: 'concurrency', focus: 'Pause/resume/cancel state machine: lost-wakeup (Notify before park), phase/progress emission ordering, terminal PhaseChanged via try_send (droppable?), session teardown on every exit, resume-cursor consistency' },
  { id: 'W12', files: [WE+'main.rs'], lens: 'security + logic', focus: 'SEC-3 DLL search lock ordering (before ANY DLL load incl delay-loads), parent watchdog (PID reuse?), tokio setup, panic hook coverage, command loop terminals, shutdown ordering (DB close), CUDA-pack mid-process activation, ORT/llama VRAM mutual exclusion' },
  { id: 'W13', files: [WE+'models/runtime.rs', WE+'models/ep_guard.rs'], lens: 'logic', focus: 'EP pick chain per vendor, user-override resolution, ep_guard arming/disarm lifecycle (cleared on GRACEFUL shutdown? reset on restart? infinite fallback loops? poison healthy EP on clean exit during load?), CUDA-pack readiness, VRAM clamp math' },
  { id: 'W14', files: [WE+'models/ram_plus.rs', WE+'models/ram_plus_batch.rs', WE+'models/mobileclip.rs'], lens: 'logic (preprocessing) + perf', focus: 'RAM++/CLIP preprocessing: resize/normalize/channel order, per-class thresholds, batch vs single output equivalence, NEW hot-loop allocations (per-pixel 4D indexing + Array4::zeros known-deferred), model-id label (mobileclip_s2 vs ViT-B/32)' },
  { id: 'W15a', files: [WE+'models/registry.rs', WE+'models/scene_vocab.rs', WE+'models/variants.rs'], lens: 'logic + security', focus: 'Registry: SHA256 pin enforcement on every artifact (any None?), variant selection, sentinel files (id-keyed never invalidates on pin bump?), vocab suppress-list, download-URL allowlist' },
  { id: 'W15b', files: [WE+'models/sface.rs', WE+'models/yunet.rs', WE+'models/scrfd.rs', WE+'models/face_align.rs'], lens: 'logic (ML preprocessing)', focus: 'Face detect/align/embed: letterbox math, 5-point alignment, BGR/RGB order, L2-norm, output parsing bounds (trusted detector output? NaN/inf), embedding dimension checks' },
  { id: 'W16a', files: [WE+'downloader.rs'], lens: 'logic + resource', focus: 'Resume/416/range-GET part integrity, parallel range assembly, disk space, retry/backoff, partial-file cleanup, progress accuracy' },
  { id: 'W16b', files: [WE+'downloader.rs'], lens: 'security', focus: 'TLS CA-allowlist pinning: bypass paths, redirect policy (https + pinned + hop cap), SHA256 verify before use, zip extraction (bomb caps, entry traversal), URL allowlist' },
  { id: 'W17', files: [WE+'ipc/mod.rs'], lens: 'ipc conformance', focus: 'Every command/event vs ipc.schema.json (read it): names/optionality/casing, unknown-command rejection, redaction before EVERY path-bearing emit (username leak for files under home dir?), error code vocabulary' },
  { id: 'W18', files: [WE+'ipc/sink.rs', WE+'ipc/bounded_read.rs', WE+'ipc/conformance.rs'], lens: 'ipc + concurrency', focus: 'Sink thread-safety, write failure when app dies (SIGPIPE/broken pipe), event ordering, terminal events never dropped/coalesced, per-event stdout flush (syscall cost), MAX_FRAME_BYTES both directions, conformance coverage gaps' },
  { id: 'W19', files: [WE+'platform.rs'], lens: 'security (unsafe FFI) + logic', focus: 'ALL unsafe blocks: buffer sizing, handle/COM leaks on error paths, string lifetimes into Win32, DXGI probe error handling, memory_tier() correctness (available vs total? container/VM?), VRAM probe (free vs total), priority boost' },
  { id: 'W20', files: [WE+'commands/bulk.rs'], lens: 'data-loss + concurrency', focus: 'Bulk tag/rename: undo journal, partial-failure isolation, writer-lock offloading, wipe interlock interactions, rename collision, path update consistency, PKEY_Keywords merge vs clobber, case-merge of duplicate tags, per-file success only on real write' },
  { id: 'W21a', files: [WE+'shell/tags.rs', WE+'shell/ocr.rs', WE+'shell/video.rs', WE+'shell/thumbnail.rs', WE+'shell/trash.rs', WE+'shell/reveal.rs'], lens: 'logic + concurrency (COM)', focus: 'COM init/apartment discipline per thread, IFileOperation error mapping, OCR language-absence fallback, Media Foundation lifetime, recycle-bin on huge/UNC paths' },
  { id: 'W21b', files: [WE+'pipeline/doc_extract.rs', WE+'pipeline/audio_meta.rs', WE+'pipeline/batch_clip.rs'], lens: 'logic + resource', focus: 'PDF/DOCX extraction bounds (zip bombs in DOCX, malformed PDF), audio metadata bounds, CLIP batch coordinator (batch boundary, partial flush, ordering)' },
  { id: 'W22', files: [WE+'commands/prewarm.rs', WE+'commands/trash.rs', WE+'commands/trash_log.rs', WE+'commands/embed.rs', WE+'commands/hardware.rs'], lens: 'logic + silent failure', focus: 'Prewarm cancel + sentinel truth, trash manifest + restore round-trip, embed bounds, hardware report accuracy, hardwareReprobed semantics' },
  { id: 'W23a', files: [WE+'db/migrations.rs', WE+'db/mod.rs'], lens: 'schema correctness', focus: 'All 16 migrations DDL, append-only discipline, downgrade guard, reader-pool read-only enforcement, busy-timeout/WAL config, FTS triggers, migration failure mid-way (txn per migration?), index-name parity with GRDB, v16 NFC asymmetry' },
  { id: 'W23b', files: [WE+'util/content_hash.rs', WE+'util/zip.rs', WE+'util/hmac.rs', WE+'util/elevation.rs', WE+'util/keywords.rs'], lens: 'logic + security', focus: 'BLAKE3 streaming correctness + legacy recipe, zip entry traversal/bomb guards, HMAC usage (constant-time?), elevation checks, keyword extraction bounds' },
  { id: 'WA1', files: [WA+'ViewModels/EngineClient.cs'], lens: 'concurrency + silent failure', focus: 'Process lifecycle: respawn backoff, sig verify, stdout frame reader (32MB cap, scanned-offset O(n) buffer), event dispatch thread marshaling, waiter timeout coverage, stale-process guards on EVERY handler, in-flight flag reset on crash' },
  { id: 'WA2', files: [WA+'ViewModels/EngineClient.Commands.cs', 'platforms/windows/src/FileID.IpcSchema/CommandPayload.cs', 'platforms/windows/src/FileID.IpcSchema/EventPayload.cs', 'platforms/windows/src/FileID.IpcSchema/Dtos.cs', 'platforms/windows/src/FileID.IpcSchema/IpcCoder.cs'], lens: 'ipc conformance', focus: 'Command builders vs schema (read ipc.schema.json), waiter correlation (single-file DA waiter interactions), null/zero-fill on missing required fields (NEW field relies on it?), enum decode of unknown values' },
  { id: 'WA3a', files: [WA+'Views/Restructure/'], lens: 'ui-state + data-loss', focus: 'Glob all .xaml.cs + .cs under Views/Restructure/: apply state machine, double-apply prevention, plan staleness vs re-scan, drill-down sheet state, partial-apply surfacing, progress + cancel UI' },
  { id: 'WA3b', files: [WA+'Views/Restructure/'], lens: 'perf', focus: 'Sankey render with 200k moves: per-frame alloc, layout complexity, UI virtualization, dispatcher storms from progress events' },
  { id: 'WA4', files: [WA+'Views/DeepAnalyze/'], lens: 'ui-state', focus: 'Glob DeepAnalyze view files: stale-Complete cleared on run-start, token streaming UI thread marshaling, crash reset of run flags, model picker truth' },
  { id: 'WA5', files: [WA+'Services/ModelInstallerService.cs', WA+'Services/ModelSlot.cs'], lens: 'logic + concurrency', focus: 'Install state machine: stall guard, cancel mid-download, integrity check before activate, slot reuse races, GPU session pool lifetime, monotonic progress' },
  { id: 'WA6', files: [WA+'Services/ReadStore.cs', WA+'Services/ClipSearchService.cs', WA+'Services/AppSettings.cs'], lens: 'security + logic', focus: 'Query building (injection), connection lifecycle (leak per query?), semantic search correctness, settings persistence races, defaults' },
  { id: 'WA7a', files: [WA+'ViewModels/LibraryViewModel.cs'], lens: 'ui-state + concurrency', focus: 'Refresh/generation guards, filter state vs engine events, selection consistency, ObservableCollection mutation thread' },
  { id: 'WA7b', files: [WA+'Views/Library/'], lens: 'ui-state + data-loss', focus: 'Glob Library view files: bulk ops (tag/rename/delete) confirmation + partial failure, thumbnail lifecycle, preview sheet file handles' },
  { id: 'WA8a', files: [WA+'Views/People/'], lens: 'ui-state', focus: 'Glob People view files: edit-during-cluster, merge sheet state, suggested-merges acceptance, name validation, person-name tag write-out wiring' },
  { id: 'WA8b', files: [WA+'ViewModels/PeopleViewModel.cs', WA+'ViewModels/CleanupViewModel.cs'], lens: 'logic + data-loss', focus: 'Merge identity stability across refresh, cleanup dedupe group selection (which copy kept), delete confirmation, trash round-trip, keeper-tag toggle wiring' },
  { id: 'WA9a', files: [WA+'MainWindow.xaml.cs', WA+'App.xaml.cs'], lens: 'logic + concurrency', focus: 'Lifecycle: single-instance, DPI changes, window close vs engine shutdown ordering, unhandled exception handler, startup auth (UI-thread RPC interactions)' },
  { id: 'WA9b', files: [WA+'Views/Sidebar/'], lens: 'ui-state', focus: 'Glob Sidebar files: queue list binding (queueState — regression watch), progress binding truth, phase display, ETA display' },
  { id: 'WA10', files: [WA+'Services/ThumbnailService.cs', WA+'Services/ThumbnailDiskCache.cs', WA+'Services/DebugLog.cs', WA+'Services/WinVerifyTrustChecker.cs'], lens: 'resource + concurrency', focus: 'Cache bounds under concurrent writers, dispose discipline, eviction correctness, trust-check threading, log growth bounds' },
  { id: 'WA11', files: [WA+'Views/Settings/', WA+'Views/'], lens: 'ui-state', focus: 'Glob Settings view files + WelcomeSheet + any remaining top-level Views not covered: install/settings UI truth, GPU selector, perf tuning controls, welcome flow' },
]

phase('Review')
log(`Lean review of ${UNITS.length} remaining units (review-only, no verify fan-out)...`)

const results = await parallel(UNITS.map(u => () =>
  agent(
    `You are auditing unit ${u.id} of FileID.\nFILES: ${Array.isArray(u.files) ? u.files.join(' | ') : u.files}\nLENS: ${u.lens}\nFOCUS: ${u.focus}\n${COMMON}\nSet unit to "${u.id}" in your output.`,
    { label: `review:${u.id}`, phase: 'Review', schema: FINDINGS }
  ).then(r => r ? { ...r, unit: r.unit || u.id } : { unit: u.id, dead: true, findings: [] })
))

const ok = results.filter(Boolean)
const withFindings = ok.filter(r => r.findings && r.findings.length)
const clean = ok.filter(r => r.verifiedClean && (!r.findings || !r.findings.length)).map(r => r.unit)
const dead = ok.filter(r => r.dead).map(r => r.unit)
const allFindings = withFindings.flatMap(r => (r.findings || []).map(f => ({ ...f, unit: r.unit })))
log(`Lean audit: ${allFindings.length} raw findings across ${withFindings.length} units; ${clean.length} clean; ${dead.length} dead/rate-limited`)
return { units: ok, allFindings, cleanUnits: clean, deadUnits: dead, total: allFindings.length }

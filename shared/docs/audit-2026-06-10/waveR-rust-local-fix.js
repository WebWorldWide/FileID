export const meta = {
  name: 'fileid-waveR-rust-local',
  description: 'Wave R: implement all 30 Rust-local findings (C1 + C2-004/005 + C6-015/016/017) in file-disjoint groups',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint file group; edits real tree, adds tests, no cargo' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const ENG = `${ROOT}/platforms/windows/src/engine/src`

const GUARDS = `
DELIBERATE GUARDS — never weaken these (your fix must preserve them); if a finding seems to ask you to weaken one, STOP and report it as skipped:
tags_evaluated/faces_evaluated/ocr_stage_ran gates; id-preserving UPSERT + v15 FTS triggers; strip_extended_length (non-canonicalizing) junction walk; the ep_guard armed-EP SET breadcrumb (your C1-001 fix only adds a CLEAR on graceful shutdown — it must NOT remove the over-disable-on-crash behavior); MoveFileExW without MOVEFILE_REPLACE_EXISTING (B3); B4 stale-plan re-read; SEC-3 DLL search lock; SEC-5 reparse checks; db_newer_than_engine downgrade refusal; the documented 'terminal events must not drop' rule (you are ENFORCING it, not relaxing). Path redaction via redact_path_for_log before any tracing of a user path.`

const COMMON = `
REPO ROOT: ${ROOT}. Engine source root: ${ENG}. You are a Rust fix implementer for the FileID Windows engine (cross-platform-clean crate; compiles + tests on macOS via cargo). Edition 2021, MSRV 1.90. No new dependencies. Default to NO comments except a one-line non-obvious WHY.

For EACH finding ID you own:
1. Read its full record in ${ROOT}/shared/docs/audit-2026-06-10/findings.json (claim, files, port_source, test_to_add). If port_source names a Windows reference, that's the proven pattern — but here you are often fixing the Windows side itself, so read the cited code directly.
2. Locate the exact code by grepping for the symbols in the claim (some finding paths are approximate — trust the symbol, not the path). Read enough surrounding code to implement correctly and idiomatically (match the file's existing error handling, tracing, locking, and test style).
3. Implement the minimal correct fix. Preserve all guards (below). Keep edits INSIDE your owned files only.
4. Add the regression test from test_to_add as an inline #[cfg(test)] test in the owned file (or extend its existing tests module). The test must FAIL before your fix and PASS after (reason about this; you cannot run cargo here).
5. If a finding is already fixed in current code, or you cannot fix it safely without touching files outside your set or weakening a guard, mark it skipped with a precise reason — do not force it.

HARD RULES:
- Edit ONLY the files in your assigned set. Do NOT touch any other file (another fixer owns it). If your fix truly needs a change outside your set, implement a local solution within your files or mark the finding skipped with the cross-file dependency noted.
- Do NOT run cargo (parallel fixers share one target dir — it would race). Self-verify by careful reasoning.
- Do NOT add new dependencies or new shared helper modules; keep helpers local to your files.
${GUARDS}

Return the structured result.`

const SCHEMA = {
  type: 'object',
  required: ['group', 'fixed', 'skipped', 'filesChanged'],
  properties: {
    group: { type: 'string' },
    fixed: { type: 'array', items: { type: 'object', required: ['id', 'summary'], properties: { id: { type: 'string' }, summary: { type: 'string' }, testAdded: { type: 'string' } } } },
    skipped: { type: 'array', items: { type: 'object', required: ['id', 'reason'], properties: { id: { type: 'string' }, reason: { type: 'string' } } } },
    filesChanged: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' },
  },
}

const GROUPS = [
  { group: 'G1-deep-analyze', files: 'pipeline/deep_analyze.rs, commands/deep_analyze.rs', ids: ['F-C1-005','F-C1-006','F-C1-020','F-C1-021','F-C1-022'],
    hint: 'C1-005 include PDFs in deepAnalyzeAll target query when PDF render ships; C1-006 honor gpuExecutionProviderOverride=cpu (omit -ngl 99 / use cpu) for the llama path; C1-020 skip_existing keys on (file, vlm_model); C1-021 only server DEATH abandons the persistent server, not a per-file error; C1-022 exclude files.failed=1 from targets.' },
  { group: 'G2-trash-bulk', files: 'commands/trash.rs, commands/bulk.rs, commands/trash_log.rs', ids: ['F-C1-003','F-C1-007','F-C1-012','F-C1-018'],
    hint: 'C1-003 restoreFromTrash returns a conflict error (not success) when destination occupied; deterministic pick on multi-entry same-path; C1-007 batch/single Recycle Bin enumeration instead of per-item PowerShell; C1-012 add a per-move recovery sidecar for bulk rename (mirror restructure B5); C1-018 order trash_log append before the irreversible step or surface its failure.' },
  { group: 'G3-scan-sink', files: 'scan_session.rs, ipc/sink.rs', ids: ['F-C1-002','F-C1-009','F-C1-013','F-C6-015'],
    hint: 'C1-002 terminal PhaseChanged(Cancelled/Failed) must use a guaranteed (blocking) send, never try_send; add a ScanComplete backstop for Failed; C1-009 cap outbound frame size in sink, emit a structured ipc_frame_too_large event instead of letting the app silently drop; C1-013 skip-set must NOT skip a file whose reparse/placeholder (dehydrated->hydrated) state changed; C6-015 coalesce stdout flushes during bursts (flush per drain, not per event) WITHOUT delaying terminal events. Do NOT touch main.rs (G4 owns it).' },
  { group: 'G4-main-watchdog', files: 'main.rs', ids: ['F-C1-010','F-C1-015'],
    hint: 'C1-010 oversize-frame error text must state MAX_FRAME_BYTES (32 MiB), not 1 MB; C1-015 pin parent identity by a process handle/creation-time captured at startup, not bare PID (TOCTOU). Keep the watchdog fix entirely within main.rs (inline any helper); do NOT edit platform.rs (G6 owns it).' },
  { group: 'G5-models-ep', files: 'models/ep_guard.rs, models/runtime.rs, models/registry.rs, commands/prewarm.rs, commands/scan.rs', ids: ['F-C1-001','F-C1-024'],
    hint: 'C1-001 clear the ep_guard crash breadcrumb on a GRACEFUL shutdown that occurs mid-model-load (so a healthy EP is not poisoned next launch) — keep the over-disable-on-actual-crash behavior intact; C1-024 invalidate the install sentinel when the pinned revision/hash changes (key it by revision like macOS), forcing re-fetch on a pin bump.' },
  { group: 'G6-platform', files: 'platform.rs', ids: ['F-C1-008','F-C2-004'],
    hint: 'C1-008 SleepGuard: ES_CONTINUOUS set+cleared must happen on the SAME thread — make the guard own a dedicated thread or use a process-wide assertion that does not depend on which tokio worker runs Drop; keep the whole fix inside platform.rs (do NOT edit scan_session.rs); C2-004 redaction must mask the username for a file directly under a home dir (…/<user>/file currently leaks <user>).' },
  { group: 'G7-restructure', files: 'pipeline/restructure.rs, commands/restructure.rs', ids: ['F-C1-004','F-C6-016'],
    hint: 'C1-004 anchor-strip must not eat the semantic butler high-confidence moves when a homogeneous source folder routes to one destination group (strip true anchors, not semantic moves); C6-016 gate the planRestructure full clip_embeddings HashMap load by memory tier (stream/cap under Low).' },
  { group: 'G8-tagging-dbwriter', files: 'pipeline/tagging.rs, pipeline/dbwriter.rs, db/migrations.rs', ids: ['F-C1-016','F-C1-019','F-C1-025','F-C2-005'],
    hint: 'C1-016 RAM++ coordinator thread spawn must degrade gracefully (no .expect panic) like sibling sites; C1-019 derive Year_ tag from creation date in the agreed timezone/boundary (converge with macOS: creation-time, local, >1990) — pick the canonical rule and pin it; C1-025 move blocking file IO (legacy_content_hash ~2MB re-read for >16MB files; video/over-cap-doc hashing) OFF the single-writer transaction / off the writer-locked path; C2-005 NFC-normalize path_search on the Windows WRITE path (dbwriter) so NFD filenames are found by NFC queries (no schema migration — write-side normalization mirroring macOS v16_path_search; existing rows heal on rescan).' },
  { group: 'G9-discovery', files: 'pipeline/discovery.rs', ids: ['F-C6-017'],
    hint: 'C6-017 parallelize the per-file metadata + file_ref syscalls (currently serialized on the single consumer thread); the jwalk already parallelizes read_dir — push the stat/file_ref work into the parallel stage or a worker pool. Keep cancellation + ordering semantics.' },
  { group: 'G10-misc-io', files: 'downloader.rs, shell/video.rs, pipeline/doc_extract.rs, pipeline/face_clustering.rs, commands/face_clustering.rs', ids: ['F-C1-011','F-C1-014','F-C1-017','F-C1-023'],
    hint: 'C1-011 keyframe extraction must scope/uninitialize the COM apartment per task (or run on a dedicated thread) so recycled tokio blocking-pool threads are not left MTA; C1-014 cap pptx member iteration (reject a zip-bomb-shaped pptx); C1-017 ranged resume seeds progress from on-disk bytes + 416 falls back to a clean re-fetch; C1-023 re-derive the name-based auto-merge guard from the under-lock phase-3 snapshot (not the phase-1 snapshot).' },
]

phase('Fix')
log(`Rust-local wave: ${GROUPS.length} disjoint fixer groups, ${GROUPS.reduce((n,g)=>n+g.ids.length,0)} findings`)

const results = await parallel(GROUPS.map(g => () =>
  agent(
    `You are fixer ${g.group}. You EXCLUSIVELY own these files (edit ONLY these): ${g.files}\nFindings to fix: ${g.ids.join(', ')}\nPer-finding hints: ${g.hint}\n${COMMON}\nSet group to "${g.group}".`,
    { label: g.group, phase: 'Fix', schema: SCHEMA }
  ).then(r => r || { group: g.group, fixed: [], skipped: g.ids.map(id => ({ id, reason: 'fixer agent died' })), filesChanged: [], notes: 'DEAD' })
))

const ok = results.filter(Boolean)
const fixed = ok.flatMap(r => (r.fixed || []).map(f => ({ ...f, group: r.group })))
const skipped = ok.flatMap(r => (r.skipped || []).map(f => ({ ...f, group: r.group })))
const changed = ok.flatMap(r => r.filesChanged || [])
log(`Wave R done: ${fixed.length} fixed, ${skipped.length} skipped, ${changed.length} files touched`)
return { groups: ok, fixed, skipped, filesChanged: changed }

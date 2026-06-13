export const meta = {
  name: 'fileid-waveC6-perf',
  description: 'Wave C6: perf/scaling fixes (macOS skip-set, off-main/streamed queries, allocs, cancellable apply) in 5 disjoint groups',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint file group' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const AE = 'platforms/apple/engine/Sources/FileIDEngine'
const APP = 'platforms/apple/app/Sources/FileID'
const WE = 'platforms/windows/src/engine/src'

const COMMON = `
REPO ROOT: ${ROOT}. Performance/scaling fixes. Correctness must be preserved exactly — these change HOW work is scheduled/allocated, never WHAT results are produced. Swift macOS code compiles via 'swift build' (tests CI-only, no Xcode); Rust compiles+tests locally. Default to NO comments except a one-line non-obvious WHY.

For EACH finding: read its record in ${ROOT}/shared/docs/audit-2026-06-10/findings.json (claim, evidence, improvement). Locate code by symbol (paths approximate). Implement the perf fix, preserving behavior/ordering/cancellation/durability. Add a test only where the perf property is unit-assertable (e.g. a pure function's complexity/no-redundant-call); UI/throughput effects are CI-build + Mac/hardware-verified — say so otherwise.

HARD RULES: edit ONLY your assigned files (+ your test files). No new dependencies. No telemetry. Preserve guards: single-writer DB, @MainActor UI isolation, redactPathForLog, cancellation mirrors, FTS triggers, terminal-event pinning. Do NOT run swift build (Swift groups); Rust group MAY run cargo to self-verify. Do NOT claim measured throughput numbers (no hardware here) — justify by mechanism + complexity.

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
  { group: 'C6-SCAN-PIPELINE', files: `${AE}/Pipeline/Discovery.swift, ${AE}/ScanCoordinator.swift, ${AE}/Storage/DBWriter.swift, ${AE}/Storage/Database.swift`,
    ids: ['F-C6-001','F-C6-004','F-C6-005','F-C6-010'],
    hint: 'F-C6-001 (HIGH, the big win — mirror the Windows discovery-time skip set): in Discovery, skip a file from the expensive ANE/Vision/CLIP tagging pipeline when the DB already has it unchanged (failed=0 AND scanned_at>=modified_at AND size matches) on a NON-forced (incremental) scan, so a rescan does not re-run ML on every unchanged file. Add the skip-predicate query (read-only) — the macOS DBWriter already has WRITE-time unchanged detection; this moves the skip UPSTREAM to discovery so the ML cost is avoided. Honor forceReprocess (full rescan still re-runs). 004: macOS DBWriter currently commits inline on the rendezvous channel so every transaction stalls all tagging workers — decouple the commit from the channel (buffer + commit without blocking producers), preserving the 100-file/200ms batch bound + crash-safety. 005: Discovery materializes+sorts the entire corpus before tagging starts — stream discovered files into the channel as found (bounded) instead of building+sorting the whole list first (preserve any required ordering via the existing mechanism or document the change). 010: DBWriter.insertOne re-prepares its statements per call — cache prepared statements (and reduce the embedding-blob copy chain on the DBWriter side) to match the Windows RETURNING/cached-statement path.' },
  { group: 'C6-TAGGING-VISION', files: `${AE}/Pipeline/Tagging.swift, ${AE}/VisionWorker.swift`,
    ids: ['F-C6-007','F-C6-008'],
    hint: '007: runVisionWithTimeout executes the Vision request on a global queue with NO autoreleasepool, so per-request intermediates accumulate on never-draining root-queue threads — wrap the Vision perform in an autoreleasepool. 008: loadImageAndEXIF re-stats every image via FileManager.attributesOfItem when DiscoveredFile.sizeBytes already holds the size — drop the redundant stat (one fewer NAS/SMB round-trip per file). Preserve all tag/EXIF results exactly.' },
  { group: 'C6-READSTORE', files: `${APP}/Database/ReadStore.swift`,
    ids: ['F-C6-002','F-C6-003'],
    hint: '002 (HIGH): semantic/similarity search materializes EVERY clip_embeddings row via fetchAll (~1 GB transient at 500k files) — stream/iterate rows in batches (GRDB cursor) computing cosine incrementally with a bounded top-K heap, instead of loading all embeddings into memory at once. Same results, bounded memory. 003 (HIGH): refreshCounters runs an O(N log N) duplicate-group window query synchronously (up to 1 Hz during a scan) — run it off the main actor and/or back it with an index + cheaper aggregate so it does not jank the UI during scans. (The off-main search methods from F-C4-006/007 already exist here; this is the memory + counters work.)' },
  { group: 'C6-APP-MISC', files: `${APP}/EngineClient.swift, ${APP}/LibraryView.swift`,
    ids: ['F-C6-011','F-C6-014','F-C4-006','F-C4-007'],
    hint: '011: raise the macOS app INBOUND IPC frame cap from 1 MiB to 32 MiB (match the engine outbound cap + Windows) with a visible oversize error instead of a silent drop — find the LineReader/frame-cap constant used by EngineClient. 014: per-second store.notifyChanged() during a live scan re-fires every visible tile .task (re-reads thumbnails) — coalesce/debounce so a tile does not re-task on every tick. F-C4-006/007 COMPLETION: switch LibraryView.reload() to call the async ReadStore methods (semanticSearchAsync / similarFilesAsync / filesAsync that ReadStore now exposes) instead of the synchronous ones on the MainActor, awaiting them and assigning rows on main (debounce the live-scan keyword reload). This completes the off-main move whose ReadStore half already landed.' },
  { group: 'C6-RESTRUCTURE-CANCEL', files: `${AE}/Pipeline/Restructure.swift, ${WE}/pipeline/restructure_apply.rs`,
    ids: ['F-C6-013'],
    hint: 'F-C6-013 (both platforms): restructure apply is an uncancellable, progress-less serial loop with a per-move DB transaction — at 100k+ moves the user gets no feedback and cannot stop it. macOS Restructure.apply: check a cancellation signal between moves (use the existing cancel mechanism) and emit periodic progress (restructureApply progress, throttled). Windows restructure_apply.rs: same — poll the cancel AtomicBool between moves and emit progress. Preserve B3/B4/SEC-5 guards, the recovery sidecar, and per-move atomicity. The Rust side: run cargo clippy --all-targets -D warnings + cargo test from platforms/windows/src/engine to self-verify (you are the only fixer touching Rust this wave).' },
]

phase('Fix')
log(`Wave C6: ${GROUPS.length} disjoint perf fixers`)

const results = await parallel(GROUPS.map(g => () =>
  agent(
    `You are fixer ${g.group}. You EXCLUSIVELY own these files (edit ONLY these + your own test files): ${g.files}\nFindings: ${g.ids.join(', ')}\nGuidance: ${g.hint}\n${COMMON}\nSet group to "${g.group}".`,
    { label: g.group, phase: 'Fix', schema: SCHEMA }
  ).then(r => r || { group: g.group, fixed: [], skipped: g.ids.map(id => ({ id, reason: 'fixer agent died' })), filesChanged: [], notes: 'DEAD' })
))

const ok = results.filter(Boolean)
const fixed = ok.flatMap(r => (r.fixed || []).map(f => ({ ...f, group: r.group })))
const skipped = ok.flatMap(r => (r.skipped || []).map(f => ({ ...f, group: r.group })))
log(`Wave C6 done: ${fixed.length} fixed, ${skipped.length} skipped`)
return { groups: ok, fixed, skipped, filesChanged: ok.flatMap(r => r.filesChanged || []) }

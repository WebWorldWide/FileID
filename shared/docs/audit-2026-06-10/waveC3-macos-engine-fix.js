export const meta = {
  name: 'fileid-waveC3-macos-engine',
  description: 'Wave C3: 45 macOS-engine findings (incl. critical gate-trio + butler port) in 7 file-disjoint groups',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint Swift file group; edits real tree, adds Swift Testing tests' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const AE = 'platforms/apple/engine/Sources/FileIDEngine/'
const AS = 'platforms/apple/shared/Sources/FileIDShared/'
const AA = 'platforms/apple/app/Sources/FileID/'

const COMMON = `
REPO ROOT: ${ROOT}. macOS platform: ${ROOT}/platforms/apple. You implement Swift fixes for the FileID macOS engine. Swift 6 strict concurrency (actors, @MainActor for UI, @unchecked Sendable only with explicit lock coverage). The engine compiles via 'swift build' but CANNOT run tests here (no Xcode) — tests run in CI (macos.yml). Default to NO comments except a one-line non-obvious WHY.

For EACH finding ID you own:
1. Read its full record in ${ROOT}/shared/docs/audit-2026-06-10/findings.json (claim, evidence, files, port_source, ruling, test_to_add).
2. If port_source names a Windows reference (platforms/windows/src/engine/src/...), READ that proven implementation — your job is a faithful Swift translation of known-good logic, not a fresh design. The Windows side is the source of truth for behavior.
3. Locate the macOS code by grepping for the symbols in the claim (finding paths may be approximate — trust the symbol). Read enough surrounding code to match the file's idioms (GRDB usage, actor isolation, IPC emit patterns, JSONLog.redactPathForLog before logging paths).
4. Implement the minimal correct fix.
5. Add a regression test as a Swift Testing @Test in the appropriate target: engine tests go in platforms/apple/Tests/EngineTests/ (create a new <Topic>Tests.swift if needed, matching the existing files' import/style); shared tests in platforms/apple/Tests/SharedTests/. The test must encode the invariant (it runs in CI). If a fix is genuinely untestable without a live model/GPU, say so and skip the test with a reason.
6. If a finding is already fixed, or cannot be fixed without editing a file outside your set or weakening a guard, mark it skipped with a precise reason.

HARD RULES:
- Edit ONLY the files in your assigned set (+ your test files under Tests/). Do NOT touch any other source file — another fixer owns it. Coordinate ONLY through stable existing APIs and the IPC schema (shared/ipc-schema/ipc.schema.json), never by editing a peer's file.
- Do NOT run 'swift build' or 'swift test' (parallel fixers + no Xcode). Self-verify by careful reasoning and by reading the types you call.
- Do NOT add dependencies (no new SPM packages). No telemetry.
- DELIBERATE GUARDS — never weaken: the macOS engine intentionally emits IPC on fd 2 (stderr); @MainActor isolation on UI; existing cancel/pause NSLock mirrors in ScanCoordinator; redactPathForLog before logging a user path; GRDB single-writer discipline. Preserve them.

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
  { group: 'C3-FACES', files: `${AE}Pipeline/FaceClustering.swift, ${AE}Pipeline/IdentityClustering.swift, ${AE}Models/HNSWIndex.swift`,
    ids: ['F-C3-002','F-C3-003','F-C3-004','F-C3-005','F-C3-006','F-C3-007','F-C3-008','F-C3-033','F-C3-041','F-C3-042'],
    hint: 'The face-clustering correctness + determinism + lifecycle cluster (mostly ports of proven Windows fixes). 002: persist under a snapshot RE-READ inside the persist transaction (Windows S0), not the phase-0 snapshot, so concurrent People edits survive. 003: never auto-merge an is_unknown person. 004: honor face_verifications "different" verdicts (build a blocked-pair set) in tightPairAutoMerge. 005: remove union-find transitive chaining that bridges two named clusters. 006: HNSW level draw uses a FIXED seed (Windows pins 0xF11E_1D00) not Float.random. 007: deterministic sorted-root iteration (not unordered Dictionary.values). 008: O(dim) running-sum centroid in Pass-2 (not full recompute). 033: skip permanently-failing rows in the pending-extraction LIMIT window so newer faces are not starved. 041: do not re-create persons whose representative_face_id was cascade-deleted mid-pass. 042: poll an EXISTING cancellation/shutdown signal inside the cluster loop so a mid-flight pass can abort cleanly — use existing primitives only; do NOT require a FileIDEngineMain edit (if you cannot abort without one, implement the in-loop check and note the main-loop wiring for C3-CORE).' },
  { group: 'C3-DA', files: `${AE}Pipeline/DeepAnalyze.swift, ${AE}Pipeline/DeepAnalyzeRunner.swift, ${AE}Pipeline/VLMDownloader.swift, ${AE}Models/DeepAnalyzeCapability.swift`,
    ids: ['F-C3-022','F-C3-023','F-C3-024','F-C3-025','F-C3-026','F-C3-027','F-C3-028','F-C3-043','F-C3-044'],
    hint: '022: parseFaceComparison must parse "not the same person"/negatives as DIFFERENT (currently inverts to SAME at 0.80 > 0.75 automerge — a real face-merge data hazard). 023: single-flight/reentrancy guard on ensureLoaded (overlapping prewarm + Deep Analyze must share one load; stable vlm_model attribution). 024: deepAnalyzeCancel must cancel an in-flight model download — run the download in a child Task the cancel path cancels (cooperative Task.isCancelled / URLSession cancel); if StreamingDownload (owned by C3-DOWNLOAD) ignores cancellation, implement the cancellable-Task wrapper on your side and note it. 025: do NOT clearCancel() at job start if a cancel was issued while queued (honor the queued cancel). 026: move the synchronous image decode OFF the DeepAnalyze actor executor so deepAnalyzeCancel is not stalled by a file on an unreachable volume. 027: escape LIKE metacharacters (_ and %) in folder-scoped Deep Analyze. 028: the deep_targets_failed exit must emit a terminal deepAnalyzeComplete (every run() exit emits exactly one). 043: only write the .fileid-verified sentinel after confirming ALL repo files (recursive listing) are present. 044: apply the 50MP decode cap on the macOS deep-analyze path; use COALESCE-style writes so a NULL proposed_name/description does not clobber a prior value (Windows parity).' },
  { group: 'C3-DOWNLOAD', files: `${AS}StreamingDownload.swift, ${AS}TLSPinning.swift, ${AS}ModelManifest.swift, ${AS}AIModels.swift`,
    ids: ['F-C3-037','F-C3-038'],
    hint: '037: free-disk-space preflight before a multi-GB VLM download (mirror the small-artifact installers; surface a clear error). 038 (port Windows download.rs E11): reject redirects that downgrade https->http or go off the host allowlist; enforce https-only + a host allowlist + a hop cap; ensure CA pinning applies to every hop (currently stops applying off appliesToHosts); require a SHA256 pin for every artifact incl. VLM non-LFS files (no Optional-with-no-gate).' },
  { group: 'C3-PERSIST', files: `${AE}Storage/DBWriter.swift, ${AE}Pipeline/Tagging.swift, ${AE}VisionWorker.swift`,
    ids: ['F-C3-001','F-C3-036'],
    hint: 'CRITICAL 001 (port the three Windows dbwriter gates): add tagsEvaluated / facesEvaluated / ocrStageRan flags to the TaggedFile the pipeline produces, set TRUE only when the Vision/OCR/face stage actually returned (VisionWorker runPrimaryPass didReturn==true) — NOT on a timeout. Gate the three unconditional DELETEs in DBWriter.insertOne (DELETE FROM tags source=auto / DELETE FROM face_prints / DELETE FROM ocr_text) behind the matching flag, so a Vision/ANE/OCR timeout on a rescan does NOT wipe prior auto-tags, OCR text, or — critically — manual person_id assignments. This is the macOS port of the proven Windows tags_evaluated/faces_evaluated/ocr_stage_ran guards. 036: a Vision-timeout image must NOT be persisted failed=false-and-empty in a way that the unchanged-skip then strands it forever — mark it for reprocessing (e.g. leave failed semantics so the next scan retries) so a first-scan timeout is recoverable.' },
  { group: 'C3-CORE', files: `${AE}FileIDEngineMain.swift, ${AE}ScanCoordinator.swift, ${AE}Storage/Database.swift, ${AE}IPC/IPCSink.swift`,
    ids: ['F-C3-029','F-C3-030','F-C3-031','F-C3-032','F-C3-039','F-C3-040','F-C3-021-wiring'],
    hint: '030 (subsumes 029): generalize the IPCSink full-buffer eviction so removeFirst() never drops a PINNED terminal event — pin ALL terminal completions (scanComplete, deepAnalyzeComplete, faceClusteringComplete, restructureApplyResult, error), evict only progress-class events. 031: a cancelled/aborted scan must still write its final scan_sessions status — GRDB task cancellation makes the pool.write throw; perform the final-status write on an uncancellable path (detached/shielded) so sessions are not left "running"/mislabeled "crashed". 032: startScan rejected for db-unavailable must emit a scan-terminal event (not just an error) so the app auto-pilot is not stranded. 039: add PRAGMA cache_spill=0 to the writer connection (Windows parity; prevents mid-transaction temp spill). 040: drain/flush the IPCSink before Darwin._exit(0) so a buffered terminal event is not lost on shutdown. F-C3-021-wiring: replace the planRestructure/applyRestructure IPC handlers that currently return not_implemented_yet with calls to the engine butler API — Restructure.proposeAll(database:libraryRoot:) -> [RestructureProposal] for planRestructure (emit restructurePlan), and Restructure.apply(proposals:database:libraryRoot:) for applyRestructure (emit restructureApplyResult). Those functions exist in Pipeline/Restructure.swift (C3-RESTRUCTURE owns + is improving their internals in parallel — call the existing signatures; do not edit Restructure.swift). Match the Windows commands/restructure.rs handler shape + the schema payloads. If you genuinely need a face-clustering cancel flag for C3-FACES 042, expose a shared signal here.' },
  { group: 'C3-RESTRUCTURE', files: `${AE}Pipeline/Restructure.swift, ${AE}Pipeline/RestructureSemantic.swift, ${AA}Views/RestructureView.swift`,
    ids: ['F-C3-009','F-C3-010','F-C3-011','F-C3-012','F-C3-013','F-C3-014','F-C3-015','F-C3-016','F-C3-017','F-C3-018','F-C3-019','F-C3-020','F-C3-021','F-C3-035'],
    hint: 'The butler port (user ruling 2: port the Windows butler into the macOS ENGINE, route the app through it, retire the app-side classifier). Port from platforms/windows/src/engine/src/{commands/restructure.rs, pipeline/restructure.rs, pipeline/restructure_semantic.rs, pipeline/restructure_apply.rs}. Improve Restructure.proposeAll + Restructure.apply IN PLACE (keep their signatures stable — C3-CORE wires the IPC handlers to them in parallel). 009: apply UPDATE must refresh path_hash (StablePathHash) too (ENG-91). 010: B4 stale-plan guard — re-read live files.path_text==oldPath before moving, else fail. 011 (D-7 ruling): collision = uniquify "name (2).ext" (n=2..9999, occupancy = on-disk lstat ∪ in-batch claimed set) + ENG-42 no-op-skip-before-uniquify — NOT skip+report. 012: a successful moveItem then failed DB UPDATE must record a recovery entry + not double-count moved+failed. 013: sanitize semantic new-group folder names via FilesystemNameSafe.componentSafe + 200-scalar cap (Windows #2). 014: used_group_names dedup + bounded numeric suffix (Windows #9). 015: constrain nearestTwoFolders/existing-folder routing to prototypes inside libraryRoot (Windows E12). 016: Anchor/Mixed/Junk tiering + anchor-move strip in proposeAll (Windows A1/A3). 017: route videos->Videos/<Year>, audio->Audio (not all into Photos/<Year>/<Month>) — Windows canonical. 018: month folder names = full English ("January".."December") — Windows canonical. 019: wire category labels = Windows lowercase vocabulary ("document"/"photo"/"video"/"audio"/"misc"). 020: missing-timestamp year handling = Windows (coerce to the same bucket Windows uses; do not silently omit). 021 (app side): make RestructureView send planRestructure to the engine + consume the restructurePlan/restructureApplyResult events (route through the engine like Windows), and retire/disable the app-side RestructureEngine classifier path. 035: the namedPerson Mixed gate must measure the RIGHT (target) person homogeneity. Pin behavior with Swift Testing tests for month names, category vocab, collision uniquify, sanitization, B4, path_hash.' },
  { group: 'C3-HW-TAGUNDO', files: `${AE}Hardware.swift, ${AS}TagWriter.swift`,
    ids: ['F-C3-045','F-C3-034'],
    hint: '045: Hardware.workerCap Intel fallback counts SMT logical threads as P-cores and lacks the Windows logical-core clamp (1.5x oversubscription on Intel Macs) — derive a correct physical/performance-core count and clamp like Windows. 034: the tag-undo journal (in TagWriter) must be rewritten/cleared on an all-unchanged batch so "Undo last tags" cannot strip a DIFFERENT earlier batch; add an age/file-identity guard like the rename journal it mirrors.' },
]

phase('Fix')
log(`Wave C3: ${GROUPS.length} disjoint macOS-engine fixers, ${GROUPS.reduce((n,g)=>n+g.ids.length,0)} finding-slots`)

const results = await parallel(GROUPS.map(g => () =>
  agent(
    `You are fixer ${g.group}. You EXCLUSIVELY own these source files (edit ONLY these + your own test files under platforms/apple/Tests/): ${g.files}\nFindings to fix: ${g.ids.join(', ')}\nPer-finding guidance: ${g.hint}\n${COMMON}\nSet group to "${g.group}".`,
    { label: g.group, phase: 'Fix', schema: SCHEMA }
  ).then(r => r || { group: g.group, fixed: [], skipped: g.ids.map(id => ({ id, reason: 'fixer agent died' })), filesChanged: [], notes: 'DEAD' })
))

const ok = results.filter(Boolean)
const fixed = ok.flatMap(r => (r.fixed || []).map(f => ({ ...f, group: r.group })))
const skipped = ok.flatMap(r => (r.skipped || []).map(f => ({ ...f, group: r.group })))
const changed = ok.flatMap(r => r.filesChanged || [])
log(`Wave C3 done: ${fixed.length} fixed, ${skipped.length} skipped, ${changed.length} files touched`)
return { groups: ok, fixed, skipped, filesChanged: changed }

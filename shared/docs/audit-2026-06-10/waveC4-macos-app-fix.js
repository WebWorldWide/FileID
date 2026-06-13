export const meta = {
  name: 'fileid-waveC4-macos-app',
  description: 'Wave C4: 21 macOS-app findings (incl. critical dual-writer) + butler app-routing, in 6 disjoint groups',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint app-file group' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const APP = 'platforms/apple/app/Sources/FileID'

const COMMON = `
REPO ROOT: ${ROOT}. You implement Swift fixes for the FileID macOS APP (SwiftUI, @MainActor @Observable EngineClient; the app spawns the engine and READS the DB via ReadStore, the engine is the single writer). App sources live under ${ROOT}/${APP}. Swift 6 strict concurrency. Default to NO comments except a one-line non-obvious WHY. Tests run only in CI (no Xcode here).

For EACH finding ID you own:
1. Read its full record in ${ROOT}/shared/docs/audit-2026-06-10/findings.json (claim, evidence, files, port_source, ruling, test_to_add).
2. Locate the code by grepping for the symbols in the claim — the finding's file paths are APPROXIMATE (e.g. "SearchStore.swift"/"WriteStore.swift"/"AutoPilot.swift" may not exist; the real files are ReadStore.swift, EngineClient.swift, the *View.swift files, etc.). Trust the symbol, find the real file, and confirm it is one of YOUR owned files before editing.
3. Implement the minimal correct fix. Match SwiftUI/@MainActor idioms in the file. UI writes must stay on the main actor; long DB/merge work must move OFF the main actor (Task.detached or an actor) and publish results back on @MainActor.
4. Add a Swift Testing @Test under platforms/apple/Tests/ (SharedTests or a new app-logic test) ONLY if the logic is unit-testable without SwiftUI/a live engine; UI-state-only fixes are CI-build-verified, so note "no unit test (UI-state)".
5. If a finding is already fixed, or needs a file outside your set, mark it skipped with a precise reason.

HARD RULES:
- Edit ONLY the files in your assigned set. Coordinate with peers ONLY through EngineClient's existing @Observable published state and the IPC schema — never edit a peer's file. If your fix needs a peer's file, implement your half and note the cross-file dependency.
- Do NOT run swift build/test. Do NOT add dependencies. No telemetry.
- GUARDS — never weaken: @MainActor isolation on UI types; the engine remains the single DB writer (the app's only writes are the existing Cleanup/People paths via ReadStore — do not add new app writers); redactPathForLog before logging paths; LavaLampBackground untouched.

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
  { group: 'C4-ENGINECLIENT', files: `${APP}/EngineClient.swift, ${APP}/MainWindow.swift, ${APP}/FileIDApp.swift`,
    ids: ['F-C4-001','F-C4-010','F-C4-011','F-C4-016-ec','F-C4-020'],
    hint: 'CRITICAL 001: "Restart Engine" must TERMINATE the running engine before spawning a new one — two live engines = two SQLite writers = corruption. Ensure restart is a stop-then-start single-flight. 010: the auto-pilot no-faces watchdog must NOT auto-run deepAnalyzeAll on the whole library (contradicts Deep-Analyze-is-opt-in) — gate/remove it (grep autoPilot/auto-pilot/noFaces). 011: shutdown() must not latch expectedExit=true unless .shutdown was actually sent, so a genuine later crash is not masked. 016 (EngineClient half): on engine crash mid-run, publish a signal/reset so in-flight UI flags (DeepAnalyze running, etc.) can clear — expose it via @Observable state for C4-DEEPANALYZE-UI to consume. 020: remove hasActiveScan dead code; make discovery-progress Tasks ordered so a late Task cannot overwrite the final count (grep hasActiveScan / discovery progress).' },
  { group: 'C4-READSTORE', files: `${APP}/ReadStore.swift`,
    ids: ['F-C4-002','F-C4-004','F-C4-006','F-C4-007'],
    hint: '002: ReadStore.Dispose()/teardown must not free the live SqliteConnection out from under an in-flight read (guard with a lock / keep the connection alive until readers drain). 004: the app-side write connection used by Person merge/move/mark-unknown + Cleanup deleteFiles needs a busy_timeout so a brief writer-lock contention does not throw/silently drop (set PRAGMA busy_timeout on the app write connection). 006: the semantic/similarity search full clip_embeddings cosine scan must NOT run synchronously on the MainActor — move it to a background Task/actor and publish results back. 007: the keyword reload() multi-table search query must not run on the MainActor on every throttled batch event — debounce + run off-main.' },
  { group: 'C4-RESTRUCTURE-APP', files: `${APP}/RestructureView.swift, ${APP}/TreeDiffView.swift, ${APP}/RestructureApplyBar.swift, ${APP}/RestructureRecommendationRow.swift`,
    ids: ['F-C3-021-app','F-C4-003','F-C4-009','F-C4-012','F-C4-019','F-C4-021'],
    hint: 'F-C3-021-app (user ruling 2): route the macOS app through the ENGINE butler — RestructureView should SEND the planRestructure IPC command and CONSUME the restructurePlan + restructureApplyResult events (the engine handlers are now wired by C3), and RETIRE/disable the app-side RestructureEngine classifier path (its logic now lives in the engine). 003: double-click on "Convert to real moves" must be single-flight + confirmed — it can permanently move/destroy files; disable the button while applying. 009: TreeDiffView must not eagerly materialize every row into AnyView per render at 100k+ proposals — lazy/virtualize. 012: per-file deselections must survive a regenerate() (do not re-select everything). 019: an Anchor "staying put" source folder must not open an empty "Nothing to show" drill-down. 021: replace bare substring person-name matching with a precise match (or rely on the engine classifier now that the app routes through it).' },
  { group: 'C4-PEOPLE-CLEANUP', files: `${APP}/PeopleView.swift, ${APP}/CleanupView.swift`,
    ids: ['F-C4-013','F-C4-018'],
    hint: '013: drag-to-merge must not run the full merge transaction synchronously on the main thread (freezes UI on large clusters) — dispatch off-main, update UI on completion. 018: the whole-library "Select all non-keepers" selection must not persist by file id across reloads such that a mid-scan keeper change silently re-selects a now-keeper for deletion — re-derive selection against current state.' },
  { group: 'C4-DEEPANALYZE-UI', files: `${APP}/DeepAnalyzeViews.swift`,
    ids: ['F-C4-014','F-C4-016'],
    hint: '014: on Deep Analyze run-start, clear deepAnalyzeLast so a stale caption is not shown beside "Starting…". 016 (UI half): when the engine crashes mid-run (consume the crash signal C4-ENGINECLIENT publishes, or the existing engine-state @Observable), clear the frozen "running" Deep Analyze UI instead of leaving it stuck until a tab switch.' },
  { group: 'C4-SETTINGS-BULK', files: `${APP}/SettingsView.swift, ${APP}/WelcomeSheet.swift, ${APP}/BulkTagSheet.swift, ${APP}/BulkRenameSheet.swift, ${APP}/CLIPTextEncoder.swift`,
    ids: ['F-C4-008','F-C4-005','F-C4-015','F-C4-017'],
    hint: '008: Bulk-tag "Replace existing" permanently wipes user tags with no undo + no confirmation — add a confirmation and ensure the undo journal captures the prior tags. 005: bulk-tag undo / a total-failure bulk rename must not clobber a valid prior undo journal with an empty batch (guard the journal write on non-empty success). 015: reset modelDownloadProgress to nil when a download completes/cancels so the Settings model picker does not show a stale "Downloading 100%". 017: CLIP Uninstall must unload the in-memory text encoder (CLIPTextEncoder) so semantic search stops running against the deleted model.' },
]

phase('Fix')
log(`Wave C4: ${GROUPS.length} disjoint macOS-app fixers, ${GROUPS.reduce((n,g)=>n+g.ids.length,0)} finding-slots`)

const results = await parallel(GROUPS.map(g => () =>
  agent(
    `You are fixer ${g.group}. You EXCLUSIVELY own these app files (edit ONLY these + your own test files under platforms/apple/Tests/): ${g.files}\nFindings: ${g.ids.join(', ')}\nGuidance: ${g.hint}\n${COMMON}\nSet group to "${g.group}".`,
    { label: g.group, phase: 'Fix', schema: SCHEMA }
  ).then(r => r || { group: g.group, fixed: [], skipped: g.ids.map(id => ({ id, reason: 'fixer agent died' })), filesChanged: [], notes: 'DEAD' })
))

const ok = results.filter(Boolean)
const fixed = ok.flatMap(r => (r.fixed || []).map(f => ({ ...f, group: r.group })))
const skipped = ok.flatMap(r => (r.skipped || []).map(f => ({ ...f, group: r.group })))
log(`Wave C4 done: ${fixed.length} fixed, ${skipped.length} skipped`)
return { groups: ok, fixed, skipped, filesChanged: ok.flatMap(r => r.filesChanged || []) }

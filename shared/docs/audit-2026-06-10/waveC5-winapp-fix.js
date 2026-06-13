export const meta = {
  name: 'fileid-waveC5-winapp',
  description: 'Wave C5: 12 Windows C#/WinUI app findings in 6 disjoint file groups (CI-verified only)',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint C# file group' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const APP = 'platforms/windows/src/FileID.App'

const COMMON = `
REPO ROOT: ${ROOT}. You implement C#/WinUI 3 fixes for the FileID Windows app (.NET 8, unpackaged desktop, MVVM, C#/XAML). App sources under ${ROOT}/${APP}. THIS CANNOT BE BUILT OR TESTED LOCALLY — verification is CI-only (windows-app.yml). So your fix MUST be a careful, pattern-matched edit to EXISTING files: read the surrounding code and mirror its idioms exactly. Default to NO comments except a one-line non-obvious WHY.

WinUI conventions you MUST follow (from platforms/windows/CLAUDE.md — violating these is a native fast-fail):
- Every EngineClient.PropertyChanged handler stays wrapped in DebugLog.SafeRun; do NOT strip SafeRun or the [APPLY:N] tracing.
- Post XAML writes through DispatcherQueue.TryEnqueue; never construct DispatcherObject-derived types (BitmapImage, SolidColorBrush, …) on a thread you didn't capture.
- Cache UI-thread-affined resources (brushes) at ctor time, not per event.
- Never imperatively mutate a XAML parent's Children mid-event-burst — own a stable container and mutate only its Children.
- Long work (DB/merge/IO) runs off the UI thread (Task.Run) with results marshaled back via DispatcherQueue.

For EACH finding ID you own:
1. Read its full record in ${ROOT}/shared/docs/audit-2026-06-10/findings.json.
2. Locate the code by grepping for the symbols in the claim — finding paths are APPROXIMATE (e.g. "RestructureViewModel.cs"/"BulkTagViewModel.cs"/"SankeyView.xaml.cs"/"PipelineProgressView.xaml.cs" may be RestructureView.xaml.cs / BulkTagSheet.xaml.cs / SankeyFlowControl.cs / SidebarPipelineProgress.xaml.cs). Confirm the real file is in YOUR set before editing.
3. Implement the minimal correct fix matching existing patterns.
4. Add an xUnit test under platforms/windows/Tests/FileID.App.Tests/ ONLY if the logic is unit-testable without the UI runtime (pure VM/helper logic); UI-thread/XAML fixes are CI-build-verified — note "no unit test (UI-runtime)".
5. If already fixed or needs a file outside your set, skip with a precise reason.

HARD RULES:
- Edit ONLY your assigned files. Coordinate only through existing public members + the IPC schema. Do NOT add NuGet packages. No telemetry.
- Do NOT edit any .csproj or add new files unless strictly required (new .cs files need a UTF-8 BOM — avoid by editing existing files).
- Preserve the gold #FFCC00 palette, LavaLampBackground, springs.

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
  { group: 'C5-READSTORE', files: `${APP}/Services/ReadStore.cs`,
    ids: ['F-C5-001'],
    hint: '001: ReadStore.Dispose() must not free the live SqliteConnection out from under an in-flight thread-pool read — guard the connection lifecycle (lock / refcount / drain) so disposal waits for or excludes active readers (mirror the macOS F-C4-002 reasoning).' },
  { group: 'C5-RESTRUCTURE', files: `${APP}/Views/RestructureView.xaml.cs, ${APP}/Views/DrillDownSheet.xaml.cs, ${APP}/Theme/SankeyFlowControl.cs`,
    ids: ['F-C5-002','F-C5-003','F-C5-010'],
    hint: '003: no double-apply prevention on Apply — re-clicking after a successful real-move apply re-applies a now-stale plan; make Apply single-flight (disable while applying / guard a busy flag). 002: DrillDownSheet eagerly builds one UIElement + fires one shell thumbnail per move uncapped — virtualize / cap so a huge group does not hang the UI. 010: Sankey Render() makes ~4 full O(Moves) passes with a String.Split+Substring per move on the UI thread — precompute once / reduce passes / move parsing off the hot render path. (Locate SankeyFlowControl.cs for the Sankey render.)' },
  { group: 'C5-BULK-LIBRARY', files: `${APP}/Views/BulkTagSheet.xaml.cs, ${APP}/Views/LibraryView.xaml.cs`,
    ids: ['F-C5-004','F-C5-012'],
    hint: '004: Bulk-tag "Replace existing" permanently wipes user tags with no undo + no confirmation — add a confirmation and ensure the prior tags are captured for undo (the grid also needs to reflect the change). 012: the selection toolbar goes stale when a background refresh prunes selected tiles — re-validate the selection set against current items after a refresh.' },
  { group: 'C5-SETTINGS', files: `${APP}/Views/SettingsView.xaml.cs`,
    ids: ['F-C5-005','F-C5-008'],
    hint: '008: the "Auto-install CUDA acceleration" toggle is dead (DisableAutoInstallCuda is read nowhere) — wire it to actually gate the CUDA auto-install (or remove the control if the setting is obsolete; prefer wiring). 005 (C# side): hardwareReprobed.execution_provider shows a fresh probe, not the engine\\u2019s actually-bound EP — display the bound EP if the event carries it, else label it clearly as "detected (not necessarily active)"; note that the authoritative fix is engine-side (report the bound EP) if the event lacks it.' },
  { group: 'C5-PEOPLE-CLEANUP', files: `${APP}/Views/PersonDetailSheet.xaml.cs, ${APP}/Views/PeopleView.xaml.cs, ${APP}/Views/CleanupView.xaml.cs`,
    ids: ['F-C5-006','F-C5-011'],
    hint: '006: PersonDetailSheet rename can write the name onto a DIFFERENT person (or no-op) after a background refresh swapped the bound instance — capture the target person identity at edit start and write by id, validating it still exists. 011: multi-select selection silently dropped when a refresh replaces a selected cluster instance — key selection by stable id and re-project after refresh.' },
  { group: 'C5-LOG-PROGRESS', files: `${APP}/Services/DebugLog.cs, ${APP}/Views/SidebarPipelineProgress.xaml.cs`,
    ids: ['F-C5-007','F-C5-009'],
    hint: '007: DebugLog truncation check + truncating rewrite run OUTSIDE s_writeLock — concurrent writes can interleave/corrupt; move the size-check+truncate inside the lock (keep the synchronous-write durability the project requires; do NOT make it async). 009: the pipeline progress strip blanks to grey on every app launch of an existing library (no DB-derived initial state) — initialize it from the persisted/last-known scan state so it does not flash empty.' },
]

phase('Fix')
log(`Wave C5: ${GROUPS.length} disjoint C# fixers, ${GROUPS.reduce((n,g)=>n+g.ids.length,0)} finding-slots (CI-verified)`)

const results = await parallel(GROUPS.map(g => () =>
  agent(
    `You are fixer ${g.group}. You EXCLUSIVELY own these C# files (edit ONLY these): ${g.files}\nFindings: ${g.ids.join(', ')}\nGuidance: ${g.hint}\n${COMMON}\nSet group to "${g.group}".`,
    { label: g.group, phase: 'Fix', schema: SCHEMA }
  ).then(r => r || { group: g.group, fixed: [], skipped: g.ids.map(id => ({ id, reason: 'fixer agent died' })), filesChanged: [], notes: 'DEAD' })
))

const ok = results.filter(Boolean)
const fixed = ok.flatMap(r => (r.fixed || []).map(f => ({ ...f, group: r.group })))
const skipped = ok.flatMap(r => (r.skipped || []).map(f => ({ ...f, group: r.group })))
log(`Wave C5 done: ${fixed.length} fixed, ${skipped.length} skipped`)
return { groups: ok, fixed, skipped, filesChanged: ok.flatMap(r => r.filesChanged || []) }

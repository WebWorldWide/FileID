export const meta = {
  name: 'fix-diff-reaudit',
  description: 'Refute-by-default re-audit of the 11-fix working-tree diff for fix-introduced regressions + completeness critic',
  phases: [{ title: 'Reaudit', detail: 'per-slice regression finders over the fix diff' }, { title: 'Verify', detail: 'refute-by-default verify' }, { title: 'Critic', detail: 'completeness critic' }],
}

const REPO = 'C:/Users/adamm/Desktop/Code/FileID'

const FINDINGS_SCHEMA = {
  type: 'object', additionalProperties: false, required: ['findings'],
  properties: { findings: { type: 'array', items: {
    type: 'object', additionalProperties: false,
    required: ['title','severity','file','line','description','impact','fix','confidence'],
    properties: {
      title: { type: 'string' }, severity: { type: 'string', enum: ['critical','high','medium','low'] },
      file: { type: 'string' }, line: { type: 'integer' }, description: { type: 'string' },
      impact: { type: 'string' }, fix: { type: 'string' }, confidence: { type: 'number' },
    } } } },
}
const VERDICT_SCHEMA = {
  type: 'object', additionalProperties: false, required: ['verdict','reasoning','real_severity'],
  properties: {
    verdict: { type: 'string', enum: ['confirmed','refuted','uncertain'] },
    reasoning: { type: 'string' }, real_severity: { type: 'string', enum: ['critical','high','medium','low','none'] },
    corrected_fix: { type: 'string' },
  },
}

const CONTEXT = `You are RE-AUDITING an uncommitted working-tree change to the FileID Windows app (git repo at ${REPO}; run "git -C ${REPO} diff" to see ALL changes). A bug-audit just landed 11 fixes; your job is to find any NEW bug the FIXES introduced (a regression, a broken edge case, a compile-passing-but-wrong change, a deadlock, an off-thread write, a logic error). The full gate is already green (engine clippy -D + 266 tests; app build 0/0; engine fmt) so do NOT report compile errors — hunt for SEMANTIC regressions the gate can't catch. The 11 fixes are: (A1) PeopleViewModel.RefreshAsync + (A2) CleanupViewModel.RefreshAsync now wrap IsLoading/ErrorMessage writes in an OnUi() dispatcher marshal; (A3) EngineClient.OnProcessExited bails early if sender != _process (stale-exit guard); (RD1) EngineClient ErrorEvent handler releases the auto-cluster gate only on Kind=="face_clustering_failed" (was Contains("cluster")); (IPC1) EngineClient MaxFrameChars 1MiB->32MiB + a StdoutFraming.OversizeDropped flag that emits a synthetic ipc_frame_too_large ErrorEvent via Apply; (RD4) EngineClient.Commands WaitForMergeSuggestionsAsync no longer resets LastMergeSuggestions=null; (E1) engine wipe.rs waits on face_cluster_active before wiping (+main.rs threads it in); (E2) deep_analyze.rs single-file failure derives cancelled from cancel.load; (E3) restructure_apply.rs has_reparse_point_in_chain normalizes both operands via strip_extended_length; (E4) restructure_semantic.rs dedups group folders on the sanitized name. Use git diff + Read to inspect the ACTUAL post-fix code. Default to "refuted"; report only a REAL regression with file:line and a concrete failure path. Empty is the expected, good outcome.`

const SLICES = [
  { key: 'app_threading', prompt: `${CONTEXT}\n\nSLICE — App threading fixes (A1, A2, A3). Examine platforms/windows/src/FileID.App/ViewModels/PeopleViewModel.cs, CleanupViewModel.cs, EngineClient.cs (OnProcessExited ~748). For A1/A2: is the new OnUi() correct (no infinite recursion, no deadlock, _disposed re-checked, prologue/catch/finally semantics preserved, does wrapping the prologue change ordering vs the await)? Could OnUi enqueue run AFTER Dispose tore down state? For A3: does the sender!=_process guard ever WRONGLY bail in the normal crash→respawn path (which must still Cleanup + respawn)? Does removing the dead process's handler / disposing it break anything? Is there a path where the LIVE engine is now NOT cleaned up when it should be (e.g. a genuine crash mis-classified as stale)? Could _process be null at the guard and mis-handle?` },
  { key: 'app_ipc', prompt: `${CONTEXT}\n\nSLICE — App IPC fixes (RD1, IPC1, RD4). Examine EngineClient.cs (ErrorEvent handler ~1064, StdoutFraming, ReadBoundedFrameAsync, StdoutLoopAsync) and EngineClient.Commands.cs (WaitForMergeSuggestionsAsync ~667). RD1: does Kind=="face_clustering_failed" still release the gate on EVERY genuine clustering failure the engine emits (grep the engine for the exact kinds it sends), or did the exact match MISS a failure kind that the old Contains caught (e.g. a JoinError/panic path emitting a different string), leaving the gate stuck? IPC1: is the 32MiB cap safe on a 4GB box (transient StringBuilder)? Does the OversizeDropped synthetic ErrorEvent fire correctly (flag reset, runs once, Apply on UI thread), and does Apply's ErrorEvent handler now mishandle kind "ipc_frame_too_large" (e.g. wrongly release a gate / set a bad state)? RD4: with the null-reset removed, does WaitForMergeSuggestionsAsync still ALWAYS complete (the handler fires only on PropertyChanged; could a value-equal reply now fail to fire and hang the 30s wait)? Verify MergeSuggestions equality semantics (Pairs is a List).` },
  { key: 'engine_fixes', prompt: `${CONTEXT}\n\nSLICE — Engine fixes (E1, E2, E3, E4, RD2). Examine platforms/windows/src/engine/src/commands/wipe.rs, main.rs (WipeLibrary arm ~807), commands/deep_analyze.rs (~167), pipeline/restructure_apply.rs (has_reparse_point_in_chain ~462), pipeline/restructure_semantic.rs (dedup ~186-218), models/bge_text.rs (~56). E1: can the new face_cluster_active wait ever NOT terminate (flag never cleared ⇒ wipe hangs forever)? Does it deadlock vs the clustering persist? Is 5s enough / what if clustering exceeds it (does wipe then still race)? Is face_cluster_active correctly threaded (right Arc, right ordering)? E3: does strip_extended_length(parent) when parent is already non-verbatim behave (no-op)? Does the ancestor walk now actually terminate at root and check every level? Any case where it loops forever or wrongly returns false? E4: can the dedup while-loop spin forever (does safe change each iteration)? Does inserting 'safe' instead of 'pretty' break any later use of used_group_names? E2: is 'cancel' the right flag, Ordering correct? RD2: is cpu_topology().p_cores ever 0 (the .max(1) guards it) and does forcing intra threads after configure_session_builder conflict with anything?` },
]

phase('Reaudit')
log(`Fix-diff re-audit: ${SLICES.length} regression slices, refute-by-default`)

const perSlice = await pipeline(
  SLICES,
  (s) => agent(s.prompt, { label: `reaudit:${s.key}`, phase: 'Reaudit', schema: FINDINGS_SCHEMA }),
  (review, s) => parallel(((review && review.findings) || []).map((f) => () =>
    agent(`${CONTEXT}\n\nIndependently VERIFY this claimed FIX-INTRODUCED regression by reading the post-fix code at ${f.file}:${f.line} (and git diff). Try HARD to refute — confirm ONLY if the fix genuinely introduced a real, reachable regression. Default = "refuted".\n\nFINDING:\n${JSON.stringify(f)}`,
      { label: `verify:${s.key}`, phase: 'Verify', schema: VERDICT_SCHEMA })
      .then((v) => ({ ...f, slice: s.key, verdict: v }))
      .catch(() => null)
  ))
)

const all = perSlice.flat().filter(Boolean)
const confirmed = all.filter((x) => x.verdict && x.verdict.verdict === 'confirmed')

phase('Critic')
const critic = await agent(`${CONTEXT}\n\nYou are the COMPLETENESS CRITIC. The re-audit above confirmed ${confirmed.length} regressions: ${JSON.stringify(confirmed.map((c) => ({ title: c.title, file: c.file, line: c.line })))}. Independently sanity-check the WHOLE 11-fix diff (git -C ${REPO} diff) one more time for anything the slice finders may have MISSED: an inconsistency between a fix and its surrounding code, a fix that only half-solves its bug, a new edge case, a comment that now lies, a test that should have been added/updated but wasn't. Report only genuinely missed, real issues.`,
  { label: 'completeness-critic', phase: 'Critic', schema: FINDINGS_SCHEMA })

log(`Re-audit done: ${confirmed.length} confirmed regressions; critic flagged ${((critic && critic.findings) || []).length}`)
return { area: 'fix_reaudit', confirmed, critic_findings: (critic && critic.findings) || [], raw_count: all.length }

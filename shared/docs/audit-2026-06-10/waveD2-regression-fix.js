export const meta = {
  name: 'fileid-waveD2-regression-fix',
  description: 'Stage D fix round: repair the 22 confirmed regressions the campaign fixes introduced',
  phases: [{ title: 'Fix', detail: 'one fixer per disjoint file group' }],
}
const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const COMMON = `
REPO ROOT: ${ROOT}. You repair REGRESSIONS that the audit-fix campaign introduced (confirmed by an adversarial re-audit). Read the full record for each of YOUR finding ids in ${ROOT}/shared/docs/audit-2026-06-10/reaudit-confirmed.json (title, claim, evidence with file:line, suggested_fix). Apply the suggested_fix (or a better equivalent), verifying it against the live code first. Edit ONLY your assigned files. Preserve the ORIGINAL audit fix's intent — you are correcting its side-effect, not reverting it. Preserve all deliberate guards. Default to no comments except a one-line non-obvious why. Do NOT run swift build (Swift groups; verified centrally). Rust groups MAY run \`cd ${ROOT}/platforms/windows/src/engine && cargo clippy --all-targets -- -D warnings && cargo test\` to self-verify. No new deps, no telemetry. Add/adjust a regression test where the fix is unit-assertable.
Return the structured result.`
const SCHEMA = { type:'object', required:['group','fixed','skipped'], properties:{
  group:{type:'string'},
  fixed:{type:'array',items:{type:'object',required:['id','summary'],properties:{id:{type:'string'},summary:{type:'string'}}}},
  skipped:{type:'array',items:{type:'object',required:['id','reason'],properties:{id:{type:'string'},reason:{type:'string'}}}},
  filesChanged:{type:'array',items:{type:'string'}}, notes:{type:'string'} } }

const WE='platforms/windows/src/engine/src'; const AE='platforms/apple/engine/Sources/FileIDEngine'; const APP='platforms/apple/app/Sources/FileID'; const AS='platforms/apple/shared/Sources/FileIDShared'; const CA='platforms/windows/src/FileID.App'
const GROUPS = [
  { group:'RG1-rust-cmds', files:`${WE}/scan_session.rs, ${WE}/commands/trash.rs, ${WE}/main.rs`,
    steer:'For the scan_session.rs C1-002 regression use suggested_fix OPTION (a): do NOT emit ScanComplete on the Failed branch — keep the (now guaranteed-send) phaseChanged(Failed) as the sole terminal frame, so the app cannot relabel a failed scan as Completed. trash.rs: make the batch-restore HashSet use [System.StringComparer]::OrdinalIgnoreCase. main.rs: a transient get_parent_pid()==None re-sample must NOT notify_waiters() (shutdown) — fall back to platform::watch_parent like the OpenProcess-failure branch.' },
  { group:'RG2-rust-models', files:`${WE}/models/registry.rs, ${WE}/downloader.rs, ${WE}/platform.rs`,
    steer:'registry.rs: a missing revision-keyed sentinel must recognize/migrate a legacy {id}.installed (or SHA-short-circuit) so the rename does NOT force a multi-GB re-download of already-installed models. downloader.rs: an oversized stale part contributes 0 to resume_seed_bytes (it is discarded), so progress cannot exceed 100%. platform.rs: home-anchor redaction must not over-redact — only treat parts[anchor+1] as a username when at least one component follows it (never collapse a path+filename to "…").' },
  { group:'RG3-mac-faces', files:`${AE}/Pipeline/FaceClustering.swift`,
    steer:'HIGH: a standalone/auto face-clustering job must not be gated on the SCAN cancel mirror (it stays set after a scan cancel, making manual clustering a silent no-op). Give clustering its own cancellation scope (or clear/ignore the scan mirror at clustering start) so runFaceClustering works after a cancelled scan. Keep the F-C3-042 mid-pass cooperative-cancel for the clustering job itself.' },
  { group:'RG4-mac-discovery', files:`${AE}/Pipeline/Discovery.swift`,
    steer:'HIGH orphan-sweep: the F-C6-001 skip must not break orphan pruning — on a skip, still bump the row scanned_at to the current scan time (cheap UPDATE/touch) so sweepOrphans does not treat skipped-but-present files as deleted (and still prunes genuinely-deleted ones). MEDIUM predicate: match DBWriter unchanged contract — skip only when current mtime EQUALS stored modified_at (and size matches); load modified_at into SkipEntry. LOW st_ino: corroborate the rename-heal identity with size (and content_hash if available) so APFS inode reuse cannot re-bind a deleted row onto an unrelated new file.' },
  { group:'RG5-mac-da-tagging', files:`${AE}/Pipeline/DeepAnalyze.swift, ${AE}/Pipeline/Tagging.swift`,
    steer:'DeepAnalyze loadTask: cancelPrewarm()/requestCancel() must not collaterally cancel a concurrent same-model run/prewarm — ref-count waiters or scope cancellation per caller (only cancel the shared load when the LAST waiter cancels). DeepAnalyze negated-same: only apply the negated-SAME override in the loose branch, never override an explicit "VERDICT: SAME" line. Tagging.swift loadImageAndEXIF: a 0/absent discovered sizeBytes means UNKNOWN, not tiny — fall back to a stat (or skip the <256 guard) instead of marking a valid image decode-failed.' },
  { group:'RG6-mac-dbwriter-sink', files:`${AE}/Storage/DBWriter.swift, ${AE}/IPC/IPCSink.swift`,
    steer:'DBWriter CLIP-backfill: F-C6-001 discovery skip makes the unchanged-file CLIP-backfill path unreachable, so a post-CLIP-install rescan never backfills embeddings. Coordinate with discovery (the skip set should EXCLUDE embeddable images lacking a clip_embeddings row) — implement the DBWriter/query side: provide a predicate/query the discovery skip can use (NOT EXISTS clip_embeddings for image kind), so such files are still processed. IPCSink: add restructurePlan to the pinned/critical terminal set (isCritical + criticalNeedles) so the planRestructure reply is never evicted, matching the all-terminals-pinned invariant.' },
  { group:'RG7-mac-app', files:`${APP}/Views/LibraryView.swift, ${APP}/EngineClient.swift, ${APP}/Views/RestructureView.swift`,
    steer:'LibraryView tile tag-dots: in-app tag edits during a live scan must still refresh tiles — track an edit-only epoch (bumped by tag/undo writes) NOT frozen during a scan, combined into the tile id. LibraryView tag-undo: consume the new undoBulkAdd `skipped` count — gate the journal clear on failed==0 && skipped==0 and surface skipped in undoStatus (mirror the rename path). EngineClient.start(): do not block the @MainActor on proc.waitUntilExit() unbounded — bound it (poll isRunning with timeout then SIGKILL) or reap off-main. RestructureView spinner: treat kind=="ipc_frame_too_large" as a terminal plan failure (clear loading) and key error-recovery on lastError identity/kind, not message equality.' },
  { group:'RG8-mac-download', files:`${AS}/StreamingDownload.swift`,
    steer:'checkSizePlausible must NOT run after a passing SHA256 (it can delete a hash-verified file) — only size-gate when expectedSHA256 == nil. preflightDiskSpace over-estimates 2x on the single-stream path — parameterize the multiplier (2x parallel, 1x single-stream).' },
  { group:'RG9-cs-app', files:`${CA}/Views/Restructure/RestructureView.xaml.cs`,
    steer:'The post-apply re-plan Task is fire-and-forget so its catch is dead code and the single-flight guard can stick on a faulted re-plan (exception silently swallowed). Observe the Task (ContinueWith on the UI dispatcher) so the guard is released and the error logged on fault. CI-only verification — match existing SafeRun/DispatcherQueue patterns.' },
]
phase('Fix')
log(`Regression fix: ${GROUPS.length} groups, 22 findings`)
const results = await parallel(GROUPS.map(g => () =>
  agent(`You are fixer ${g.group}. Own ONLY: ${g.files}. Find YOUR finding ids in reaudit-confirmed.json (the ones whose file is in your set). Steer: ${g.steer}\n${COMMON}\nSet group to "${g.group}".`,
    { label:g.group, phase:'Fix', schema:SCHEMA }).then(r => r || {group:g.group,fixed:[],skipped:[{id:'?',reason:'dead'}],filesChanged:[]})
))
const ok=results.filter(Boolean)
return { fixed: ok.flatMap(r=>(r.fixed||[]).map(f=>({...f,group:r.group}))), skipped: ok.flatMap(r=>(r.skipped||[]).map(f=>({...f,group:r.group}))), filesChanged: ok.flatMap(r=>r.filesChanged||[]) }

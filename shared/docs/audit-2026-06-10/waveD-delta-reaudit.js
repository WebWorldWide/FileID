export const meta = {
  name: 'fileid-waveD-delta-reaudit',
  description: 'Stage D: regression sweep over the campaign diff (main...HEAD) — find bugs the FIXES introduced',
  phases: [{ title: 'Review', detail: 'one reviewer per subsystem diff' }, { title: 'Verify', detail: 'adversarial check per finding' }],
}

const ROOT = '/Users/adamnolle/Desktop/Code/FileID'

const COMMON = `
REPO ROOT: ${ROOT}. You are doing a REGRESSION RE-AUDIT of an audit-fix campaign (branch fix/audit-2026-06-10, 32 commits). Examine ONLY what the campaign CHANGED: run \`cd ${ROOT} && git diff main...HEAD -- <your files>\` and review the diff. Your job: find bugs the FIXES introduced — NOT pre-existing issues the fixes were meant to resolve, and NOT new feature requests.

Look for: (1) regressions — a fix that breaks a previously-working path; (2) a WEAKENED deliberate guard (tags_evaluated/faces_evaluated/ocr_stage_ran, B3/B4/SEC-3/SEC-5, MOVEFILE no-REPLACE, strip_extended_length, redactPathForLog, single-writer DB, @MainActor isolation, terminal-event pinning, NFC path_search, db_newer_than_engine); (3) cross-wave INTERACTION bugs — e.g. a caller updated in one wave that mismatches a signature changed in another, a new field not populated end-to-end, an event published but never consumed (or vice-versa); (4) concurrency hazards introduced by a fix (await across a lock, !Send across await, actor reentrancy, dropped terminal events); (5) data-loss or crash a fix could cause (an unguarded DELETE/move, a force-unwrap, a panic/.expect, a silent error swallow).

The code BUILDS (cargo clippy -D warnings + cargo test green on Rust; swift build clean; C# is CI-only). So compile errors are already gone — hunt LOGIC/semantic regressions. Report only defensible, code-level findings (file:line, the specific changed lines, mechanism, trigger, impact). If the diff for your files is clean, return findings: [] and clean: true.`

const FIND = {
  type: 'object', required: ['group', 'findings'],
  properties: {
    group: { type: 'string' }, clean: { type: 'boolean' },
    findings: { type: 'array', items: { type: 'object', required: ['title','file','severity','claim','evidence'],
      properties: { title:{type:'string'}, file:{type:'string'}, line:{type:'integer'}, severity:{enum:['critical','high','medium','low']}, category:{type:'string'}, claim:{type:'string'}, evidence:{type:'string'}, suggested_fix:{type:'string'} } } },
  },
}
const VERDICT = { type:'object', required:['refuted','reasoning'], properties:{ refuted:{type:'boolean'}, reasoning:{type:'string'}, severityAdjust:{enum:['critical','high','medium','low','unchanged']} } }

const G = [
  { id:'D-win-engine-cmds', files:'platforms/windows/src/engine/src/commands/ platforms/windows/src/engine/src/main.rs platforms/windows/src/engine/src/scan_session.rs platforms/windows/src/engine/src/sleep_guard.rs' },
  { id:'D-win-engine-pipeline', files:'platforms/windows/src/engine/src/pipeline/' },
  { id:'D-win-engine-models-ipc', files:'platforms/windows/src/engine/src/models/ platforms/windows/src/engine/src/ipc/ platforms/windows/src/engine/src/platform.rs platforms/windows/src/engine/src/downloader.rs platforms/windows/src/engine/src/db/' },
  { id:'D-mac-engine-faces', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/IdentityClustering.swift platforms/apple/engine/Sources/FileIDEngine/Models/HNSWIndex.swift' },
  { id:'D-mac-engine-pipeline', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/Discovery.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyzeRunner.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift platforms/apple/engine/Sources/FileIDEngine/VisionWorker.swift' },
  { id:'D-mac-engine-restructure-storage', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/Restructure.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/RestructureSemantic.swift platforms/apple/engine/Sources/FileIDEngine/Storage/DBWriter.swift platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift platforms/apple/engine/Sources/FileIDEngine/IPC/IPCSink.swift platforms/apple/engine/Sources/FileIDEngine/Hardware.swift' },
  { id:'D-mac-shared', files:'platforms/apple/shared/Sources/FileIDShared/' },
  { id:'D-mac-app', files:'platforms/apple/app/Sources/FileID/' },
  { id:'D-cs-app', files:'platforms/windows/src/FileID.App/ platforms/windows/src/FileID.IpcSchema/' },
  { id:'D-ipc-schema', files:'shared/ipc-schema/ipc.schema.json' },
]

phase('Review')
log(`Delta re-audit over ${G.length} subsystem diffs`)
const results = await pipeline(G,
  g => agent(`Re-audit group ${g.id}. Files: ${g.files}\n${COMMON}\nSet group to "${g.id}".`, { label:`review:${g.id}`, phase:'Review', schema:FIND }),
  (rev, g) => {
    if (!rev || !rev.findings || !rev.findings.length) return { group:g.id, confirmed:[], rejected:[] }
    return parallel(rev.findings.map(f => () =>
      agent(`Adversarially verify this REGRESSION claim about a FileID audit-fix (root ${ROOT}). Read \`git diff main...HEAD -- ${f.file}\` and the cited code. Is this a REAL new bug the fix introduced (not pre-existing, not already guarded)? Default refuted=true unless you can reproduce the logic chain. Finding: ${JSON.stringify(f)}`,
        { label:`verify:${g.id}`, phase:'Verify', schema:VERDICT })
        .then(v => ({ ...f, group:g.id, confirmed: v ? !v.refuted : false, reason: v?.reasoning, sev: (v?.severityAdjust && v.severityAdjust!=='unchanged') ? v.severityAdjust : f.severity })))
    ).then(j => { const ok=j.filter(Boolean); return { group:g.id, confirmed:ok.filter(x=>x.confirmed), rejected:ok.filter(x=>!x.confirmed) } })
  }
)
const all = results.filter(Boolean)
const confirmed = all.flatMap(r => r.confirmed||[])
const rejected = all.flatMap(r => r.rejected||[])
log(`Delta re-audit: ${confirmed.length} confirmed regressions, ${rejected.length} rejected`)
return { confirmed, rejected, cleanGroups: all.filter(r=>!(r.confirmed||[]).length).map(r=>r.group) }

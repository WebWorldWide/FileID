export const meta = {
  name: 'fileid-waveD3-reaudit-round2',
  description: 'Stage D round 2: regression sweep over the REGRESSION-FIX diff (0852024..HEAD) — bugs the fixes-to-the-fixes introduced',
  phases: [{ title: 'Review' }, { title: 'Verify' }],
}
const ROOT = '/Users/adamnolle/Desktop/Code/FileID'
const BASE = '0852024'  // last commit before the round-1 regression fixes
const COMMON = `
REPO ROOT: ${ROOT}. Round-2 regression sweep. The campaign's round-1 fixes were themselves just patched to repair 22 regressions; verify THOSE patches (the diff \`cd ${ROOT} && git diff ${BASE}..HEAD -- <your files>\`) didn't introduce NEW bugs. Same lenses as before: regressions, weakened guards (tags_evaluated/faces_evaluated/B3/B4/SEC-*/MOVEFILE/redact/@MainActor/single-writer/terminal-pinning), cross-file interaction (signature/field/event mismatches), concurrency hazards (await-across-lock, !Send-across-await, actor reentrancy, ref-count leaks/double-cancel), data-loss/crash. The tree BUILDS (cargo clippy -D warnings + 336 tests green; swift build clean) — hunt LOGIC regressions only. Report only defensible code-level findings (file:line, changed lines, mechanism, trigger, impact). Clean diff → findings:[], clean:true.`
const FIND = { type:'object', required:['group','findings'], properties:{ group:{type:'string'}, clean:{type:'boolean'},
  findings:{type:'array',items:{type:'object',required:['title','file','severity','claim','evidence'],properties:{title:{type:'string'},file:{type:'string'},line:{type:'integer'},severity:{enum:['critical','high','medium','low']},claim:{type:'string'},evidence:{type:'string'},suggested_fix:{type:'string'}}}} } }
const VERDICT = { type:'object', required:['refuted','reasoning'], properties:{ refuted:{type:'boolean'}, reasoning:{type:'string'}, severityAdjust:{enum:['critical','high','medium','low','unchanged']} } }
const G = [
  { id:'R2-rust-cmds', files:'platforms/windows/src/engine/src/scan_session.rs platforms/windows/src/engine/src/commands/trash.rs platforms/windows/src/engine/src/main.rs' },
  { id:'R2-rust-models', files:'platforms/windows/src/engine/src/downloader.rs platforms/windows/src/engine/src/models/registry.rs platforms/windows/src/engine/src/platform.rs' },
  { id:'R2-mac-faces', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/IdentityClustering.swift' },
  { id:'R2-mac-discovery', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/Discovery.swift platforms/apple/engine/Sources/FileIDEngine/Storage/DBWriter.swift platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift' },
  { id:'R2-mac-da-tagging', files:'platforms/apple/engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift platforms/apple/engine/Sources/FileIDEngine/Pipeline/Tagging.swift platforms/apple/engine/Sources/FileIDEngine/IPC/IPCSink.swift' },
  { id:'R2-mac-app', files:'platforms/apple/app/Sources/FileID/EngineClient.swift platforms/apple/app/Sources/FileID/Views/LibraryView.swift platforms/apple/app/Sources/FileID/Views/RestructureView.swift platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift' },
  { id:'R2-cs-app', files:'platforms/windows/src/FileID.App/Views/Restructure/RestructureView.xaml.cs' },
]
phase('Review')
const results = await pipeline(G,
  g => agent(`Round-2 re-audit ${g.id}. Files: ${g.files}\n${COMMON}\nSet group to "${g.id}".`, { label:`r2:${g.id}`, phase:'Review', schema:FIND }),
  (rev,g) => {
    if (!rev||!rev.findings||!rev.findings.length) return { group:g.id, confirmed:[] }
    return parallel(rev.findings.map(f => () =>
      agent(`Adversarially verify this round-2 regression claim (root ${ROOT}). Read \`git diff ${BASE}..HEAD -- ${f.file}\` + the code. Real NEW bug from the round-1 patches? Default refuted=true unless reproduced. Finding: ${JSON.stringify(f)}`,
        { label:`v2:${g.id}`, phase:'Verify', schema:VERDICT })
        .then(v => ({ ...f, group:g.id, confirmed: v?!v.refuted:false, sev:(v?.severityAdjust&&v.severityAdjust!=='unchanged')?v.severityAdjust:f.severity, reason:v?.reasoning })))
    ).then(j => ({ group:g.id, confirmed:j.filter(Boolean).filter(x=>x.confirmed) }))
  }
)
const confirmed = results.filter(Boolean).flatMap(r=>r.confirmed||[])
log(`Round-2 re-audit: ${confirmed.length} confirmed new regressions`)
return { confirmed, clean: confirmed.length===0 }

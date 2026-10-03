# FileID next-version handoff — 2026-10-03

## Current checkpoint — sparse timeline chapter drafts merged

PR [#214](https://github.com/WebWorldWide/FileID/pull/214) is merged to `main` at `ba12b17b738bca1567e37daa72ed8cdcc5b985e6` (head `08f21faf1f60750ffd090700c840495346659ada`). It adds macOS-only sparse caption-transition chapter drafts with low confidence, source/model provenance, clear review labels, automatic detail refresh, and guards that preserve user edits and honor cancellation/source changes. The exact-head macOS workflow `37143652457`, action-pin check `37143652446`, and local 491-test/Release-build validation passed. PR feature branch was deleted.

Post-merge macOS `37145111901` and repository-policy `37145111879` both pass on `ba12b17b738bca1567e37daa72ed8cdcc5b985e6`. The macOS run passed Release engine/app and Store-sandbox builds, 491 tests, cross-engine merge, health/catalog recovery, smoke, and privacy probes. GitHub reported existing Node.js 20 action deprecations only. The previous main baseline at `6cde4a817e276b5b325fa36f9a7926e2abdf073a` has all seven main workflows green, including Linux and Windows engine/app jobs on isolated Adlon guest disks.

## Current implementation branch — timestamped video speech

Branch `codex/mac-video-transcripts` adds on-device Apple Speech to the existing macOS timeline job. It exports audio to protected temporary files, processes overlapping 45-second cores, resumes verified chunks, reports per-chunk progress, saves timestamped passage evidence with source/model provenance, and records incomplete intervals when local recognition is unavailable or fails. Speech requests explicitly require on-device recognition and are cancellable. Six focused tests, all 497 Swift tests, and standard plus Store-sandbox Release app/engine builds pass locally. Run `git status`, inspect this branch and its PR/checks before continuing; exact-head CI, merge, and main checks remain pending. Real speech acceptance still needs granted Speech permission, a compatible on-device recognizer, and owner hardware.

TestFlight is not uploaded. Owner-managed Mac App Store profile, usable Apple Distribution app identity, Mac Installer Distribution identity, and App Store Connect API key are still required. `platforms/apple/scripts/testflight.sh` is the upload path after provision; complete Apple processing and sandbox scan/export/model-download acceptance before claiming delivery. Keep all credentials out of Git and Adlon.

## Instructions for next agent

Inspect current `main`, `git status`, remote branches, PR #213, and the newest results recorded in `shared/docs/STATE.md` before editing. Timestamped speech passages now have a macOS implementation; continue with dense shot/activity analysis, video face tracks, and truthful visual coverage. Brief events between samples remain unknown; preserve user chapter edits. Then follow `shared/docs/NEXT_VERSION.md` through face quality, best takes, conversion/enhancement/reframing, and local chat/search.

Do not start new Windows/Linux feature work or hardware acceptance until the owner is on the PC. Preserve the separate open Windows Store PR #213 and `/Users/adamnolle/Desktop/Code/FileID-msix-store` worktree. The merged chapter branch is already deleted; do not remove unrelated branches. Adlon is example data only: use established guest disks, never mount/access the corpus, and never write databases, caches, sidecars, thumbnails, logs, exports, temp files, or test fixtures there. TestFlight signing assets remain owner-managed.

## Current checkpoint — Mac event/time retrieval merged and CI green

PR #210 is merged to `main` as `e272fb233eaf7db2180e10c7a497f5d631c8e0b7`. Chat search resolves exact titles from user-edited event groups and explicit `MM:SS` or `HH:MM:SS` requests. It filters files by event membership and returns existing chapter/passage intervals that contain the requested time plus person observations within 15 seconds. Stale chapter/passage evidence is excluded, and refinements retain event/time filters. This is retrieval over catalog evidence; automatic timeline population and open-ended event detection remain unimplemented. No schema, IPC, model, dependency, or network changes.

All five GitHub Actions workflows passed against that exact merge commit:

- macOS app and engine: [run 37125792358](https://github.com/WebWorldWide/FileID/actions/runs/37125792358), including 483 tests in 96 suites, Release builds, recovery probes, and privacy checks.
- Repository policy: [run 37125792306](https://github.com/WebWorldWide/FileID/actions/runs/37125792306).
- Linux engine, CLI, TUI, and GTK app: [run 37125802324](https://github.com/WebWorldWide/FileID/actions/runs/37125802324), all jobs on `adlon-fileid-linux`.
- Windows engine: [run 37125802333](https://github.com/WebWorldWide/FileID/actions/runs/37125802333); x64 and ARM64 cross-builds on `adlon-fileid-windows`, native ARM64 on GitHub-hosted hardware.
- Windows app: [run 37126581678](https://github.com/WebWorldWide/FileID/actions/runs/37126581678), x64 and ARM64 cross-builds on `adlon-fileid-windows`.

Adlon remains example data only. CI guests do not mount its data volume; no example files were read, copied, indexed, or written. The feature branch was deleted after merge and `origin` currently contains only `main`. An unrelated existing local `codex/store-msix` worktree is preserved; inspect its handoff before considering cleanup.

## Next agent instructions

Continue the active next-version goal on macOS. The Mac app is not complete or release-ready. Read `shared/docs/NEXT_VERSION.md`, `shared/docs/NEXT.md`, `shared/docs/ARCHITECTURE.md`, `shared/docs/CHAT.md`, and `shared/docs/SCHEDULER.md` before selecting the next slice. Next, turn the timeline foundation into bounded, cancellable population of timestamped evidence and coverage; never report an unexamined interval as evidence that an event did not happen. Then measure scheduler contention on internal-drive fixtures before changing resource caps, and continue People accuracy, best takes, and media tools.

The owner explicitly deferred new Windows/Linux product work and physical acceptance until they are on that PC. Keep shared contracts coherent, but do not start those ports yet. Adlon is read-only example data; mutation tests use internal-drive fixtures. Keep CI jobs on guest disks and serialize Windows engine/app workflows because they share one Windows VM. Start implementation on a fresh `codex/` branch, validate the exact head, merge through a PR, delete its feature branch, and update `STATE.md`, `NEXT.md`, `CHAT.md`, `DECISIONS.md`, and this handoff. A green packaging workflow is not full release acceptance.

The accepted goal remains active; this checkpoint is not completion or a request to pause it.

## Accepted index-job integration

PR #201 passed all eleven checks at `e86f17c467916164c89af3dfc83069051c6d0654` and merged as `d1208df1967cc35d1aa27b5ec32ed8f0eb17b39c`; all eleven merged-main checks passed and its branch is removed. Hosted macOS ran all 466 tests and the actual catalog/index recovery fixtures. Adlon Linux/Windows jobs passed, including the genuine 48-test zero-skip IPC gate; native ARM64 remains hosted and the Adlon ARM64 cross-build proves compilation/lint. Local Release engine/app products and Release health/index process probes pass. Inspect STATE.md for exact run IDs. The first hosted release compile at `8f36caf4a3b8be397a28f09b771c79613a3ed361` failed the compiler type-check budget; it is superseded by the verified typed-helper fix.

## Active Mac shared-resource scheduler

PR #203 passed exact-head macOS/policy checks at `8794d771fd507936f75d7ad1801fcaf9ff760bbc` and merged as `186af22280b0cd9339d821ec71cf8e004e576496`. Merged-main macOS/policy plus Adlon Linux, Windows app and Windows Engine checks all passed; see STATE.md for run IDs and exact runner names. The source branch was deleted and `origin` contains only `main`.

This does not yet cover every native file worker, the Windows/Linux engines, separate accelerator budgets, multiple resident VLMs, CPU load or I/O throughput measurement, or preemption/bounded index yielding. Next Mac work must extend this admission to every heavy pipeline, validate contention on representative hardware, then continue text/catalog indexing, person/event retrieval, model routing, chat, faces, timeline, best-take and conversion milestones. Windows/Linux implementation and physical validation stay deferred until the owner is on that PC.

Latest increments route face-print extraction and per-file scan tagging through cancellable background CPU permits. PR #206 is merged; its exact-head and all five merged-main workflows passed. The existing local batch JSONL event now reports admission-wait P50/P95/max and files delayed over 1 ms, separate from `perFileTotalMs`; see STATE.md and SCHEDULER.md. The existing 8-case performance suite is synthetic discovery/database writing and does not measure Vision/tagging throughput. Standard FileID model directories are empty, so no model-backed throughput result is claimed. Next, run a representative internal-drive scan with local weights while interactive work runs; record stage time, admission wait, rate and memory before changing caps or adding I/O/memory estimates. Keep the full NEXT_VERSION.md backlog active, defer Windows/Linux work until the owner is on that PC, and never use Adlon example data as a source or output.

The normal macOS no-wipe launcher now delegates MLX kernel compilation to the pinned `scripts/ensure_mlx_metallib.sh` helper. This fixes the stale duplicated Metal command that failed on Xcode 27; `run.sh --no-wipe` rebuilt the 97 MB library and launched FileID with local library state preserved. Bootstrap script hashes and tests were updated together. The app process was observed running. `FileIDEngine` was not in the immediate process list, so a separate actual-process fixture ran the bundled engine against an isolated database and passed index recovery/cancel/retry with no visual model. No UI-driven inference run was performed. Do not access Adlon corpus data.

The initial PR policy check caught a stale `DeepAnalyze.swift` digest because it is an explicitly reviewed network-capable source. Diff review found no download or transport changes; only scheduler admission and model-memory lifetime changed. The digest was refreshed, all 23 runtime-egress mutation tests pass, and `check_runtime_egress.py --known-blockers` reports no additions. The old-head policy failure is superseded by the green exact-head run.

## Active native index-job follow-up

Native validation is 466 tests / 94 suites, including durable index recovery, memory deferral/historical caches, partial-update cancellation with unchanged prior snapshots, explicit failure/cancellation retry, and compaction/snapshot cancellation. Actual native health, cross-engine catalog round trip and native index-job IPC recovery/retry probes pass. `shared/scripts/check_catalog_index_jobs.py` uses only isolated synthetic SQLite fixtures and no visual model. macOS CI now runs both catalog and index-job process fixtures. Inspect the current PR/main checks before extending the source. General atomic resource reservations, fairness and multi-model routing remain next; PC implementation stays deferred.

## Current day-end checkpoint

PR #198 passed all eighteen checks at `b220584041001be18d40ed3dcd0450fbb568e00c` and merged as `94c96d51caf43007c147b0bb584bfcec8124e3e7`. Native validation is 461 tests / 94 suites; pinned Rust validation is 394 library, 395 executable and two IPC integration tests, with two preexisting hardware cases ignored. Main CI acceptance is recorded in STATE.md; inspect its exact run IDs rather than assuming an online runner proves a gate passed.

Next implementation should begin on a new `codex/` branch from current main. Keep Windows/Linux new features and physical acceptance deferred to the owner's PC. The native index-job follow-up now reuses catalog_jobs priority/checkpoint/state, dispatches controls by kind, supports explicit pause/cancel/retry, recovers interrupted jobs and estimates memory including historical caches. Verify its current PR/main tests first. Next add shared atomic CPU/I/O/memory reservations and pressure/fairness handling; preserve actor/worker ownership and SQLite revision checks, then add text/catalog indexes and model roles. Progress counts never replace verified snapshot/SQLite checkpoints. Keep local builds serialized and outputs on the internal drive.

The republished Store head `bc2765dd6f3160dd047db59a9bafb93c7b687daf` is preserved at verified tag `archive/2026-10-02/store-msix-republished-v2`. Old Store engine run `37088953979` and package run `37089711380` were cancelled because their legacy branch workflows occupied the persistent main-only Adlon runner; cancellation is policy cleanup, not acceptance. Its remote branch was removed with an exact-head lease. The separate local Store checkout remains untouched.

Dependency proposal PR #199 is preserved at `archive/2026-10-02/dependabot-remediation` (`df4405c9f32cf4642dc34ef8a43d84704a174b5c`), closed and removed from remote heads for main-only cleanup. Its fifteen hosted checks passed, but actual RAM++ export compatibility remains unvalidated. Resume its security updates narrowly; do not claim all dependency alerts are remediated.

## Active Mac retrieval checkpoint

Use the current main/PR state, not historical branch names below. Catalog v23 adds transactional embedding epochs/change history and source/eligibility triggers; `CatalogVectorIndex` restores or rebuilds pinned CLIP snapshots and applies deltas. `CatalogSearch` validates vector/model/limit inputs, hydrates against current SQLite fingerprints, and merges keyword/evidence/visual ranks. File scope deduplicates moments before limits; refreshed matching keeps graph ownership safe across concurrent requests. Native Library uses engine IPC v1.6. Cold index preparation is asynchronous and returns `indexing`; keyword fallback remains visible. Rust/C# mirror the optional fields and PC execution explicitly rejects unsupported visual mode.

Next: validate native durable index jobs, implement shared atomic resource admission/fairness, add text/catalog indexes and richer filters, verify actual end-to-end performance, then continue all remaining NEXT_VERSION.md milestones. Preserve the PC deferral, Adlon corpus protections, six native tabs/palette/springs, sole-writer direction, offline inference and exact model/license promotion gates. Do not claim the full plan or release gates are complete.

Local native test command must set `FILEID_TEST_ENGINE_PATH` to the freshly built executable and use `--no-parallel`. Otherwise spawned conversion/cancellation workers cannot find the helper in an external scratch directory. Build/test sources stay frozen while a local compiler is running. Use pinned Rust 1.90 and external `CARGO_TARGET_DIR`; inspect required exact-head and merged-main checks. All source work must reach main with recovery tags for any unrelated branches; never touch the separate Store checkout or merge its divergent WIP wholesale.

## Current owner priority

The owner has a Windows/Linux PC and deferred new port implementation and hardware validation until working there. Continue the full macOS plan first; retain the port backlog below for that machine. Health PR #194 passed all eighteen checks at 455f3f0e55eda40f654ee476e510138f5c3e9de8 and merged at db7e4e9fbd1cd65ca0219de5a6384fd5fad9c5cf. All nineteen health merged-main checks passed, including the Adlon Linux/Windows jobs.

The rebased, unvalidated thirteen-file Windows restoration is preserved at remote annotated tag archive/2026-10-02/windows-safety-restore-rebased, commit e5f51dbe621a285327b15a07e984e36125ca6b22. Inspect its final commit narrowly from a new branch based on latest main when resuming on the PC. The earlier stage-one worktree is archived; its tag and broader WIP remain available.

## Integrated work

Foundation PR #186, runner bootstrap #188, stable People identities #189, and transactional People merges #190 are merged. PR #190 passed all sixteen checks at 02c6ff9da97e8b1125f8eb78e36956e09baade14 before merge. Swift passes 437 tests / 89 suites; Rust passes 393 library and 394 executable tests plus two registry integrations, with two preexisting ignored corpus cases. Actual-process merge fixtures cover six cases per engine, including stale selections and forced SQLite rollback. Explicit user merges still delete source person rows; aliases/history remain unfinished.

## macOS continuation

Cache-compatibility PR #195 passed both required checks and is merged as `144bc4b20851e8b0baf873103200225e0c57192d`. Graph/snapshot PR #196 passed both exact-head checks at `dff844f9be1fdcfcc9e98c8783f5bdcf43332960` and merged as `f9c2c6b461087c84d70a441eec92cc32a61dd5db`; its fresh merged-main Mac run `37084108015` and policy run `37084108059` both passed. All 450 native tests across 93 suites, engine build and actual-process health probes pass. Benchmark instructions are in `platforms/apple/benchmarks/README.md`, with raw parameters/results/source hashes in `shared/test-corpus/benchmarks/macos-hnsw-2026-10-02.json`. Real face and semantic-search accuracy remain unvalidated.

Historical graph checkpoint, superseded by PR #198 above. The verified CLIP-space change was the active milestone at that point. Run native Swift tests with `FILEID_TEST_ENGINE_PATH` pointing at the freshly built worker, and serialize native builds on this 16 GB Mac. New cache rows use the pinned artifact/preprocessing identity; old rows are refreshed through inference during rescans, never relabeled by dimension. Persistent indexing, engine-owned hybrid retrieval, the scheduler and the rest of `NEXT_VERSION.md` remain unfinished. The native HNSW snapshot API is not connected to retrieval yet; its source-revision argument must come from validated SQLite change tracking, and external entity IDs still need a persistent mapping. Do not reuse it by checking only file timestamps. Do not equate passing synthetic fixtures with model accuracy or hardware acceptance.

## Health contract follow-up (merged)

PR #194 passed all eighteen checks at `455f3f0e55eda40f654ee476e510138f5c3e9de8` and merged to `db7e4e9fbd1cd65ca0219de5a6384fd5fad9c5cf`. Rust 1.90 and all 438 then-current Swift tests passed; actual-process health probes passed for both engines. The dedicated Windows IPC gate executes all 48 tests, TRX validation and formatting. All nineteen health merged-main checks passed; the actual Adlon x64 job executed all 48 IPC tests with no skips. Legacy full app service-test discovery still skips. On the PC, wire generation-bound health waiters into the UI lifecycle and retire old-generation waiters before publishing a replacement generation.

## Deferred PC priority: Windows safety and real test execution

Recovery tag: archive/2026-10-02/windows-test-parity-wip at 5d06976c7aa8171a201dbe183f941d6dab896ff9. This is an unfinished, unvalidated recovery checkpoint; do not merge it wholesale.

Fetch that tag and create a new codex/ branch from current main. Inspect the draft diff and port coherent repairs. Historical a1d7108^ contains prior safeguards, but preserve the current native XAML, virtualization, palette, springs and LavaLampBackground. Production safety behavior was removed while its tests remained. The ordinary Windows app workflow also skips both test projects because it checks a doubled relative Tests path. A green current-main app job therefore proves build/publish/format/privacy/smoke, not full service tests.

Draft PR #191 introduced a genuine TRX gate and exposed remaining app test compilation failures. Do not weaken or exclude tests to make it green. Its latest tested head was 73b947b8d0d3b273aa0188ac43b9b0ef0d374f72; later ReadStore and CleanupViewModel changes in the WIP tag have not been built/tested.

Completed draft changes include AppPaths isolation, test-instance mutexes, FolderPicker validation, bounded thumbnail queue/fallback allocation, trace gating, shared Undo history, and terminal-result-based bulk rename/tag journaling. The strict TRX report guard passed nine cases on the Windows VM. This is not the app suite.

Remaining groups:

- File-state paths: complete Adlon read-only enforcement for DB/model/log/output roots and platform-specific aliases, including Windows shares. The current guards are not yet a universal server/mount policy. Prove rejection with internal fixtures; never test writes on the corpus.
- Engine lifecycle: code lives in FileID.App/ViewModels/EngineClient.cs and EngineClient.Commands.cs. Wire EngineLifecycleCoordinator, generation-bound health waiters and a canonical health IPC command, safe persisted real/shortcut Undo readers, intentional-stop status, serialized restart/close guards, bounded framing test access.
- Cleanup: wire Exact/Similar mode and explicit selections. Add exact-trash proofs to the canonical schema and all engines before using them; current Rust silently ignoring unknown proof fields would be unsafe. Preserve stable keeper/victim identities, mutation guards, staged claims and recovery. The restored WIP model is not yet wired into the view.
- People: finish wiring/verify restored PersonFileIdsAsync and structured names against existing tests.
- Restructure: frozen-plan/request-generation guards, paged/repeater selections, quality query, Undo routing and honest completion. Mirror canonical Cancelled/Planned/Remaining/ShortcutUndoToken fields into C# and Rust rather than inventing client-only DTOs.
- Fix the specific CA1861 repeated-array fixtures. Keep analyzer enforcement.
- Run both genuine app suites, build/publish x64 and ARM64, dotnet format, IPC conformance, Rust tests/Clippy, and native checks before integration.

Failure logs on the internal Mac disk: /Users/adamnolle/.codex/fileid-builds/windows-app-parity-failure.log and windows-app-archived-failure.log. GitHub run 37025206254/job 110897963808 preserves the current draft failure.

## Adlon boundaries and runner administration

Read CI_RUNNERS.md. Corpus /srv/data is unmounted in both guests and strictly read-only. Never write caches, DBs, logs, thumbnails, exports or fixture data there; never modify original files. Runner storage is confined to existing guest disks.

Linux guest root filled during CI. Its existing qcow2 on the host internal /var/lib/libvirt/images was expanded live from 80 to 128 GB with virsh blockresize, then growpart /dev/vda 1 and resize2fs /dev/vda1. No reboot or other repository process termination. About 47 GB free immediately afterward. Monitor capacity before large cache restores.

Windows runner service is Network Service; administrator SSH success is not service-account acceptance. SDKs use the runner-owned tool cache; Bash/Python/Clang need per-job GITHUB_PATH. The official Visual Studio Clang component installed successfully (exit 0), and clang 19.1.5 starts. Do not use bootstrapper-only --wait with setup.exe, force process closure, or reboot other jobs. Compiler-path PR #192 passed all four hosted checks and merged at dddc365007a84a98fa8adf1704cced71ac2254a6. Fresh main run 37033359014 passed x64 and ARM64 cross-builds on adlon-fileid-windows and native ARM64 on hosted hardware; the service-account compiler preparation and all build/test/smoke/privacy gates completed successfully.

Main 3d46a969 also passed macOS Swift, Linux packaging and all six hosted native-tools jobs (run 37028488216). Main bb33211 engine x64 and both app packaging matrices passed on adlon-fileid-windows. Its original ARM64 engine cross-build failed before Clang installation; fresh main dddc365 passed all three engine jobs in run 37033359014. Linux no-space failures were infrastructure failures. After expansion, all four Linux jobs passed on adlon-fileid-linux at main 3d46a969 in run 37028488104. GTK UI parity is still unfinished.

## Recovery and cleanup

Every deleted GitHub branch must have a verified remote annotated archive tag whose peeled commit matches its last head. New tags archive/2026-10-02/<branch> preserve advanced heads independently of October 1 archives. Store remote head a9f3006 is preserved; local Store head 9d1a093 has separate archive/2026-10-02/store-local-worktree. Keep /Users/adamnolle/Desktop/Code/FileID-msix-store intact. Do not apply abandoned changes blindly, force-push main, or publish a release for runner testing.

## Remaining next-version scope

Text/catalog ANN indexes, richer hybrid retrieval and real end-to-end performance; durable resource-budgeted scheduling and measured multi-model routing; typed reversible chat operations; companion-aware/preference-driven names; incremental calibrated faces and exemplars; dense audio/shot/person/outcome analysis and automatic chapters/best takes; portable media workers, stabilization/upscale/reframing and fidelity checks; licensed document/archive/data/ebook/CAD tools and saved recipes; real Windows and GTK feature parity; strict privacy, model/hardware, performance, recovery, signing and distribution gates.

Strict runtime-egress still fails six existing artifact URLs. --known-blockers only proves no additions. New model research candidates have not been promoted: exact weights/licenses/hashes/runtime compatibility and identical-fixture benchmarks are required. No new model downloads or accuracy claims were made. SQLite committed migrations are immutable; append the next migration when required. Keep engine sole-writer migration work explicit.


## Checkpoint recovery and checkout selection

The implementation worktree path formerly attached to this task is no longer present. The primary checkout is `/Users/adamnolle/Desktop/Code/FileID`; inspect Git status and current artifacts before creating or reusing a worktree. All source work in this checkpoint is merged. The republished Store proposal is preserved at `archive/2026-10-02/store-msix-republished` (`07c0ad8e688e0293484ae628e9f3c7aeac862e90`); it diverges extensively from main and is unvalidated, so inspect narrowly. Its separate local Store checkout remains untouched. Do not infer that archived proposals were integrated or accepted.

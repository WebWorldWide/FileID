# FileID next-version handoff — 2026-10-02

The accepted next-version goal remains active after the owner resumed sustained work. Preserve the requested recovery checkpoint, validated main integrations, GitHub branch cleanup and Adlon CI evidence while continuing the full plan. A milestone does not complete the release or authorize pausing the resumed goal. Continue from shared/docs/NEXT_VERSION.md and NEXT.md; do not infer release readiness from a green packaging workflow.

## Integrated work

Foundation PR #186, runner bootstrap #188, stable People identities #189, and transactional People merges #190 are merged. PR #190 passed all sixteen checks at 02c6ff9da97e8b1125f8eb78e36956e09baade14 before merge. Swift passes 437 tests / 89 suites; Rust passes 393 library and 394 executable tests plus two registry integrations, with two preexisting ignored corpus cases. Actual-process merge fixtures cover six cases per engine, including stale selections and forced SQLite rollback. Explicit user merges still delete source person rows; aliases/history remain unfinished.

## First priority: Windows safety and real test execution

Recovery tag: archive/2026-10-02/windows-test-parity-wip at 5d06976c7aa8171a201dbe183f941d6dab896ff9. This is an unfinished, unvalidated recovery checkpoint; do not merge it wholesale.

Fetch that tag and create a new codex/ branch from current main. Inspect the draft diff and port coherent repairs. Historical a1d7108^ contains prior safeguards, but preserve the current native XAML, virtualization, palette, springs and LavaLampBackground. Production safety behavior was removed while its tests remained. The ordinary Windows app workflow also skips both test projects because it checks a doubled relative Tests path. A green current-main app job therefore proves build/publish/format/privacy/smoke, not full service tests.

Draft PR #191 introduced a genuine TRX gate and exposed remaining app test compilation failures. Do not weaken or exclude tests to make it green. Its latest tested head was 73b947b8d0d3b273aa0188ac43b9b0ef0d374f72; later ReadStore and CleanupViewModel changes in the WIP tag have not been built/tested.

Completed draft changes include AppPaths isolation, test-instance mutexes, FolderPicker validation, bounded thumbnail queue/fallback allocation, trace gating, shared Undo history, and terminal-result-based bulk rename/tag journaling. The strict TRX report guard passed nine cases on the Windows VM. This is not the app suite.

Remaining groups:

- File-state paths: complete Adlon read-only enforcement for DB/model/log/output roots and platform-specific aliases, including Windows shares. The current guards are not yet a universal server/mount policy. Prove rejection with internal fixtures; never test writes on the corpus.
- Engine lifecycle: wire EngineLifecycleCoordinator, generation-bound health waiters and a canonical health IPC command, safe persisted real/shortcut Undo readers, intentional-stop status, serialized restart/close guards, bounded framing test access.
- Cleanup: wire Exact/Similar mode and explicit selections. Add exact-trash proofs to the canonical schema and all engines before using them; current Rust silently ignoring unknown proof fields would be unsafe. Preserve stable keeper/victim identities, mutation guards, staged claims and recovery. The restored WIP model is not yet wired into the view.
- People: finish wiring/verify restored PersonFileIdsAsync and structured names against existing tests.
- Restructure: frozen-plan/request-generation guards, paged/repeater selections, quality query, Undo routing and honest completion. Mirror canonical Cancelled/Planned/Remaining/ShortcutUndoToken fields into C# and Rust rather than inventing client-only DTOs.
- Fix the specific CA1861 repeated-array fixtures. Keep analyzer enforcement.
- Run both genuine app suites, build/publish x64 and ARM64, dotnet format, IPC conformance, Rust tests/Clippy, and native checks before integration.

Failure logs on the internal Mac disk: /Users/adamnolle/.codex/fileid-builds/windows-app-parity-failure.log and windows-app-archived-failure.log. GitHub run 37025206254/job 110897963808 preserves the current draft failure.

## Adlon boundaries and runner administration

Read CI_RUNNERS.md. Corpus /srv/data is unmounted in both guests and strictly read-only. Never write caches, DBs, logs, thumbnails, exports or fixture data there; never modify original files. Runner storage is confined to existing guest disks.

Linux guest root filled during CI. Its existing qcow2 on the host internal /var/lib/libvirt/images was expanded live from 80 to 128 GB with virsh blockresize, then growpart /dev/vda 1 and resize2fs /dev/vda1. No reboot or other repository process termination. About 47 GB free immediately afterward. Monitor capacity before large cache restores.

Windows runner service is Network Service; administrator SSH success is not service-account acceptance. SDKs use the runner-owned tool cache; Bash/Python/Clang need per-job GITHUB_PATH. The official Visual Studio Clang component installed successfully (exit 0), and clang 19.1.5 starts. Do not use bootstrapper-only --wait with setup.exe, force process closure, or reboot other jobs. Inspect a fresh main ARM64 cross-build result after the compiler-path workflow fix.

Main 3d46a969 also passed macOS Swift, Linux packaging and all six hosted native-tools jobs (run 37028488216). Main bb33211 engine x64 and both app packaging matrices passed on adlon-fileid-windows. Its ARM64 engine cross-build failed before Clang installation. Linux no-space failures were infrastructure failures. After expansion, all four Linux jobs passed on adlon-fileid-linux at main 3d46a969 in run 37028488104. GTK UI parity is still unfinished.

## Recovery and cleanup

Every deleted GitHub branch must have a verified remote annotated archive tag whose peeled commit matches its last head. New tags archive/2026-10-02/<branch> preserve advanced heads independently of October 1 archives. Store remote head a9f3006 is preserved; local Store head 9d1a093 has separate archive/2026-10-02/store-local-worktree. Keep /Users/adamnolle/Desktop/Code/FileID-msix-store intact. Do not apply abandoned changes blindly, force-push main, or publish a release for runner testing.

## Remaining next-version scope

Persistent model-separated ANN/hybrid retrieval; durable resource-budgeted scheduling and measured multi-model routing; typed reversible chat operations; companion-aware/preference-driven names; incremental calibrated faces and exemplars; dense audio/shot/person/outcome analysis and automatic chapters/best takes; portable media workers, stabilization/upscale/reframing and fidelity checks; licensed document/archive/data/ebook/CAD tools and saved recipes; real Windows and GTK feature parity; strict privacy, model/hardware, performance, recovery, signing and distribution gates.

Strict runtime-egress still fails six existing artifact URLs. --known-blockers only proves no additions. New model research candidates have not been promoted: exact weights/licenses/hashes/runtime compatibility and identical-fixture benchmarks are required. No new model downloads or accuracy claims were made. SQLite committed migrations are immutable; append the next migration when required. Keep engine sole-writer migration work explicit.

# FileID next-version continuation — 2026-10-02

The full accepted next-version goal remains active. Complete the ledger in NEXT_VERSION.md; passing a milestone does not complete the release. The owner authorizes coherent commits and pushes, integration into main after checks pass, and eventual branch cleanup with preserved recovery refs. Never force-push main or merge unreviewed proposals.

## Current work and evidence

People PR #189 is merged on main at ff6eda2 after all sixteen fresh checks passed. The Desktop checkout still holds `codex/stable-person-identities`. The internal worktree `/Users/adamnolle/.codex/worktrees/fileid-people-stable-ids/FileID` holds `codex/windows-test-parity`, a new unfinished native app/test-gate draft. Its base includes the separate `codex/atomic-person-merges` follow-up (PR #190): a forced SQLite DELETE failure reproduced a partial Rust merge, and structured names were lost in the Swift engine. The fix passes local full suites and six actual-process merge cases on each engine. The macOS workflow includes that cross-engine regression. Inspect fresh hosted checks before integration; explicit source aliases/history remain unfinished. The foundation is on main through merged PR #186 (`c3044395`); the Adlon Windows bootstrap PR #188 is merged at `bb33211`. The previously saved stable-ID prototype has been applied and completed for this bounded persistence milestone; do not reapply the old patch.

Stable People clustering retains identity rows, names, creation times and references, updates analysis/assignments transactionally, and preserves unknown/offline records. Shared Swift/Rust fixtures cover deterministic ID reuse and protected partitions. Fresh negative corrections and same-image observations split raw clusters. Native automatic consolidation retains absorbed unnamed rows; empty unnamed records stay out of People cards. Explicit user merge aliases, incremental assignment, calibrated accuracy and correction UI remain unfinished.

Validation on this host: Swift 437 tests / 89 suites; Rust 393 library and 394 executable tests plus two registry integrations (two preexisting ignored corpus tests); actual Rust 1.90 Clippy with warnings denied; catalog schema and current-doc checks; actual Swift/Rust catalog, Tools and Chat round trips; privacy scan of native app and both engine binaries. All sixteen GitHub checks passed for original People head `7f03fff`; the documentation conflicts with bootstrap main have been resolved. Inspect the fresh merge head checks before integration. Native VLM/face accuracy, other GPU vendors and all release gates are not established by these tests.

The Windows draft restores isolated paths/picker handling, confirmed rename/tag Undo, ChangeLog facade and bounded/completing thumbnail requests. The strict TRX gate intentionally exposes the remaining missing safety APIs. Use the archived compiler log and historical a1d7108^ source as references; preserve current native XAML and do not restore whole historical UI files blindly. Repair all remaining app test failures and formatting before merging. No exclusions or disabled suites establish acceptance.

## Adlon CI

Both FileID runners are registered and online in existing VMs on separate guest disks. The Linux main jobs passed on adlon-fileid-linux. The main Windows jobs exposed Bash PATH and Program Files SDK permissions. Bootstrap PR #188 supplies Bash/Python via GITHUB_PATH and uses the runner-owned SDK cache. GitHub runner expressions belong in step env, not job env. All six hosted checks passed and #188 merged. Actual main x64 engine is running on adlon-fileid-windows and passed service-account setup, Clippy and release compilation; its test build is running. The remaining Windows matrix jobs queue behind that runner. Inspect every actual main result before claiming server acceptance. Do not report hosted PR checks as server execution.

Read CI_RUNNERS.md for guest paths and service details. Preserve other repositories' runners; do not restart the host or kill unrelated builds. Windows service is Network Service; test its actual CI access, not just an administrator SSH session. Linux guest has about 19 GB free at the last measurement; monitor capacity before adding SDKs.

The existing Windows app workflow skips tests due to the wrong relative Tests path. Archived draft PR #187 preserves the stricter gate and exposes preexisting app test API drift. Fix the drift and enable real TRX proof; do not weaken the gate or claim a green build proved all app tests. Linux currently builds a placeholder shell while its six-tab modules remain unwired; port UI acceptance is still required.

## Safeguards

Adlon corpus is strictly read-only example data. Never write databases, caches, thumbnails, logs, sidecars, tags, outputs or temporary files there; never rename, move, repair or delete its contents. Runner administration is authorized only on separate CI guest disks; corpus is not mounted in those VMs. Use internal temporary fixtures for modifications. Keep the shared read-only path guards, including missing paths and symlink aliases.

Preserve native six tabs, palette, springs and LavaLampBackground. IPC changes land in shared/ipc-schema/ipc.schema.json first and mirror Swift/Rust/C#; GTK inherits Rust. Committed migrations are immutable; append v23 if needed. Verify model licenses, hashes, runtime compatibility and measured quality/latency before promotion. No telemetry. Strict runtime-egress still fails six existing artifact URLs; --known-blockers proves no additions, not release acceptance.

## Build and test paths

Keep builds on the internal drive, outside FileProvider. Avoid concurrent heavy Swift/Rust builds on this 16 GB Mac, and do not edit source during its build/test.

- Swift scratch: `/Users/adamnolle/.codex/fileid-builds/swift-stable`. Use DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer, FILEID_TEST_ENGINE_PATH=<scratch>/debug/FileIDEngine, `swift test --package-path platforms/apple --build-system native --scratch-path <scratch> --jobs 4`.
- Rust target: `/Users/adamnolle/.codex/fileid-builds/rust-stable`. Prepend `/Users/adamnolle/.rustup/toolchains/1.90-aarch64-apple-darwin/bin` to PATH; cargo test/build/clippy --locked --jobs 4 with that target directory. Confirm actual Clippy version; rustup run alone can select the Homebrew executable.
- Build/test logs are beside those directories. Native FileID/FileIDEngine and Rust FileIDEngine binaries support the shared/scripts/check_*_roundtrip.py scripts, including check_people_merge_roundtrip.py. Run binary privacy and catalog/doc policy checks.

## Remaining priorities

Finish validated main integration and Adlon Windows CI, then continue persistent hybrid indexes and offline availability, durable resource-budgeted scheduler/model routing, typed reversible chat operations, companion naming/preferences, incremental People processing/exemplars/calibration, dense temporal evidence/ASR/shots/tracks/chapters, best-take outcomes, stabilization/upscaling/reframing, broad licensed tool adapters, recipes, stronger restructure and port UI. Complete the accepted physical-hardware, fidelity, latency, privacy, signing and distribution gates before declaring the goal complete.

Keep STATE newest-first, NEXT actionable, DECISIONS append-only, and MODELS/ARCHITECTURE/SHIP truthful. Preserve the separate `/Users/adamnolle/Desktop/Code/FileID-msix-store` worktree. Remote proposal heads are archived under annotated tags listed in ARCHIVED_BRANCHES.md; verify peeled hashes and exact-head leases before deleting branches, preserving any newer head first. No unreviewed proposals are authorized for merging.

FileProvider's duplicate numbered snapshots and invalid main-2 ref were preserved under `/Users/adamnolle/.codex/fileid-handoffs/sync-duplicates-2026-10-02` before removal. Canonical tracked files were retained. If duplicates return, inspect and preserve differing copies before cleaning compiler/Git inputs.

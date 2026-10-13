# FileID branch consolidation — 2026-10-13

The owner requested one `main` branch. The source integrations were reviewed in PRs #235 and #238. Historic checkpoint commits are retained as ancestors of `main` when their refs are removed; retaining a commit does not activate its old source tree.

| Branch | Disposition |
| --- | --- |
| `codex/store-startup-and-listing` | Already ancestral to consolidated Store PR #238. |
| `codex/windows-ui-integration`, `codex/linux-ui-integration`, `codex/native-parity-integration` | Exact branch tips merged into #238; duplicate draft PRs #239, #240, and #237 closed. |
| `parity/windows-ui`, `parity/native-apple-motion`, `parity/native-apple-accessibility`, `parity/native-perf-harness` | Compatible source selectively ported into the three follow-ons above. The older shared snapshots would roll back newer engine and media changes. |
| `parity/linux-ui` | Compatible People, Cleanup, Settings, and readiness source ported. Its old native runtime stub and timeline fixture use removed modules or stale IPC, so the current Deep, Restructure, and workbench implementations remain. |
| `parity/completion-chat` | Superseded by the current Store workbench and request-state implementation. It adds no unique file absent from the consolidated tree. |
| `parity/completion-media` | Old media-worker, adapter, timing, and child-process modules are absent from the current tree. The vetted FFmpeg/OpenH264 source-preserving video path in `commands/video_export.rs` and `commands/tools.rs` supersedes that experimental route. Any wider conversion behavior from the checkpoint remains a product follow-up, not a release claim. |
| `parity/completion-runtime` | The current resident service supersedes its old `resident.rs` and `onnx_residency.rs`. Its pinned Linux llama.cpp/whisper.cpp source builder, evidence scripts, patch, and manifest are unique and **not integrated**: they require dependency, license, binary provenance, offline-policy, and native inference review. Preserve the checkpoint commit in `main` history for later extraction. |
| `codex/fileid-completion-progress-20261012` | Documentation-only checkpoint described an unpublished, uncompiled worktree and is superseded by current `STATE.md`, `NEXT.md`, and `SHIP.md`. Preserve its commit in history. |

No unfinished checkpoint code is implicitly accepted for packaging. Production gates remain in `NEXT.md` and `SHIP.md`, including signed native packages, physical GPU/model checks, throughput, privacy capture, codec/legal review, crash/restart output recovery, and owner-deferred local macOS testing.

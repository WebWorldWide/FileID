# FileID — multi-platform repo

On-device AI file organizer: tag, dedupe, restructure, and rename tens of thousands of files locally — no cloud, no telemetry — on every major desktop OS.

## Layout

```
FileID/
├── platforms/
│   ├── apple/      ← macOS — Swift / SwiftUI / MLX / GRDB
│   ├── windows/    ← Windows — Rust engine ('fileid-engine') + WinUI 3 / .NET 8
│   └── linux/      ← deferred; engine is cross-platform-clean, UI port unstarted
├── shared/
│   ├── ipc-schema/ ← canonical IPC contract (JSON Schema → Swift/Rust/C# DTOs)
│   ├── docs/       ← cross-platform docs (see Persistence files)
│   ├── test-corpus/← shared regression corpus + assertions
│   └── scripts/    ← cross-platform helpers (model export/install)
└── README.md
```

Both apps are feature-complete across six tabs (Library · People · Cleanup · Deep Analyze · Restructure · Settings). macOS remains the **visual + behavioral reference**; Windows currently leads on the commercial-clean model stack (merged, CI-green) with the macOS mirror in progress.

## Per-platform dev guides

Read the one for the work in front of you:
- `platforms/windows/CLAUDE.md` — Rust engine, WinUI 3, ONNX Runtime (DirectML/CUDA/…), llama.cpp.
- `platforms/apple/CLAUDE.md` — Swift engine + SwiftUI app, MLX, GRDB.

## Cross-platform principles (apply everywhere)

- **No telemetry, ever.** No analytics, crash-reporting, update pings, or download instrumentation. The only network egress is user-initiated model downloads from `huggingface.co`. CI scans every shipped binary for telemetry strings as a release blocker. Never propose a feature that violates this.
- **Commercial-clean, Apache-2.0.** The project is Apache-2.0 (root `LICENSE`); every default model weight is permissively licensed (Apache-2.0 / MIT) so the app can be open-sourced *and* commercialized. No non-commercial weights in the shipped set. New models go through `shared/docs/MODELS.md` with the license vetted.
- **Performance is a feature.** Match or beat the macOS pipeline (≥140 files/s on comparable mid-tier hardware). Use the GPU/NPU when present; degrade gracefully to CPU.
- **The IPC contract is the contract.** Anything new lands in `shared/ipc-schema/ipc.schema.json` first; the per-platform DTOs mirror it. Schema drift = build break.
- **macOS is the visual reference; ports are 1:1.** Same palette (gold `#FFCC00`, lavender `#B19BCE`, cyan `#A0E2EA`, pink `#F2A6C0`), same springs (response 0.35–0.4 / dampingFraction 0.78–0.8), same `LavaLampBackground`. Native primitives per platform — never web tech.
- **No new dependencies without asking.** Locked sets per platform, documented in the platform guide; new crates/packages need a `DECISIONS.md` justification.
- **Default to no comments.** Add one only when the *why* is non-obvious (workaround, invariant, perf pitfall). Don't narrate the code.

## How we work

- **Verify, don't assume.** The Windows engine + app compile, lint, and test headlessly in the dev env — self-verify every change (`cargo clippy --all-targets -D warnings`, `cargo test`; `dotnet build`/`test`/`format --verify-no-changes`). `cargo check` passing is not proof of correctness; the WinUI runtime/GPU path and all macOS Swift need the user's hardware.
- **On-hardware checks** run on the dev RTX 2060 against the `G:\TrueNAS` corpus via `platforms/windows/build/iterate.ps1` (+ `scan_assertions.py`). Tune ML thresholds against real data, not by guess.
- **Land work on a branch, then merge to `main` and confirm GitHub CI is green** (engine + app workflows). Commit/push when asked.
- **Keep the record current:** newest entry on top of `STATE.md`; update `NEXT.md`; append non-obvious calls to `DECISIONS.md`.
- Preserve the user's signature touches: `LavaLampBackground` (and its Win2D port), the gold palette, springs-everywhere motion.

## Persistence files

- `shared/docs/STATE.md` — session log (newest first).
- `shared/docs/NEXT.md` — next-session priorities + acceptance criteria.
- `shared/docs/DECISIONS.md` — append-only rationale (cross-platform).
- `shared/docs/MODELS.md` — canonical model registry + licenses.
- `shared/docs/ARCHITECTURE.md` — two-binary IPC design, scan pipeline, ML stack.
- `shared/docs/RESTRUCTURE.md` — butler-grade restructure design + phased build.
- `shared/docs/SHIP.md` — v1.0 release-readiness inventory.
- `~/.claude/projects/<project-key>/memory/MEMORY.md` — auto-memory index.

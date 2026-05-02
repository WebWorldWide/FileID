# FileID — multi-platform repo

On-device AI file organizer. Tag, dedupe, restructure, rename tens of thousands of files locally, with no telemetry, on every major desktop OS.

## Layout

```
FileID/
├── platforms/
│   ├── apple/      ← macOS (Swift / SwiftUI / MLX) — currently the canonical reference
│   ├── windows/    ← Windows (Rust engine + WinUI 3 / .NET) — in progress
│   └── linux/      ← Linux — deferred to Phase 5; placeholder
├── shared/
│   ├── ipc-schema/ ← canonical IPC contract (JSON Schema → Swift/Rust/C# generators)
│   ├── docs/       ← cross-platform docs (architecture, decisions, privacy, models, state, next, ship, visual-language)
│   ├── test-corpus/← shared regression test corpus + assertions
│   └── scripts/    ← cross-platform helper scripts (model installers, etc.)
└── README.md
```

## Per-platform CLAUDE.md

Each platform owns its own dev guide. Read the right one for the work in front of you:

- `platforms/apple/CLAUDE.md` — macOS (Swift, SwiftUI, MLX, GRDB)
- `platforms/windows/CLAUDE.md` — Windows (Rust engine, WinUI 3, ONNX Runtime DirectML, llama.cpp). _Created during Phase 0 of the Windows port._

## Cross-platform principles (apply everywhere)

- **No telemetry, ever.** No analytics SDK, no crash-reporting service, no update pings, no model-download instrumentation. Only network egress is user-initiated model downloads. See `shared/docs/PRIVACY.md`. Enforced by CI binary scan. **Do not propose features that violate this.**
- **Performance is a product feature.** Match or beat the macOS pipeline (≥140 files/s on comparable mid-tier hardware) on every platform. Use the GPU/NPU when present.
- **The IPC contract is the contract.** Anything new lands in `shared/ipc-schema/ipc.schema.json` first; codegen emits per-platform DTOs.
- **The macOS app is the visual reference.** Windows + Linux are 1:1 ports. Same palette (gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0), same animations (springs at response 0.35–0.4 / dampingFraction 0.78–0.8), same LavaLampBackground. Per-platform native primitives, never web tech.
- **No new dependencies without asking.** Locked sets per platform; documented in the platform CLAUDE.md.
- **Default to no comments.** Add only when the WHY is non-obvious.

## Working principles

- User runs the build. Type-check or `cargo check` passing isn't proof of correctness — verify on real hardware before claiming done.
- Update `shared/docs/STATE.md` (latest entry on top) and `shared/docs/NEXT.md` after meaningful work.
- Append to `shared/docs/DECISIONS.md` for non-obvious calls — cross-platform.
- Preserve `LavaLampBackground.swift` and its Win2D / future-Linux ports. User's favorite.

## Persistence files

- `shared/docs/STATE.md` — cross-platform session log.
- `shared/docs/NEXT.md` — next-session priorities + acceptance criteria.
- `shared/docs/DECISIONS.md` — append-only rationale.
- `shared/docs/SHIP.md` — v1.0 release-readiness inventory.
- `~/.claude/projects/<project-key>/memory/MEMORY.md` — auto-memory.

# Contributing to FileID

> The "you're new here" guide. Pair this with `TESTING.md` (how to test) and `COVERAGE.md` (per-module targets).

FileID is an on-device, privacy-first AI file organizer — tag, dedupe, restructure, rename tens of thousands of files locally. Two platforms ship today: **Windows** (Rust engine `fileid-engine` + WinUI 3 / .NET 8 C# app) and **macOS** (Swift/SwiftUI engine + app, MLX). Linux is deferred. On every platform two binaries talk newline-delimited JSON over stdio; the engine owns a SQLite WAL database (single writer). The project is Apache-2.0.

## Setup

### Windows

```powershell
git clone <repo>
cd FileID/platforms/windows
pwsh build/build.ps1            # x64 release build of the engine
pwsh build/build.ps1 -RunTests  # + cargo test
```

`build.ps1` builds `FileIDEngine.exe` under `dist/x64/FileID/`. The WinUI 3 app builds from `FileID.sln` (`msbuild` / `dotnet build` — see `platforms/windows/CLAUDE.md`).

Prereqs:
- Visual Studio 2022 (or Build Tools) with the **.NET desktop development** workload plus the **Windows App SDK / WinUI** MSBuild tooling. WinUI 3's PriGen/MRT targets need VS's `AppxPackage` tasks; the standalone .NET SDK alone won't build the app.
- Rust 1.90 (`rustup install 1.90 && rustup default 1.90`). MSRV is pinned in `rust-toolchain.toml`.
- PowerShell 7+ (`pwsh`).

### macOS

```bash
git clone <repo>
cd FileID/platforms/apple
bash run.sh   # wipes DB + transient caches (keeps model weights), builds engine + app, opens the bundled .app
```

Prereqs:
- Xcode 16+ with the Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`).
- `cmake` for the MLX `mlx.metallib` GPU kernels (`brew install cmake`).

## Workflow

1. **Branch** from `main`. Name it after the change (`fix-prewarm-race`, not `bug123`).
2. **Make the change locally.** Build + test continuously, not at the end.
3. **Match the lint gates** before opening the PR:
   ```powershell
   # Windows — engine (from platforms/windows/src/engine)
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo deny check          # license + advisory + dup-version + source allowlist
   cargo audit               # advisory scan (CI runs this soft-warn; cargo deny is the hard gate)

   # Windows — app (from platforms/windows)
   dotnet format FileID.sln --verify-no-changes
   dotnet list package --vulnerable --include-transitive
   ```
   ```bash
   # macOS (from platforms/apple)
   swift-format lint --strict --recursive Sources Tests
   swift build -Xswiftc -warnings-as-errors
   ```
4. **Run the relevant tests.** See `TESTING.md`. Coverage on the touched modules must stay within 2 pp of the `COVERAGE.md` baseline (the gate is planned for the coverage CI job — see `COVERAGE.md`).
5. **Update docs** (`STATE.md`, `NEXT.md`, `DECISIONS.md`) per the rules below.
6. **Open the PR.** CI runs the same lint + test + privacy gates as your local checks (a cross-platform parity gate is planned but not yet implemented — see `TESTING.md`).

### CI matrix

| Workflow | Builds | Gates |
|---|---|---|
| `windows-engine.yml` | x64, arm64-native, arm64-cross | `cargo fmt --check`, `clippy --all-targets -D warnings`, `cargo deny`, soft-warn `cargo audit`, source-URL allowlist, build, test, startup + `verifyCudaPack` smokes, telemetry-string scan |
| `windows-app.yml` | x64, arm64 | `msbuild` Debug + Release, self-contained publish, xUnit tests, `dotnet format`, vulnerable-package scan, telemetry-string scan, startup smoke |
| `macos.yml` | swiftpm | `swift build`/`swift test`, startup smoke, source-URL allowlist, telemetry-string scan |

`cargo fmt --check` is effectively a no-op: `rustfmt.toml` sets `disable_all_formatting = true` (the codebase uses hand-aligned columns rustfmt can't preserve). The gate stays wired so it starts enforcing if that setting is ever dropped — style is enforced by review.

On-hardware verification (the third TESTING.md layer) runs on an RTX 2060 against the real corpus via `platforms/windows/build/iterate.ps1` and `platforms/apple/scripts/iterate.sh`.

## When to update which doc

- `shared/docs/STATE.md` — every meaningful change. Newest entry on top. One-paragraph summary plus what you ran to verify.
- `shared/docs/NEXT.md` — clear what's now done; add what your PR uncovered.
- `shared/docs/DECISIONS.md` — append-only. One entry per **non-obvious** decision (alternatives considered, why this one). Format: `## YYYY-MM-DD — Title`.
- `shared/docs/SHIP.md` — only on release-track changes.
- `shared/docs/COVERAGE.md` — only when the per-module baseline shifts.
- Per-platform `CLAUDE.md` — when you add a new module/directory.

## Hard rules (CI gates these — don't work around them)

1. **No telemetry, ever.** No analytics SDK, no crash reporter, no auto-update pings, no model-download instrumentation. The only outbound traffic is user-initiated model downloads from `huggingface.co` plus a small set of runtime/help hosts; the canonical list + rationale live in `PRIVACY.md`. CI scans every shipped binary for the 22 forbidden telemetry strings, and scans all source for off-allowlist URLs. Both are release blockers. Never weaken or remove these guarantees.
2. **Path redaction in every log line that contains a user path.** Rust: `redact_path_for_log(path)`. C#: `PathRedactor.Redact(path)`. Swift: `redactPathForLog(_:)`. Audited at PR time.
3. **No new dependency without a `DECISIONS.md` entry + sign-off.** Dev-deps and test-deps included — `cargo deny check` and the source-URL allowlist will catch them.
4. **Single-writer DB.** The engine owns the only writer connection. The app reads through ephemeral read-only connections. Migrations (Rust `db/`, Swift `Database.swift`) are append-only and must stay byte-faithful across the two engines.
5. **No `--no-verify`, no `--no-gpg-sign`, no skipping hooks.** If a hook fails, fix the underlying issue.
6. **No `#[allow(dead_code)]` / `#pragma warning disable` / `// swiftlint:disable` without a comment explaining why.** Silent suppression is a lint failure.
7. **`LavaLampBackground` is off-limits** (Swift `LavaLampBackground.swift` + the Win2D `LavaLampBackground.cs`). User's favorite touch; do not change without explicit sign-off.

## Common contribution recipes

### Adding a new IPC command

The IPC schema is the contract — it lands first, then each platform mirrors it.

1. Add the variant to `shared/ipc-schema/ipc.schema.json`. Bump the schema version.
2. Add the Rust variant to the `CommandPayload` enum in `platforms/windows/src/engine/src/ipc/mod.rs`.
3. Add the C# DTO in `platforms/windows/src/FileID.IpcSchema/CommandPayload.cs`.
4. Add the Swift variant in `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift`.
5. Write the handler: `platforms/windows/src/engine/src/commands/<domain>.rs` (Windows) and the Swift dispatcher (macOS).
6. Wire the dispatch arm: `handle_line` in `platforms/windows/src/engine/src/main.rs` (Windows) and `FileIDEngineMain.swift` (macOS).
7. Add a round-trip test in `FileID.IpcSchema.Tests/IpcCommandTests.cs` **and** `Tests/SharedTests/IPCProtocolTests.swift`.

Once the `shared/parity-tests/` harness exists (not yet — see `TESTING.md`), the parity job will catch any wire-shape drift between the two engines.

### Adding a new model

The model stack is commercial-clean — every default weight is Apache-2.0 or MIT. Keep it that way; a new model's license goes in the `DECISIONS.md` entry.

1. Add the entry to the model registry in `platforms/windows/src/engine/src/models/registry.rs` (append a `lookup_full` match arm + a `sentinel_path` arm) and the macOS analog. Sentinels land at `%LOCALAPPDATA%\FileID\Models\.sentinels\<id>.installed` (Windows) / `~/Library/Application Support/FileID/Models/.sentinels/` (macOS).
2. Add the ONNX/GGUF loader in `platforms/windows/src/engine/src/models/<name>.rs`.
3. Wire it into the pipeline — e.g. `ModelStack::load_default` in `pipeline/tagging.rs`.
4. Add the row to the Welcome-sheet model installer.
5. Document it in `shared/docs/MODELS.md`.

### Adding a test

See `TESTING.md`. Short version:
- **Rust:** inline `#[cfg(test)] mod tests` at the bottom of the module.
- **C#:** a `[Fact]` xUnit method in a `*Tests.cs` under `platforms/windows/Tests/FileID.App.Tests/` (or `FileID.IpcSchema.Tests/`). xUnit auto-discovers.
- **Swift:** a `@Test` Swift Testing function in a new file under `Tests/`.

### Adding a property test

`proptest` is already a Rust dev-dep. Use the macro inside a `#[cfg(test)]` block:

```rust
proptest::proptest! {
    #[test]
    fn my_invariant(input in "<strategy>") {
        proptest::prop_assert!(predicate(&input));
    }
}
```

The strategy is a regex-like generator. See `util/path_safety.rs` for working examples. (C# property tests via `FsCheck` are planned but not yet wired — see `TESTING.md`.)

### Adding a parity fixture

**Not yet implemented.** Neither `shared/parity-tests/` nor its CI job exists. When the harness lands, fixtures will live there with a README describing the format.

## Working on Restructure

Restructure is being overhauled to "butler-grade" — see `RESTRUCTURE.md` for the full design. The architecture is cluster-then-name: geometry finds groups from fused signals (CLIP + tags + time), a local VLM only names/justifies them, and a cheap classifier routes the long tail to the nearest existing folder. Phase 1 (semantic classify + learn-your-style routing) has landed in the engine (`pipeline/restructure_semantic.rs`, `cluster_suggestions.rs`); P2 VLM naming, P3 confidence tiers + reversible move journal, and P4 the Win2D Sankey upgrade follow. The Sankey is the chosen primary reorg visualization — match the macOS reference.

## Code style

Per all three `CLAUDE.md` files:

- **Default to no comments.** Add one only when the WHY is non-obvious (a workaround, a constraint, a perf invariant).
- **One commit per logical sub-step.** "Extract EngineProcessManager" is one commit. "Move 17 random things" is not.
- **Match the existing patterns** rather than inventing new ones. The codebase has consistent idioms across platforms — port them, don't reinterpret. The Windows + Linux apps are 1:1 ports of macOS: same palette, same springs, same `LavaLampBackground`, native primitives only (never web tech).
- **No backwards-compat shims** for code that's never shipped. If you remove a thing, remove it.
- **Error messages must be actionable.** "Couldn't open DB at C:\path — try reinstalling" is good; "DB error" is not.

## When in doubt

Open a draft PR with the change + a question in the description. The maintainer feedback loop is faster than guessing.

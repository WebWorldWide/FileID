# Contributing to FileID

> The 30-minute "you're new here" guide. Pair this with `TESTING.md` (how to test) and `COVERAGE.md` (per-module targets).

## Setup

### Windows

```powershell
git clone <repo>
cd FileID/platforms/windows
pwsh build/build.ps1            # x64 dev build of engine + app
pwsh build/build.ps1 -RunTests  # + cargo test + dotnet test
```

Prereqs:
- Visual Studio 2022 (or Build Tools) with the **.NET desktop development** + **Windows App SDK C++ tools** workloads.
- Rust 1.90 (`rustup install 1.90 && rustup default 1.90`).
- PowerShell 7+ (`pwsh`).

### macOS

```bash
git clone <repo>
cd FileID/platforms/apple
bash run.sh   # wipes DB, builds engine + app, opens the bundled .app
```

Prereqs:
- Xcode 15+ with the Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`).
- `cmake` for the MLX VLM kernels (`brew install cmake`).

## Workflow

1. **Branch** from `main`. Name it after the change (`fix-prewarm-race`, not `bug123`).
2. **Make the change locally**. Build + test continuously, not at the end.
3. **Match the lint gates** before opening the PR:
   ```powershell
   # Windows
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo deny check
   cargo audit
   dotnet format --verify-no-changes
   dotnet list package --vulnerable
   ```
   ```bash
   # macOS
   swift-format lint --strict --recursive Sources Tests
   swift build -Xswiftc -warnings-as-errors
   ```
4. **Run the relevant tests**. See `TESTING.md`. Coverage on the touched modules must stay within 2 pp of `COVERAGE.md` baseline.
5. **Update docs** (`STATE.md`, `NEXT.md`, `DECISIONS.md`) per the rules below.
6. **Open PR**. The CI matrix runs the same lint + test + privacy gates as your local checks. (A cross-platform parity gate is planned but not yet implemented — see `TESTING.md`.)

## When to update which doc

- `shared/docs/STATE.md` — every meaningful change. Top entry on top. One-paragraph summary plus what you ran to verify.
- `shared/docs/NEXT.md` — clear what's now done; add what your PR uncovered.
- `shared/docs/DECISIONS.md` — append-only. One entry per **non-obvious** decision (alternatives considered, why this one). Format: `## YYYY-MM-DD — Title`.
- `shared/docs/SHIP.md` — only on release-track changes.
- `shared/docs/COVERAGE.md` — only when the per-module baseline shifts.
- Per-platform `CLAUDE.md` — when you add a new module/directory.

## Hard rules (CI gates these — don't try to work around)

1. **No telemetry, ever.** No analytics SDK, no crash reporter, no auto-update pings, no model-download instrumentation. The only allowed outbound traffic is the user-initiated model downloads to the 5-host allowlist (HuggingFace, GitHub, nvidia.com, plus the two help links). Detailed list + rationale in `PRIVACY.md`. CI binary scan rejects any of the 22 forbidden telemetry strings.
2. **Path redaction in every log line containing user paths.** Rust: `redact_path_for_log(path)`. C#: `PathRedactor.Redact(path)`. Swift: `redactPathForLog(_:)`. Audited at PR time.
3. **No new dependency without a `DECISIONS.md` entry + sign-off.** Test deps included. Even dev-deps need the entry — they show up in `cargo deny check`.
4. **Single-writer DB.** The engine owns the only writer connection. The app reads via ephemeral read-only connections.
5. **No `--no-verify`, no `--no-gpg-sign`, no skipping hooks.** If a hook fails, fix the underlying issue.
6. **No `#[allow(dead_code)]` / `#pragma warning disable` / `// swiftlint:disable` without a comment explaining why.** Silent suppression is a CI lint failure.
7. **`LavaLampBackground` is off-limits** (Swift + C# variants). User's favorite touch; do not change without explicit sign-off.

## Common contribution recipes

### Adding a new IPC command

1. Add the variant to `shared/ipc-schema/ipc.schema.json`. Bump the schema version.
2. Hand-add the Rust variant in `engine/src/ipc/mod.rs` (`CommandPayload` enum).
3. Hand-add the C# DTO in `FileID.IpcSchema/CommandPayload.cs`.
4. Hand-add the Swift variant in `shared/Sources/FileIDShared/IPCProtocol.swift`.
5. Write the handler in `engine/src/commands/<domain>.rs` (Windows) and the Swift dispatcher (macOS).
6. Wire the dispatcher call in `engine/src/main.rs` `handle_line` (Windows) and `FileIDEngineMain.swift` (macOS).
7. Add a round-trip test in `FileID.IpcSchema.Tests/IpcCommandTests.cs` AND `Tests/SharedTests/IPCProtocolTests.swift`.
8. Once the `shared/parity-tests/` harness exists (not yet implemented — see `TESTING.md`), the parity test will catch any wire-shape drift.

### Adding a new model

1. Add the registry entry in `engine/src/models/registry.rs` (Windows) + the macOS analog.
2. Add a `.installed` sentinel path under `%LOCALAPPDATA%\FileID\Models\.sentinels\` (Windows) / `~/Library/Application Support/FileID/Models/.sentinels/` (macOS).
3. Add the ONNX / GGUF loader in `engine/src/models/<name>.rs`.
4. Wire it into `pipeline/tagging.rs` `ModelStack::load_default`.
5. Add a row to the Welcome sheet model installer.
6. Document in `shared/docs/MODELS.md`.

### Adding a new test

See `TESTING.md`. The short version:
- Inline `#[cfg(test)]` at the bottom of the Rust module.
- `[Fact]` xUnit method in a new `*Tests.cs` under `Tests/FileID.App.Tests/`.
- `@Test` Swift Testing function in a new file under `Tests/`.

### Adding a property test

`proptest` is a Rust dev-dep already. Use the macro:

```rust
proptest::proptest! {
    #[test]
    fn my_invariant(input in "<strategy>") {
        proptest::prop_assert!(predicate(&input));
    }
}
```

The strategy is a regex-like generator. See `util/path_safety.rs` for working examples.

### Adding a parity fixture

**Not yet implemented.** The `shared/parity-tests/` directory and its CI job don't exist yet. When the harness lands, fixtures will live there with a README describing the format.

## Code style

Per all three `CLAUDE.md` files:

- **Default to no comments.** Only add when the WHY is non-obvious (a workaround, a constraint, a perf invariant).
- **One commit per logical sub-step.** "Extract EngineProcessManager" is one commit. "Move 17 random things" is not.
- **Match the existing patterns** rather than inventing new ones. The codebase has consistent idioms across platforms — port them, don't reinterpret.
- **No backwards-compat shims** for code that's never been released. If you remove a thing, remove it.
- **Error messages must be actionable.** "Couldn't open DB at C:\path — try reinstalling" is good; "DB error" is not.

## When in doubt

Open a PR with a draft + a question in the description. The maintainer feedback loop is faster than guessing right.

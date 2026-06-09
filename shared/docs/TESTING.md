# FileID — Testing Guide

> Testing philosophy, per-platform commands, and how to add a new test. Companion to `COVERAGE.md` (per-module targets) and `CONTRIBUTING.md` (PR workflow).

FileID is two binaries per platform — a Rust/Swift engine and a C#/Swift app — talking newline-delimited JSON over stdio, with the engine owning a SQLite WAL DB (migrations v1–v12, byte-faithful with macOS GRDB). Tests live at every layer of that stack and are enforced by CI: `windows-engine.yml`, `windows-app.yml`, `macos.yml`.

## Philosophy

Three layers, in order of cost:

1. **Unit / example tests** — fast, specific input → expected output. Most of the suite. Inline `#[cfg(test)]` (Rust), xUnit `[Fact]` (C#), `@Test` (Swift).
2. **Property tests** — generate random inputs and assert invariants hold. `proptest` (Rust; a dev-dependency, used across `util::path_safety`, `util::zip`, `util::hmac`, `ipc`, `pipeline::dbwriter`, `pipeline::face_clustering`, and more). The layer that catches the bugs you'd never write a unit test for.
3. **Integration tests** — slow (seconds-to-minutes), spawn the real engine, drive a real corpus, assert end-to-end behavior. `iterate.ps1` (Windows) + `iterate.sh` (macOS).

Layers that are **planned but not yet implemented** (do not assume they run today):

- **C# property tests** — `FsCheck.Xunit` is not yet a dependency; no C# property tests exist.
- **Fuzz testing** — no `cargo-fuzz` or `SharpFuzz` targets exist.
- **Cross-platform parity testing** — the idea is that the Rust and Swift engines must produce byte-identical output for a shared fixture. Neither the `shared/parity-tests/` harness nor a CI parity job exists yet.

## What CI runs

All three workflows run on push to `main` and on PRs touching the relevant paths.

### `windows-engine.yml` (Rust) — x64, arm64-native, arm64-cross

- `cargo fmt --all -- --check` (placeholder: `rustfmt.toml` disables all formatting).
- `cargo clippy --target <t> --all-targets -- -D warnings` (style-only lints are `allow`-listed in `Cargo.toml`, with justifications).
- `cargo deny check` (x64) — license allowlist + advisory + duplicate-version + source allowlist. This is the **hard** advisory gate.
- `cargo audit` (x64) — **soft-warn** (`continue-on-error`). See the `DECISIONS.md` 2026-05-16 entry for why; `cargo deny` is the real gate.
- Source URL allowlist scan (x64) — greps all `.rs/.cs/.xaml` source for `https?://` hosts; every host must be on the allowlist (`huggingface.co`, `github.com`, `developer.download.nvidia.com`, `developer.nvidia.com`, plus XAML namespace URNs). Loopback is exempt (the local llama-server is IPC, not egress).
- `cargo build --release` then `cargo test --target <t> --all-targets` (tests skipped on `arm64-cross`).
- Smokes: engine startup emits a `ready` event carrying `HardwareInfo.executionProvider`; `verifyCudaPack` emits a `hardwareReprobed` event. CI hosts have no GPU, so the smoke asserts CPU fallback works on each arch.
- **Telemetry-string scan** of `FileIDEngine.exe` (ASCII + UTF-16). Failing is a release blocker, no exception.

### `windows-app.yml` (.NET) — x64, arm64

- `msbuild` Debug + Release + self-contained Publish (msbuild, not `dotnet build`, so WinUI 3's PriGen/MRT/XAML tooling is found).
- `dotnet test` of both test projects (x64 only): `FileID.IpcSchema.Tests` and `FileID.App.Tests`.
- `dotnet format FileID.sln --verify-no-changes` (x64).
- `dotnet list package --vulnerable --include-transitive` — fails on any vulnerable package (x64).
- **Telemetry-string scan** of the published `FileID.exe` and every shipped `*.dll` (x64 + arm64).
- App startup smoke (x64) — launches the published exe; a crash inside `OnLaunched` surfaces as an immediate non-zero exit.

### `macos.yml` (Swift) — macos-15

- `swift build -c release` (engine + app), `swift test` (SharedTests + EngineTests; corpus tests self-skip when the corpus isn't present).
- Engine startup smoke (asserts a `ready` event; note the macOS engine writes IPC to **stderr**, the Windows engine to **stdout**).
- Source URL allowlist scan (same allowlist as Windows) + **telemetry-string scan** of both binaries.

The forbidden-telemetry-string list is identical across all three workflows and `platforms/windows/build/publish-bundle.ps1`; changes must land in all four.

## Per-platform commands

### Windows engine (Rust)

```powershell
cd platforms/windows/src/engine

# All tests, including property tests (mirror CI with --all-targets)
cargo test --target x86_64-pc-windows-msvc --all-targets

# One module
cargo test --target x86_64-pc-windows-msvc util::path_safety

# Show println!() output
cargo test --target x86_64-pc-windows-msvc -- --nocapture

# Coverage (requires `cargo install cargo-llvm-cov` once)
cargo llvm-cov --workspace --html --lcov --output-dir target/coverage
# → HTML at target/coverage/index.html

# Lint + format + supply-chain (CI gates)
cargo fmt --all -- --check
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
cargo deny check
cargo audit   # soft-warn in CI; cargo deny is the hard advisory gate
```

The MSRV is pinned to **1.90** in `rust-toolchain.toml`; CI uses the same toolchain.

### Windows app (.NET / xUnit)

```powershell
cd platforms/windows

# Both test projects
dotnet test Tests/FileID.IpcSchema.Tests/FileID.IpcSchema.Tests.csproj
dotnet test Tests/FileID.App.Tests/FileID.App.Tests.csproj

# Coverage
dotnet test --collect:"XPlat Code Coverage" --results-directory coverage
reportgenerator -reports:coverage/**/coverage.cobertura.xml -targetdir:coverage/html

# Format + vuln scan (CI gates)
dotnet format FileID.sln --verify-no-changes
dotnet list package --vulnerable --include-transitive
```

`FileID.IpcSchema.Tests` targets `net8.0`; `FileID.App.Tests` targets `net8.0-windows10.0.19041.0` (x64-only) because the app does. Building the WinUI app via `dotnet build` alone fails on a GitHub runner without VS's AppxPackage tasks — CI uses `msbuild`.

### Windows integration (`iterate.ps1`)

```powershell
# Against a real library (recommended — there is no committed corpus)
pwsh platforms/windows/build/iterate.ps1 -Corpus C:\Users\you\Pictures

# Custom throughput floor (default 100 files/sec; RTX-class is 140)
pwsh platforms/windows/build/iterate.ps1 -Corpus C:\path -ThroughputTarget 140

# Useful switches: -SkipBuild, -SkipWipe (incremental rescan), -Verbose
# Exit codes: 0 = all pass; 1 = assertion fail; 2 = environment/build fail
```

`iterate.ps1` builds the engine, colocates the pinned ONNX Runtime + DirectML DLLs, wipes the DB, drives a full scan + face-clustering pass, then runs assertions A1–A12. It defaults `-Corpus` to `shared/test-corpus`, but that directory ships empty — unlike macOS there is no corpus generator, so pass `-Corpus` at a real library. A1–A11 are inline PowerShell checks (no crash, throughput, memory cap, WAL checkpointed, no WER dumps, no telemetry strings in the engine binary, …); A12 shells out to `build/scan_assertions.py` to verify DB content: that RAM++/CLIP actually emitted tags and that face embeddings are 128-d SFace blobs (512 bytes), which rejects stale 512-d/2048-byte ArcFace data from the old face stack.

### macOS engine + app (Swift)

```bash
cd platforms/apple
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test

swift test --enable-code-coverage
swift-format lint --strict --recursive Sources Tests
swift build -Xswiftc -warnings-as-errors
```

### macOS integration (`iterate.sh`)

```bash
cd platforms/apple
bash scripts/iterate.sh   # same 0/1/2 exit-code semantics as iterate.ps1
```

`iterate.sh` drives the engine through a full scan + face clustering, then delegates its assertions to `scripts/test_assertions.py`. The fixture corpus is auto-generated by `scripts/build_corpus.sh` and is **not committed** (see `Tests/Corpus/README.md`).

## How to add a new test

### Example-based unit test

- **Rust**: add `#[cfg(test)] mod tests { use super::*; ... }` to the bottom of the module. Use `assert_eq!`, `assert!`, `?` on `Result`.
- **C#**: add a `*Tests.cs` to the relevant project under `Tests/`. `public class FooTests` with `[Fact]` methods; xUnit auto-discovers.
- **Swift**: add `@Test func …` to a Swift Testing struct under `Tests/`; `swift test` auto-discovers.

### Property test (Rust)

```rust
proptest::proptest! {
    #[test]
    fn my_invariant(s in "[a-z]{1,10}") {
        proptest::prop_assert!(my_fn(&s));
    }
}
```

For Swift, prefer `@Test(arguments: [...])` parameterized cases — no new dependency. C# property tests are not wired up yet.

### Integration assertion

Add an assertion to `iterate.ps1` (Windows) or `iterate.sh` / `test_assertions.py` (macOS). Both harnesses drive a real engine via stdin commands and check DB rows + exit codes after the scan. Follow the existing A1–A12 format on Windows.

## Test reading order for a new contributor

1. `platforms/windows/src/engine/src/util/path_safety.rs` — example + property tests for the simplest module.
2. `platforms/windows/Tests/FileID.IpcSchema.Tests/IpcCommandTests.cs` — round-trip xUnit tests.
3. `platforms/apple/Tests/SharedTests/IPCProtocolTests.swift` — Swift Testing equivalents.
4. `platforms/windows/build/iterate.ps1` — the A1–A12 integration harness.

## Common gotchas

- **Rust:** clippy can flag legitimate test patterns (e.g. `assert_eq!(x, x)` in a determinism check). Annotate with `#[allow(clippy::eq_op)]` plus a one-line reason.
- **C#:** `FileID.App.Tests` targets `net8.0-windows10.0.19041.0` and is x64-only because the app is. Keep app-service tests UI-thread-independent — `DispatcherObject` types (BitmapImage, SolidColorBrush) need a manual harness, which is deferred.
- **Swift Testing:** `@Test` is the current world; `XCTest` is legacy. New code uses `@Test`. A few migrated tests still use `XCTest` — fine for now.
- **Coverage gate:** `COVERAGE.md` is the per-module baseline; a drop > 2 percentage points on a module blocks merge. If a refactor legitimately drops coverage, bump the baseline in the same PR with a one-line note.

## Privacy posture for tests

- Tests MUST NOT make network calls (HuggingFace, GitHub, anywhere). The engine's only network surface is `downloader.rs`, and it is mocked in tests.
- Tests MAY write to `std::env::temp_dir()` but MUST clean up.
- Tests MUST NOT touch the user's real `%LOCALAPPDATA%\FileID\` — use a temp dir or in-memory SQLite (`Connection::open_in_memory()`).

See `shared/docs/PRIVACY.md` for the full enforcement story (CI telemetry-string scan + source URL allowlist).

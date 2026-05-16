# FileID — Testing Guide

> Testing philosophy, per-platform commands, and how to add a new test. Companion to `COVERAGE.md` (per-module targets) and `CONTRIBUTING.md` (PR workflow).

## Philosophy

Three layers, in order of cost:

1. **Unit / example tests** — fast (< 1 ms each), specific input → expected output. Most of the suite. Inline `#[cfg(test)]` (Rust), xUnit `[Fact]` (C#), `@Test` (Swift).
2. **Property tests** — fast (~100 cases each), generates random inputs and asserts invariants hold. `proptest` (Rust), `FsCheck` (C#), `@Test(arguments:)` parameterized (Swift). The layer that catches the bugs you'd never write a unit test for.
3. **Integration tests** — slow (seconds-to-minutes), spawns real engine, drives a real corpus, asserts end-to-end behavior. `iterate.ps1` (Windows) + `iterate.sh` (macOS).

Plus three optional depth layers:

4. **Fuzz testing** — `cargo-fuzz` (Rust), `SharpFuzz` (C#). Weekly cron, out of PR CI.
5. **Parity testing** — Rust and Swift engines must produce byte-identical outputs for the same fixture. `shared/parity-tests/`. PR gate.
6. **Snapshot testing** — `swift-snapshot-testing` for the six main macOS views. WinUI 3 snapshots are deferred (Mica + theme flakiness).

## Per-platform commands

### Windows engine (Rust)

```powershell
# All tests including property tests
cd platforms/windows/src/engine
cargo test --target x86_64-pc-windows-msvc

# Just one module
cargo test --target x86_64-pc-windows-msvc util::path_safety

# Verbose: show println!() output
cargo test --target x86_64-pc-windows-msvc -- --nocapture

# Coverage report (requires `cargo install cargo-llvm-cov` once)
cargo llvm-cov --workspace --html --lcov --output-dir target/coverage
# → HTML at target/coverage/html/index.html

# Lint + format (Phase 6 gate)
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings

# Security + license audit (Phase 6 gate)
cargo deny check
cargo audit
```

### Windows app (.NET / xUnit)

```powershell
cd platforms/windows
dotnet test FileID.sln -c Debug

# Just one project
dotnet test Tests/FileID.App.Tests/FileID.App.Tests.csproj

# Coverage report
dotnet test --collect:"XPlat Code Coverage" --results-directory coverage
reportgenerator -reports:coverage/**/coverage.cobertura.xml -targetdir:coverage/html

# Lint + format (Phase 6 gate)
dotnet format --verify-no-changes
dotnet list package --vulnerable --include-transitive
```

### Windows integration

```powershell
# 5K-file synthetic corpus, default 11 assertions
pwsh platforms/windows/build/iterate.ps1

# Real library
pwsh platforms/windows/build/iterate.ps1 -Corpus C:\Users\you\Pictures

# Custom throughput target (per SHIP.md Appendix W)
pwsh platforms/windows/build/iterate.ps1 -ThroughputTarget 140

# Exit codes: 0 = all pass; 1 = assertion fail; 2 = environment/build fail
```

### macOS engine + app (Swift)

```bash
cd platforms/apple
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test

# Coverage report
swift test --enable-code-coverage
xcrun llvm-cov export --format=lcov \
  -instr-profile=$(swift test --show-codecov-path | head -n1 | xargs dirname)/default.profdata \
  $(swift build --show-bin-path)/FileIDPackageTests.xctest/Contents/MacOS/FileIDPackageTests \
  > coverage/swift.lcov

# Format + lint (Phase 6 gate)
swift-format lint --strict --recursive Sources Tests
swift build -Xswiftc -warnings-as-errors
```

### macOS integration

```bash
cd platforms/apple
bash scripts/iterate.sh

# Same exit-code semantics as iterate.ps1.
```

## How to add a new test

### Example-based unit test

1. **Rust**: add `#[cfg(test)] mod tests { use super::*; ... }` to the bottom of the module file. Use `assert_eq!`, `assert!`, `?` for `Result`.
2. **C#**: add a new `*Tests.cs` to `Tests/FileID.App.Tests/`. Class `public class FooTests` with `[Fact]` methods. xUnit auto-discovers.
3. **Swift**: add `@Test func ...` to a Swift Testing struct under `Tests/`. `swift test` auto-discovers.

### Property test

1. **Rust**: dev-dep `proptest = "1"` (already added). Inside a `#[cfg(test)] mod tests` block, use:
   ```rust
   proptest::proptest! {
       #[test]
       fn my_invariant(s in "[a-z]{1,10}") {
           proptest::prop_assert!(my_fn(&s));
       }
   }
   ```
2. **C#**: dev-dep `FsCheck.Xunit` (pending Phase 7). Use `[Property]` instead of `[Fact]`.
3. **Swift**: use `@Test(arguments: [...])` parameterized. No new dep needed.

### Integration test

Add a new assertion to `iterate.ps1` (Windows) or `iterate.sh` (macOS). The harnesses both drive a real engine via stdin commands and check DB rows + exit codes after the scan completes. See the existing 11 assertions for format.

### Parity test (cross-platform regression guard)

Add a deterministic fixture to `shared/parity-tests/`. The CI parity job runs the Rust engine against the fixture, exports JSON, and asserts byte-identical with the Swift engine's pre-recorded snapshot. Any platform drift fails CI. Pending implementation in Phase 7.

## Test reading order for a new contributor

1. `platforms/windows/src/engine/src/util/path_safety.rs` — example + property tests for the simplest module.
2. `platforms/windows/Tests/FileID.IpcSchema.Tests/IpcCommandTests.cs` — round-trip xUnit tests.
3. `platforms/apple/Tests/SharedTests/IPCProtocolTests.swift` — Swift Testing equivalents.
4. `platforms/apple/scripts/iterate.sh` — the 11-assertion integration harness.

## Common gotchas

- **Rust:** clippy can flag legitimate test patterns (e.g., `assert_eq!(x, x)` in a determinism check). Annotate with `#[allow(clippy::eq_op)]` + a one-line comment.
- **C#:** the `FileID.App.Tests` project targets `net8.0-windows10.0.19041.0` because the app does. Tests that only need `net8.0` should still match — keeps the test discovery uniform.
- **Swift Testing:** the `swift-testing` framework is the new world; `XCTest` is legacy. New code uses `@Test`, not `XCTestCase`. A few migrated tests still use `XCTest` — fine for now.
- **CI coverage:** the Phase 8 gate compares against the baseline in `COVERAGE.md`. If you legitimately need to drop coverage on a module (e.g., you split a function and the tests haven't migrated yet), bump the baseline in the same PR with a one-line note.

## Privacy posture for tests

- Tests MUST NOT make network calls (HuggingFace, GitHub, anywhere). The engine's only network surface is `downloader::*` and that's mocked in tests.
- Tests MAY write to `std::env::temp_dir()` but MUST clean up.
- Tests MUST NOT read the user's real `%LOCALAPPDATA%\FileID\` — use a temp dir or an in-memory SQLite (`Connection::open_in_memory()`).

See `shared/docs/PRIVACY.md` for the full enforcement story (CI binary scan + URL allowlist).

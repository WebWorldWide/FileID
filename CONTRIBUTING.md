# Contributing to FileID

The full guide — environment setup, build-from-source for each platform, the CI
gates your change has to pass, testing, and troubleshooting — lives at
**[`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md)**.

This file exists so GitHub surfaces that guide from the repo root.

## The short version

```bash
./build.sh -windows    # or -mac / -linux — build and run
```

Before you open a pull request, the gates that matter:

```bash
# Rust — engine, CLI, TUI, and the Linux app
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check

# Windows app (.NET) — note the Tests projects sit outside FileID.sln
dotnet build platforms/windows/FileID.sln
dotnet format platforms/windows/FileID.sln --verify-no-changes
dotnet test platforms/windows/Tests/FileID.App.Tests
dotnet test platforms/windows/Tests/FileID.IpcSchema.Tests

# Repository policy (action pins, supply chain, licenses, egress, docs)
python -m unittest discover -s shared/scripts -p 'test_*.py'
```

`shared/scripts/run_local_audit_gate.sh` runs the whole set in one go.

## House rules worth knowing up front

- **No telemetry, ever.** No analytics, crash reporting, or update pings. CI
  scans every shipped binary against a deny-list as a release blocker.
- **The IPC schema is the contract.** Anything new lands in
  `shared/ipc-schema/ipc.schema.json` first; the Swift, Rust, and C# DTOs mirror
  it. Schema drift is a build break.
- **macOS is the visual reference.** The Windows and Linux apps are 1:1 ports —
  same palette, same springs, same `LavaLampBackground`. Native primitives only;
  no web tech.
- **No new dependencies without asking.** Each platform has a locked set; a new
  crate or package needs a justification in `shared/docs/DECISIONS.md`.
- **Comments explain *why*, not *what*.** Add one only when the reason is
  non-obvious — a workaround, an invariant, a performance pitfall.
- **Rust is pinned to 1.90** (`rust-toolchain.toml`), which is what CI uses. If
  you have a newer standalone Rust ahead of the rustup shims on `PATH`, your
  local build is not testing what CI tests.

Found a security problem? Don't open an issue — see [`SECURITY.md`](SECURITY.md).

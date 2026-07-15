# Contributing to FileID

> The "you're new here" guide. Pair this with `TESTING.md` (how to test) and `COVERAGE.md` (per-module targets).

FileID is an on-device, privacy-first AI file organizer — tag, dedupe, restructure, rename tens of thousands of files locally. Three desktop apps share one cross-platform Rust engine: **macOS** (Swift/SwiftUI engine + app, MLX — the visual reference), **Windows** (Rust engine `fileid-engine` + WinUI 3 / .NET 8 C# app), and **Linux** (GTK4 + libadwaita over the same engine), plus a headless `fileid` CLI and a ratatui TUI. On every platform two binaries talk newline-delimited JSON over stdio; the engine owns a SQLite WAL database (single writer). The project is Apache-2.0.

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

### CLI (`fileid`) — cross-platform

The `fileid` command-line front-end lives at `platforms/cli/`. **It's cross-OS** (builds and runs on macOS, Linux, and Windows despite living under `platforms/`): it links the shared Rust engine as a library and uses only portable `std` + bundled SQLite. It is its own standalone Cargo workspace.

```bash
git clone <repo>
cd FileID/platforms/cli
cargo build --release          # compiles the shared engine too (first build is slow)
./target/release/fileid --help

# self-verify (same gates as the engine; pinned Rust 1.90 via rust-toolchain.toml)
cargo clippy --all-targets -- -D warnings
cargo test                     # model-free, isolated scan→search→info smoke test
```

There is **no separate engine build step** — `fileid-engine` is a path dependency (`../windows/src/engine`, `default-features = false`) compiled into the single `fileid` binary. The MVP is read/query + plan only (model-free); see `platforms/cli/README.md` for the command reference, the library-location rules (`--db` / `$FILEID_DB` / `$CFFIXED_USER_HOME` / engine default), and the documented follow-ons (apply commands, semantic search, full-pipeline scan, TUI).

Prereqs:
- Rust 1.90 (`rustup install 1.90`). The crate pins it via `rust-toolchain.toml`.
- A C toolchain for the engine's bundled SQLite / native deps (Xcode CLT on macOS, `build-essential` on Linux, MSVC on Windows) — same as building the engine.

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
| `linux.yml` | ubuntu engine + CLI + TUI + GTK app | locked format/clippy `-D warnings`, tests, builds, schema checks, and telemetry-string scans |
| `packaging.yml` | GNOME 49 Flatpak | generated Cargo-source drift test, offline-sandbox release build, bundle + SHA-256 artifact |
| `policy.yml` | workflow policy | immutable external Action reference enforcement + negative tests |

`cargo fmt --check` is effectively a no-op: `rustfmt.toml` sets `disable_all_formatting = true` (the codebase uses hand-aligned columns rustfmt can't preserve). The gate stays wired so it starts enforcing if that setting is ever dropped — style is enforced by review.

On-hardware verification (the third TESTING.md layer) runs on an RTX 2060 against the real corpus via `platforms/windows/build/iterate.ps1` and `platforms/apple/scripts/iterate.sh`.

## Linux distribution & packaging

The Linux GTK4 app ships to **every distro** through declarative packaging under `packaging/` — see [`packaging/README.md`](../../packaging/README.md) for the full matrix and per-channel build commands. One Cargo binary (`fileid-linux`) plus the engine it spawns (`FileIDEngine`); each channel just wraps them and reuses the **single** desktop/metadata/icon source in `platforms/linux/data/` (never a copy).

| Channel | Covers | Native dep on |
|---|---|---|
| **Flatpak** (primary) | Debian/Ubuntu/Arch/Gentoo/NixOS/Fedora/openSUSE — anywhere Flatpak runs | GNOME 49 runtime (backward-compatible with the GTK 4.14/libadwaita 1.5 API floor) |
| **AppImage** (secondary) | Most x86_64 distros, no install/sandbox | bundles its own GTK4/libadwaita |
| **Nix flake** | NixOS / any Nix user | nixpkgs `gtk4`/`libadwaita`/`onnxruntime` |
| **AUR `PKGBUILD`** | Arch / Manjaro / EndeavourOS | system `gtk4`/`libadwaita`/`onnxruntime` |

Rules when touching packaging:

- **Reuse `platforms/linux/data/`.** The `.desktop`, `metainfo.xml`, and `.svg` icon are the single source for all channels; add new desktop assets there, not in a channel directory.
- **No telemetry in `finish-args`.** The Flatpak grants `--share=network` for exactly one reason — user-initiated HuggingFace model downloads — and never `--filesystem=host`. Do not add host-wide or background-network permissions.
- **Keep Flatpak source-complete and offline.** `cargo-sources.json` must exactly match `platforms/linux/Cargo.lock`; run `generate-cargo-sources.py --check`. The Rust SDK extension supplies the toolchain, and the SHA-pinned ONNX Runtime archives enter through manifest sources plus `ORT_LIB_LOCATION`. Never restore rustup, build-network access, or a Cargo/ort download fallback.
- **Flatpak is a required gate.** `.github/workflows/packaging.yml` builds and bundles it without `continue-on-error`. Before Flathub submission, replace the bounded local source directories with the immutable archive of the audited release commit and regenerate sources from that archive's lockfile.

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

---

# Build & run — full reference

> The **Setup** section above is the minimal engine/CLI build. This appendix is the full app build-and-run flow (the one-command path the root `README.md` points here for), plus release packaging, troubleshooting, and the hardware/ML reference tables.

## One command, every platform

From the repo root, in any bash shell (Git Bash on Windows, Terminal on macOS, anything on Linux):

```bash
./build.sh -windows                    # Windows: full fresh-install build + run
./build.sh -mac                        # macOS:   build + launch
bash platforms/linux/build/build.sh    # Linux:   build the GTK4 + libadwaita app + run
```

Defaults pick a sensible "I want to see this run" path: wipe any prior install, build Release, drop a runnable copy at `~/Desktop/FileID/`, and launch the app.

On Windows without a bash shell, `build.sh` is just a dispatcher to a PowerShell script — call it directly (works in built-in Windows PowerShell 5.1 *and* PowerShell 7):

```powershell
# From the repo root. Equivalent to ./build.sh -windows
.\platforms\windows\build\build-all.ps1 -Wipe -Release -Desktop -Run
```

> Use `.\platforms\windows\build\build-all.ps1`, **not** `pwsh ...`. If you copied a `pwsh` command and got `'pwsh' is not recognized`, you have Windows PowerShell 5.1 (no `pwsh` on PATH) — drop the `pwsh` prefix and run the `.ps1` directly, or `winget install Microsoft.PowerShell` to get PowerShell 7.

> ⚠️ **`./build.sh -windows` defaults to wiping your local install.** It deletes `%LOCALAPPDATA%\FileID\` — including downloaded model weights (multi-GB) and your scan database. Pass `--no-wipe` to iterate without re-downloading.

```bash
./build.sh -windows --no-wipe       # iterate without re-downloading models
./build.sh -windows --no-run        # just build, don't launch
./build.sh -windows --debug         # debug build (faster cycle)
./build.sh --help                   # full flag list
```

## Windows

One-time setup (~10 minutes if you don't have the toolchains):

| Tool | Version | Install |
| --- | --- | --- |
| Rust | 1.90+ | https://rustup.rs |
| .NET SDK | 8 or 9 | `winget install Microsoft.DotNet.SDK.8` |
| Visual Studio Build Tools 2022 | 17.x with UWP MSBuild component | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.UWP.MSBuild"` |
| (ARM64 cross-compile only) | MSVC ARM64 toolchain | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.VC.Tools.ARM64"` |

PowerShell — either built-in Windows PowerShell 5.1 or PowerShell 7 (`winget install Microsoft.PowerShell`) works.

`./build.sh -windows` maps to `build-all.ps1 -Wipe -Release -Desktop -Run` and does, in order:

1. **Wipe** any prior FileID install — `~\Desktop\FileID\`, `%LOCALAPPDATA%\FileID\` (DB + models + logs), and build artifacts (`target/`, `bin/`, `obj/`, `dist/`). This is the fresh-install path; pass `--no-wipe` to iterate without losing downloaded models.
2. Probe toolchains; print the exact `winget` install command if any are missing.
3. `cargo build --release --target x86_64-pc-windows-msvc` → `FileIDEngine.exe`.
4. `dotnet publish FileID.App --self-contained` → `FileID.exe` + companion DLLs.
5. Stage `FileIDEngine.exe` alongside `FileID.exe`.
6. Copy the publish folder to `~\Desktop\FileID\`.
7. Launch `FileID.exe`.

Unified `./build.sh` flags:

| Flag | What it does |
| --- | --- |
| (default) `-windows` | Wipe + Release + Desktop staging + Run |
| `--no-wipe` | Skip the destructive wipe (preserves models + DB) |
| `--no-run` | Build only, don't launch |
| `--no-desktop` | Build but don't stage to Desktop |
| `--debug` | Debug build (faster iteration; needs .NET SDK on host to launch) |
| `--tests` | Run cargo + dotnet tests |
| `--arm64` | Cross-compile for Snapdragon WoA |
| `--vlm-native` | Build with native llama.cpp bindings (requires cmake) |
| `--sign` | Authenticode-sign every binary (needs `FILEID_SIGN_THUMBPRINT` env var) |
| `--help` | Full flag list |

Underlying `build-all.ps1` flags (use directly when you want finer control):

| Flag | What it does |
| --- | --- |
| `-Wipe` | Full destructive wipe (Desktop + LocalAppData + build artifacts) |
| `-Wipe -PreserveModels` | Full wipe **except** downloaded model weights — DB + logs + settings + sentinels cleared, no multi-GB re-download |
| `-WipeDbOnly` | Lightest wipe — delete only `fileid.sqlite{,-wal,-shm}` for a fresh scan; keeps models, logs, settings, and build artifacts |
| `-Clean` | Wipe build artifacts only (cargo + dotnet + `dist/`; preserves all user data) |
| `-Desktop` | Stage to Desktop (implies `-Release`) |
| `-Run` | Launch the app after build |
| `-Release` | Release build (default for the unified script) |
| `-RunTests` | Run cargo + xUnit tests |
| `-SkipEngine` | Only rebuild the WinUI 3 app |
| `-SkipApp` | Only rebuild the Rust engine |
| `-Arm64` | Cross-compile for ARM64 |
| `-VlmNative` | Native llama.cpp bindings |
| `-Sign -Thumbprint <hex>` | Authenticode-sign every binary |

**The three iteration commands you'll reach for most** — run from the repo root in Windows Terminal. These assume PowerShell 7 (`pwsh`); on built-in Windows PowerShell 5.1 just drop the `pwsh` prefix and call `.\platforms\windows\build\build-all.ps1 …` directly. All three build **Debug** (engine + app) by default — add `-Release` for the slower self-contained build that ships, and `-Run` to launch the app when the build finishes.

```powershell
# 1. Build clean — clear build artifacts (cargo clean + dotnet clean + dist/),
#    then a full from-scratch rebuild. Your library DB and downloaded models are
#    left untouched. Use when a build is behaving stale or after switching branches.
pwsh platforms\windows\build\build-all.ps1 -Clean

# 2. Build + database wipe, keep models — incremental rebuild, then delete ONLY
#    fileid.sqlite{,-wal,-shm} so the next launch re-scans and re-tags from scratch.
#    Downloaded models (and logs/settings) survive, so nothing re-downloads.
#    Close the app first: a running engine holds the SQLite file open.
pwsh platforms\windows\build\build-all.ps1 -WipeDbOnly

# 3. Just rebuild — fast incremental build of engine + app, no wipe of anything.
pwsh platforms\windows\build\build-all.ps1
```

Examples with the optional add-ons: `... -Clean -Run` (clean rebuild then launch), `... -WipeDbOnly -Run` (fresh scan then launch), `... -Run` (rebuild then launch). Want the heavier "fresh install but don't re-download the multi-GB models" reset instead of just the DB? Use `-Wipe -PreserveModels`.

### Release build (one downloadable installer for everyone)

```powershell
# Local test build (no signing)
.\platforms\windows\build\publish-bundle.ps1 -SkipSign

# Signed release with a certificate already available to SignTool
.\platforms\windows\build\publish-bundle.ps1 `
  -SignThumbprint A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4 `
  -SignerSubject "CN=Verified publisher"

# Managed/cloud provider adapter
.\platforms\windows\build\publish-bundle.ps1 `
  -SigningAdapter .\provider-sign.ps1 `
  -SignerSubject "CN=Verified publisher" `
  -SignerPublicKeySha256 "64_HEX_DIGITS_FOR_THE_APPROVED_KEY"
```

Produces under `platforms\windows\dist\installer\`:

| Artifact | Audience |
| --- | --- |
| `FileIDSetup.exe` | **End users** — one download, auto-picks x64 vs ARM64 at install |
| `FileID-x64.msi` | IT admins (SCCM/Intune for x64 desktops/laptops) |
| `FileID-arm64.msi` | IT admins (Snapdragon WoA fleets) |

The `publish-bundle.ps1` script:
1. Cross-compiles the Rust engine for x64 + ARM64.
2. Publishes the WinUI 3 app for both architectures (self-contained .NET, ReadyToRun).
3. Stages the engine alongside the app in each publish dir.
4. Signs every unsigned executable payload while preserving valid vendor signatures (skip with `-SkipSign`).
5. Builds both per-arch MSIs via WiX v4.
6. Signs both MSIs.
7. Builds the WiX Burn bundle (`FileIDSetup.exe` with both MSIs embedded).
8. Detaches and signs the Burn engine, reattaches it, then signs the final bundle.
9. Verifies every new signature, trusted timestamp, publisher subject, and same-release signer public key.
10. **Privacy gate**: greps every shipped binary for telemetry strings. Zero hits required.

Pass `-SkipArm64` for an x64-only release. Provider onboarding and the adapter contract are documented in [`WINDOWS_SIGNING.md`](WINDOWS_SIGNING.md).

## macOS

```bash
./build.sh -mac                 # build engine + app and launch
bash platforms/apple/run.sh     # the underlying script — same result
./build.sh -mac --tests         # run swift test first
```

See [`platforms/apple/CLAUDE.md`](../../platforms/apple/CLAUDE.md) for the macOS-specific dev guide.

## Linux

The Linux front-end is a **GTK4 + libadwaita** app that shares the cross-platform Rust engine with Windows. Install the GTK toolchain, then build + run via the platform script (see [`platforms/linux/README.md`](../../platforms/linux/README.md) and [`platforms/linux/CLAUDE.md`](../../platforms/linux/CLAUDE.md)):

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev   # or your distro's equivalent
bash platforms/linux/build/build.sh                              # build the GTK4 app
./platforms/linux/dist/fileid/fileid-linux                       # run it
```

The app is feature-complete across the six tabs and compile-verified in CI (`linux.yml`); on-hardware polish is ongoing. The headless **CLI** and **TUI** build standalone and run anywhere:

```bash
cd platforms/cli && cargo build --release && ./target/release/fileid --help
cd platforms/tui && cargo run --release
```

To package the app for distribution (Flatpak / AppImage / Nix / AUR), see the **Linux distribution & packaging** section above and [`packaging/README.md`](../../packaging/README.md).

## Repository layout (detailed)

```
FileID/
├── platforms/
│   ├── apple/                  # macOS — SwiftUI / MLX / CoreML
│   ├── windows/                # Windows — WinUI 3 (.NET 8) + Rust engine
│   │   ├── src/
│   │   │   ├── FileID.App/         # WinUI 3 desktop app (C# + XAML)
│   │   │   ├── FileID.Theme/       # Reusable theme + motion primitives
│   │   │   ├── FileID.IpcSchema/   # Generated C# DTOs for the IPC contract
│   │   │   └── engine/             # Rust crate — DB + ML + scan pipeline (cross-platform)
│   │   ├── installer/
│   │   │   ├── FileID.Msi/         # Per-arch WiX v4 MSI project
│   │   │   └── FileID.Bundle/      # WiX Burn bootstrapper bundle
│   │   ├── build/
│   │   │   ├── build-all.ps1       # Dev build (engine + app + run)
│   │   │   ├── publish-bundle.ps1  # Release build (sign + MSI + bundle)
│   │   │   └── build.ps1           # Engine-only Phase 0 build
│   │   └── Tests/                  # xUnit tests for the IPC schema
│   ├── linux/                  # Linux — GTK4 + libadwaita app (shares the engine)
│   │   ├── src/                    # GTK4 app shell + six tabs
│   │   ├── data/                   # .desktop, AppStream metainfo, app icon SVG
│   │   └── build/build.sh          # Dev build (app + run)
│   ├── cli/                    # `fileid` — cross-platform CLI (links the engine in-process)
│   └── tui/                    # `fileid-tui` — ratatui terminal UI
├── packaging/                  # Linux distribution recipes
│   ├── flatpak/                    # Flatpak manifest (primary channel)
│   ├── appimage/                   # AppImage build script
│   ├── nix/                        # Nix flake
│   └── aur/                        # Arch PKGBUILD
├── shared/
│   ├── ipc-schema/             # Canonical IPC contract (JSON Schema)
│   ├── docs/                   # Architecture, decisions, models, contributing
│   ├── test-corpus/            # Cross-platform regression assertions
│   └── scripts/                # Shared helpers (model installers, etc.)
└── README.md
```

## GPU acceleration — every vendor

Out of the box, FileID picks the best path for the user's hardware:

| Hardware | EP / backend | Performance Pack? |
| --- | --- | --- |
| NVIDIA RTX | DirectML default; CUDA opt-in | NVIDIA CUDA Pack (~600 MB) |
| AMD | DirectML | — |
| Intel iGPU + Arc | DirectML default; OpenVINO opt-in | Intel OpenVINO Pack (~300 MB) |
| Snapdragon X Elite (WoA) | DirectML default; QNN NPU opt-in | Snapdragon NPU Pack (~150 MB) |
| Apple Silicon (macOS) | CoreML + ANE | — |
| CPU floor | AVX2/AVX-512 (x64) or NEON (arm64) | — |

DirectML covers every Windows GPU vendor in one shipped backend. Performance Packs (Settings → Performance) are user-initiated downloads that swap in the vendor-native EP for a perf bump on detected hardware.

## ML stack

All default weights are permissively licensed (Apache-2.0 / MIT). The Windows column is live; **Linux runs the same Rust engine and ONNX stack as Windows**, and macOS is adopting it (rows marked *lockstep pending* — see [`MODELS.md`](MODELS.md)).

| Capability | macOS | Windows |
| --- | --- | --- |
| Image tagging | RAM++ *(lockstep pending)* | **RAM++ Swin-L @384** (ONNX, Apache-2.0) — 4585-tag auto-tagger |
| Image embedding | CLIP ViT-B/32 *(lockstep pending)* | **CLIP ViT-B/32** (ONNX, MIT) — 512-d, byte-compatible |
| Text embedding | OpenAI CLIP text | OpenAI CLIP text (ONNX) + BPE tokenizer port |
| Face detect | Vision (`VNDetectFaceRectangles`) | **YuNet** (ONNX, MIT) |
| Face embed | SFace *(lockstep pending)* | **SFace** (ONNX, Apache-2.0, DirectML/CUDA EP) — 128-d |
| OCR | `VNRecognizeText` | `Windows.Media.Ocr` (built-in WinRT) |
| VLM (Deep Analyze) | MLX (Qwen 7B · Gemma) | llama.cpp + GGUF — Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 |
| PDF | PDFKit | pdfium-render |
| Video frame | AVAssetImageGenerator | Media Foundation `IMFSourceReader` |

Full mapping: [`ARCHITECTURE.md`](ARCHITECTURE.md).

## State directories

User data lives outside the install dir so an uninstall doesn't wipe it. Use Settings → Advanced → "Wipe local state" when you want a fresh start.

| Path (Windows) | Path (macOS) | Contents |
| --- | --- | --- |
| `%LOCALAPPDATA%\FileID\fileid.sqlite` | `~/Library/Application Support/FileID/fileid.sqlite` | Main library DB (WAL mode) |
| `%LOCALAPPDATA%\FileID\logs\` | `~/Library/Logs/FileID/` | Engine + app logs (local-only, daily rotation) |
| `%LOCALAPPDATA%\FileID\Models\` | `~/Library/Application Support/FileID/Models/` | ONNX/CoreML weights |
| `%LOCALAPPDATA%\FileID\Models\HuggingFace\` | same parent | VLM weights (Qwen, Gemma, MiniCPM-V) |
| `%LOCALAPPDATA%\FileID\thumbs.cache\` | same parent | Thumbnail cache |
| `%LOCALAPPDATA%\FileID\face_crops\` | same parent | Face crop JPEGs for People view |
| `%LOCALAPPDATA%\FileID\settings.json` | same parent | Per-user settings (GPU EP override, etc.) |

On **Linux** the same tree lives under `$XDG_DATA_HOME/FileID/` (default `~/.local/share/FileID/`) — the CLI, TUI, and GTK app all read/write this one library.

## Troubleshooting

### Windows — build / run errors

| Symptom | Fix |
| --- | --- |
| `pwsh: command not found` | You have Windows PowerShell 5.1, not PowerShell 7. Either drop the `pwsh` prefix (`.\platforms\windows\build\build-all.ps1 ...`) or `winget install Microsoft.PowerShell`. |
| `The '<' operator is reserved for future use` | You typed a literal `<placeholder>` from a code block. PowerShell parses `<` as redirection. Strip the angle brackets, pass the value directly. |
| `cargo: command not found` | Install Rust: https://rustup.rs |
| `dotnet SDK not found` | `winget install Microsoft.DotNet.SDK.8` |
| `Microsoft.Build.Packaging.Pri.Tasks.dll missing` | VS Build Tools UWP component missing: `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.UWP.MSBuild"` |
| ARM64 cross-compile fails: `cl.exe not found` | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.VC.Tools.ARM64"`, or pass `-SkipArm64`. |
| App launches but says **"side-by-side configuration is incorrect"** | Check `Get-WinEvent -LogName Application \| Where ProviderName -eq SideBySide` for the actual missing assembly / unsupported manifest setting. Common causes: (a) `app.manifest` declares a setting in an XML namespace the OS doesn't know (e.g. `2024/WindowsSettings` is invalid; use `2020/WindowsSettings`); (b) `Bootstrap.TryInitialize`'s major.minor in `Program.cs` doesn't match the WinAppSDK package version in `Directory.Packages.props`. |
| App launches then immediately exits with **`Microsoft.UI.Xaml.dll` faulting at `0xC000027B`** | The main app's `FileID.pri` is missing from the publish folder. `dotnet publish` strips it on .NET 8 + WinAppSDK 1.7+. The `CopyPriFilesToPublish` MSBuild target in `FileID.App.csproj` fixes this — verify with `dir "%LOCALAPPDATA%\FileID-App\FileID.pri"`. |
| App launches then exits with **`CoreMessagingXP.dll` fault** after activation | Win2D's `CanvasAnimatedControl` is incompatible with the OS build. LavaLamp uses one; if you re-enable it on Windows 11 26200+ you'll see this. Stays disabled until LavaLamp is rewritten on `Microsoft.UI.Composition`. |
| App launches but engine pill stays **"Starting…"** | `FileIDEngine.exe` isn't beside `FileID.exe`. The build script copies it automatically — verify with `dir "%LOCALAPPDATA%\FileID-App\FileIDEngine.exe"`. |
| WinAppSDK runtime missing at app launch | Self-contained publish bundles it — but for non-self-contained Debug builds, install the runtime once: `winget install Microsoft.WindowsAppRuntime.1.7` (pinned in `Directory.Packages.props`). |
| Welcome sheet shows **"Failed: Couldn't download &lt;model&gt;.onnx: HTTP 404"** | An upstream HuggingFace repo was reorganized after the URL was wired. Check `shared/docs/STATE.md` for the most recent URL-refresh entry; the canonical paths live in `platforms/windows/src/engine/src/models/registry.rs` and `shared/docs/MODELS.md`. Update + rebuild the engine, restage `FileIDEngine.exe` beside `FileID.exe`, click Retry. |

### macOS

See [`platforms/apple/CLAUDE.md`](../../platforms/apple/CLAUDE.md).

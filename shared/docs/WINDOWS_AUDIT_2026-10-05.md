# Windows codebase audit — 2026-10-05

## Status

This is an in-progress release audit, not a security sign-off. The requested Windows scope contains 399 tracked review targets across the Rust engine, WinUI app, IPC integration, packaging, scripts, and tests. The managed security scan `c55087c1-207f-4b7b-99da-f1c1de769530` is still at discovery 0/399 and has not produced a canonical report. Do not treat this document as proof that every file has been reviewed.

The audit is running on branch `codex/store-startup-and-listing` at `754f8b0b`. `origin/main` is also `754f8b0b`; there are 56 local dirty files, so these results are not covered by GitHub CI. No commit, push, Store upload, or submission was made.

## Verified checks

- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --all-targets` — passed, 427 tests.
- `dotnet format FileID.sln --verify-no-changes` — passed.
- `dotnet test FileID.sln --no-restore` — passed.
- `git diff --check` — passed.
- `cargo fmt --all -- --check` — passed.
- `cargo deny check` — exited successfully with 12 warnings (duplicate, yanked, and license-not-encountered categories); review those warnings before tightening policy.
- `dotnet list FileID.sln package --vulnerable --include-transitive` — passed with no advisory matches.
- Tracked Windows sources/build scripts were searched for outbound telemetry/analytics endpoints and SDKs; no matches.
- `publish-store.ps1` rebuilt the Store MSIX and `verify-store-package.ps1` verified its Store identity and payload as `AdamNolle.FileID 0.1.2.0`. Candidate: [`FileID-0.1.2-x64.msix`](../../platforms/windows/dist/store/FileID-0.1.2-x64.msix), SHA-256 `511E52493191CB93252850499BCA1D0458F0ECF24A57AA1BE6F5E8EF463848B3` (208,487,701 bytes). This was a static package check; it was not installed or launched.
- The App Certification Kit is installed but was not run because its runtime checks may install and launch the candidate package.

The Rust audit change adds a 64 MiB aggregate decompressed-member budget for document extraction and a compressed multi-member ZIP regression case. The complete Rust suite passes with that change.

## Findings and release gates

### Store candidate still has an unresolved startup/resource check

The rebuilt Store package PRI statically contains `ms-resource://FileID/Files/AppResources/Theme.xbf`. The approved side-by-side diagnostics package still failed resolving `ms-appx:///AppResources/Theme.xaml`, but its package identity did not match the PRI identity. The exact Store-identity 0.1.2.0 package now passes static identity/payload verification; it has not been installed or launched, so the runtime resource issue is still unresolved. The package includes six logo PNG assets; their visual rendering and Partner Center listing screenshots remain unverified. The currently installed Store package was left untouched.

### P1 — Restructure apply can move a replacement file under a stale plan

Confirmed in `platforms/windows/src/engine/src/pipeline/restructure_apply.rs` and the `RestructureMove` IPC DTO: apply re-reads the database row and checks that its path still matches `source`, then performs path-based filesystem moves. The DTO carries `file_id` and paths but no filesystem identity or content fingerprint. Replacing the on-disk file while leaving the catalog row/path unchanged can therefore make an old plan move the replacement. Add a regression test and bind the plan to the source file before release. If that identity crosses IPC, update `shared/ipc-schema/ipc.schema.json` first and keep all generated DTOs aligned.

### GPU runtime acceptance is still open

The current machine reports an NVIDIA GeForce RTX 5080 (driver 617.14, compute capability 12.0), but no FileID CUDA execution-provider or cuDNN runtime files were found in the release output. The CUDA path has not been verified on this hardware. NVIDIA/cuDNN terms have not been accepted, so do not fetch those vendor runtime files until the owner accepts the upstream terms.

### Local diagnostic trust cleanup is still open

The temporary diagnostic certificate `FFB7242B4EE19668075E1077093AF886835AC20E` remains in the local Windows certificate stores and its private key remains present. Removal was authorized but the PowerShell removal attempt was blocked by shell policy; no alternate deletion method was attempted. This is a workstation cleanup item, separate from the Store package.

### Release and user-facing checks still open

- Run GitHub engine and app workflows on the exact reviewed commit after changes are landed.
- Verify a freshly rebuilt Store-identity package launches and that its logo/theme resources load. Do not overwrite the current Store install during diagnostics.
- Remove the temporary diagnostic certificate and its private key using the supported Windows certificate-management path.
- Run the App Certification Kit and the approved on-hardware library/corpus checks after runtime-test approval.
- Verify Store listing screenshots and logo assets in Partner Center. The demo video remains excluded by the owner's instruction.
- Remove the temporary diagnostic certificate entries. Cleanup was previously approved but the supported removal attempt was rejected by shell policy; no alternate deletion was attempted.

## Next audit steps

1. Complete the managed Windows security scan and publish its canonical coverage/findings report.
2. Fix the confirmed restructure stale-plan source identity gap and add a regression test.
3. Rebuild and statically verify the Store package from the final source state.
4. Complete runtime, CUDA, Store listing, and exact-head CI gates before calling the update ready.

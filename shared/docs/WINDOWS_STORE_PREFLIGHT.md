# Windows Store preflight — October 1, 2026

Use `codex/store-msix` in `C:\Users\adamm\.codex\worktrees\store-msix\FileID`, draft PR #183. Partner Center product `9PC8HSD86887` stays draft: no upload, submission, or publication. Future features are specified in [POST_STORE_FEATURES.md](POST_STORE_FEATURES.md) for all desktop platforms and follow the current release.

## Verification

| Check | Result |
| --- | --- |
| x64 Release app build | Patched app built and published successfully during the required local Store command |
| Cleanup and exact-trash proof tests | 18 passed |
| Compilable app subset | 333 passed, 27 failed, 360 total |
| Complete app test project | Cannot compile: 44 missing API diagnostics across four files |
| IPC tests | 47 passed |
| Runtime egress gate and its tests | Gate passed; 24 tests passed |
| Changed app C# formatting | Passed |
| Hosted x64 Store package at `2fd1f21` | Passed; independently inspected locally |
| Actual Store package installation and UI smoke | Unverified |

The diagnostic subset used a temporary copy of the test project with assembly name `FileID.App.Tests`, excluding only `EngineLifecycleSafetyContractTests.cs`, `EngineHealthWaiterTests.cs`, `RestructureUndoRoutingTests.cs`, and `AdversarialLifecycleAndUiContractTests.cs`. This is not a full-suite pass. The temporary project is not committed; the real test project retains every test.

Machine-local logs are under `platforms/windows/dist/store-packages/`: `app-tests-current.log`, `focused-app-tests-build.log`, `cleanup-focused-tests.log`, `compilable-app-subset.log`, `ipc-tests-current.log`, `cleanup-format.log`, and `test-results/*.trx`.

## Full-suite compile blockers

| Test file | Diagnostics | Required behavior |
| --- | --- | --- |
| EngineLifecycleSafetyContractTests | 22 | Persisted journal discovery, bounded parsing, identity validation, fallback ordering |
| EngineHealthWaiterTests | 9 | Schema-first health IPC, generation/PID/nonce-bound waiters, retirement before publishing new generation |
| RestructureUndoRoutingTests | 12 | Integrated undo routing, cancellation counts, accurate status; schema and DTO alignment |
| AdversarialLifecycleAndUiContractTests | 1 | Final close requires terminal stop with no live process or pending start |

Implement real behavior while preserving safety assertions; no placeholder APIs solely to compile tests.

## Observed subset failures

All classes are under `FileID.App.Tests`. Review source-text contracts against current behavior before concluding each failure is a runtime bug. Exact method names and assertion messages are preserved in `test-results/compilable-app-subset.trx`.

| Class | Failed tests | Areas |
| --- | --- | --- |
| EventHandlerSafetyContractTests | 2 | Reduced-motion subscriber isolation; safe PropertyChanged handlers |
| RecentChangesUiContractTests | 3 | People/trash undo terminal confirmation; pending badge and close gate |
| DeepAnalyzeCompletionContractTests | 1 | Full completion marker |
| PreviewLifecycleContractTests | 4 | Bounded image fallback; complete thumbnail payload; transient unload; dialog lifecycle |
| SigningPolicyContractTests | 1 | Embedded publisher policy and verification |
| InstallerContractTests | 8 | MSBuild test workflow; cross-architecture upgrades; unelevated launch/support URL; runtime bootstrap; native payload requirements; version/tag guard; Burn architecture chain; branded accessible assets |
| MissingFileVisibilityContractTests | 1 | Soft-missing rows in library summary queries |
| SuggestedMergesBusyStateTests | 2 | Busy indicator and clearing every exit path |
| SettingsEngineStopSafetyTests | 1 | Clear scan presentation before publishing stopped |
| UiInteractionSafetyContractTests | 4 | Library trash reentry; keyboard tile context menu; restructure repeater DataContext; people keyboard actions |

## Package evidence

Latest downloaded upload before this cleanup patch:

`C:\Users\adamm\.codex\worktrees\store-msix\FileID\platforms\windows\dist\store-packages\ci-2fd1f21\local-20260928-133549-7400\FileID.StorePackage_1.0.0.0_x64.msixupload`

SHA-256: `5C9FE90F729611CAD500487239DDA8CB689195CF03CC9B6566468A853DC4CBFC`.

Verified x64 identity `AdamNolle.FileID`, version `1.0.0.0`, publisher `CN=B6BC6354-0217-4C63-8B82-7040B465A25E`, launch path `FileID.App\FileID.exe`, and colocated app/engine/ORT/DirectML/pdfium payload. This includes the explicit-download fix but predates the cleanup patch. Inspect an exact-head package before release work.

The required local `cd platforms\windows; .\build\publish-store-msix.ps1` was rerun after the cleanup patch. Engine Release build and x64 app Release build/publish succeeded; packaging failed in VS 2026 at `GenerateAppxPackageRecipe` (`APPX0002` / `MSB4018`, null reference). No new local `.msixupload` resulted. Log: `local-build-cleanup.log`. Hosted VS 2022 succeeds. Install remains unverified after reserved-publisher unsigned install failed (`0x80073D2C`) and loose registration required Developer Mode (`0x80073CFF`). No certificates or security policies changed.

## Resume order

1. Inspect branch status and exact-head CI.
2. Resolve 44 compile diagnostics and 27 observed failures; run all Windows tests with VS MSBuild/VSTest, formatting, Rust clippy/tests, and release policy gates.
3. Build/download and inspect final x64 upload. Perform actual package install/launch, isolated scan, model fallback, cleanup/undo smoke. WACK is optional.
4. Prepare listing assets/certification while retaining Partner Center draft. Website must describe preparation until Store availability is real.

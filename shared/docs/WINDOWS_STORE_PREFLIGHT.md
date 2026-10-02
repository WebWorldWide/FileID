## Exact-head Store artifact - `cf5a540`

- Adlon Store run `37005249596` completed successfully and produced `platforms/windows/dist/store-packages/adlon-cf5a540/local-20261002-121437-9348/FileID.StorePackage_1.0.0.0_x64.msixupload` (127,428,745 bytes; SHA-256 `AEE0A7DCAEE538581605CE3E3A5B5D8EE9D609D79017A2319815208854C87160`). Nested MSIX SHA-256: `73E23E65DB7794A736D66575C7BA3D933D68A4CCF3941C293393C57DB7A64F71`.
- Independently verified identity `AdamNolle.FileID`, publisher `CN=B6BC6354-0217-4C63-8B82-7040B465A25E`, version `1.0.0.0`, architecture x64, executable `FileID.App\\FileID.exe`, and required app/engine/ONNX Runtime/DirectML/PDFium/logo payloads. GitHub artifact only; not uploaded to Partner Center.
- Runtime-egress digest correction is pushed in `0188da1`; Adlon policy run `37006425289` passed. App run `37005249671` and engine run `37005249594` are still pending/in progress from `cf5a540`, whose app source is unchanged by the digest/docs commit.
## Current validation checkpoint — October 2, 2026

- Signed-in Microsoft account: `adammnolle@gmail.com`; two-step verification ON; Gmail and authenticator both show “Up to date.” No security methods changed.
- Pushed head `fa145357ef78af01c8af53747f8a4d23884a1599` on `codex/store-msix`; five local lifecycle changes remain uncommitted.
- Local Release app suite: 393 passed, 40 failed, 433 total. IPC schema tests, engine `cargo test`, engine clippy, and changed-file format verification passed. No bug-free or release-ready claim.
- Local Store script reached MSIX recipe generation but failed under VS 18: `APPX0002`/`MSB4018`, `GenerateAppxPackageRecipe`, null reference in `MrmSupportLibrary.GetLocation`. Supplying the installed Windows SDK `mrmsupport.dll` path did not fix it; no new local upload.
- Adlon policy run `37001649379` passed. App run `37001649639`: x64 Debug/Release build and self-contained publish passed, ARM64 app build/test passed, but x64 VS VSTest aborted with “Could not find testhost.” Workflow is locally updated to run the already-built test projects via `dotnet test --no-build --no-restore`; push and validate on Adlon.
- Adlon Store run `37001649369` passed on `fa14535`. Artifact `platforms/windows/dist/store-packages/adlon-fa14535/local-20261002-115818-4916/FileID.StorePackage_1.0.0.0_x64.msixupload` (127,421,868 bytes; SHA-256 `BF0417F55EE35C5718401146F21B53FA6387BFEB14500BE118FDB5D4C21026BE`). Nested MSIX SHA-256 `9C849F5F76EF1A5D3280A98B62ABE1F544BCABADCD935BE51B94C41B306B831A`. Verified identity `AdamNolle.FileID`, publisher `CN=B6BC6354-0217-4C63-8B82-7040B465A25E`, version `1.0.0.0`, architecture x64, executable `FileID.App\FileID.exe`, plus required app, engine, inference/runtime DLLs, PDFium, and logos. This artifact predates the five uncommitted changes and is not the final candidate.
- Partner Center product `9PC8HSD86887` remains draft. Nothing was uploaded to Partner Center, no submission was started, and nothing was published.
## Latest lifecycle checkpoint — 2026-10-02

- x64 Release app build: passed (one Win2D AnyCPU warning). Changed-file whitespace verification: passed.
- Focused `EngineLifecycleSafetyContractTests`: 15 passed, 16 failed, 31 total. This is a release blocker; see the latest `STATE.md` entry and `NEXT.md` for failure areas and resume order. No full-suite pass is claimed.
- Persisted undo parsing now lives outside `EngineClient` so the filesystem readers can be tested without initializing a WinUI dispatcher. The reader is not yet proven compatible with engine-produced persisted data and is not fully wired into recovery.
- The previously inspected `.msixupload` was built from `d5389c8`, before the current lifecycle changes; it is not suitable for submission. Build/inspect a new artifact from the final validated head.
- Keep the Partner Center product as a draft. No upload, submission, or publication is authorized.

# Windows Store preflight — October 1, 2026

Use `codex/store-msix` in `C:\Users\adamm\.codex\worktrees\store-msix\FileID`, draft PR #183. Partner Center product `9PC8HSD86887` stays draft: no upload, submission, or publication. Future features are specified in [POST_STORE_FEATURES.md](POST_STORE_FEATURES.md) for all desktop platforms and follow the current release.

## Verification

Adlon checkpoint, October 2: source `d5389c8`, app run `36996942164`, x64 job `110805708304`. Python/.NET/MSBuild/toolchain provisioning, restore, Debug/Release builds and self-contained publish passed. The complete app test project reproduced 23 diagnostics: 22 in `EngineLifecycleSafetyContractTests.cs`, one in `AdversarialLifecycleAndUiContractTests.cs`. Log: `platforms/windows/dist/store-packages/adlon-d5389c8-app-x64.log`. Store run `36996942177` / job `110805708446` completed successfully, including the required script and GitHub artifact upload.

Adlon Store job `110805708446` completed the required script and created GitHub artifact `FileID-Store-x64` (artifact ID `11222752291`) from source `d5389c8`. Downloaded and inspected upload:

- Path: `platforms/windows/dist/store-packages/adlon-d5389c8/local-20261002-104716-9468/FileID.StorePackage_1.0.0.0_x64.msixupload`
- Upload SHA-256: `90B80EDA1823E0164609F90F35F5707E549272A7221255B308961484458DB2D1`
- Nested MSIX SHA-256: `5952945595BF8B87F498E36A3C318FCECA652651A9B5D0C5ABB1DE1F42FFDFCE`
- Manifest identity: `AdamNolle.FileID`, publisher `CN=B6BC6354-0217-4C63-8B82-7040B465A25E`, version `1.0.0.0`, architecture `x64`, executable `FileID.App/FileID.exe`.
- Payload checks confirmed `FileID.exe`, `FileIDEngine.exe`, `onnxruntime.dll`, `onnxruntime_providers_shared.dll`, `DirectML.dll`, `pdfium.dll`, `Images/Square44x44Logo.png`, and `Images/Square150x150Logo.png`.

This is GitHub artifact validation only. It has not been uploaded to Partner Center. The app test compilation failure and 27 previously observed failing contracts still block readiness.

| Check | Result |
| --- | --- |
| x64 Release app build | Passed after health channel changes |
| Cleanup and exact-trash proof tests | 18 passed |
| Compilable app subset | 360 passed, 27 failed, 387 total; same 27 failure names |
| Complete app test project | Cannot compile: 23 missing API diagnostics across two files |
| Restructure routing tests | All 17 passed as part of the subset |
| Rust tests/clippy | Full tests and clippy with warnings denied passed; new cancelled/failed undo retry regression passed |
| IPC tests | 54 passed, including health round trips/missing-field rejection, cancellation counts and legacy defaults |
| Runtime egress gate and its tests | Gate passed; 24 tests passed |
| Changed app C# formatting | Passed |
| Hosted x64 Store package at `2fd1f21` | Passed; independently inspected locally |
| Actual Store package installation and UI smoke | Unverified |

The current diagnostic subset used a temporary copy of the test project with assembly name `FileID.App.Tests`, excluding only `EngineLifecycleSafetyContractTests.cs` and `AdversarialLifecycleAndUiContractTests.cs`. This is not a full-suite pass. The temporary project is not committed; the real test project retains every test. All 10 health tests and all 17 restructure routing tests now compile and pass. Failure names were compared with the previous restructure subset and are unchanged.

Machine-local logs are under `platforms/windows/dist/store-packages/`: `app-tests-current.log`, `focused-app-tests-build.log`, `cleanup-focused-tests.log`, `compilable-app-subset.log`, `ipc-tests-current.log`, `cleanup-format.log`, and `test-results/*.trx`.

Newest evidence: `app-tests-restructure-current.log` (32 diagnostics), `restructure-app-subset.log` / `test-results/restructure-app-subset.trx` (350/27), `restructure-routing-tests.log`, `restructure-full-engine-tests.log`, `restructure-clippy.log`, `restructure-ipc-tests.log` (49 tests; explicitly run `Tests/FileID.IpcSchema.Tests/bin/x64/Debug/net8.0/FileID.IpcSchema.Tests.dll`), `restructure-format.log`, `restructure-app-release.log`, and `restructure-egress-tests.log`. The 27 failing app test names are unchanged from the earlier run.

## Restructure safety changes verified

Windows Rust/C# now mirror the cancellation, planned, remaining, and optional shortcut-token fields already present in the canonical schema and Swift DTO. The engine counts only unprocessed rows as cancellation remainder, preserves failed/cancelled undo journals, skips previously restored rows on retry, and opens a new forward journal only after a successful move. A Windows filesystem regression exercises cancellation, a partial failure, and successful retry without duplicate restores. A second regression verifies that an already-placed file is not counted as a move or given a new undo journal.

The app separates apply/undo and real-move/shortcut completion text, serializes restructure commands until their terminal result or error, registers a generation-bound undo waiter before sending, and records real-move history only after engine confirmation. Undo history stays retryable when confirmation fails or cancellation leaves remaining work. Health waiters are implemented and verified. Persistent journal discovery, final close, and the 27 other contract failures remain incomplete.

## Health channel verification

Startup stays Starting until both raw Ready and a generation/PID/nonce-bound health reply arrive from the captured process. Replies resolve on the stdout reader before UI dispatch; cleanup retires waiters before incrementing generation. Unexpected stdout EOF/read failure or failed startup probe triggers bounded recovery of that captured process. Expected shutdown is exempt; no periodic idle heartbeat is added.

All 10 app health tests passed. A live engine probe using isolated LOCALAPPDATA/database/model paths passed Ready PID validation, three distinct echoed nonce/PID replies, malformed-command rejection, a subsequent valid probe, and clean shutdown. No downloads were requested. Full Rust tests and clippy passed, with a pre-existing renamed-lint notice. Changed C# whitespace/style verification passed; runtime egress gate and all 24 gate tests passed.

Local evidence under `platforms/windows/dist/store-packages/`: `health-app-focused-build-final.log`, `health-focused-final-tests.log`, `test-results/health-focused-final.trx`, `health-full-app-build.log` (23 diagnostics), `health-full-engine-tests.log`, `health-clippy-final.log`, `health-ipc-tests.log`, `test-results/health-ipc.trx`, `health-ipc-smoke.log`, `health-format-verify.log`, `health-style-verify.log`, `health-egress-tests.log`.

Hosted VS 2022 Store packaging passed at `9e93344` (run `36860708963`), before health changes; Windows app CI failed at that same commit. The inspected local upload listed below predates cleanup, restructure, and health changes. Download and inspect the final commit's package before release work.

## Full-suite compile blockers

| Test file | Diagnostics | Required behavior |
| --- | --- | --- |
| EngineLifecycleSafetyContractTests | 22 | Persisted journal discovery, bounded parsing, identity validation, fallback ordering |
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

The required local `cd platforms\windows; .\build\publish-store-msix.ps1` was rerun after the health patch. Engine Release build and x64 app Release build/publish succeeded; packaging failed in VS 2026 at `GenerateAppxPackageRecipe` (`APPX0002` / `MSB4018`, null reference). No new local `.msixupload` resulted. Newest log: `local-build-health.log`; earlier cleanup rerun: `local-build-cleanup.log`. Hosted VS 2022 succeeds. Install remains unverified after reserved-publisher unsigned install failed (`0x80073D2C`) and loose registration required Developer Mode (`0x80073CFF`). No certificates or security policies changed.

## Resume order

1. Inspect branch status and exact-head CI.
2. Resolve 23 compile diagnostics and 27 observed failures; run all Windows tests with VS MSBuild/VSTest, formatting, Rust clippy/tests, and release policy gates.
3. Build/download and inspect final x64 upload. Perform actual package install/launch, isolated scan, model fallback, cleanup/undo smoke. WACK is optional.
4. Prepare listing assets/certification while retaining Partner Center draft. Website must describe preparation until Store availability is real.

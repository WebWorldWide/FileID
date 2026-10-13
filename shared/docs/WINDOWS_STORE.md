# Microsoft Store submission

## Current owner-reported status — 2026-10-05

The owner reports that FileID was approved by Microsoft Store. This PC has the Store-installed `AdamNolle.FileID` package version `0.1.1.0`; next update target `0.1.2.0`. Partner Center was not rechecked in this turn, so the exact approved submission/package state remains owner-reported. GitHub `main` and this checkout are at `754f8b0b69b0e04ac6764cff01fa34d136ebb8ad`; PRs #219 and #220 are merged, and all 18 workflow checks for that commit completed successfully. GitHub's combined status endpoint still returns `pending` without any status contexts, so treat only the individual workflow checks as confirmed.

The approved download is not yet a release-ready experience. Windows Application Error records from 2026-10-04 show, and a new launch on 2026-10-05 12:00:52 UTC reproduced, Store package `0.1.1.0` failing in `Microsoft.UI.Xaml.dll` with `0xC000027B`; the app log records `XamlParseException` in `MainWindow.InitializeComponent()`. An unpackaged Release build reached `OnLaunched complete` and emitted engine-ready/UI binding events on this PC, but that does not clear the Store-package launch gate.

Remaining gates: diagnose and verify a fixed packaged build; build a higher package version and verify the exact MSIX; run App Certification Kit and inspect its report; capture several authentic app screenshots (the current listing draft has only one genuine Deep Analyze screen); validate CUDA scanning and CUDA Deep Analyze on the RTX 5080; and rerun strict release/privacy/runtime-egress checks on the final release commit. Screenshot set after launch is repaired: Library, People, Cleanup, Deep Analyze, Restructure, and Settings, each captured from FileID with a representative copied test corpus where relevant. The old `G:\TrueNAS` corpus is not mounted; use a copied representative corpus. Do not use the promo-video draft or upload/submit an update until these gates pass.

## Historical submission snapshot — 2026-10-03

PR #213 is merged into `main` at `a11cbd09`; its exact-head checks passed, and the merged source tree matches the Store-package build source. Main-branch CI for `a11cbd09` is running. The validated package currently in Partner Center is the older `0.1.0.0` package. A repeated upload of the same package identity was rejected and removed; the saved package remains in the draft.

Partner Center product `9PC8HSD86887`, submission `1152921505702035520`: worldwide availability is saved at the free base price; Properties, Age ratings, Packages, the English (United States) listing, and Submission Options are complete. The age rating is IARC 12+ / ESRB Teen. The listing has six feature bullets, the description and short description, and one genuine Deep Analyze screenshot. Automatic publishing after certification is selected. The runFullTrust use and basic verification guidance are in Additional Testing Information.

The previous `0.1.1.0` package replaced `0.1.0.0`. For the next update, `platforms/windows/VERSION`, the Rust engine version, Cargo lock root entry, and Store manifest are aligned at `0.1.2` / `0.1.2.0`, higher than the currently installed Store package. A baseline `0.1.2.0` MSIX has been built locally and passed static identity/payload verification; it still includes the unresolved Store startup failure, so no candidate has passed launch or WACK. Nothing has been uploaded, and certification has not been requested.

The local App Certification Kit attempt produced no report and did not install the package; WACK remains unverified. The former `G:\TrueNAS` corpus is not mounted on the current RTX 5080 system; hardware acceptance requires an available copied corpus, and vendor/model acceptance remains open. After fixing and testing Store-package startup, build and verify the `0.1.2.0` MSIX, run App Certification Kit and inspect its report, complete hardware and screenshot gates, then upload and submit that exact package in Partner Center and monitor Microsoft's result.
## Reserved product

Verified in Partner Center on 2026-10-02. FileID already exists as an **MSIX or PWA app**, with status **Not started**. Use this product rather than reserving a second name or creating an MSI/EXE listing.

| Field | Value |
|---|---|
| Store ID | `9PC8HSD86887` |
| Package name | `AdamNolle.FileID` |
| Publisher | `CN=B6BC6354-0217-4C63-8B82-7040B465A25E` |
| Publisher display name | `Adam Nolle` |
| Package family | `AdamNolle.FileID_kp7xpwd7dcz0a` |
| Partner Center | https://partner.microsoft.com/en-us/dashboard/products/9PC8HSD86887/overview |

## Build and upload artifact

From `platforms/windows`, with pinned Rust, .NET 8, Visual Studio WinUI build tools and Windows SDK available:

```powershell
pwsh -NoProfile -File build/publish-store.ps1
pwsh -NoProfile -File build/verify-store-package.ps1 -Path dist/store/FileID-0.1.0-x64.msix
```

The script builds the native engine, publishes the existing WinUI app with `FileIDStoreBuild=true`, stages ONNX Runtime/DirectML/PDFium, and stages SHA-256-pinned Vulkan llama.cpp, whisper.cpp, and OpenVINO provider bundles with license notices. Optional CUDA runtimes are installed locally by the user; Settings shows the expected directories and verifies installed files. FileID does not download vendor runtime archives. The script checks binary privacy and runs MakeAppx validation. Bundled runtime assets are resolved from the installed package; idle app startup performs no downloads. Version comes from `platforms/windows/VERSION`; the fourth MSIX version component is zero. Initial Store packaging is **x64 only**; native ARM64 remains a separate runtime/hardware verification gate.

The existing Windows App Runtime 1.7 and VC++ Desktop frameworks are declared Store-managed dependencies. .NET is self-contained. Store builds skip the unpackaged bootstrapper; MSI/dev builds retain it. The engine remains a bundled child process using the existing IPC contract. Store builds do not silently fetch runtime binaries. No new library dependencies or web UI are introduced.

`dist/store/FileID-<version>-x64.msix` and its checksum are upload artifacts. Partner Center accepts unsigned MSIX uploads; Microsoft signs the distributed package. Local sideloading requires a trusted matching publisher certificate and the declared frameworks. Do not create/install trust certificates as part of normal build verification. The **Windows Store package** GitHub workflow builds artifacts without publishing or submitting them.

## Release blockers

Packaging success is not release acceptance. Before submission:

- Pass Rust Clippy/tests, **both** C# test projects with nonempty passing reports, solution formatting, IPC parity, binary privacy, and hosted CI on the exact release commit. `dotnet test FileID.sln` currently discovers no tests; run the projects explicitly.
- The strict runtime-egress gate must pass without `--known-blockers`. Runtime archives are provisioned through the Store package or installed locally by the user. Model downloads stay on Hugging Face; idle-startup downloads and other network calls are prohibited.
- Verify installed MSIX launch from Start, engine startup, model install/cancel/retry, all six tabs, folder access, scanning, duplicate safety, restructure preview/Undo, restart, upgrade and uninstall on a fresh Windows profile. Use copies for mutation checks. Verify WinUI/GPU behavior on hardware.
- Run Windows App Certification Kit and inspect its report. MakeAppx validation does not substitute for certification or installed-app testing.
- Review and include approved native dependency redistribution notices and optional model terms.
- Partner Center properties/category and privacy/support URLs are complete. The IARC answers are filled in the open form; previewing/submitting them is the next step and requires action-time confirmation for IARC terms acceptance. Set the reversible pricing draft to free worldwide unless the publisher supplies a different choice. Finish the English listing, screenshots, capability justification and certification notes.
- Upload the exact validated package and verify Partner Center accepts identity/dependencies. Submit after the remaining gates pass.

## Listing draft

**Name:** FileID

**Short description:** Organize, search, and clean up your files with AI that runs on your PC.

**Description:** FileID helps you bring order to large file collections. Build a searchable library, find people in photos, review duplicates, analyze files in more detail, and preview folder and filename changes before applying them. AI processing runs locally on your PC. Your files stay on your device, and FileID does not collect telemetry. Model downloads require an internet connection; once installed, models run offline. Optional models may require additional disk space and acceptance of their upstream terms.

**Features:** Local file library and search; people and photo organization; duplicate review and cleanup; optional deeper AI analysis; previewable folder restructuring and renaming; CPU and supported GPU acceleration.

**Capability justification (`runFullTrust`):** FileID is a native WinUI desktop file organizer. Its bundled Rust engine runs as a local child process over stdio and accesses user-selected folders to index files and perform explicitly requested, previewable file operations. The engine provides SQLite persistence, native shell operations and on-device inference. The application does not send user files to a server or collect telemetry.

**Certification notes:** Launch from Start and add a small test folder. Install models explicitly when testing AI features; downloads can be cancelled and retried. Deep Analyze is optional and needs a larger model. Use disposable copies for cleanup/restructure tests. The Windows App Runtime and VC++ Desktop frameworks are package dependencies; a separate .NET installation is not required.

These are drafts, not saved Partner Center fields. Verify current behavior and the final release version before copying them. Capture real application screenshots; launch-smoke diagnostics are not an approved Store gallery.

## Official references

- [MakeAppx package creation](https://learn.microsoft.com/en-us/windows/msix/package/create-app-package-with-makeappx-tool)
- [MSIX Store requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements)
- [MSI/EXE Store requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements)

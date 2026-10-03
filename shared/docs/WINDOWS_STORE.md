# Microsoft Store submission

## Current submission status — 2026-10-03

Partner Center already has the FileID product (Store ID `9PC8HSD86887`, package identity `AdamNolle.FileID`). The current draft has Properties, Packages, and Pricing marked Complete; pricing is saved as free, worldwide, publicly discoverable, with release ASAP. The English listing text and publisher name are saved, but Listing is still incomplete because it requires at least one desktop screenshot (Microsoft recommends four). The existing Partner Center MSIX is still the earlier upload with SHA-256 `6bde5a99232f216ccc4b721711fdee3d427420e16aeac8acacf67476d896e11c`. A rebuilt package from branch head `9900e2ab` passed local Store package verification; SHA-256 `6f9e52ab2377d9756629525a539389a5850053e2ddb58241b3f4a405d8f67673`. It has not yet been uploaded.

The saved age-rating results are IARC 12+ / ESRB Teen. The publisher explicitly confirmed legal age and authorized accepting IARC’s terms; the declaration was accepted and the rating saved. Partner Center currently displays the Current Rating ID as Pending. Certification notes explain the `runFullTrust` capability for the bundled local engine. Remaining release gates are screenshots, Windows App Certification Kit, fresh-profile install/launch/upgrade validation, exact-head hosted CI, and physical GPU/model validation. The Store product has not been submitted or published.

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

The script builds the native engine, publishes the existing WinUI app with `FileIDStoreBuild=true`, stages ONNX Runtime/DirectML/PDFium, and stages SHA-256-pinned Vulkan llama.cpp, whisper.cpp, and OpenVINO provider bundles with license notices. It checks binary privacy and runs MakeAppx validation. Bundled runtime assets are resolved from the installed package; app runtime downloads remain explicit user actions for Hugging Face model artifacts only. Version comes from `platforms/windows/VERSION`; the fourth MSIX version component is zero. Initial Store packaging is **x64 only**; native ARM64 remains a separate runtime/hardware verification gate.

The existing Windows App Runtime 1.7 and VC++ Desktop frameworks are declared Store-managed dependencies. .NET is self-contained. Store builds skip the unpackaged bootstrapper; MSI/dev builds retain it. The engine remains a bundled child process using the existing IPC contract. Store builds do not silently fetch runtime binaries. No new library dependencies or web UI are introduced.

`dist/store/FileID-<version>-x64.msix` and its checksum are upload artifacts. Partner Center accepts unsigned MSIX uploads; Microsoft signs the distributed package. Local sideloading requires a trusted matching publisher certificate and the declared frameworks. Do not create/install trust certificates as part of normal build verification. The **Windows Store package** GitHub workflow builds artifacts without publishing or submitting them.

## Release blockers

Packaging success is not release acceptance. Before submission:

- Pass Rust Clippy/tests, **both** C# test projects with nonempty passing reports, solution formatting, IPC parity, binary privacy, and hosted CI on the exact release commit. `dotnet test FileID.sln` currently discovers no tests; run the projects explicitly.
- The strict runtime-egress gate now passes without `--known-blockers`. Runtime archives are provisioned through the Store package; normal app startup does not fetch them. Keep user-initiated model downloads Hugging Face-only.
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

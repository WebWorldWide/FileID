# Microsoft Store submission

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

The script builds the native engine, publishes the existing WinUI app with `FileIDStoreBuild=true`, stages ONNX Runtime/DirectML/PDFium, checks binary privacy, and runs MakeAppx validation. Version comes from `platforms/windows/VERSION`; the fourth MSIX version component is zero. Initial Store packaging is **x64 only**; native ARM64 remains a separate runtime/hardware verification gate.

The existing Windows App Runtime 1.7 and VC++ Desktop frameworks are declared Store-managed dependencies. .NET is self-contained. Store builds skip the unpackaged bootstrapper; MSI/dev builds retain it. The engine remains a bundled child process using the existing IPC contract. No new library dependencies or web UI are introduced.

`dist/store/FileID-<version>-x64.msix` and its checksum are upload artifacts. Partner Center accepts unsigned MSIX uploads; Microsoft signs the distributed package. Local sideloading requires a trusted matching publisher certificate and the declared frameworks. Do not create/install trust certificates as part of normal build verification. The **Windows Store package** GitHub workflow builds artifacts without publishing or submitting them.

## Release blockers

Packaging success is not release acceptance. Before submission:

- Pass Rust Clippy/tests, **both** C# test projects with nonempty passing reports, solution formatting, IPC parity, binary privacy, and hosted CI on the exact release commit. `dotnet test FileID.sln` currently discovers no tests; run the projects explicitly.
- Resolve the existing strict runtime-egress blocker. Optional runtime artifacts include GitHub/NVIDIA URLs. `python shared/scripts/check_runtime_egress.py --known-blockers` is a regression baseline only; the command **without** that flag must pass before release. Preserve the Hugging Face-only runtime policy and provision other runtimes through vetted packaging.
- Verify installed MSIX launch from Start, engine startup, model install/cancel/retry, all six tabs, folder access, scanning, duplicate safety, restructure preview/Undo, restart, upgrade and uninstall on a fresh Windows profile. Use copies for mutation checks. Verify WinUI/GPU behavior on hardware.
- Run Windows App Certification Kit and inspect its report. MakeAppx validation does not substitute for certification or installed-app testing.
- Review and include approved native dependency redistribution notices and optional model terms.
- Complete Partner Center pricing/markets, properties/category, age rating, privacy/support URLs, English listing, screenshots, capability justification and certification notes. The owner chooses pricing and markets; Apache-2.0 licensing does not imply a Store price.
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

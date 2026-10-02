# FileID CI/CD on Adlon

GitHub Actions schedules the pipeline; Adlon executes the Windows/x64 and Linux/x64 jobs. Artifacts and job results remain attached to the GitHub commit. Store packaging creates a GitHub artifact only: it does not upload to Partner Center, submit, or publish FileID.

## Runners

| Runner | Labels | Work |
| --- | --- | --- |
| `adlon-fileid-windows` | `self-hosted`, `Windows`, `X64`, `fileid-adlon` | Windows app builds, x64 engine tests, ARM64 cross builds, Store MSIX, Windows release builds |
| `adlon-fileid-linux` | `self-hosted`, `Linux`, `X64`, `fileid-adlon` | Linux engine/CLI/TUI/GTK, repository policy, Flatpak, website and release deployment jobs |

Both runners are registered to `WebWorldWide/FileID`. On October 2, 2026 both were online. The Adlon Windows VM (`windows11-pro`) has Visual Studio 2022 Build Tools 17.14, packaging/PriGen tasks, VSTest, and Windows SDK 10.0.26100 x64 MakeAppx. `platforms/windows/build/verify-ci-toolchain.ps1` checks the required tooling before app/Store builds. Windows app and Store workflows select VS 2022 explicitly to avoid the local VS 2026 packaging failure.

Native macOS and native ARM64 jobs retain their platform-specific hosted runners. A Windows/x64 cross build is not a native ARM64 runtime test. Fork pull requests use hosted runners; their code is not executed on Adlon. Same-repository pull requests and trusted push/manual events select Adlon.

## Triggers and validation

Windows app, engine, Store, Linux, tools, Flatpak and policy workflows accept `main` and `codex/store-msix` pushes, existing path filters, pull requests and manual dispatch. Windows app triggers include its tests and the canonical IPC schema. Website deployment retains the `main` trigger. Release publishing retains its existing tag/dry-run controls.

Actions remain pinned to immutable commits. Python 3.12 is provisioned explicitly because self-hosted runners cannot rely on the hosted image's preinstalled Python. Workflow permissions retain read-only defaults; write permissions are isolated to the existing website/release publishing jobs. Superseded Windows validation runs are cancelled per workflow/ref.

Run the Store preparation pipeline from the authorized branch:

```powershell
gh workflow run windows-store-package.yml --ref codex/store-msix
gh workflow run windows-app.yml --ref codex/store-msix
gh workflow run policy.yml --ref codex/store-msix
gh run list --branch codex/store-msix
```

Verify the job's actual runner name and head SHA, not just the runner label in YAML. Download `FileID-Store-x64` only from the final code commit, inspect its manifest/payload and retain its SHA-256. A successful package build does not replace the full app/engine tests or the installed package runtime checks in [WINDOWS_STORE_PREFLIGHT.md](WINDOWS_STORE_PREFLIGHT.md).

## Operations

The existing Adlon SSH alias reaches the VM host. Runner services execute inside the Windows and Linux CI VMs; they must be online before dispatching. Inspect GitHub's repository runner status and the exact queued job before restarting anything. Do not restart a VM or service while it has a live job. Runner credentials, signing material and account passwords are not stored in this repository or logs.

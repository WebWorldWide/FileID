# FileID CI runners

## 2026-10-01 setup

The owner's Adlon server hosts two existing, separate CI VMs. FileID has its own repository-scoped runner in each VM; existing runners for other repositories were left running.

| Runner | VM | Runner files and checkouts | Service account |
|---|---|---|---|
| `adlon-fileid-linux` | `adlon-ci-linux`, Ubuntu 24.04, x64 | `/home/actions/personal-ci-runners/FileID`, `_work` underneath | `actions` |
| `adlon-fileid-windows` | `ADLON-CI-WIN`, Windows x64 | `C:\personal-ci\runners\FileID`, `_work` underneath | `NT AUTHORITY\NETWORK SERVICE` |

Both runners use the custom `fileid-adlon` label plus GitHub's OS/architecture labels. Registration tokens were obtained with the repository API and were not saved in the repository or displayed. Runner 2.337.0 was installed from official GitHub release assets with the release API's SHA-256 verified before extraction:

- Linux x64: `70920811a4f8ad4328818682bca5c6469c1c942fab52448868071d0063816613`.
- Windows x64: `1150692afa94e71f872017e254ea55b6eece1eece3fe7e3a6d4c93d0a1b85cfc`.

Automatic runner updates remain enabled. These are CI dependencies, not FileID runtime downloads. Services start with their VMs. The VMs already existed. The Linux guest disk was subsequently expanded as documented below; no corpus disk was attached.

## Routing and trust boundary

The four jobs in `linux.yml` use Adlon for `main` pushes and manual runs of `main`. `windows-app.yml` uses the Windows VM for both x64 builds and ARM64 cross-builds. `windows-engine.yml` uses it for x64 and ARM64 cross-builds. Native Windows ARM64 execution remains on GitHub's ARM runner. macOS remains on GitHub's Apple hardware.

Pull requests, including forks, continue to use GitHub-hosted runners. Manual runs of another branch also remain hosted. Persistent runners execute only the exact reviewed `main` reference, with pull-request events explicitly excluded. `check_self_hosted_runner_policy.py` and its mutation tests enforce this routing in repository-policy CI. Do not introduce `pull_request_target`, remove the main-reference guard, or route unreviewed source onto the VMs.

Packaging, native-tools matrix jobs, and tag-triggered release publication retain their existing hosted runners. The Adlon setup does not certify signing, physical ARM/GPU devices, or the complete next-version release. Do not publish a release merely to test the runner setup.

## Adlon data remains read-only

The host's data volume is `/srv/data`; it is not mounted or shared into either CI VM. Linux runner data is on the guest's ext4 `/dev/vda1` root disk. Windows runner data is on the guest's `C:` disk. No Adlon example files were used, changed, indexed, or copied by this setup. Do not attach the host data volume, add network shares, or put runner files, model caches, logs, generated fixtures, or exports there.

At setup the Linux guest had about 30 GB free on its 77 GB root disk; Windows `C:` had about 138 GB free. The Linux VM shares four CPUs and 8 GB RAM with other repository runners. Keep one FileID runner per VM and avoid parallel FileID workers until capacity is measured. CI is not a reason to stop another repository's jobs. Monitor guest disk space before restoring large caches or adding packaging SDKs.

## Administration and verification

The existing SSH aliases on the owner's Mac are `adlon` for the host and `windows-vm` for Windows through Adlon. Linux guest access is:

```sh
ssh -J adlon actions@192.168.122.65
gh api repos/WebWorldWide/FileID/actions/runners
gh run list --branch main --limit 25
gh run view RUN_ID --log-failed
```

The owner's existing Ed25519 public key was added to the CI guest's `actions` authorized keys for administration. No private key was copied. The guest address is libvirt DHCP; use `ssh adlon 'virsh domifaddr adlon-ci-linux'` if it changes.

Linux service: `actions.runner.WebWorldWide-FileID.adlon-fileid-linux.service`. Admin commands run inside the guest:

```sh
sudo systemctl status actions.runner.WebWorldWide-FileID.adlon-fileid-linux.service
sudo journalctl -u actions.runner.WebWorldWide-FileID.adlon-fileid-linux.service --since today
```

Windows service: `actions.runner.WebWorldWide-FileID.adlon-fileid-windows`. Use PowerShell inside the VM to inspect `Get-Service` and the runner's `_diag` logs. The Windows SSH default shell is `cmd`; use PowerShell `-EncodedCommand` for multiline scripts, never interpolate credentials into shell text.

Before claiming a gate passed, inspect the exact commit's GitHub run and its runner name. An online registration is not proof a build passed. Do not weaken checks to make a self-hosted environment pass; install its required build tools or retain the appropriate hosted hardware gate.

## Service-account prerequisites

Adlon's Network Service account needs per-job Git Bash and Python directories in GITHUB_PATH. The Windows workflows resolve the installed Python through its launcher and explicitly add Git's bin directory before toolchain setup. Machine PATH updates alone do not prove an already running service inherited them. .NET installs into runner.tool_cache/dotnet, a FileID-owned writable cache, rather than changing permissions on Program Files. DOTNET_CLI_TELEMETRY_OPTOUT is set. The existing verified .NET 8.0.425 SDK was copied into that cache; setup-dotnet can maintain it under the service account.

The initial main Windows runs failed before compiling FileID: Install Rust could not find Bash; Setup .NET 8 could not write to Program Files. Linux's four main jobs passed on adlon-fileid-linux. Host and guest root disks remain separate from the example-data volume. A host restart interrupted the administrative SSH session; runner services recovered and SDK, ARM64 compiler and WinUI packaging-task availability were rechecked. These failures require fresh Windows runs after the bootstrap fix; do not count the initial failed jobs as acceptance.

### ARM64 cross-build compiler

The actual main x64 engine job passed on adlon-fileid-windows at bb33211. Its ARM64 cross-build failed in ring's native build because clang was absent; the installed MSVC ARM64 compiler alone is insufficient. Install Microsoft.VisualStudio.Component.VC.Llvm.Clang into the existing Build Tools instance using Microsoft's installer, without forcing closed processes or rebooting the host. The engine workflow locates that component with vswhere, verifies clang starts and adds its directory through GITHUB_PATH only for the Adlon cross job. Hosted/native ARM checks and all Clippy/build assertions remain unchanged. Confirm a fresh actual service-account result after installation; the failed cross job is not acceptance.

The component IDs and paths are documented by [Microsoft's Build Tools component directory](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools?view=visualstudio) and [Clang support guide](https://learn.microsoft.com/en-us/cpp/build/clang-support-msbuild?view=msvc-170). This is a CI build-tool prerequisite, not a new shipped app dependency. Corpus storage remains unmounted in the runner guests.

### GitHub expression scope

`runner.tool_cache` is available in a step environment, not a job environment. Keep `DOTNET_INSTALL_DIR` on the setup-dotnet step; GitHub rejects the workflow before scheduling jobs if that expression moves to `jobs.build.env`. The bootstrap PR is #188; validate its exact main commit on both Adlon Windows matrices after merge.

## 2026-10-02 disk capacity and actual results

The Linux guest filled its 80 GB virtual disk during CI. Setup failed with `No space left on device` while writing the FileID worker log. Its existing `/var/lib/libvirt/images/adlon-ci-linux.qcow2` resides on the host internal root disk, which had about 651 GB free; the corpus is a different disk. Expanded the guest live to 128 GB with `virsh blockresize adlon-ci-linux vda 128G`, then ran `growpart /dev/vda 1` and `resize2fs /dev/vda1` inside the guest. The resulting ext4 root has about 123 GB usable, with 47 GB free immediately after expansion. No reboot, other repository process termination, or corpus access was needed. Retry the failed main jobs and verify every result; expansion alone does not prove CI passed.

Main `bb33211` Windows x64 engine and both x64/ARM64 app packaging jobs passed on `adlon-fileid-windows` (runs 37016957661 and 37016958500). The ARM64 engine cross-build failed because Clang was absent. The official `Microsoft.VisualStudio.Component.VC.Llvm.Clang` component subsequently installed with exit 0; `vswhere` reports the installation complete, and Clang 19.1.5 starts from `VC\Tools\Llvm\x64\bin`. The workflow must supply that directory to the service account. Native ARM execution remains hosted.

Use `setup.exe modify` with `--quiet --norestart --noUpdateInstaller` for this existing Visual Studio instance. `--wait` is a bootstrapper-only option, not supported by setup.exe. Do not use `--force`, reboot, or close other builds. In PowerShell, `Start-Process -Wait -PassThru` can collect the actual exit code. Check the component and executable afterward.

The current main Windows app workflow skips its test suites because of an incorrect doubled relative Tests path. Its passing packaging jobs therefore are not full app-test acceptance. Archived Windows repair PR #191 restores strict TRX execution, but exposes unresolved production/test API drift. Resume the repair from `archive/2026-10-02/windows-test-parity-wip`, without weakening test, analyzer, or format gates.

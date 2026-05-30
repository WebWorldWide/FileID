<#
.SYNOPSIS
  FileID dev bootstrap — Windows.

.DESCRIPTION
  Installs the SCRIPTABLE toolchain for the FileID repo and builds the isolated
  RAM++ export venv, then prints the GUI-gated steps it can't reliably automate
  (Visual Studio + Windows App SDK). Idempotent — skips anything already present.

  Installs: Rust (rustup), Python 3.11, .NET 8 SDK, Git, WiX (dotnet tool), and
  the .venv-ramplus/ export environment (pinned, from requirements-ramplus.txt).

.PARAMETER IncludeVisualStudio
  Also install Visual Studio 2022 Community + the WinUI/.NET-desktop workloads
  via winget (~several GB). Off by default — the command is printed instead so a
  large IDE install is never a surprise.

.PARAMETER SkipExportVenv
  Skip creating the RAM++ export venv (toolchain only).

.EXAMPLE
  pwsh shared\scripts\setup-dev.ps1
#>
[CmdletBinding()]
param(
  [switch]$IncludeVisualStudio,
  [switch]$SkipExportVenv
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Scripts = $PSScriptRoot

function Have($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }
function Info($m) { Write-Host "[setup] $m" -ForegroundColor Cyan }
function Ok($m) { Write-Host "[ ok ] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[warn] $m" -ForegroundColor Yellow }

if (-not (Have winget)) {
  Warn "winget not found. Install 'App Installer' from the Microsoft Store, then re-run."
  Warn "Everything below needs winget to install the toolchain."
  exit 1
}

function Winget-Ensure($id, $probe) {
  if ($probe -and (Have $probe)) { Ok "$id already present ($probe)"; return }
  Info "installing $id ..."
  winget install -e --id $id --accept-source-agreements --accept-package-agreements --silent
  Ok "$id"
}

Info "FileID dev bootstrap (Windows). Repo: $RepoRoot"

# --- Scriptable toolchain ---------------------------------------------------
Winget-Ensure "Git.Git" "git"
Winget-Ensure "Rustlang.Rustup" "rustc"          # Rust engine (cargo)
Winget-Ensure "Python.Python.3.11" "py"          # RAM++ export tooling
Winget-Ensure "Microsoft.DotNet.SDK.8" "dotnet"  # WinUI 3 / C# app

# WiX (MSI installer) — a dotnet global tool, not a winget package.
if (Have dotnet) {
  if (-not (Have wix)) { Info "installing WiX (dotnet tool) ..."; dotnet tool install --global wix 2>$null; Ok "WiX" }
  else { Ok "WiX already present" }
}

# rustup just installed but not on PATH for this session — locate cargo.
if (-not (Have cargo)) {
  $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
  if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
}

# --- RAM++ export venv (pinned) --------------------------------------------
if (-not $SkipExportVenv) {
  $venv = Join-Path $RepoRoot ".venv-ramplus"
  $req = Join-Path $Scripts "requirements-ramplus.txt"
  Info "creating pinned RAM++ export venv at $venv ..."
  if (Test-Path $venv) { Warn "removing stale $venv (re-pinning deps)"; Remove-Item -Recurse -Force $venv }
  # `py -3.11` bypasses the Microsoft Store python.exe alias stub.
  py -3.11 -m venv $venv
  $vpy = Join-Path $venv "Scripts\python.exe"
  & $vpy -m pip install --upgrade pip
  & $vpy -m pip install -r $req
  # recognize-anything: install WITHOUT deps so it can't drag in conflicting
  # latest timm/transformers — requirements-ramplus.txt owns the versions.
  & $vpy -m pip install --no-deps "git+https://github.com/xinyu1205/recognize-anything.git"
  Info "verifying the ram_plus import resolves ..."
  & $vpy -c "from ram.models import ram_plus; print('ram_plus import OK')"
  Ok "RAM++ export venv ready"
}

# --- GUI-gated tools (printed, not auto-run unless asked) -------------------
$vsArgs = '--add Microsoft.VisualStudio.Workload.ManagedDesktop ' +
          '--add Microsoft.VisualStudio.ComponentGroup.WindowsAppSDK.Cs ' +
          '--includeRecommended'
if ($IncludeVisualStudio) {
  Info "installing Visual Studio 2022 Community + WinUI workloads (large) ..."
  winget install -e --id Microsoft.VisualStudio.2022.Community --accept-source-agreements --accept-package-agreements --silent --override "$vsArgs"
  Ok "Visual Studio 2022"
} else {
  Warn "Visual Studio is NOT auto-installed (it's multi-GB). The WinUI 3 app needs it (PriGen can't run from plain CLI)."
  Write-Host "      To install it:  winget install -e --id Microsoft.VisualStudio.2022.Community --override `"$vsArgs`"" -ForegroundColor DarkGray
  Write-Host "      (or re-run this script with -IncludeVisualStudio)" -ForegroundColor DarkGray
}

Write-Host ""
Ok "Toolchain ready. Next:"
Write-Host "  Engine:  pwsh platforms\windows\build\build-all.ps1" -ForegroundColor Gray
Write-Host "  App:     open platforms\windows\FileID.sln in Visual Studio 2022, Build (x64)" -ForegroundColor Gray
Write-Host "  RAM++:   .\.venv-ramplus\Scripts\Activate.ps1 ; then run shared\scripts\export_ram_plus_onnx.py" -ForegroundColor Gray
Write-Host "           (see the export command in shared/docs or the script's header)" -ForegroundColor Gray

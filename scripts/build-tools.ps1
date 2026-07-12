[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Arch,
    [string]$InstallDir
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$EngineDir = Join-Path $RepoRoot 'platforms/windows/src/engine'
$CliDir = Join-Path $RepoRoot 'platforms/cli'
$TuiDir = Join-Path $RepoRoot 'platforms/tui'
$Arch = if ([string]::IsNullOrWhiteSpace($Arch)) {
    if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq
        [System.Runtime.InteropServices.Architecture]::Arm64) { 'arm64' } else { 'x64' }
} else {
    $Arch
}
$Target = if ($Arch -eq 'arm64') {
    'aarch64-pc-windows-msvc'
} else {
    'x86_64-pc-windows-msvc'
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $cargoHome = if ([string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
        Join-Path $HOME '.cargo'
    } else {
        $env:CARGO_HOME
    }
    $InstallDir = Join-Path $cargoHome 'bin'
}
$InstallDir = [System.IO.Path]::GetFullPath($InstallDir)

$Rustup = Get-Command rustup -ErrorAction SilentlyContinue
if ($Rustup) {
    $script:CargoProgram = $Rustup.Source
    $script:CargoPrefix = @('run', '1.90', 'cargo')
    $rustcVersion = & $Rustup.Source run 1.90 rustc --version
    if ($LASTEXITCODE -ne 0 -or $rustcVersion -notmatch '^rustc 1\.90\.') {
        throw 'Rust 1.90 is required; install it with: rustup toolchain install 1.90'
    }
} else {
    $Cargo = Get-Command cargo -ErrorAction Stop
    $script:CargoProgram = $Cargo.Source
    $script:CargoPrefix = @()
    $cargoVersion = & $Cargo.Source --version
    if ($LASTEXITCODE -ne 0 -or $cargoVersion -notmatch '^cargo 1\.90\.') {
        throw "Rust/Cargo 1.90 is required; found: $cargoVersion"
    }
}

function Invoke-Cargo {
    param([string[]]$Arguments)
    & $script:CargoProgram @script:CargoPrefix @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo exited with code $LASTEXITCODE"
    }
}

Write-Host "==> [1/3] Building FileIDEngine ($Target, release)"
Invoke-Cargo @(
    'build', '--release', '--locked',
    '--target', $Target, '--manifest-path', (Join-Path $EngineDir 'Cargo.toml')
)

Write-Host "==> [2/3] Building fileid CLI ($Target, release)"
Invoke-Cargo @(
    'build', '--release', '--locked', '--target', $Target,
    '--manifest-path', (Join-Path $CliDir 'Cargo.toml')
)

Write-Host "==> [3/3] Building fileid-tui ($Target, release)"
Invoke-Cargo @(
    'build', '--release', '--locked', '--target', $Target,
    '--manifest-path', (Join-Path $TuiDir 'Cargo.toml')
)

$EngineOut = Join-Path $EngineDir "target/$Target/release"
$CliOut = Join-Path $CliDir "target/$Target/release"
$TuiOut = Join-Path $TuiDir "target/$Target/release"
$Artifacts = @(
    @{ Source = Join-Path $EngineOut 'FileIDEngine.exe'; Name = 'FileIDEngine.exe' },
    @{ Source = Join-Path $CliOut 'fileid.exe'; Name = 'fileid.exe' },
    @{ Source = Join-Path $TuiOut 'fileid-tui.exe'; Name = 'fileid-tui.exe' }
)

$RuntimeScript = Join-Path $RepoRoot 'platforms/windows/build/fetch-runtime-deps.ps1'
$RuntimeOutput = & $RuntimeScript -Architecture $Arch
foreach ($line in $RuntimeOutput) {
    if ($line -match '^RUNTIME_DLL=(.+)$') {
        $source = $Matches[1]
        $Artifacts += @{ Source = $source; Name = [System.IO.Path]::GetFileName($source) }
    }
}
$RuntimeNames = @($Artifacts | ForEach-Object Name)
foreach ($required in @('onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'DirectML.dll', 'pdfium.dll')) {
    if ($RuntimeNames -notcontains $required) {
        throw "runtime fetch did not resolve $required"
    }
}

foreach ($artifact in $Artifacts) {
    if (-not (Test-Path -LiteralPath $artifact.Source -PathType Leaf)) {
        throw "required build artifact is missing: $($artifact.Source)"
    }
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
foreach ($artifact in $Artifacts) {
    $destination = Join-Path $InstallDir $artifact.Name
    $temporary = "$destination.tmp-$PID"
    Copy-Item -LiteralPath $artifact.Source -Destination $temporary -Force
    Move-Item -LiteralPath $temporary -Destination $destination -Force
    Write-Host "    $($artifact.Name) -> $destination"
}

& (Join-Path $InstallDir 'fileid.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'installed fileid.exe did not start' }
& (Join-Path $InstallDir 'fileid-tui.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'installed fileid-tui.exe did not start' }

Write-Host "Done. The CLI, TUI, engine, and required ONNX/DirectML/PDFium DLLs are in $InstallDir"

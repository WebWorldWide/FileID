# FileID Windows — cross-compile build (any host, ARM64 target).
#
# Produces FileIDEngine.exe for `aarch64-pc-windows-msvc`. Drives the same
# cargo invocation as build.ps1 but with the ARM64 target triple.
#
# To run on a x64 dev box you need:
#   rustup target add aarch64-pc-windows-msvc
# (rust-toolchain.toml already requests this so `cargo build --target ...`
# fetches the std lib automatically.)
#
# Native ARM64 hardware (Snapdragon X Elite Copilot+) can also run this
# script with no flag changes — Cargo handles host vs target detection.

param(
    [switch]$Clean,
    [switch]$Release = $true,
    [switch]$RunTests
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src/engine")
$DistDir     = Join-Path $PlatformDir "dist/arm64"
$StagingDir  = Join-Path $DistDir "FileID"

Write-Host "FileID Windows build (ARM64)" -ForegroundColor Cyan
Write-Host "  engine: $EngineDir"
Write-Host "  dist:   $StagingDir"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found. Install Rust via https://rustup.rs and re-run."
}

# Ensure the ARM64 toolchain target is installed.
$installed = (rustup target list --installed) -split "`n"
if (-not ($installed -contains "aarch64-pc-windows-msvc")) {
    Write-Host "Adding aarch64-pc-windows-msvc target..." -ForegroundColor Yellow
    rustup target add aarch64-pc-windows-msvc
}

# ARM64 cross-compile requires the MSVC ARM64 build tools (cl.exe). They
# are NOT installed by default with Visual Studio. Install via:
#   Visual Studio Installer → Modify → Individual Components →
#   "MSVC v143 - VS 2022 C++ ARM64/ARM64EC build tools (Latest)"
# Or with the unattended switch:
#   vs_buildtools.exe modify --add Microsoft.VisualStudio.Component.VC.Tools.ARM64
# Bundled-SQLite (rusqlite) compiles a C amalgamation; without ARM64
# cl.exe it fails the cc-rs probe. CI's windows-11-arm runner builds
# ARM64 natively so it sidesteps this; local devs need the build tools.

if ($Clean) {
    Write-Host "Cleaning previous build artifacts..." -ForegroundColor Yellow
    Push-Location $EngineDir; cargo clean; Pop-Location
    if (Test-Path $DistDir) { Remove-Item -Recurse -Force $DistDir }
}

Push-Location $EngineDir
try {
    $profileFlag = if ($Release) { "--release" } else { "" }
    Write-Host "Building FileIDEngine (aarch64-pc-windows-msvc)..." -ForegroundColor Cyan
    cargo build $profileFlag --target aarch64-pc-windows-msvc

    if ($RunTests) {
        # Tests can only run if we're on ARM64 hardware (cross-runs aren't
        # supported by default). On x64 hosts we skip — CI's `windows-11-arm`
        # runner picks up the test pass instead.
        $hostArch = (Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture
        # Architecture: 0 = x86, 9 = x64, 12 = ARM64 (per Win32_Processor docs)
        if ($hostArch -eq 12) {
            Write-Host "Running cargo tests on native ARM64 host..." -ForegroundColor Cyan
            cargo test --target aarch64-pc-windows-msvc
        } else {
            Write-Host "Skipping tests (cross-target ARM64 from non-ARM64 host)" -ForegroundColor Yellow
        }
    }
}
finally {
    Pop-Location
}

$BuildDir = if ($Release) {
    Join-Path $EngineDir "target/aarch64-pc-windows-msvc/release"
} else {
    Join-Path $EngineDir "target/aarch64-pc-windows-msvc/debug"
}
$EngineExe = Join-Path $BuildDir "FileIDEngine.exe"
if (-not (Test-Path $EngineExe)) {
    Write-Error "Engine binary not at expected path: $EngineExe"
}

New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
Copy-Item $EngineExe (Join-Path $StagingDir "FileIDEngine.exe") -Force

$EngineSize = (Get-Item (Join-Path $StagingDir "FileIDEngine.exe")).Length / 1MB
Write-Host ("FileIDEngine.exe (ARM64) size: {0:F1} MB" -f $EngineSize) -ForegroundColor Green
Write-Host "Build complete." -ForegroundColor Green
Write-Host "Stage: $StagingDir"

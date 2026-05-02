# FileID Windows — dev build (x64 host, x64 target).
#
# Builds:
#   - FileIDEngine.exe (Rust release with LTO; x64)
#
# Layout produced under platforms/windows/dist/x64/:
#   FileID/
#     FileIDEngine.exe
#     (DLL companions land here once ORT + llama.cpp + pdfium integration
#      lands in Phase 1+. Phase 0 ships only the engine binary.)
#
# Phase 1+ extends this script to build FileID.App via `dotnet publish`,
# stage acrylic shaders + Win2D companions, and stamp version metadata.

param(
    [switch]$Clean,
    [switch]$Release = $true,
    [switch]$RunTests
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Resolve repo root + platforms/windows root, regardless of where this is invoked from.
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src/engine")
$DistDir     = Join-Path $PlatformDir "dist/x64"
$StagingDir  = Join-Path $DistDir "FileID"

Write-Host "FileID Windows build (x64)" -ForegroundColor Cyan
Write-Host "  engine: $EngineDir"
Write-Host "  dist:   $StagingDir"

# ─── 1. Toolchain probes ─────────────────────────────────────────────────────
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found. Install Rust via https://rustup.rs and re-run."
}

# ─── 2. Clean ───────────────────────────────────────────────────────────────
if ($Clean) {
    Write-Host "Cleaning previous build artifacts..." -ForegroundColor Yellow
    Push-Location $EngineDir
    cargo clean
    Pop-Location
    if (Test-Path $DistDir) { Remove-Item -Recurse -Force $DistDir }
}

# ─── 3. Build engine ────────────────────────────────────────────────────────
Push-Location $EngineDir
try {
    $profileFlag = if ($Release) { "--release" } else { "" }
    Write-Host "Building FileIDEngine ($([IO.Path]::Combine('release', 'x86_64-pc-windows-msvc')))..." -ForegroundColor Cyan
    cargo build $profileFlag --target x86_64-pc-windows-msvc

    if ($RunTests) {
        Write-Host "Running cargo tests..." -ForegroundColor Cyan
        cargo test --target x86_64-pc-windows-msvc
    }
}
finally {
    Pop-Location
}

# ─── 4. Stage ───────────────────────────────────────────────────────────────
$BuildDir = if ($Release) {
    Join-Path $EngineDir "target/x86_64-pc-windows-msvc/release"
} else {
    Join-Path $EngineDir "target/x86_64-pc-windows-msvc/debug"
}
$EngineExe = Join-Path $BuildDir "FileIDEngine.exe"
if (-not (Test-Path $EngineExe)) {
    Write-Error "Engine binary not at expected path: $EngineExe"
}

New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
Copy-Item $EngineExe (Join-Path $StagingDir "FileIDEngine.exe") -Force

# Strip optional symbols / verify the binary is self-contained.
$EngineSize = (Get-Item (Join-Path $StagingDir "FileIDEngine.exe")).Length / 1MB
Write-Host ("FileIDEngine.exe size: {0:F1} MB" -f $EngineSize) -ForegroundColor Green

Write-Host "Build complete." -ForegroundColor Green
Write-Host "Stage: $StagingDir"

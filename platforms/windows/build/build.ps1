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
    [switch]$RunTests,
    [switch]$WipeDb
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

# ─── 2b. Wipe library database (keep downloaded models) ──────────────────────
# Deletes %LOCALAPPDATA%\FileID\fileid.sqlite{,-wal,-shm} so the next launch
# does a full re-scan + re-tag from scratch. Leaves Models\ (CLIP / ArcFace /
# VLM — hundreds of MB) and thumbs.cache\ untouched, so nothing re-downloads
# and thumbnails stay warm. Use after a tagging change when an incremental
# rescan would skip files already in the DB. Close the app first — a running
# engine holds the SQLite file open.
if ($WipeDb) {
    $FileIdData = Join-Path $env:LOCALAPPDATA "FileID"
    $dbFiles = Get-ChildItem -Path $FileIdData -Filter "fileid.sqlite*" -File -ErrorAction SilentlyContinue
    if (-not $dbFiles) {
        Write-Host "WipeDb: no database at $FileIdData (already clean)." -ForegroundColor Yellow
    }
    else {
        foreach ($f in $dbFiles) {
            try {
                Remove-Item -LiteralPath $f.FullName -Force -ErrorAction Stop
                Write-Host "WipeDb: removed $($f.Name)" -ForegroundColor Yellow
            }
            catch {
                Write-Warning "WipeDb: could not delete $($f.Name) — is FileID still running? Close it and re-run. ($($_.Exception.Message))"
            }
        }
        Write-Host "WipeDb: database cleared; Models\ and thumbs.cache\ preserved." -ForegroundColor Green
    }
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

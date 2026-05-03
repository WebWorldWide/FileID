# FileID Windows — full dev build (engine + WinUI 3 app, x64).
#
# This is the canonical "I just cloned the repo, build me a runnable app"
# command. Chains every step:
#
#   1. Toolchain probes  (cargo, dotnet, MSBuild via VS Build Tools)
#   2. Optional clean    (cargo clean + dotnet clean + remove dist/)
#   3. Engine build      (Rust release LTO, x64)
#   4. App build         (dotnet build solution OR dotnet publish for -Release)
#   5. Stage             (copy FileIDEngine.exe alongside FileID.exe)
#   6. Smoke             (verify both binaries present, sized sanely)
#   7. Optional run      (Start-Process FileID.exe)
#
# Usage:
#   pwsh build/build-all.ps1                    # Debug, x64
#   pwsh build/build-all.ps1 -Release           # Release self-contained publish
#   pwsh build/build-all.ps1 -Run               # Build + launch FileID.exe
#   pwsh build/build-all.ps1 -Clean             # Wipe everything first
#   pwsh build/build-all.ps1 -SkipEngine        # WinUI-only iteration
#   pwsh build/build-all.ps1 -SkipApp           # Engine-only iteration
#   pwsh build/build-all.ps1 -RunTests          # cargo test + dotnet test
#
# Failure modes the script catches and explains (instead of leaking a
# raw stack trace):
#   - cargo not in PATH                  → install Rust via rustup
#   - x64 Rust target missing            → auto-add via rustup
#   - dotnet SDK 8 not installed         → winget install
#   - VS Build Tools UWP component       → winget add command + URL
#   - WinAppSDK runtime missing at run   → winget install (only on -Run)
#
# Exit code: 0 on success, 1 on any failure.

param(
    [switch]$Release,
    [switch]$Clean,
    [switch]$Run,
    [switch]$RunTests,
    [switch]$SkipEngine,
    [switch]$SkipApp,
    [switch]$Desktop
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# -Desktop implies -Release: a publish folder is what users actually want
# to drop on their Desktop and double-click. Debug builds need a .NET SDK
# on the host to launch.
if ($Desktop) { $Release = $true }

# ─── Paths ──────────────────────────────────────────────────────────────────
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src/engine")
$AppCsproj   = Join-Path $PlatformDir "src/FileID.App/FileID.App.csproj"
$Solution    = Join-Path $PlatformDir "FileID.sln"
$DistDir     = Join-Path $PlatformDir "dist/x64"
$StagingDir  = Join-Path $DistDir "FileID"

# Fixed target framework — the .csproj pins net8.0-windows10.0.19041.0.
$AppTfm      = "net8.0-windows10.0.19041.0"
$AppRid      = "win-x64"

$Configuration = if ($Release) { "Release" } else { "Debug" }
$RustProfile   = if ($Release) { "release" } else { "debug" }
$RustFlag      = if ($Release) { "--release" } else { "" }

Write-Host "FileID Windows — full build" -ForegroundColor Cyan
Write-Host "  configuration: $Configuration"
Write-Host "  engine:        $EngineDir"
Write-Host "  solution:      $Solution"
Write-Host "  staging:       $StagingDir"
Write-Host ""

# ─── 1. Toolchain probes ────────────────────────────────────────────────────
function Require-Command($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        Write-Host "ERROR: '$name' not found on PATH." -ForegroundColor Red
        Write-Host "       $hint" -ForegroundColor Yellow
        exit 1
    }
}

if (-not $SkipEngine) {
    Require-Command "cargo" "Install Rust via https://rustup.rs and re-open your shell."
    # Ensure x64 target is installed.
    $targets = & rustup target list --installed 2>$null
    if ($LASTEXITCODE -eq 0 -and $targets -notcontains "x86_64-pc-windows-msvc") {
        Write-Host "Adding rust target x86_64-pc-windows-msvc..." -ForegroundColor Yellow
        & rustup target add x86_64-pc-windows-msvc
    }
}

if (-not $SkipApp) {
    Require-Command "dotnet" "Install .NET 8 SDK: winget install Microsoft.DotNet.SDK.8"
    $dotnetVer = (& dotnet --version 2>$null)
    if (-not ($dotnetVer -match "^(8|9)\.")) {
        Write-Host "ERROR: dotnet SDK $dotnetVer found; need 8.x or 9.x." -ForegroundColor Red
        Write-Host "       winget install Microsoft.DotNet.SDK.8" -ForegroundColor Yellow
        exit 1
    }
}

# ─── 2. Clean ───────────────────────────────────────────────────────────────
if ($Clean) {
    Write-Host "Cleaning previous build artifacts..." -ForegroundColor Yellow
    if (-not $SkipEngine) {
        Push-Location $EngineDir
        try { & cargo clean } finally { Pop-Location }
    }
    if (-not $SkipApp) {
        & dotnet clean $Solution -c $Configuration -p:Platform=x64 --nologo | Out-Null
    }
    if (Test-Path $DistDir) { Remove-Item -Recurse -Force $DistDir }
}

# ─── 3. Engine build + dotnet restore (in parallel) ────────────────────────
# Cargo build and NuGet restore are fully independent — running them
# concurrently saves 20-40 seconds on a cold build. The dotnet build/
# publish step below joins on both before continuing.
$restoreJob = $null
if (-not $SkipApp) {
    Write-Host "Restoring NuGet packages (background)..." -ForegroundColor Cyan
    $restoreJob = Start-Job -ScriptBlock {
        param($solution)
        & dotnet restore $solution --nologo *>&1
    } -ArgumentList $Solution
}

if (-not $SkipEngine) {
    Write-Host "Building engine ($RustProfile, x86_64-pc-windows-msvc)..." -ForegroundColor Cyan
    Push-Location $EngineDir
    try {
        if ($Release) {
            & cargo build --release --target x86_64-pc-windows-msvc
        } else {
            & cargo build --target x86_64-pc-windows-msvc
        }
        if ($RunTests) {
            Write-Host "Running cargo tests..." -ForegroundColor Cyan
            & cargo test --target x86_64-pc-windows-msvc
        }
    } finally {
        Pop-Location
    }

    $EngineBuildExe = Join-Path $EngineDir "target/x86_64-pc-windows-msvc/$RustProfile/FileIDEngine.exe"
    if (-not (Test-Path $EngineBuildExe)) {
        Write-Host "ERROR: Engine binary missing at $EngineBuildExe" -ForegroundColor Red
        if ($restoreJob) { Stop-Job $restoreJob; Remove-Job $restoreJob }
        exit 1
    }
    New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
    Copy-Item $EngineBuildExe (Join-Path $StagingDir "FileIDEngine.exe") -Force
    $sz = [math]::Round((Get-Item (Join-Path $StagingDir "FileIDEngine.exe")).Length / 1MB, 1)
    Write-Host ("  FileIDEngine.exe staged ({0} MB)" -f $sz) -ForegroundColor Green
}

# ─── 4. App build ───────────────────────────────────────────────────────────
if (-not $SkipApp) {
    if ($restoreJob) {
        # Wait for the restore that started in step 3.
        $null = Wait-Job $restoreJob
        Receive-Job $restoreJob | Out-Null
        Remove-Job $restoreJob
    }

    if ($Release) {
        Write-Host "Publishing FileID.App ($Configuration, $AppRid, self-contained)..." -ForegroundColor Cyan
        & dotnet publish $AppCsproj `
            -c $Configuration `
            -r $AppRid `
            --self-contained true `
            /p:PublishReadyToRun=true `
            -p:Platform=x64 `
            --nologo
    } else {
        Write-Host "Building FileID solution ($Configuration, x64)..." -ForegroundColor Cyan
        & dotnet build $Solution -c $Configuration -p:Platform=x64 --nologo
    }

    if ($RunTests) {
        Write-Host "Running xUnit tests..." -ForegroundColor Cyan
        & dotnet test (Join-Path $PlatformDir "Tests/FileID.IpcSchema.Tests/FileID.IpcSchema.Tests.csproj") `
            --nologo --no-build -c $Configuration
    }
}

# ─── 5. Stage engine alongside app ──────────────────────────────────────────
function Resolve-AppOutputDir {
    if ($Release) {
        return Join-Path $PlatformDir "src/FileID.App/bin/x64/$Configuration/$AppTfm/$AppRid/publish"
    } else {
        return Join-Path $PlatformDir "src/FileID.App/bin/x64/$Configuration/$AppTfm/$AppRid"
    }
}

if (-not $SkipApp) {
    $AppOutDir = Resolve-AppOutputDir
    $AppExe    = Join-Path $AppOutDir "FileID.exe"
    if (-not (Test-Path $AppExe)) {
        Write-Host "ERROR: FileID.exe missing at $AppExe" -ForegroundColor Red
        Write-Host "       The .NET build's output layout may have changed; verify the TFM/RID still match the csproj." -ForegroundColor Yellow
        exit 1
    }
    if (-not $SkipEngine) {
        $StagedEngine = Join-Path $StagingDir "FileIDEngine.exe"
        if (Test-Path $StagedEngine) {
            Copy-Item $StagedEngine (Join-Path $AppOutDir "FileIDEngine.exe") -Force
            Write-Host "  FileIDEngine.exe colocated with FileID.exe" -ForegroundColor Green
        }
    }

    # ─── 6. Smoke ───────────────────────────────────────────────────────────
    $appSize = [math]::Round((Get-Item $AppExe).Length / 1KB, 0)
    Write-Host ("  FileID.exe       OK ({0} KB)" -f $appSize) -ForegroundColor Green
    $bootstrap = Join-Path $AppOutDir "Microsoft.WindowsAppRuntime.Bootstrap.dll"
    if (Test-Path $bootstrap) {
        Write-Host "  WinAppSDK bootstrap DLL OK" -ForegroundColor Green
    } else {
        Write-Host "  WARN: Microsoft.WindowsAppRuntime.Bootstrap.dll not found beside FileID.exe." -ForegroundColor Yellow
        Write-Host "        Self-contained mode should pull this in; if missing the app will fail to launch." -ForegroundColor Yellow
    }
}

# ─── 7. Install to LocalAppData + Desktop shortcut ──────────────────────────
# WinUI 3 self-contained publish produces ~900 companion files (the .NET
# runtime, WinAppSDK runtime, Win2D, project DLLs). Dumping that on the
# Desktop is unfriendly. Instead: put the files in %LOCALAPPDATA% (out of
# sight) and put ONE shortcut named "FileID" on the Desktop. User sees one
# icon, double-clicks it, app runs — like every other Windows app.
$InstallDir = $null
if ($Desktop) {
    if ($SkipApp) {
        Write-Host "Cannot -Desktop with -SkipApp (nothing to install)." -ForegroundColor Red
        exit 1
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA "FileID-App"
    $ShortcutPath = Join-Path $env:USERPROFILE "Desktop\FileID.lnk"
    Write-Host ""
    Write-Host "Installing to $InstallDir..." -ForegroundColor Cyan

    # If a previous run is still running, the .exe is locked. Kill it
    # first so we can replace files cleanly.
    Get-Process -Name "FileID", "FileIDEngine" -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "  Stopping running $($_.Name) (PID $($_.Id))..." -ForegroundColor Yellow
        try { $_ | Stop-Process -Force -ErrorAction Stop } catch { }
    }
    Start-Sleep -Milliseconds 200

    if (Test-Path $InstallDir) {
        Remove-Item -Recurse -Force $InstallDir
    }
    $src = Resolve-AppOutputDir
    Copy-Item -Recurse $src $InstallDir
    $fileCount = (Get-ChildItem -Recurse $InstallDir | Measure-Object).Count
    $totalMb = [math]::Round(((Get-ChildItem -Recurse $InstallDir | Measure-Object -Property Length -Sum).Sum / 1MB), 1)
    Write-Host ("  Installed $fileCount files ({0} MB) under LocalAppData" -f $totalMb) -ForegroundColor Green

    # Create / refresh the Desktop shortcut. WScript.Shell COM is the
    # idiomatic way to make a .lnk from PowerShell; no extra deps.
    $InstalledExe = Join-Path $InstallDir "FileID.exe"
    $shell = New-Object -ComObject WScript.Shell
    $sc = $shell.CreateShortcut($ShortcutPath)
    $sc.TargetPath = $InstalledExe
    $sc.WorkingDirectory = $InstallDir
    $sc.IconLocation = $InstalledExe
    $sc.Description = "FileID - on-device AI file organizer"
    $sc.Save()
    Write-Host "  Desktop shortcut: $ShortcutPath" -ForegroundColor Green
}

# ─── 8. Run ─────────────────────────────────────────────────────────────────
if ($Run) {
    if ($SkipApp) {
        Write-Host "Cannot -Run with -SkipApp." -ForegroundColor Red
        exit 1
    }
    # If -Desktop was set, launch from the installed copy. Otherwise run
    # from the build output dir.
    $LaunchDir = if ($Desktop) { $InstallDir } else { Resolve-AppOutputDir }
    $LaunchExe = Join-Path $LaunchDir "FileID.exe"
    Write-Host "Launching $LaunchExe..." -ForegroundColor Cyan
    Start-Process $LaunchExe -WorkingDirectory $LaunchDir
}

Write-Host ""
Write-Host "Build complete." -ForegroundColor Green
if ($Desktop) {
    Write-Host "  Double-click 'FileID' on your Desktop to run." -ForegroundColor Cyan
    Write-Host "  (App files installed under %LOCALAPPDATA%\FileID-App\)"
} elseif (-not $SkipApp) {
    Write-Host "  App:    $(Resolve-AppOutputDir)\FileID.exe"
}
if (-not $SkipEngine) {
    Write-Host "  Engine: $StagingDir\FileIDEngine.exe"
}

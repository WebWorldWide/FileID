# FileID Windows — release publish + WiX Burn bundle.
#
# This is the canonical "I'm cutting a release" command. Produces ONE
# downloadable artifact for end users: dist/installer/FileIDSetup.exe.
#
# What it chains:
#   1. Toolchain probes (cargo, dotnet, MSVC ARM64 cl.exe, WiX v4 SDK)
#   2. Cross-compile engine for both x86_64-pc-windows-msvc and aarch64-pc-windows-msvc
#   3. dotnet publish FileID.App for both win-x64 and win-arm64 (self-contained, R2R)
#   4. Stage FileIDEngine.exe alongside FileID.exe in each publish dir
#   5. Sign every .exe + .dll under each publish dir (skipped via -SkipSign)
#   6. Build per-arch MSIs (FileID-x64.msi + FileID-arm64.msi) via WiX
#   7. Sign both MSIs
#   8. Build Burn bundle (FileIDSetup.exe wrapping both MSIs)
#   9. Sign FileIDSetup.exe (Burn re-attaches embedded MSIs after build,
#      so the bundle MUST be signed AFTER its inner MSIs are signed,
#      otherwise the embedded copies are unsigned)
#  10. Smoke: bootstrapper exists, sized sanely, signature verifies
#  11. Privacy gate: grep shipped binaries for telemetry strings
#
# Usage:
#   pwsh build/publish-bundle.ps1 -SkipSign                 # local test build (no cert)
#   pwsh build/publish-bundle.ps1 -SignThumbprint <SHA1>    # signed release build
#   pwsh build/publish-bundle.ps1 -SkipArm64                # skip ARM64 (x64-only release)
#
# Final artifact: platforms/windows/dist/installer/FileIDSetup.exe.
# Secondary artifacts (for IT admins): FileID-x64.msi + FileID-arm64.msi
# in the same folder.

param(
    [switch]$SkipSign,
    [string]$SignThumbprint = "",
    [string]$TimestampServer = "http://timestamp.digicert.com",
    [switch]$SkipArm64,
    [switch]$SkipPrivacyGate
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src/engine")
$AppCsproj   = Join-Path $PlatformDir "src/FileID.App/FileID.App.csproj"
$Solution    = Join-Path $PlatformDir "FileID.sln"
$MsiProj     = Join-Path $PlatformDir "installer/FileID.Msi/FileID.Msi.wixproj"
$BundleProj  = Join-Path $PlatformDir "installer/FileID.Bundle/FileID.Bundle.wixproj"
$DistDir     = Join-Path $PlatformDir "dist/installer"

$AppTfm = "net8.0-windows10.0.19041.0"

# Telemetry strings the privacy gate refuses to ship. Anything matching
# any of these in the final shipped binaries fails the build.
$ForbiddenTelemetryStrings = @(
    # MUST stay in sync with .github/workflows/windows-engine.yml's
    # privacy gate. Add to both lists when adding a new SDK marker.
    "sentry.io",
    "io.sentry",
    "applicationinsights",
    "applicationinsights.azure.com",
    "googletagmanager",
    "google-analytics.com",
    "segment.io",
    "segment.com",
    "mixpanel.com",
    "amplitude.com",
    "posthog.com",
    "datadoghq",
    "bugsnag",
    "rollbar.com",
    "honeycomb.io",
    "newrelic.com",
    "raygun.io",
    "firebase",
    "firebaseio.com",
    "appcenter.ms",
    "in.appcenter.ms",
    "crashpad",
    "breakpad"
)

Write-Host "FileID release publish + bundle" -ForegroundColor Cyan
Write-Host "  Skip ARM64:    $SkipArm64"
Write-Host "  Skip sign:     $SkipSign"
Write-Host "  Skip privacy:  $SkipPrivacyGate"
Write-Host ""

# ─── 1. Toolchain probes ────────────────────────────────────────────────────
function Require-Command($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        Write-Host "ERROR: '$name' not found on PATH." -ForegroundColor Red
        Write-Host "       $hint" -ForegroundColor Yellow
        exit 1
    }
}

Require-Command "cargo" "Install Rust via https://rustup.rs"
Require-Command "dotnet" "winget install Microsoft.DotNet.SDK.8"

if (-not $SkipArm64) {
    $targets = & rustup target list --installed 2>$null
    if ($targets -notcontains "aarch64-pc-windows-msvc") {
        Write-Host "Adding rust target aarch64-pc-windows-msvc..." -ForegroundColor Yellow
        & rustup target add aarch64-pc-windows-msvc
    }
    # Verify MSVC ARM64 toolchain is installed (cargo will fail cryptically without it).
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $arm64Cl = & $vswhere -find "VC\Tools\MSVC\*\bin\Hostx64\arm64\cl.exe" 2>$null | Select-Object -First 1
        if (-not $arm64Cl) {
            Write-Host "WARN: MSVC ARM64 toolchain not detected. Install via:" -ForegroundColor Yellow
            Write-Host "      winget install Microsoft.VisualStudio.2022.BuildTools --override `"--add Microsoft.VisualStudio.Component.VC.Tools.ARM64`"" -ForegroundColor Yellow
            Write-Host "      Pass -SkipArm64 to bypass." -ForegroundColor Yellow
        }
    }
}

if (-not $SkipSign -and [string]::IsNullOrEmpty($SignThumbprint)) {
    Write-Host "ERROR: -SignThumbprint <SHA1> required (or pass -SkipSign for unsigned local builds)." -ForegroundColor Red
    exit 1
}

# ─── 2. Build engine for each arch ─────────────────────────────────────────
function Build-Engine($triple) {
    Write-Host "Building engine ($triple, release)..." -ForegroundColor Cyan
    Push-Location $EngineDir
    try { & cargo build --release --target $triple } finally { Pop-Location }
}

Build-Engine "x86_64-pc-windows-msvc"
if (-not $SkipArm64) {
    Build-Engine "aarch64-pc-windows-msvc"
}

# ─── 3. Publish app for each arch ──────────────────────────────────────────
function Publish-App($rid, $platform) {
    Write-Host "Publishing FileID.App ($rid)..." -ForegroundColor Cyan
    & dotnet publish $AppCsproj `
        -c Release `
        -r $rid `
        --self-contained true `
        /p:PublishReadyToRun=true `
        -p:Platform=$platform `
        --nologo
}

Publish-App "win-x64" "x64"
if (-not $SkipArm64) {
    Publish-App "win-arm64" "arm64"
}

# ─── 4. Stage engine into each publish dir ─────────────────────────────────
function Resolve-PublishDir($rid, $platform) {
    return Join-Path $PlatformDir "src/FileID.App/bin/$platform/Release/$AppTfm/$rid/publish"
}

function Resolve-EngineExe($triple) {
    return Join-Path $EngineDir "target/$triple/release/FileIDEngine.exe"
}

function Stage-Engine($triple, $rid, $platform) {
    $src = Resolve-EngineExe $triple
    $dst = Resolve-PublishDir $rid $platform
    if (-not (Test-Path $src)) { throw "Missing engine binary: $src" }
    if (-not (Test-Path $dst)) { throw "Missing publish dir: $dst" }
    Copy-Item $src (Join-Path $dst "FileIDEngine.exe") -Force
}

Stage-Engine "x86_64-pc-windows-msvc" "win-x64" "x64"
if (-not $SkipArm64) {
    Stage-Engine "aarch64-pc-windows-msvc" "win-arm64" "arm64"
}

# ─── 5. Sign published binaries ────────────────────────────────────────────
function Sign-Binary($path) {
    if ($SkipSign) { return }
    & signtool sign /fd SHA256 /tr $TimestampServer /td SHA256 /sha1 $SignThumbprint $path | Out-Null
}

function Sign-PublishDir($dir) {
    if ($SkipSign) { return }
    Write-Host "Signing binaries under $dir..." -ForegroundColor Cyan
    $files = Get-ChildItem -Path $dir -Recurse -Include *.exe, *.dll
    foreach ($f in $files) {
        Sign-Binary $f.FullName
    }
}

Sign-PublishDir (Resolve-PublishDir "win-x64" "x64")
if (-not $SkipArm64) {
    Sign-PublishDir (Resolve-PublishDir "win-arm64" "arm64")
}

# ─── 6. Build per-arch MSIs ────────────────────────────────────────────────
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

Write-Host "Building FileID-x64.msi..." -ForegroundColor Cyan
& dotnet build $MsiProj -c Release -p:Platform=x64 --nologo

if (-not $SkipArm64) {
    Write-Host "Building FileID-arm64.msi..." -ForegroundColor Cyan
    & dotnet build $MsiProj -c Release -p:Platform=arm64 --nologo
}

# ─── 7. Sign MSIs ──────────────────────────────────────────────────────────
$MsiX64   = Join-Path $DistDir "FileID-x64.msi"
$MsiArm64 = Join-Path $DistDir "FileID-arm64.msi"
Sign-Binary $MsiX64
if (-not $SkipArm64) { Sign-Binary $MsiArm64 }

# ─── 8. Build Burn bundle ──────────────────────────────────────────────────
Write-Host "Building FileIDSetup.exe (Burn bundle)..." -ForegroundColor Cyan
& dotnet build $BundleProj -c Release --nologo

$BundleExe = Join-Path $DistDir "FileIDSetup.exe"
if (-not (Test-Path $BundleExe)) {
    Write-Host "ERROR: Bundle not produced at $BundleExe" -ForegroundColor Red
    exit 1
}

# ─── 9. Sign bundle ────────────────────────────────────────────────────────
# Burn re-attaches the embedded MSIs after the bundle is built; the bundle
# itself MUST be re-signed last so the outer Authenticode signature is
# valid AFTER the embedded MSIs are stamped in. WiX docs call this out
# explicitly — `insignia` is the tool but signtool on the final .exe works.
Sign-Binary $BundleExe

# ─── 10. Smoke ─────────────────────────────────────────────────────────────
$bundleSize = [math]::Round((Get-Item $BundleExe).Length / 1MB, 1)
Write-Host ""
Write-Host "Smoke checks:" -ForegroundColor Cyan
Write-Host ("  FileIDSetup.exe       OK ({0} MB)" -f $bundleSize) -ForegroundColor Green
$msiSize = [math]::Round((Get-Item $MsiX64).Length / 1MB, 1)
Write-Host ("  FileID-x64.msi        OK ({0} MB)" -f $msiSize) -ForegroundColor Green
if (-not $SkipArm64) {
    $msiSize = [math]::Round((Get-Item $MsiArm64).Length / 1MB, 1)
    Write-Host ("  FileID-arm64.msi      OK ({0} MB)" -f $msiSize) -ForegroundColor Green
}

if (-not $SkipSign) {
    $sig = Get-AuthenticodeSignature $BundleExe
    if ($sig.Status -ne "Valid") {
        Write-Host "ERROR: Bundle signature status is $($sig.Status)" -ForegroundColor Red
        exit 1
    }
    Write-Host "  Authenticode          OK (signed by $($sig.SignerCertificate.Subject))" -ForegroundColor Green
}

# ─── 11. Privacy gate ──────────────────────────────────────────────────────
if (-not $SkipPrivacyGate) {
    Write-Host ""
    Write-Host "Privacy gate: scanning shipped binaries..." -ForegroundColor Cyan
    $hits = @()
    $publishDirs = @((Resolve-PublishDir "win-x64" "x64"))
    if (-not $SkipArm64) {
        $publishDirs += (Resolve-PublishDir "win-arm64" "arm64")
    }
    foreach ($d in $publishDirs) {
        $files = Get-ChildItem -Path $d -Recurse -Include *.exe, *.dll
        foreach ($f in $files) {
            foreach ($needle in $ForbiddenTelemetryStrings) {
                $found = Select-String -Path $f.FullName -Pattern $needle -SimpleMatch -List -ErrorAction SilentlyContinue
                if ($found) {
                    $hits += [pscustomobject]@{ File = $f.FullName; Pattern = $needle }
                }
            }
        }
    }
    if ($hits.Count -gt 0) {
        Write-Host "ERROR: Privacy gate found $($hits.Count) telemetry-pattern hit(s):" -ForegroundColor Red
        $hits | Format-Table -AutoSize
        Write-Host "       Refusing to ship. Investigate or pass -SkipPrivacyGate to bypass (NOT for releases)." -ForegroundColor Yellow
        exit 1
    }
    Write-Host "  Privacy gate          OK (zero telemetry strings)" -ForegroundColor Green
}

Write-Host ""
Write-Host "Release artifacts staged under:" -ForegroundColor Green
Write-Host "  $DistDir\FileIDSetup.exe   ← canonical user-facing download"
Write-Host "  $DistDir\FileID-x64.msi    ← for IT admins (SCCM/Intune)"
if (-not $SkipArm64) {
    Write-Host "  $DistDir\FileID-arm64.msi  ← for IT admins (Snapdragon WoA)"
}

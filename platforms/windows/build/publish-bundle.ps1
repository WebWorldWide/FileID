# FileID Windows — release publish + WiX Burn bundle.
#
# This is the canonical "I'm cutting a release" command. Produces ONE
# downloadable artifact for end users: dist/installer/FileIDSetup.exe.
#
# What it chains:
#   1. Toolchain probes (cargo, dotnet, MSVC ARM64 cl.exe, WiX v4 SDK)
#   2. Download + SHA256-verify the pinned Windows App Runtime and native ML/PDF prerequisites
#   3. Cross-compile engine for both x86_64-pc-windows-msvc and aarch64-pc-windows-msvc
#   4. dotnet publish FileID.App for both win-x64 and win-arm64 (.NET self-contained, R2R)
#   5. Stage FileIDEngine.exe alongside FileID.exe in each publish dir
#   6. Sign FileID-owned .exe + .dll files in each publish dir (skipped via -SkipSign)
#   7. Build per-arch MSIs (FileID-x64.msi + FileID-arm64.msi) via WiX
#   8. Sign both MSIs
#   9. Build Burn bundle (runtime prerequisite + architecture-matched MSI)
#  10. Sign FileIDSetup.exe (Burn re-attaches embedded packages after build,
#      so the bundle MUST be signed AFTER its inner MSIs are signed,
#      otherwise the embedded copies are unsigned)
#  11. Smoke: bootstrapper exists, sized sanely, signature verifies
#  12. Privacy gate: grep shipped binaries for telemetry strings
#  13. Write SHA256SUMS.txt for every produced installer artifact
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

# A release build must never bypass signing or the privacy gate. release.yml
# sets CI_RELEASE=true on the actual signing path; if -SkipSign / -SkipPrivacyGate
# slipped through there it would ship unsigned/unverified binaries to users —
# exactly the failure these gates exist to prevent. Local dev builds (CI_RELEASE
# unset) may still -SkipSign for cert-less iteration; a cert-less CI dry-run must
# NOT set CI_RELEASE.
if ($env:CI_RELEASE -eq 'true') {
    if ($SkipSign) {
        Write-Host "ERROR: -SkipSign is forbidden on a release build (would ship unsigned binaries)." -ForegroundColor Red
        exit 1
    }
    if ($SkipPrivacyGate) {
        Write-Host "ERROR: -SkipPrivacyGate is forbidden on a release build (would ship unverified binaries)." -ForegroundColor Red
        exit 1
    }
}

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src/engine")
$AppCsproj   = Join-Path $PlatformDir "src/FileID.App/FileID.App.csproj"
$Solution    = Join-Path $PlatformDir "FileID.sln"
$MsiProj     = Join-Path $PlatformDir "installer/FileID.Msi/FileID.Msi.wixproj"
$BundleProj  = Join-Path $PlatformDir "installer/FileID.Bundle/FileID.Bundle.wixproj"
$DistDir     = Join-Path $PlatformDir "dist/installer"
$PrereqDir   = Join-Path $PlatformDir "dist/prereqs"
$FetchRuntimeScript = Join-Path $ScriptDir "fetch-runtime-deps.ps1"

$AppTfm = "net8.0-windows10.0.19041.0"
$WinAppRuntimeVersion = "1.7.250606001"
$WinAppRuntimeX64Url = "https://aka.ms/windowsappsdk/1.7/$WinAppRuntimeVersion/windowsappruntimeinstall-x64.exe"
$WinAppRuntimeArm64Url = "https://aka.ms/windowsappsdk/1.7/$WinAppRuntimeVersion/windowsappruntimeinstall-arm64.exe"
$WinAppRuntimeX64Sha256 = "0bd5e81e5475d97bf3a2e73d7abe34dcf43a9ab9226534aba51d1757ec0b2ce1"
$WinAppRuntimeArm64Sha256 = "d02fe67517b9c72d14ed5fdd41d8b667e40b6a8b76872d43677a20d28b6cbeab"

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
Require-Command "rustup" "Install Rust via https://rustup.rs"
Require-Command "dotnet" "winget install Microsoft.DotNet.SDK.8"

$rustVersion = & rustup run 1.90 rustc --version 2>$null
if ($LASTEXITCODE -ne 0 -or $rustVersion -notmatch '^rustc 1\.90\.') {
    Write-Host "ERROR: Rust 1.90 is required, but 'rustup run 1.90 rustc --version' did not return a 1.90 toolchain." -ForegroundColor Red
    Write-Host "       Install it with: rustup toolchain install 1.90 --profile minimal" -ForegroundColor Yellow
    exit 1
}
Write-Host "  Rust toolchain: $rustVersion" -ForegroundColor Green

$requiredRustTargets = @("x86_64-pc-windows-msvc")
if (-not $SkipArm64) { $requiredRustTargets += "aarch64-pc-windows-msvc" }
$installedRustTargets = @(& rustup target list --toolchain 1.90 --installed 2>$null)
foreach ($target in $requiredRustTargets) {
    if ($installedRustTargets -notcontains $target) {
        Write-Host "Adding Rust 1.90 target $target..." -ForegroundColor Yellow
        & rustup target add --toolchain 1.90 $target
        if ($LASTEXITCODE -ne 0) {
            Write-Host "ERROR: rustup could not install target '$target' for Rust 1.90." -ForegroundColor Red
            exit 1
        }
    }
}

if (-not $SkipArm64) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        Write-Host "ERROR: Visual Studio Installer's vswhere.exe is missing; the ARM64 MSVC toolchain cannot be verified." -ForegroundColor Red
        Write-Host "       Install Visual Studio 2022 Build Tools with the C++ workload and ARM64 build tools." -ForegroundColor Yellow
        Write-Host "       Pass -SkipArm64 only for a deliberately x64-only local build." -ForegroundColor Yellow
        exit 1
    }

    $arm64VsInstall = @(& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath 2>$null) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        Select-Object -First 1
    if (-not $arm64VsInstall) {
        $fallbackVsInstall = @(& $vswhere -latest -products * -property installationPath 2>$null) |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            Select-Object -First 1
        Write-Host "ERROR: MSVC ARM64/ARM64EC build tools are not installed." -ForegroundColor Red
        if ($fallbackVsInstall) {
            $setupExe = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe"
            Write-Host "       Install the missing component with:" -ForegroundColor Yellow
            Write-Host "       & '$setupExe' modify --installPath '$fallbackVsInstall' --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 --includeRecommended --passive --norestart" -ForegroundColor Yellow
        } else {
            Write-Host "       Install Visual Studio 2022 Build Tools, then select 'MSVC v143 ARM64/ARM64EC build tools' and a Windows 10/11 SDK." -ForegroundColor Yellow
        }
        Write-Host "       Pass -SkipArm64 only for a deliberately x64-only local build." -ForegroundColor Yellow
        exit 1
    }

    $arm64Cl = Get-ChildItem -Path (Join-Path $arm64VsInstall "VC\Tools\MSVC\*\bin\Hostx64\arm64\cl.exe") -File -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -ExpandProperty FullName -First 1
    if (-not $arm64Cl) {
        Write-Host "ERROR: Visual Studio reports the ARM64 component, but Hostx64\arm64\cl.exe is missing under '$arm64VsInstall'." -ForegroundColor Red
        Write-Host "       Repair 'MSVC ARM64/ARM64EC build tools' in Visual Studio Installer." -ForegroundColor Yellow
        exit 1
    }

    $arm64ToolRoot = (Get-Item -LiteralPath $arm64Cl).Directory.Parent.Parent.Parent.FullName
    $arm64Link = Join-Path (Split-Path -Parent $arm64Cl) "link.exe"
    $arm64Crt = Join-Path $arm64ToolRoot "lib\arm64\libcmt.lib"
    $windowsSdkArm64Lib = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\Lib\*\um\arm64\kernel32.lib" -File -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -ExpandProperty FullName -First 1
    $missingArm64Files = @($arm64Link, $arm64Crt) | Where-Object { -not (Test-Path -LiteralPath $_) }
    if ($missingArm64Files.Count -gt 0 -or -not $windowsSdkArm64Lib) {
        Write-Host "ERROR: The ARM64 compiler is present, but its linker/CRT or Windows SDK ARM64 libraries are incomplete." -ForegroundColor Red
        $missingArm64Files | ForEach-Object { Write-Host "       Missing: $_" -ForegroundColor Yellow }
        if (-not $windowsSdkArm64Lib) {
            Write-Host "       Missing: Windows Kits\10\Lib\<version>\um\arm64\kernel32.lib" -ForegroundColor Yellow
        }
        Write-Host "       Repair the ARM64 build tools and install a Windows 10/11 SDK in Visual Studio Installer." -ForegroundColor Yellow
        exit 1
    }
    Write-Host "  ARM64 MSVC:    $arm64Cl" -ForegroundColor Green
    Write-Host "  ARM64 SDK:     $windowsSdkArm64Lib" -ForegroundColor Green
}

if (-not $SkipSign -and [string]::IsNullOrEmpty($SignThumbprint)) {
    Write-Host "ERROR: -SignThumbprint <SHA1> required (or pass -SkipSign for unsigned local builds)." -ForegroundColor Red
    exit 1
}

# ─── 2. Build engine for each arch ─────────────────────────────────────────
function Get-Sha256Hex($path) {
    $stream = [System.IO.File]::OpenRead($path)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $bytes = $sha.ComputeHash($stream) } finally { $sha.Dispose() }
        return ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
    } finally {
        $stream.Dispose()
    }
}

function Assert-MicrosoftSignature($path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    $subject = $signature.SignerCertificate.Subject
    if ($signature.Status -ne "Valid" -or $subject -notmatch '(^|,\s*)CN=Microsoft Corporation(,|$)') {
        throw "Prerequisite signature validation failed for $path. Status=$($signature.Status), signer='$subject'."
    }
}

function Ensure-PinnedDownload($url, $path, $expectedSha256) {
    if (Test-Path $path) {
        $existingHash = Get-Sha256Hex $path
        if ($existingHash -eq $expectedSha256) {
            Assert-MicrosoftSignature $path
            Write-Host "  Reusing SHA256 + Authenticode-verified $(Split-Path -Leaf $path)" -ForegroundColor Green
            return
        }
        Remove-Item -LiteralPath $path -Force
    }

    $tempPath = "$path.download"
    Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue
    try {
        $downloaded = $false
        for ($attempt = 1; $attempt -le 3 -and -not $downloaded; $attempt++) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tempPath
                $downloaded = $true
            } catch {
                if ($attempt -eq 3) { throw }
                Start-Sleep -Seconds 1
            }
        }

        $actualSha256 = Get-Sha256Hex $tempPath
        if ($actualSha256 -ne $expectedSha256) {
            throw "SHA256 mismatch for $url. Expected $expectedSha256, got $actualSha256."
        }
        Move-Item -LiteralPath $tempPath -Destination $path -Force
        Assert-MicrosoftSignature $path
        Write-Host "  Downloaded + SHA256 + Authenticode-verified $(Split-Path -Leaf $path)" -ForegroundColor Green
    } finally {
        Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue
    }
}

function Assert-ReleaseSourceContract {
    $productSource = Get-Content -Raw (Join-Path $PlatformDir "installer/FileID.Msi/Product.wxs")
    if ($productSource.Contains('<CustomAction Id="LaunchFileID"')) {
        throw "Product.wxs must not auto-launch FileID from the elevated MSI execute sequence."
    }
    if (-not $productSource.Contains('ARPHELPLINK" Value="https://github.com/WebWorldWide/FileID/issues"')) {
        throw "Product.wxs ARPHELPLINK must point to FileID support."
    }

    $bundleSource = Get-Content -Raw (Join-Path $PlatformDir "installer/FileID.Bundle/Bundle.wxs")
    foreach ($required in @(
        'Id="WindowsAppRuntimeX64"',
        'Id="FileIDx64"',
        'Id="FileIDArm64"',
        'LaunchTarget="[ProgramFiles64Folder]FileID\FileID.exe"'
    )) {
        if (-not $bundleSource.Contains($required)) {
            throw "Bundle.wxs is missing release contract marker: $required"
        }
    }
}

[xml]$centralPackages = Get-Content (Join-Path $PlatformDir "Directory.Packages.props")
$declaredWinAppRuntime = @($centralPackages.Project.ItemGroup.PackageVersion) |
    Where-Object { $_.Include -eq "Microsoft.WindowsAppSDK" } |
    Select-Object -ExpandProperty Version -First 1
if ($declaredWinAppRuntime -ne $WinAppRuntimeVersion) {
    throw "Windows App SDK version drift: Directory.Packages.props has '$declaredWinAppRuntime', but publish-bundle.ps1 pins runtime '$WinAppRuntimeVersion'."
}

Assert-ReleaseSourceContract
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
foreach ($staleArtifact in @("FileIDSetup.exe", "FileID-x64.msi", "FileID-arm64.msi", "SHA256SUMS.txt")) {
    Remove-Item -LiteralPath (Join-Path $DistDir $staleArtifact) -Force -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Force -Path $PrereqDir | Out-Null
Write-Host "Staging Windows App Runtime $WinAppRuntimeVersion prerequisites..." -ForegroundColor Cyan
Ensure-PinnedDownload $WinAppRuntimeX64Url (Join-Path $PrereqDir "WindowsAppRuntimeInstall-x64.exe") $WinAppRuntimeX64Sha256
if (-not $SkipArm64) {
    Ensure-PinnedDownload $WinAppRuntimeArm64Url (Join-Path $PrereqDir "WindowsAppRuntimeInstall-arm64.exe") $WinAppRuntimeArm64Sha256
}

if (-not (Test-Path -LiteralPath $FetchRuntimeScript -PathType Leaf)) {
    throw "Native runtime fetcher is missing: $FetchRuntimeScript"
}

function Resolve-NativeRuntimeDlls($architecture) {
    Write-Host "Staging pinned native runtime inputs ($architecture)..." -ForegroundColor Cyan
    $resolved = @{}
    $output = @(& $FetchRuntimeScript -Architecture $architecture)
    foreach ($line in $output) {
        if ($line -match '^RUNTIME_DLL=(.+)$') {
            $path = $Matches[1]
            $resolved[[System.IO.Path]::GetFileName($path)] = $path
        }
    }
    foreach ($required in @("onnxruntime.dll", "onnxruntime_providers_shared.dll", "DirectML.dll", "pdfium.dll")) {
        if (-not $resolved.ContainsKey($required) -or -not (Test-Path -LiteralPath $resolved[$required] -PathType Leaf)) {
            throw "fetch-runtime-deps.ps1 did not resolve required $architecture payload '$required'."
        }
    }
    if ($resolved.Count -ne 4) {
        throw "fetch-runtime-deps.ps1 returned $($resolved.Count) runtime payloads for $architecture; expected exactly four."
    }
    return $resolved
}

$RuntimeDllsByArchitecture = @{
    x64 = Resolve-NativeRuntimeDlls "x64"
}
if (-not $SkipArm64) {
    $RuntimeDllsByArchitecture.arm64 = Resolve-NativeRuntimeDlls "arm64"
}

function Build-Engine($triple) {
    Write-Host "Building engine ($triple, release)..." -ForegroundColor Cyan
    Push-Location $EngineDir
    try { & rustup run 1.90 cargo build --release --locked --target $triple } finally { Pop-Location }
}

Build-Engine "x86_64-pc-windows-msvc"
if (-not $SkipArm64) {
    Build-Engine "aarch64-pc-windows-msvc"
}

# ─── 3. Publish app for each arch ──────────────────────────────────────────
function Resolve-PublishDir($rid, $platform) {
    return Join-Path $PlatformDir "src/FileID.App/bin/$platform/Release/$AppTfm/$rid/publish"
}

function Publish-App($rid, $platform) {
    Write-Host "Publishing FileID.App ($rid)..." -ForegroundColor Cyan
    $publishDir = [System.IO.Path]::GetFullPath((Resolve-PublishDir $rid $platform))
    $appBinRoot = [System.IO.Path]::GetFullPath((Join-Path $PlatformDir "src/FileID.App/bin"))
    if (-not $publishDir.StartsWith($appBinRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean publish directory outside FileID.App/bin: $publishDir"
    }
    Remove-Item -LiteralPath $publishDir -Recurse -Force -ErrorAction SilentlyContinue
    & dotnet publish $AppCsproj `
        -c Release `
        -r $rid `
        --self-contained true `
        /p:PublishReadyToRun=true `
        -p:Platform=$platform `
        "-p:PublishDir=$publishDir" `
        -p:RestoreLockedMode=true `
        --nologo
}

Publish-App "win-x64" "x64"
if (-not $SkipArm64) {
    Publish-App "win-arm64" "arm64"
}

# ─── 4. Stage engine into each publish dir ─────────────────────────────────
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

function Get-PeMachine($path) {
    $stream = [System.IO.File]::OpenRead($path)
    try {
        $reader = [System.IO.BinaryReader]::new($stream)
        try {
            if ($reader.ReadUInt16() -ne 0x5A4D) { throw "Not a PE executable: $path" }
            $stream.Position = 0x3C
            $peOffset = $reader.ReadUInt32()
            $stream.Position = $peOffset
            if ($reader.ReadUInt32() -ne 0x00004550) { throw "Invalid PE signature: $path" }
            return $reader.ReadUInt16()
        } finally {
            $reader.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function Assert-PeMachine($path, $expectedMachine, $architecture) {
    $actualMachine = Get-PeMachine $path
    if ($actualMachine -ne $expectedMachine) {
        throw "Architecture mismatch: $path is PE machine 0x$($actualMachine.ToString('X4')), expected $architecture (0x$($expectedMachine.ToString('X4')))."
    }
}

function Stage-NativeRuntimeDlls($architecture, $rid, $platform, $expectedMachine) {
    $publishDir = Resolve-PublishDir $rid $platform
    foreach ($entry in $RuntimeDllsByArchitecture[$architecture].GetEnumerator()) {
        $destination = Join-Path $publishDir $entry.Key
        Copy-Item -LiteralPath $entry.Value -Destination $destination -Force
        Assert-PeMachine $destination $expectedMachine $architecture
    }
}

Stage-Engine "x86_64-pc-windows-msvc" "win-x64" "x64"
Stage-NativeRuntimeDlls "x64" "win-x64" "x64" 0x8664
Assert-PeMachine (Join-Path (Resolve-PublishDir "win-x64" "x64") "FileID.exe") 0x8664 "x64"
Assert-PeMachine (Join-Path (Resolve-PublishDir "win-x64" "x64") "FileIDEngine.exe") 0x8664 "x64"
if (-not $SkipArm64) {
    Stage-Engine "aarch64-pc-windows-msvc" "win-arm64" "arm64"
    Stage-NativeRuntimeDlls "arm64" "win-arm64" "arm64" 0xAA64
    Assert-PeMachine (Join-Path (Resolve-PublishDir "win-arm64" "arm64") "FileID.exe") 0xAA64 "ARM64"
    Assert-PeMachine (Join-Path (Resolve-PublishDir "win-arm64" "arm64") "FileIDEngine.exe") 0xAA64 "ARM64"
}

# ─── 5. Sign published binaries ────────────────────────────────────────────
function Sign-Binary($path) {
    if ($SkipSign) { return }
    & signtool sign /fd SHA256 /tr $TimestampServer /td SHA256 /sha1 $SignThumbprint $path | Out-Null
    # signtool's failures (expired/absent cert, timestamp-server timeout, denied
    # access) are non-fatal to the pipe unless we check $LASTEXITCODE — without
    # this the script sails on and ships an UNSIGNED bundle that trips SmartScreen
    # on every user's machine. Mirrors sign.ps1's per-target check.
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: signtool failed (exit $LASTEXITCODE) for $path — refusing to ship a partially-signed bundle." -ForegroundColor Red
        exit 1
    }
}

function Sign-PublishDir($dir) {
    if ($SkipSign) { return }
    Write-Host "Signing FileID-owned binaries under $dir..." -ForegroundColor Cyan
    foreach ($name in @("FileID.exe", "FileID.dll", "FileIDEngine.exe", "FileID.Theme.dll", "FileID.IpcSchema.dll")) {
        $path = Join-Path $dir $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "FileID-owned signing target is missing: $path"
        }
        Sign-Binary $path
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        if ($signature.Status -ne "Valid") {
            throw "Authenticode verification failed after signing $path. Status=$($signature.Status)."
        }
    }
}

Sign-PublishDir (Resolve-PublishDir "win-x64" "x64")
if (-not $SkipArm64) {
    Sign-PublishDir (Resolve-PublishDir "win-arm64" "arm64")
}

# ─── 6. Build per-arch MSIs ────────────────────────────────────────────────
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

Write-Host "Building FileID-x64.msi..." -ForegroundColor Cyan
& dotnet build $MsiProj -c Release -p:Platform=x64 -p:RestoreLockedMode=true --nologo

if (-not $SkipArm64) {
    Write-Host "Building FileID-arm64.msi..." -ForegroundColor Cyan
    & dotnet build $MsiProj -c Release -p:Platform=arm64 -p:RestoreLockedMode=true --nologo
}

# ─── 7. Sign MSIs ──────────────────────────────────────────────────────────
$MsiX64   = Join-Path $DistDir "FileID-x64.msi"
$MsiArm64 = Join-Path $DistDir "FileID-arm64.msi"
Sign-Binary $MsiX64
if (-not $SkipArm64) { Sign-Binary $MsiArm64 }

# ─── 8. Build Burn bundle ──────────────────────────────────────────────────
Write-Host "Building FileIDSetup.exe (Burn bundle)..." -ForegroundColor Cyan
$includeArm64 = (-not $SkipArm64).ToString().ToLowerInvariant()
& dotnet build $BundleProj -t:Rebuild -c Release -p:IncludeArm64=$includeArm64 -p:RestoreLockedMode=true --nologo

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

    # The bundle being Valid doesn't prove the inner MSIs are signed — a
    # half-signed set still trips SmartScreen + WinVerifyTrust after install.
    # Verify every shipped MSI too (the publish-dir exes are signed via
    # Sign-PublishDir above; the MSIs are the user-facing secondary artifacts).
    $msis = @($MsiX64)
    if (-not $SkipArm64) { $msis += $MsiArm64 }
    foreach ($msi in $msis) {
        $ms = Get-AuthenticodeSignature $msi
        if ($ms.Status -ne "Valid") {
            Write-Host "ERROR: MSI signature status is $($ms.Status) for $msi" -ForegroundColor Red
            exit 1
        }
    }
    Write-Host "  Authenticode (MSIs)   OK" -ForegroundColor Green
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
    $binaryEncoding = [System.Text.Encoding]::GetEncoding(28591)
    foreach ($d in $publishDirs) {
        $files = Get-ChildItem -Path $d -Recurse -Include *.exe, *.dll
        foreach ($f in $files) {
            $binaryText = $binaryEncoding.GetString([System.IO.File]::ReadAllBytes($f.FullName))
            foreach ($needle in $ForbiddenTelemetryStrings) {
                if ($binaryText.IndexOf($needle, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
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

$releaseArtifacts = @($BundleExe, $MsiX64)
if (-not $SkipArm64) { $releaseArtifacts += $MsiArm64 }
$checksumLines = foreach ($artifact in $releaseArtifacts) {
    "$(Get-Sha256Hex $artifact)  $(Split-Path -Leaf $artifact)"
}
$checksumPath = Join-Path $DistDir "SHA256SUMS.txt"
$checksumLines | Set-Content -LiteralPath $checksumPath -Encoding ascii
Write-Host "  SHA256 checksums      OK ($(Split-Path -Leaf $checksumPath))" -ForegroundColor Green

Write-Host ""
Write-Host "Release artifacts staged under:" -ForegroundColor Green
Write-Host "  $DistDir\FileIDSetup.exe   ← canonical user-facing download"
Write-Host "  $DistDir\FileID-x64.msi    ← for IT admins (SCCM/Intune)"
if (-not $SkipArm64) {
    Write-Host "  $DistDir\FileID-arm64.msi  ← for IT admins (Snapdragon WoA)"
}
Write-Host "  $DistDir\SHA256SUMS.txt    ← SHA256 integrity manifest"

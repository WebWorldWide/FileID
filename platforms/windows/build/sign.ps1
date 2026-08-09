# Authenticode codesigning helper.
#
# Signs every .exe and .dll under the given directory using signtool.exe
# with an EV certificate identified by SHA1 thumbprint. Used by:
#   - publish-bundle.ps1 (post-build, pre-MSI)
#   - build-all.ps1 -Sign flag
#
# Without an actual EV cert in your store, this script no-ops with a
# friendly message. Once you've purchased + installed an EV cert
# (DigiCert / SSL.com / Sectigo, ~$300/year + identity verification),
# pass the thumbprint via -Thumbprint or set FILEID_EV_THUMBPRINT in
# your shell. Then this script + the existing wixproj produce a fully
# signed MSI + Burn bundle on every build.
#
# Usage:
#   pwsh build/sign.ps1 -Path dist/x64/FileID -Thumbprint ABC123...
#   $env:FILEID_EV_THUMBPRINT = 'ABC123...'; pwsh build/sign.ps1 -Path ...

param(
    [Parameter(Mandatory=$true)]
    [string]$Path,
    [string]$Thumbprint,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [string]$Description = "FileID -- on-device AI file organizer",
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

if (-not $Thumbprint) { $Thumbprint = $env:FILEID_EV_THUMBPRINT }
if (-not $Thumbprint) {
    if (-not $Quiet) {
        Write-Host "sign.ps1: no EV thumbprint provided. Pass -Thumbprint or set FILEID_EV_THUMBPRINT." -ForegroundColor Yellow
        Write-Host "          Skipping codesigning. Build artifacts will ship as Unsigned (engine WinVerifyTrust warns + allows in dev)." -ForegroundColor DarkGray
    }
    exit 0
}

# Locate signtool.exe -- ships with the Windows SDK; vswhere can find the
# latest installed location.
$signtool = $null
$candidates = @(
    "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe",
    "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.22621.0\x64\signtool.exe",
    "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.22000.0\x64\signtool.exe",
    "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.0.19041.0\x64\signtool.exe"
)
foreach ($c in $candidates) {
    if (Test-Path $c) { $signtool = $c; break }
}
if (-not $signtool) {
    Write-Host "sign.ps1: signtool.exe not found. Install Windows 10/11 SDK." -ForegroundColor Red
    exit 1
}

if (-not (Test-Path $Path)) {
    Write-Host "sign.ps1: path '$Path' does not exist." -ForegroundColor Red
    exit 1
}

# Discover signable artifacts. .exe + .dll are the canonical Authenticode
# targets; we skip .pdb, .json, etc.
$targets = Get-ChildItem -Path $Path -Recurse -Include *.exe, *.dll -File
if ($targets.Count -eq 0) {
    Write-Host "sign.ps1: no .exe / .dll found under $Path" -ForegroundColor Yellow
    exit 0
}

if (-not $Quiet) {
    Write-Host "Signing $($targets.Count) binaries with thumbprint $Thumbprint..." -ForegroundColor Cyan
}

foreach ($target in $targets) {
    & $signtool sign `
        /fd SHA256 `
        /tr $TimestampUrl /td SHA256 `
        /sha1 $Thumbprint `
        /d $Description `
        $target.FullName | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAILED: $($target.FullName)" -ForegroundColor Red
        exit 1
    }
    if (-not $Quiet) {
        Write-Host "  signed: $($target.Name)" -ForegroundColor DarkGreen
    }
}

if (-not $Quiet) {
    Write-Host "Done -- $($targets.Count) binaries signed." -ForegroundColor Green
}

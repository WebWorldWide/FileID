# Authenticode codesigning helper.
#
# Signs every .exe and .dll under the given directory using signtool.exe
# with a certificate-store code-signing key identified by SHA1 thumbprint. Used by:
#   - publish-bundle.ps1 (post-build, pre-MSI)
#   - build-all.ps1 -Sign flag
#
# This helper is fail-closed: invoking it explicitly always signs and verifies
# each unsigned FileID payload or returns a nonzero exit. Release packaging also
# supports managed providers through publish-bundle.ps1.
#
# Usage:
#   pwsh build/sign.ps1 -Path dist/x64/FileID -Thumbprint ABC123...
#   $env:FILEID_SIGN_THUMBPRINT = 'ABC123...'; pwsh build/sign.ps1 -Path ...

param(
    [Parameter(Mandatory=$true)]
    [string]$Path,
    [string]$Thumbprint,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [string]$Description = "FileID -- on-device AI file organizer",
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

if (-not $Thumbprint) { $Thumbprint = $env:FILEID_SIGN_THUMBPRINT }
if (-not $Thumbprint) { $Thumbprint = $env:FILEID_EV_THUMBPRINT }
if (-not $Thumbprint) {
    throw "sign.ps1: explicit signing requires -Thumbprint or FILEID_SIGN_THUMBPRINT."
}
$Thumbprint = $Thumbprint.Replace(" ", "").Replace(":", "")
$signingCertificate = @(
    Get-ChildItem Cert:\CurrentUser\My, Cert:\LocalMachine\My -ErrorAction SilentlyContinue
) | Where-Object { $_.Thumbprint -eq $Thumbprint } | Select-Object -First 1
if (-not $signingCertificate) {
    throw "sign.ps1: certificate '$Thumbprint' was not found in CurrentUser or LocalMachine certificate stores."
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
    throw "sign.ps1: no .exe / .dll found under $Path"
}

if (-not $Quiet) {
    Write-Host "Signing $($targets.Count) binaries with thumbprint $Thumbprint..." -ForegroundColor Cyan
}

foreach ($target in $targets) {
    $existing = Get-AuthenticodeSignature -LiteralPath $target.FullName
    if ($existing.Status -eq "Valid") {
        $fileIdOwned = $target.BaseName -eq "FileID" `
            -or $target.BaseName -eq "FileIDEngine" `
            -or $target.BaseName.StartsWith("FileID.", [StringComparison]::OrdinalIgnoreCase)
        if ($fileIdOwned -and (
            $existing.SignerCertificate.Thumbprint -ne $Thumbprint `
            -or -not $existing.TimeStamperCertificate)) {
            throw "sign.ps1: existing FileID signature does not match the requested signer/timestamp: $($target.FullName)."
        }
        if (-not $Quiet) { Write-Host "  preserved verified signature: $($target.Name)" -ForegroundColor DarkGray }
        continue
    }
    if ($existing.Status -ne "NotSigned") {
        throw "sign.ps1: refusing to replace invalid signature on $($target.FullName). Status=$($existing.Status)."
    }
    & $signtool sign `
        /fd SHA256 `
        /tr $TimestampUrl /td SHA256 `
        /sha1 $Thumbprint `
        /d $Description `
        $target.FullName | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "sign.ps1: signing failed for $($target.FullName)."
    }
    & $signtool verify /pa /all /v $target.FullName | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "sign.ps1: signature verification failed for $($target.FullName)."
    }
    $verified = Get-AuthenticodeSignature -LiteralPath $target.FullName
    if (($verified.Status -ne "Valid") -or
        ($verified.SignerCertificate.Thumbprint -ne $Thumbprint) -or
        (-not $verified.TimeStamperCertificate)) {
        throw "sign.ps1: signer identity or trusted timestamp verification failed for $($target.FullName)."
    }
    if (-not $Quiet) {
        Write-Host "  signed + verified: $($target.Name)" -ForegroundColor DarkGreen
    }
}

if (-not $Quiet) {
    Write-Host "Done -- $($targets.Count) binaries signed." -ForegroundColor Green
}

# Stage the native runtime DLLs required beside FileIDEngine.exe for one
# architecture and print RUNTIME_DLL=<path> for each:
#
#   - onnxruntime.dll + onnxruntime_providers_shared.dll: ONNX Runtime 1.22.0
#     (DirectML + CPU) built from source without --use_telemetry by
#     build-ort-runtime.ps1, locally or by the ort-runtime workflow. Microsoft's
#     Microsoft.ML.OnnxRuntime.DirectML NuGet is a telemetry build and is never
#     used (DECISIONS 2026-11-10).
#   - DirectML.dll: NuGet Microsoft.AI.DirectML 1.15.4.
#   - pdfium.dll: GitHub bblanchon/pdfium-binaries chromium/7857 (Apache-2.0).
#
# Every remote archive and extracted DLL is SHA-256 pinned and re-verified on
# each run; stale or corrupt cache entries are discarded, not trusted.

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64',
    [string]$OrtRuntimeDir = '',
    [string]$CacheDir = ''
)

$ErrorActionPreference = 'Stop'

$OrtVersion = '1.22.0'
$DmlVersion = '1.15.4'
$PdfiumVersion = '7857'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $CacheDir = Join-Path $ScriptDir 'runtime-cache'
}
$CacheDir = [System.IO.Path]::GetFullPath($CacheDir)
if ([string]::IsNullOrWhiteSpace($OrtRuntimeDir)) {
    $OrtRuntimeDir = Join-Path $CacheDir "onnxruntime-$OrtVersion-dml-notelemetry\$Architecture"
}
$OrtRuntimeDir = [System.IO.Path]::GetFullPath($OrtRuntimeDir)
$DownloadDir = Join-Path $CacheDir 'downloads'
New-Item -ItemType Directory -Force -Path $DownloadDir | Out-Null

$DmlArchiveSha256 = '4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9'
$PdfiumArchiveSha256 = if ($Architecture -eq 'arm64') {
    '12238aba08002328fb8adc7225921771427eee1cf463cca3694beecf41e4d7c5'
} else {
    'b904e3898f952984fb744e0c8eb36512b5ee527124796108ed419a5b4da3c6d9'
}
$ExpectedDllHashes = if ($Architecture -eq 'arm64') {
    @{
        'DirectML.dll' = '77b0db83ff903f2323f5caf538499d75af6038bbea23b7959f7d232d9a4ab9d4'
        'pdfium.dll' = '4c08eb84f354a104d5c8a59795f007773d87c372f04b1026b4deeaa496d1df27'
    }
} else {
    @{
        'DirectML.dll' = '9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1'
        'pdfium.dll' = 'ebddbc781afbffb6f76c8e674e5900665a8676e778a91c4130b9afcb4a8a812a'
    }
}

# ONNX Runtime's ETW provider metadata when it joins Microsoft's telemetry
# provider group {4f50731a-89cf-4782-b3e0-dce8c90476ba}: only --use_telemetry
# builds carry it. Same check as shared/scripts/check_binary_privacy.py.
$OrtTelemetryProvider = [byte[]]([System.Text.Encoding]::ASCII.GetBytes('Microsoft.ML.ONNXRuntime') + [byte[]](
    0x00, 0x13, 0x00, 0x01,
    0x1a, 0x73, 0x50, 0x4f, 0xcf, 0x89, 0x82, 0x47, 0xb3, 0xe0, 0xdc, 0xe8, 0xc9, 0x04, 0x76, 0xba))

function Get-Sha256 {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = $hasher.ComputeHash($stream)
        return ([System.BitConverter]::ToString($bytes)).Replace('-', '').ToLowerInvariant()
    } finally {
        $hasher.Dispose()
        $stream.Dispose()
    }
}

function Assert-Sha256 {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected,
        [Parameter(Mandatory)][string]$Label
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "missing $Label at $Path"
    }
    $actual = Get-Sha256 -Path $Path
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "SHA256 mismatch for $Label ($Path): expected $Expected, got $actual"
    }
}

function Get-PinnedArchive {
    param(
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$ExpectedSha256
    )
    if (Test-Path -LiteralPath $Destination -PathType Leaf) {
        try {
            Assert-Sha256 -Path $Destination -Expected $ExpectedSha256 -Label 'cached runtime archive'
            return
        } catch {
            Write-Host "  discarding invalid cached archive: $Destination" -ForegroundColor Yellow
            Remove-Item -LiteralPath $Destination -Force
        }
    }

    $temporary = "$Destination.tmp-$PID"
    Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Write-Host "  fetching $Url (attempt $attempt/3)..." -ForegroundColor DarkGray
            Invoke-WebRequest -Uri $Url -OutFile $temporary -UseBasicParsing
            Assert-Sha256 -Path $temporary -Expected $ExpectedSha256 -Label $Url
            Move-Item -LiteralPath $temporary -Destination $Destination -Force
            return
        } catch {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            if ($attempt -eq 3) { throw }
            Start-Sleep -Seconds $attempt
        }
    }
}

function Expand-PinnedArchive {
    param(
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$ArchiveName,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [Parameter(Mandatory)][string]$Extract,
        [switch]$Tar
    )
    $archive = Join-Path $DownloadDir $ArchiveName
    Get-PinnedArchive -Url $Url -Destination $archive -ExpectedSha256 $ExpectedSha256

    $marker = Join-Path $Extract '.archive-sha256'
    $markerValue = if (Test-Path -LiteralPath $marker -PathType Leaf) {
        (Get-Content -LiteralPath $marker -Raw).Trim()
    } else {
        ''
    }
    if ($markerValue -ne $ExpectedSha256) {
        if (Test-Path -LiteralPath $Extract) {
            Remove-Item -LiteralPath $Extract -Recurse -Force
        }
        New-Item -ItemType Directory -Force -Path $Extract | Out-Null
        if ($Tar) {
            & tar -xzf $archive -C $Extract
            if ($LASTEXITCODE -ne 0) {
                throw "tar extraction of $archive failed with exit code $LASTEXITCODE"
            }
        } else {
            Add-Type -AssemblyName System.IO.Compression.FileSystem
            [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $Extract)
        }
        Set-Content -LiteralPath $marker -Value $ExpectedSha256 -Encoding ascii -NoNewline
    }
}

function Get-PeMachine {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $reader = New-Object System.IO.BinaryReader($stream)
        $stream.Position = 0x3C
        $stream.Position = $reader.ReadUInt32() + 4
        return $reader.ReadUInt16()
    } finally {
        $stream.Dispose()
    }
}

function Assert-TelemetryFreeOrt {
    param([Parameter(Mandatory)][string]$Path)
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $text = $latin1.GetString([System.IO.File]::ReadAllBytes($Path))
    if ($text.Contains($latin1.GetString($OrtTelemetryProvider))) {
        throw "$Path is a Microsoft ONNX Runtime build with Windows telemetry (--use_telemetry). FileID ships only the build from build-ort-runtime.ps1."
    }
}

$OrtArtifacts = [ordered]@{
    'onnxruntime.dll' = Join-Path $OrtRuntimeDir 'onnxruntime.dll'
    'onnxruntime_providers_shared.dll' = Join-Path $OrtRuntimeDir 'onnxruntime_providers_shared.dll'
}
foreach ($entry in $OrtArtifacts.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) {
        throw ("No telemetry-free ONNX Runtime $OrtVersion for $Architecture at $OrtRuntimeDir ($($entry.Key) missing). " +
            "Build it with build-ort-runtime.ps1 -Architecture $Architecture, or download the " +
            "onnxruntime-dml-notelemetry-$Architecture artifact of the 'ONNX Runtime (no telemetry)' workflow into that folder.")
    }
}
$ExpectedMachine = if ($Architecture -eq 'arm64') { 0xAA64 } else { 0x8664 }
foreach ($entry in $OrtArtifacts.GetEnumerator()) {
    if ((Get-PeMachine -Path $entry.Value) -ne $ExpectedMachine) {
        throw "$($entry.Value) is not a $Architecture binary."
    }
    Assert-TelemetryFreeOrt -Path $entry.Value
}
$OrtFileVersion = (Get-Item -LiteralPath $OrtArtifacts['onnxruntime.dll']).VersionInfo.FileVersion
if ($OrtFileVersion -notlike '1.22.*') {
    throw "$($OrtArtifacts['onnxruntime.dll']) reports version '$OrtFileVersion'; the ort crate needs 1.22.x."
}

$DmlExtract = Join-Path $CacheDir "directml-$DmlVersion"
$PdfiumExtract = Join-Path $CacheDir "pdfium-$PdfiumVersion-$Architecture"
Expand-PinnedArchive `
    -Url "https://www.nuget.org/api/v2/package/Microsoft.AI.DirectML/$DmlVersion" `
    -ArchiveName "Microsoft.AI.DirectML.$DmlVersion.nupkg" `
    -ExpectedSha256 $DmlArchiveSha256 `
    -Extract $DmlExtract
Expand-PinnedArchive `
    -Url "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$PdfiumVersion/pdfium-win-$Architecture.tgz" `
    -ArchiveName "pdfium-win-$Architecture-$PdfiumVersion.tgz" `
    -ExpectedSha256 $PdfiumArchiveSha256 `
    -Extract $PdfiumExtract `
    -Tar

$DmlPlatform = if ($Architecture -eq 'arm64') { 'arm64-win' } else { 'x64-win' }
$PinnedArtifacts = [ordered]@{
    'DirectML.dll' = Join-Path $DmlExtract "bin\$DmlPlatform\DirectML.dll"
    'pdfium.dll' = Join-Path $PdfiumExtract 'bin\pdfium.dll'
}
foreach ($entry in $PinnedArtifacts.GetEnumerator()) {
    Assert-Sha256 -Path $entry.Value -Expected $ExpectedDllHashes[$entry.Key] -Label $entry.Key
}

foreach ($entry in $OrtArtifacts.GetEnumerator()) {
    Write-Output "RUNTIME_DLL=$($entry.Value)"
}
foreach ($entry in $PinnedArtifacts.GetEnumerator()) {
    Write-Output "RUNTIME_DLL=$($entry.Value)"
}

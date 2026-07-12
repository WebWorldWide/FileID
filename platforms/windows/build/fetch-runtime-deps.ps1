# Fetch the native runtime DLLs required beside FileIDEngine.exe.
#
# Every remote archive is version- and SHA256-pinned. Cached archives and the
# selected extracted DLLs are reverified on every invocation; stale/corrupt
# cache entries are discarded rather than trusted through a marker file.

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64',
    [string]$OrtVersion = '1.22.0',
    [string]$DmlVersion = '1.15.4',
    [string]$PdfiumVersion = '7857',
    [string]$CacheDir = ''
)

$ErrorActionPreference = 'Stop'

if ($OrtVersion -ne '1.22.0' -or $DmlVersion -ne '1.15.4' -or $PdfiumVersion -ne '7857') {
    throw 'runtime version overrides require adding reviewed SHA256 pins to fetch-runtime-deps.ps1'
}

if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $CacheDir = Join-Path $ScriptDir 'runtime-cache'
}
$CacheDir = [System.IO.Path]::GetFullPath($CacheDir)
$DownloadDir = Join-Path $CacheDir 'downloads'
New-Item -ItemType Directory -Force -Path $DownloadDir | Out-Null

$OrtArchiveSha256 = '29f9872d786236b79aa83f94482f3a17c14297e4833768d6d0ed4883ee732e60'
$DmlArchiveSha256 = '4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9'
$PdfiumArchiveSha256 = if ($Architecture -eq 'arm64') {
    '12238aba08002328fb8adc7225921771427eee1cf463cca3694beecf41e4d7c5'
} else {
    'b904e3898f952984fb744e0c8eb36512b5ee527124796108ed419a5b4da3c6d9'
}

$ExpectedDllHashes = if ($Architecture -eq 'arm64') {
    @{
        'onnxruntime.dll' = 'c544001fbb76c7217fce76e9c24dada4a35d263b7ae3c0024474d9d7323a888f'
        'onnxruntime_providers_shared.dll' = 'f89d81ba9d248629cd8c044fa8d8212b1278b9e0aeaa2670a06af9f874cf412a'
        'DirectML.dll' = '77b0db83ff903f2323f5caf538499d75af6038bbea23b7959f7d232d9a4ab9d4'
        'pdfium.dll' = '4c08eb84f354a104d5c8a59795f007773d87c372f04b1026b4deeaa496d1df27'
    }
} else {
    @{
        'onnxruntime.dll' = '95366724919f4e95ecc60010912ed538ad9804b6683fbd0aad389749102834b9'
        'onnxruntime_providers_shared.dll' = 'dea79756b1ef0deb317115aa5da45eca8946eafcf1be2dbd9fa3b309551faae5'
        'DirectML.dll' = '9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1'
        'pdfium.dll' = 'ebddbc781afbffb6f76c8e674e5900665a8676e778a91c4130b9afcb4a8a812a'
    }
}

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

function Expand-PinnedNuGet {
    param(
        [Parameter(Mandatory)][string]$Package,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [Parameter(Mandatory)][string]$Extract
    )
    $archive = Join-Path $DownloadDir "$Package.$Version.nupkg"
    $url = "https://www.nuget.org/api/v2/package/$Package/$Version"
    Get-PinnedArchive -Url $url -Destination $archive -ExpectedSha256 $ExpectedSha256

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
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $Extract)
        Set-Content -LiteralPath $marker -Value $ExpectedSha256 -Encoding ascii -NoNewline
    }
}

function Expand-PinnedPdfium {
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [Parameter(Mandatory)][string]$Extract
    )
    $archive = Join-Path $DownloadDir "pdfium-win-$Architecture-$Version.tgz"
    $url = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$Version/pdfium-win-$Architecture.tgz"
    Get-PinnedArchive -Url $url -Destination $archive -ExpectedSha256 $ExpectedSha256

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
        & tar -xzf $archive -C $Extract
        if ($LASTEXITCODE -ne 0) {
            throw "pdfium tar extraction failed with exit code $LASTEXITCODE"
        }
        Set-Content -LiteralPath $marker -Value $ExpectedSha256 -Encoding ascii -NoNewline
    }
}

$OrtExtract = Join-Path $CacheDir "ort-directml-$OrtVersion"
$DmlExtract = Join-Path $CacheDir "directml-$DmlVersion"
$PdfiumExtract = Join-Path $CacheDir "pdfium-$PdfiumVersion-$Architecture"

Expand-PinnedNuGet -Package 'Microsoft.ML.OnnxRuntime.DirectML' -Version $OrtVersion -ExpectedSha256 $OrtArchiveSha256 -Extract $OrtExtract
Expand-PinnedNuGet -Package 'Microsoft.AI.DirectML' -Version $DmlVersion -ExpectedSha256 $DmlArchiveSha256 -Extract $DmlExtract
Expand-PinnedPdfium -Version $PdfiumVersion -ExpectedSha256 $PdfiumArchiveSha256 -Extract $PdfiumExtract

$DmlPlatform = if ($Architecture -eq 'arm64') { 'arm64-win' } else { 'x64-win' }
$Artifacts = [ordered]@{
    'onnxruntime.dll' = Join-Path $OrtExtract "runtimes\win-$Architecture\native\onnxruntime.dll"
    'onnxruntime_providers_shared.dll' = Join-Path $OrtExtract "runtimes\win-$Architecture\native\onnxruntime_providers_shared.dll"
    'DirectML.dll' = Join-Path $DmlExtract "bin\$DmlPlatform\DirectML.dll"
    'pdfium.dll' = Join-Path $PdfiumExtract 'bin\pdfium.dll'
}

foreach ($entry in $Artifacts.GetEnumerator()) {
    Assert-Sha256 -Path $entry.Value -Expected $ExpectedDllHashes[$entry.Key] -Label $entry.Key
}

foreach ($entry in $Artifacts.GetEnumerator()) {
    Write-Output "RUNTIME_DLL=$($entry.Value)"
}

# Stage the native runtime DLLs required beside FileIDEngine.exe for one
# architecture and print RUNTIME_DLL=<path> for each:
#
#   - pdfium.dll: GitHub bblanchon/pdfium-binaries chromium/7857 (Apache-2.0).
#
# Every remote archive and extracted DLL is SHA-256 pinned and re-verified on
# each run; stale or corrupt cache entries are discarded, not trusted.

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64',
    [string]$CacheDir = ''
)

$ErrorActionPreference = 'Stop'

$PdfiumVersion = '7857'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $CacheDir = Join-Path $ScriptDir 'runtime-cache'
}
$CacheDir = [System.IO.Path]::GetFullPath($CacheDir)
$DownloadDir = Join-Path $CacheDir 'downloads'
New-Item -ItemType Directory -Force -Path $DownloadDir | Out-Null

$PdfiumArchiveSha256 = if ($Architecture -eq 'arm64') {
    '12238aba08002328fb8adc7225921771427eee1cf463cca3694beecf41e4d7c5'
} else {
    'b904e3898f952984fb744e0c8eb36512b5ee527124796108ed419a5b4da3c6d9'
}
$ExpectedDllHashes = if ($Architecture -eq 'arm64') {
    @{
        'pdfium.dll' = '4c08eb84f354a104d5c8a59795f007773d87c372f04b1026b4deeaa496d1df27'
    }
} else {
    @{
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

$PdfiumExtract = Join-Path $CacheDir "pdfium-$PdfiumVersion-$Architecture"
Expand-PinnedArchive `
    -Url "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$PdfiumVersion/pdfium-win-$Architecture.tgz" `
    -ArchiveName "pdfium-win-$Architecture-$PdfiumVersion.tgz" `
    -ExpectedSha256 $PdfiumArchiveSha256 `
    -Extract $PdfiumExtract `
    -Tar

$PinnedArtifacts = [ordered]@{
    'pdfium.dll' = Join-Path $PdfiumExtract 'bin\pdfium.dll'
}
foreach ($entry in $PinnedArtifacts.GetEnumerator()) {
    Assert-Sha256 -Path $entry.Value -Expected $ExpectedDllHashes[$entry.Key] -Label $entry.Key
}

foreach ($entry in $PinnedArtifacts.GetEnumerator()) {
    Write-Output "RUNTIME_DLL=$($entry.Value)"
}

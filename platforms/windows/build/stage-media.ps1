param(
    [Parameter(Mandatory = $true)][string]$SourceDir,
    [Parameter(Mandatory = $true)][string]$DestinationRoot
)

$ErrorActionPreference = 'Stop'
$source = [IO.Path]::GetFullPath($SourceDir)
$destination = [IO.Path]::GetFullPath((Join-Path $DestinationRoot 'media'))
if (-not [IO.Path]::IsPathRooted($SourceDir) -or
    [IO.Path]::GetPathRoot($SourceDir) -cne [IO.Path]::GetPathRoot($source) -or
    -not (Test-Path -LiteralPath $source -PathType Container)) {
    throw 'Media source must be an existing absolute directory.'
}
if ((Get-Item -LiteralPath $source).Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'Media source must be a regular directory.'
}
if ($env:CI_RELEASE -eq 'true') {
    throw 'Portable media distribution still requires codec patent and signed-package clearance.'
}
if (-not (Test-Path -LiteralPath $DestinationRoot -PathType Container)) {
    throw 'Media destination root must already exist.'
}

$required = @('ffmpeg.exe', 'ffprobe.exe', 'SHA256SUMS', 'FFmpeg-LGPL-2.1.txt',
    'OpenH264-BSD-2-Clause.txt', 'BUILD-AND-SOURCE.txt', 'ffmpeg-9.0.2.tar.xz',
    'openh264-2.6.0.tar.gz')
$allowed = $required + @('ffmpeg-9.0.2.tar.xz.asc', 'avcodec-63.dll', 'avdevice-63.dll',
    'avfilter-12.dll', 'avformat-63.dll', 'avutil-61.dll', 'libopenh264.dll',
    'swresample-7.dll', 'swscale-10.dll')
foreach ($entry in Get-ChildItem -LiteralPath $source -Force) {
    if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint -or $entry.PSIsContainer) {
        throw "Media runtime cannot contain links or subdirectories: $($entry.Name)"
    }
    if ($allowed -cnotcontains $entry.Name) {
        throw "Media runtime contains an unexpected file: $($entry.Name)"
    }
}
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $source $name) -PathType Leaf)) {
        throw "Media runtime is missing $name."
    }
}
$manifest = Get-Content -LiteralPath (Join-Path $source 'SHA256SUMS')
if ($manifest.Count -lt 4) { throw 'Media checksum manifest is incomplete.' }
$listed = @()
$expected = @{}
foreach ($line in $manifest) {
    if ($line -cnotmatch '^([0-9a-f]{64})  (?:\./)?([A-Za-z0-9._-]+)$') {
        throw "Invalid media checksum entry: $line"
    }
    $file = Get-Item -LiteralPath (Join-Path $source $Matches[2])
    if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Media file must be regular: $($Matches[2])"
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash
    if ($actual -cne $Matches[1].ToUpperInvariant()) {
        throw "Media checksum mismatch: $($Matches[2])"
    }
    $listed += $Matches[2]
    $expected[$Matches[2]] = $Matches[1].ToUpperInvariant()
}
foreach ($name in @('ffmpeg.exe', 'ffprobe.exe', 'ffmpeg-9.0.2.tar.xz', 'openh264-2.6.0.tar.gz')) {
    if ($listed -cnotcontains $name) {
        throw "Media checksum manifest is missing $name"
    }
}
foreach ($binary in Get-ChildItem -LiteralPath $source -File | Where-Object { $_.Extension -in @('.exe', '.dll') }) {
    if ($listed -cnotcontains $binary.Name) {
        throw "Media binary is not checksummed: $($binary.Name)"
    }
}
foreach ($binary in @('ffmpeg.exe', 'ffprobe.exe')) {
    $path = Join-Path $source $binary
    if ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Media executable must be a regular file: $binary"
    }
    $version = & $path -version 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or $version -notmatch 'version 9\.0\.2' -or
        $version -notmatch '--enable-libopenh264' -or $version -notmatch '--disable-network' -or
        $version -match '--enable-gpl|--enable-nonfree') {
        throw "Media runtime does not match the approved LGPL configuration: $binary"
    }
}

if (Test-Path -LiteralPath $destination) {
    throw "Media destination already exists: $destination"
}
Copy-Item -LiteralPath $source -Destination $destination -Recurse
foreach ($name in $expected.Keys) {
    $staged = Join-Path $destination $name
    if ((Get-Item -LiteralPath $staged).Attributes -band [IO.FileAttributes]::ReparsePoint -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $staged).Hash -cne $expected[$name]) {
        throw "Staged media checksum mismatch: $name"
    }
}
Write-Host "Staged vetted development media runtime in $destination"

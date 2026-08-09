[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Corpus,
    [Parameter(Mandatory = $true)][string]$Dest,
    [int]$PerExtension = 1,
    [int]$KeyPerExtension = 3,
    [int]$MaxFileMB = 256,
    [int]$MaxTotalMB = 8192
)

$ErrorActionPreference = 'Stop'

function Get-Sha256([string]$Path) {
    $stream = [System.IO.File]::OpenRead($Path)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

if (-not (Test-Path -LiteralPath $Corpus -PathType Container)) {
    throw "Corpus not found: $Corpus"
}
if (Test-Path -LiteralPath $Dest) {
    throw "Destination already exists: $Dest"
}
if ($PerExtension -lt 1 -or $KeyPerExtension -lt 1 -or $MaxFileMB -lt 1 -or $MaxTotalMB -lt 1) {
    throw 'Sampling bounds must all be positive.'
}

$keyExtensions = [System.Collections.Generic.HashSet[string]]::new(
    [string[]]@(
        '.jpg', '.jpeg', '.png', '.gif', '.bmp', '.tif', '.tiff', '.heic', '.heif', '.webp',
        '.mov', '.mp4', '.mpg', '.mpeg', '.avi', '.wmv', '.mkv',
        '.pdf', '.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx', '.txt', '.rtf', '.csv',
        '.wav', '.mp3', '.m4a', '.flac', '.ogg', '.obj'
    ),
    [System.StringComparer]::OrdinalIgnoreCase
)
$maxFileBytes = [int64]$MaxFileMB * 1MB
$maxTotalBytes = [int64]$MaxTotalMB * 1MB
$files = @(Get-ChildItem -LiteralPath $Corpus -Recurse -File -Force -ErrorAction SilentlyContinue)
if ($files.Count -eq 0) {
    throw "No files found under $Corpus"
}

$selected = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
foreach ($group in ($files | Group-Object { $_.Extension.ToLowerInvariant() } | Sort-Object Name)) {
    $eligible = @($group.Group | Where-Object { $_.Length -le $maxFileBytes } |
        Sort-Object Length, FullName)
    if ($eligible.Count -eq 0) {
        continue
    }
    $take = if ($keyExtensions.Contains($group.Name)) { $KeyPerExtension } else { $PerExtension }
    $take = [Math]::Min($take, $eligible.Count)
    for ($i = 0; $i -lt $take; $i++) {
        $index = if ($take -eq 1) {
            [int][Math]::Floor(($eligible.Count - 1) / 2)
        } else {
            [int][Math]::Round($i * ($eligible.Count - 1) / ($take - 1))
        }
        $candidate = $eligible[$index]
        if (-not ($selected | Where-Object FullName -EQ $candidate.FullName)) {
            $selected.Add($candidate)
        }
    }
}

$runningBytes = 0L
$bounded = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
foreach ($file in ($selected | Sort-Object Extension, Length, FullName)) {
    if ($runningBytes + $file.Length -gt $maxTotalBytes) {
        continue
    }
    $bounded.Add($file)
    $runningBytes += $file.Length
}
if ($bounded.Count -eq 0) {
    throw 'No files fit within the requested total-size budget.'
}

New-Item -ItemType Directory -Path $Dest | Out-Null
$manifest = [System.Collections.Generic.List[object]]::new()
$index = 0
foreach ($file in $bounded) {
    $index++
    $extension = if ([string]::IsNullOrEmpty($file.Extension)) { '_none' } else { $file.Extension.TrimStart('.').ToLowerInvariant() }
    $bucket = Join-Path $Dest $extension
    New-Item -ItemType Directory -Force -Path $bucket | Out-Null
    $target = Join-Path $bucket ("{0:D4}_{1}" -f $index, $file.Name)
    Copy-Item -LiteralPath $file.FullName -Destination $target
    $manifest.Add([pscustomobject]@{
        source = $file.FullName
        relativeDestination = $target.Substring($Dest.TrimEnd('\').Length + 1)
        extension = $file.Extension.ToLowerInvariant()
        sizeBytes = $file.Length
        sha256 = Get-Sha256 $target
    })
}

$manifestPath = Join-Path $Dest 'manifest.json'
$manifest | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath $manifestPath -Encoding utf8
[pscustomobject]@{
    sourceFiles = $files.Count
    selectedFiles = $manifest.Count
    selectedExtensions = @($manifest.extension | Sort-Object -Unique).Count
    selectedBytes = $runningBytes
    destination = $Dest
    manifest = $manifestPath
}

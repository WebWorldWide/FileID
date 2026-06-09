<#
.SYNOPSIS
  Copy N random images from a corpus into a fixed sample folder for RAM++ tag tuning.

.DESCRIPTION
  Builds a small, stable sample so tag quality can be compared apples-to-apples
  across tuning iterations: run this ONCE to populate the sample, then re-run
  `iterate.ps1 -Corpus <sample> -SkipBuild` repeatedly while editing
  `ram_plus_suppress.txt` / FILEID_RAMPLUS_PRECISION_FLOOR, diffing tag_report.py.

.EXAMPLE
  .\sample_corpus.ps1 -Corpus 'G:\TrueNAS\photos' -Count 100
  .\sample_corpus.ps1 -Corpus 'G:\TrueNAS\photos' -Count 100 -Seed 42 -Clean
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Corpus,
    [int]$Count = 100,
    [string]$Dest = (Join-Path $env:TEMP 'fileid_ram_sample'),
    [int]$Seed = 0,            # 0 = nondeterministic; non-zero = reproducible pick
    [switch]$Clean             # wipe $Dest before copying
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Corpus)) {
    throw "Corpus not found: $Corpus"
}

$exts = @('.jpg', '.jpeg', '.png', '.heic', '.heif', '.webp', '.bmp', '.gif', '.tif', '.tiff')

Write-Host "Enumerating images under $Corpus ..."
$files = Get-ChildItem -LiteralPath $Corpus -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $exts -contains $_.Extension.ToLowerInvariant() }

if (-not $files -or @($files).Count -eq 0) {
    throw "No images found under $Corpus"
}
Write-Host "Found $(@($files).Count) images."

if ($Seed -ne 0) { Get-Random -SetSeed $Seed | Out-Null }
$take = [Math]::Min($Count, @($files).Count)
$picked = Get-Random -InputObject $files -Count $take

if ($Clean -and (Test-Path -LiteralPath $Dest)) {
    Remove-Item -LiteralPath $Dest -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $Dest | Out-Null

$i = 0
foreach ($f in $picked) {
    $i++
    # Flatten with an index prefix so same-named files from different folders don't collide.
    $target = Join-Path $Dest ("{0:D4}_{1}" -f $i, $f.Name)
    Copy-Item -LiteralPath $f.FullName -Destination $target -Force
}

Write-Host "Copied $take images -> $Dest"
if ($Seed -ne 0) { Write-Host "(deterministic; seed=$Seed)" }
Write-Host ""
Write-Host "Next:"
Write-Host "  .\iterate.ps1 -Corpus '$Dest'             # full build + scan"
Write-Host "  python .\tag_report.py                    # inspect tag quality"
Write-Host "  # edit ram_plus_suppress.txt / set FILEID_RAMPLUS_PRECISION_FLOOR, then:"
Write-Host "  .\iterate.ps1 -Corpus '$Dest' -SkipBuild  # rescan only, no rebuild"

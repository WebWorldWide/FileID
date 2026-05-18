# FileID — synthetic test-corpus generator.
#
# Generates a deterministic mix of small files for the GUI regression
# harness. Default 50K files in five types: 60% JPG, 20% PNG, 10% PDF,
# 5% TXT, 5% DOCX. Layout: $OutDir/AA/BB/file_NNNNN.ext where AA/BB are
# the first two pairs of the file's index in base-26 (gives ~2K
# leaf folders for 50K files, stress-tests directory walking).
#
# This is NOT a substitute for a real photo library — faces, EXIF, and
# image content are synthetic — but it exercises the same hot paths
# (path discovery, dHash, MIME sniff, MB throughput) that crash the app.
#
# Usage:
#   pwsh build/gen-corpus.ps1 -Count 50000 -OutDir C:\Temp\FileIDCorpus
#   pwsh build/gen-corpus.ps1 -Count 1000 -OutDir C:\Temp\Tiny  # quick test

param(
    [int]$Count = 50000,
    [Parameter(Mandatory=$true)][string]$OutDir,
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

if ($Clean -and (Test-Path $OutDir)) {
    Write-Host "Cleaning $OutDir" -ForegroundColor Yellow
    Remove-Item -LiteralPath $OutDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# Mix percentages (must sum to 100).
$mix = @(
    @{ ext='.jpg';  pct=60 },
    @{ ext='.png';  pct=20 },
    @{ ext='.pdf';  pct=10 },
    @{ ext='.txt';  pct=5  },
    @{ ext='.docx'; pct=5  }
)

function Get-LeafDir([int]$idx) {
    # 50K → 2 levels of base-26 directory (676 first level × 676 second = 456,976 slots)
    $first  = [char]([byte][char]'A' + ($idx / 676) % 26)
    $second = [char]([byte][char]'A' + ($idx / 26)  % 26)
    return Join-Path $OutDir "$first$second"
}

function Write-Jpg([string]$path, [int]$idx) {
    $w = 64 + ($idx % 96)
    $h = 64 + (($idx * 7) % 96)
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $hue = ($idx * 17) % 360
        # cheap deterministic color from index
        $r = ($idx * 13) % 256
        $gn = ($idx * 29) % 256
        $b = ($idx * 41) % 256
        $brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, $r, $gn, $b))
        $g.FillRectangle($brush, 0, 0, $w, $h)
        $txt = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
        $g.DrawString("$idx", (New-Object System.Drawing.Font('Arial', 10)), $txt, 2, 2)
        $brush.Dispose(); $txt.Dispose()
        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Jpeg)
    } finally {
        $g.Dispose(); $bmp.Dispose()
    }
}

function Write-Png([string]$path, [int]$idx) {
    $w = 48 + ($idx % 80)
    $h = 48 + (($idx * 11) % 80)
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $r = ($idx * 7) % 256
        $gn = ($idx * 23) % 256
        $b = ($idx * 53) % 256
        $brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, $r, $gn, $b))
        $g.FillEllipse($brush, 0, 0, $w, $h)
        $brush.Dispose()
        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $g.Dispose(); $bmp.Dispose()
    }
}

# Minimal valid PDF: 1 page, no fonts. Spec-correct enough for shell preview.
function Write-Pdf([string]$path, [int]$idx) {
    $content = "BT /F1 12 Tf 50 700 Td (File $idx) Tj ET"
    $clen = $content.Length
    $body = @"
%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >> endobj
4 0 obj << /Length $clen >> stream
$content
endstream endobj
xref
0 5
0000000000 65535 f
0000000009 00000 n
0000000056 00000 n
0000000103 00000 n
0000000174 00000 n
trailer << /Size 5 /Root 1 0 R >>
startxref
$($body.Length)
%%EOF
"@
    [System.IO.File]::WriteAllText($path, $body, [System.Text.Encoding]::ASCII)
}

function Write-Txt([string]$path, [int]$idx) {
    $body = "FileID synthetic corpus file #$idx`r`nGenerated for GUI regression testing.`r`n"
    [System.IO.File]::WriteAllText($path, $body, [System.Text.Encoding]::UTF8)
}

# Minimal valid .docx — Office Open XML zip with 4 required parts.
function Write-Docx([string]$path, [int]$idx) {
    $tempDir = Join-Path $env:TEMP "filed-docx-$([Guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
    try {
        New-Item -ItemType Directory -Force -Path (Join-Path $tempDir "_rels") | Out-Null
        New-Item -ItemType Directory -Force -Path (Join-Path $tempDir "word") | Out-Null
        $ct = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'
        $rels = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'
        $doc = "<?xml version=`"1.0`" encoding=`"UTF-8`" standalone=`"yes`"?><w:document xmlns:w=`"http://schemas.openxmlformats.org/wordprocessingml/2006/main`"><w:body><w:p><w:r><w:t>FileID synthetic corpus #$idx</w:t></w:r></w:p></w:body></w:document>"
        [System.IO.File]::WriteAllText((Join-Path $tempDir "[Content_Types].xml"), $ct, [System.Text.Encoding]::UTF8)
        [System.IO.File]::WriteAllText((Join-Path $tempDir "_rels\.rels"), $rels, [System.Text.Encoding]::UTF8)
        [System.IO.File]::WriteAllText((Join-Path $tempDir "word\document.xml"), $doc, [System.Text.Encoding]::UTF8)
        if (Test-Path $path) { Remove-Item -LiteralPath $path -Force }
        [System.IO.Compression.ZipFile]::CreateFromDirectory($tempDir, $path)
    } finally {
        if (Test-Path $tempDir) { Remove-Item -LiteralPath $tempDir -Recurse -Force }
    }
}

# Build the extension picker by expanding each entry's pct into a flat array.
$picker = @()
foreach ($entry in $mix) { 1..$entry.pct | ForEach-Object { $picker += $entry.ext } }

Write-Host "Generating $Count files into $OutDir" -ForegroundColor Cyan
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$lastReport = 0
for ($i = 0; $i -lt $Count; $i++) {
    $leaf = Get-LeafDir $i
    if (-not (Test-Path $leaf)) { New-Item -ItemType Directory -Force -Path $leaf | Out-Null }
    $ext = $picker[$i % 100]
    $name = "file_{0:D5}{1}" -f $i, $ext
    $path = Join-Path $leaf $name
    if (Test-Path $path) { continue }
    switch ($ext) {
        '.jpg'  { Write-Jpg  $path $i }
        '.png'  { Write-Png  $path $i }
        '.pdf'  { Write-Pdf  $path $i }
        '.txt'  { Write-Txt  $path $i }
        '.docx' { Write-Docx $path $i }
    }
    if (($i - $lastReport) -ge 1000) {
        $rate = [math]::Round($i / [math]::Max(1, $sw.Elapsed.TotalSeconds), 1)
        Write-Host ("  {0,7:N0} / {1,-7:N0} ({2} files/s)" -f $i, $Count, $rate)
        $lastReport = $i
    }
}
$sw.Stop()
Write-Host ("Done: {0} files in {1:N1}s ({2:N0} files/s)" -f $Count, $sw.Elapsed.TotalSeconds, ($Count / [math]::Max(1, $sw.Elapsed.TotalSeconds))) -ForegroundColor Green

# FileID -- icon asset generator.
#
# One-shot Windows-side asset pipeline:
#   shared/docs/assets/FileID-Windows.png  (master, Windows-specific design)
#     |
#     +--> platforms/windows/src/FileID.App/Assets/FileID.ico  (multi-res .ico)
#     +--> platforms/windows/src/FileID.App/Assets/Logo/FileID-{16,96,256}.png
#     +--> platforms/windows/installer/FileID.Bundle/theme/logo.png  (130x102 letterboxed)
#
# Why FileID-Windows.png and not FileID-AppIcon.png? FileID-AppIcon.png
# is the macOS-style squircle icon (rounded-square with iridescent border)
# meant for the Dock; FileID-Windows.png is the Windows-style design with
# its own framing. Use the Windows master so the .exe icon, taskbar, and
# Welcome hero look native on Windows.
#
# Run once per master change. Generated assets are committed; end users
# do not need to run this script. Idempotent: re-running with no master
# change is a no-op (compares mtimes).
#
# Tooling: System.Drawing.Common (built-in on .NET / .NET Framework 4.x
# under Windows PowerShell 5.1). Zero external deps.

param(
    [switch]$Force    # regenerate even if mtimes say up-to-date
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot    = Resolve-Path (Join-Path $ScriptDir "..\..\..")
$Master      = Join-Path $RepoRoot "shared\docs\assets\FileID-Windows.png"
$AssetsDir   = Join-Path $RepoRoot "platforms\windows\src\FileID.App\Assets"
$LogoDir     = Join-Path $AssetsDir "Logo"
$BundleTheme = Join-Path $RepoRoot "platforms\windows\installer\FileID.Bundle\theme"
$MsiTheme    = Join-Path $RepoRoot "platforms\windows\installer\FileID.Msi\theme"

if (-not (Test-Path $Master)) {
    Write-Host "ERROR: master not found at $Master" -ForegroundColor Red
    exit 1
}

New-Item -ItemType Directory -Force -Path $AssetsDir   | Out-Null
New-Item -ItemType Directory -Force -Path $LogoDir     | Out-Null
New-Item -ItemType Directory -Force -Path $BundleTheme | Out-Null
New-Item -ItemType Directory -Force -Path $MsiTheme    | Out-Null

$IcoOut         = Join-Path $AssetsDir "FileID.ico"
$Logo16         = Join-Path $LogoDir "FileID-16.png"
$Logo96         = Join-Path $LogoDir "FileID-96.png"
$Logo256        = Join-Path $LogoDir "FileID-256.png"
$BundleLogo     = Join-Path $BundleTheme "logo.png"
$BundleSide     = Join-Path $BundleTheme "sidebar.png"
$MsiBanner      = Join-Path $MsiTheme "banner.bmp"
$MsiDialog      = Join-Path $MsiTheme "dialog.bmp"

# Idempotency: skip if every output is newer than the master.
$masterMtime = (Get-Item $Master).LastWriteTimeUtc
$outputs = @($IcoOut, $Logo16, $Logo96, $Logo256, $BundleLogo, $BundleSide, $MsiBanner, $MsiDialog)
$allUpToDate = $true
foreach ($o in $outputs) {
    if (-not (Test-Path $o) -or (Get-Item $o).LastWriteTimeUtc -lt $masterMtime) {
        $allUpToDate = $false
        break
    }
}
if ($allUpToDate -and -not $Force) {
    Write-Host "All icon outputs up to date; nothing to do (-Force to regenerate)." -ForegroundColor DarkGray
    exit 0
}

Write-Host "Loading master $Master ..." -ForegroundColor Cyan
$srcImg = [System.Drawing.Image]::FromFile($Master)
try {
    Write-Host "  master size: $($srcImg.Width) x $($srcImg.Height)" -ForegroundColor DarkGray

    function Resize-To([int]$size) {
        $bmp = New-Object System.Drawing.Bitmap $size, $size
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        try {
            $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
            $g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $g.Clear([System.Drawing.Color]::Transparent)
            $g.DrawImage($srcImg, 0, 0, $size, $size)
        } finally { $g.Dispose() }
        return $bmp
    }

    function Save-Png([System.Drawing.Bitmap]$bmp, [string]$path) {
        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    }

    # --- Logo PNGs ----------------------------------------------------
    Write-Host "Generating Logo PNGs..." -ForegroundColor Cyan
    foreach ($pair in @(
        @{ Size = 16;  Out = $Logo16 },
        @{ Size = 96;  Out = $Logo96 },
        @{ Size = 256; Out = $Logo256 }
    )) {
        $bmp = Resize-To $pair.Size
        try {
            Save-Png $bmp $pair.Out
            Write-Host "  $($pair.Out)  ($($pair.Size)x$($pair.Size))" -ForegroundColor DarkGreen
        } finally { $bmp.Dispose() }
    }

    # --- Burn bundle logo (130x102 letterbox) -------------------------
    Write-Host "Generating Burn theme logo (130x102 letterbox)..." -ForegroundColor Cyan
    $boxW = 130; $boxH = 102
    $bundleBmp = New-Object System.Drawing.Bitmap $boxW, $boxH
    $g = [System.Drawing.Graphics]::FromImage($bundleBmp)
    try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $g.Clear([System.Drawing.Color]::Transparent)
        # Preserve aspect ratio; letterbox the shorter side.
        $scale = [Math]::Min($boxW / $srcImg.Width, $boxH / $srcImg.Height)
        $drawW = [int]($srcImg.Width * $scale)
        $drawH = [int]($srcImg.Height * $scale)
        $offX = ($boxW - $drawW) / 2
        $offY = ($boxH - $drawH) / 2
        $g.DrawImage($srcImg, [int]$offX, [int]$offY, $drawW, $drawH)
    } finally { $g.Dispose() }
    Save-Png $bundleBmp $BundleLogo
    $bundleBmp.Dispose()
    Write-Host "  $BundleLogo  (130x102)" -ForegroundColor DarkGreen

    # --- Installer brand surfaces ------------------------------------
    Write-Host "Generating installer brand surfaces..." -ForegroundColor Cyan
    $sideBmp = New-Object System.Drawing.Bitmap 165, 400
    $sideG = [System.Drawing.Graphics]::FromImage($sideBmp)
    try {
        $sideG.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $sideGradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            (New-Object System.Drawing.Rectangle 0, 0, 165, 400),
            [System.Drawing.Color]::FromArgb(255, 12, 14, 18),
            [System.Drawing.Color]::FromArgb(255, 35, 30, 42),
            90.0)
        try { $sideG.FillRectangle($sideGradient, 0, 0, 165, 400) } finally { $sideGradient.Dispose() }
        foreach ($blob in @(
            @{ Color = [System.Drawing.Color]::FromArgb(78, 160, 226, 234); X = -44; Y = 250; W = 160; H = 160 },
            @{ Color = [System.Drawing.Color]::FromArgb(72, 177, 155, 206); X = 62; Y = 185; W = 145; H = 170 },
            @{ Color = [System.Drawing.Color]::FromArgb(54, 242, 166, 192); X = -20; Y = 80; W = 115; H = 130 }
        )) {
            $brush = New-Object System.Drawing.SolidBrush $blob.Color
            try { $sideG.FillEllipse($brush, $blob.X, $blob.Y, $blob.W, $blob.H) } finally { $brush.Dispose() }
        }
        $gold = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 204, 0))
        try { $sideG.FillRectangle($gold, 0, 0, 6, 400) } finally { $gold.Dispose() }
        $sideG.DrawImage($srcImg, 24, 34, 118, 118)
    } finally { $sideG.Dispose() }
    Save-Png $sideBmp $BundleSide
    $sideBmp.Dispose()

    $bannerBmp = New-Object System.Drawing.Bitmap 493, 58
    $bannerG = [System.Drawing.Graphics]::FromImage($bannerBmp)
    try {
        $bannerG.Clear([System.Drawing.Color]::FromArgb(255, 248, 248, 250))
        $bannerGold = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 204, 0))
        try { $bannerG.FillRectangle($bannerGold, 0, 54, 493, 4) } finally { $bannerGold.Dispose() }
        $bannerG.DrawImage($srcImg, 439, 4, 48, 48)
    } finally { $bannerG.Dispose() }
    $bannerBmp.Save($MsiBanner, [System.Drawing.Imaging.ImageFormat]::Bmp)
    $bannerBmp.Dispose()

    $dialogBmp = New-Object System.Drawing.Bitmap 493, 312
    $dialogG = [System.Drawing.Graphics]::FromImage($dialogBmp)
    try {
        $dialogG.Clear([System.Drawing.Color]::FromArgb(255, 248, 248, 250))
        $dialogGradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            (New-Object System.Drawing.Rectangle 0, 0, 164, 312),
            [System.Drawing.Color]::FromArgb(255, 13, 15, 19),
            [System.Drawing.Color]::FromArgb(255, 41, 34, 49),
            90.0)
        try { $dialogG.FillRectangle($dialogGradient, 0, 0, 164, 312) } finally { $dialogGradient.Dispose() }
        $dialogG.DrawImage($srcImg, 24, 46, 116, 116)
        $dialogGold = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 204, 0))
        try { $dialogG.FillRectangle($dialogGold, 160, 0, 4, 312) } finally { $dialogGold.Dispose() }
    } finally { $dialogG.Dispose() }
    $dialogBmp.Save($MsiDialog, [System.Drawing.Imaging.ImageFormat]::Bmp)
    $dialogBmp.Dispose()
    Write-Host "  $BundleSide  (165x400)" -ForegroundColor DarkGreen
    Write-Host "  $MsiBanner  (493x58)" -ForegroundColor DarkGreen
    Write-Host "  $MsiDialog  (493x312)" -ForegroundColor DarkGreen

    # --- Multi-resolution .ico ----------------------------------------
    # ICO format: 6-byte header, 16-byte ICONDIRENTRY per image, then
    # raw image data appended. We embed PNG-encoded images (modern Vista+
    # ICO format) for sizes >= 64; classic uncompressed BMP DIB for the
    # smaller sizes too (System.Drawing's PNG encoder is fine for all
    # sizes -- Windows accepts PNG entries down to 16x16).
    Write-Host "Generating multi-resolution FileID.ico..." -ForegroundColor Cyan
    $sizes = @(16, 32, 48, 64, 128, 256)

    # Encode each size to a PNG byte array first.
    $pngs = @{}
    foreach ($s in $sizes) {
        $bmp = Resize-To $s
        try {
            $ms = New-Object System.IO.MemoryStream
            $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
            $pngs[$s] = $ms.ToArray()
            $ms.Dispose()
        } finally { $bmp.Dispose() }
    }

    $ico = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter $ico
    try {
        # ICONDIR header.
        $w.Write([UInt16]0)               # reserved
        $w.Write([UInt16]1)               # type = 1 (icon)
        $w.Write([UInt16]$sizes.Count)    # image count

        # Compute offsets. First image data starts after the dir + entries.
        $offset = 6 + (16 * $sizes.Count)
        $entryStream = New-Object System.IO.MemoryStream
        $entryWriter = New-Object System.IO.BinaryWriter $entryStream
        $imageData = New-Object System.Collections.Generic.List[byte[]]
        try {
            foreach ($s in $sizes) {
                $bytes = $pngs[$s]
                $width  = if ($s -ge 256) { 0 } else { $s }   # 0 means 256 in ICO
                $height = if ($s -ge 256) { 0 } else { $s }
                $entryWriter.Write([byte]$width)
                $entryWriter.Write([byte]$height)
                $entryWriter.Write([byte]0)                  # palette count
                $entryWriter.Write([byte]0)                  # reserved
                $entryWriter.Write([UInt16]1)                # color planes
                $entryWriter.Write([UInt16]32)               # bits per pixel
                $entryWriter.Write([UInt32]$bytes.Length)    # size of image data
                $entryWriter.Write([UInt32]$offset)          # offset
                $imageData.Add($bytes)
                $offset += $bytes.Length
            }
            $w.Write($entryStream.ToArray())
            foreach ($d in $imageData) { $w.Write($d) }
        } finally {
            $entryWriter.Dispose()
            $entryStream.Dispose()
        }
        [System.IO.File]::WriteAllBytes($IcoOut, $ico.ToArray())
    } finally { $w.Dispose() }
    $sizeKb = [math]::Round((Get-Item $IcoOut).Length / 1024, 1)
    Write-Host "  $IcoOut  ($sizeKb KB, $($sizes.Count) resolutions)" -ForegroundColor DarkGreen
}
finally {
    $srcImg.Dispose()
}

Write-Host ""
Write-Host "Done." -ForegroundColor Green

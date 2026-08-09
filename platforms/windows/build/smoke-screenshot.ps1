# FileID Windows -- end-to-end smoke harness with screenshot capture.
#
# Launches the staged FileID.exe, waits for the window to appear, captures
# the primary monitor at intervals, then exits the app cleanly. Output:
#   build/smoke-out/launch.png      -- ~1s after window appears
#   build/smoke-out/welcome.png     -- ~3s after launch (welcome sheet should be up)
#   build/smoke-out/post-click.png  -- ~6s after launch (was 3s after Tab+Enter)
#   build/smoke-out/app.log         -- snapshot of %LOCALAPPDATA%\FileID\logs\app.log
#
# Use this from another shell to verify the install flow remotely:
#   pwsh build/smoke-screenshot.ps1
#
# Caller can then read app.log for [INSTALL] / [IPC OUT] / [IPC IN] traces.

param(
    [int]$LaunchWaitSeconds = 4,
    [int]$WelcomeWaitSeconds = 4,
    [int]$PostClickWaitSeconds = 4
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$OutDir    = Join-Path $ScriptDir "smoke-out"
$AppExe    = Join-Path $env:USERPROFILE "Desktop\FileID\FileID.exe"

if (-not (Test-Path $AppExe)) {
    $AppExe = Join-Path $env:LOCALAPPDATA "FileID-App\FileID.exe"
}
if (-not (Test-Path $AppExe)) {
    Write-Host "ERROR: no FileID.exe found at Desktop\FileID\ or %LOCALAPPDATA%\FileID-App\." -ForegroundColor Red
    Write-Host "Run 'pwsh build-all.ps1 -Desktop' first." -ForegroundColor Red
    exit 1
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Write-Host "Smoke harness output -> $OutDir"
Write-Host "Launching: $AppExe"

function Capture-Screen([string]$path) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Host "  -> $path"
}

# Launch + wait for window.
$proc = Start-Process -FilePath $AppExe -PassThru
Write-Host "  PID = $($proc.Id)"
Start-Sleep -Seconds $LaunchWaitSeconds
Capture-Screen (Join-Path $OutDir "launch.png")

# Wait for welcome sheet animation.
Start-Sleep -Seconds $WelcomeWaitSeconds
Capture-Screen (Join-Path $OutDir "welcome.png")

# Click "Install all" (heuristic: hit Tab N times then Enter; better:
# leave manual click for human; this script just captures a 3rd shot
# after another beat of "if user clicked, here's what should appear").
Start-Sleep -Seconds $PostClickWaitSeconds
Capture-Screen (Join-Path $OutDir "post-click.png")

# Snapshot app.log.
$appLog = Join-Path $env:LOCALAPPDATA "FileID\logs\app.log"
if (Test-Path $appLog) {
    Copy-Item -Force $appLog (Join-Path $OutDir "app.log")
    Write-Host "  -> $(Join-Path $OutDir 'app.log')"
} else {
    Write-Host "  (no app.log found at $appLog)" -ForegroundColor Yellow
}

# Clean shutdown.
try {
    if (-not $proc.HasExited) {
        Write-Host "Closing FileID..."
        $proc.CloseMainWindow() | Out-Null
        Start-Sleep -Seconds 2
        if (-not $proc.HasExited) {
            $proc.Kill()
        }
    }
} catch { }

Write-Host "Smoke harness done."

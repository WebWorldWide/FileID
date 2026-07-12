param(
    [string]$AppExe = "C:\Program Files\FileID\FileID.exe",
    [int]$StartupSeconds = 10,
    [int]$PreviewSeconds = 8,
    [string]$OutputDir = (Join-Path $PSScriptRoot "preview-smoke-out")
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$proc = Start-Process -FilePath $AppExe -PassThru
$previousForeground = [IntPtr]::Zero
$windowHandle = [IntPtr]::Zero
try {
    Start-Sleep -Seconds $StartupSeconds
    $root = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ProcessIdProperty,
            $proc.Id))
    if (-not $root) { throw "FileID window not found for PID $($proc.Id)" }

    $elements = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $rows = foreach ($el in $elements) {
        [pscustomobject]@{
            Name = $el.Current.Name
            ControlType = $el.Current.ControlType.ProgrammaticName
            AutomationId = $el.Current.AutomationId
            IsOffscreen = $el.Current.IsOffscreen
        }
    }
    $rows | ConvertTo-Json -Depth 3 | Set-Content (Join-Path $OutputDir "before.json") -Encoding utf8

    Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class PreviewSmokeMouse {
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
}
"@
    $windowHandle = [IntPtr]$root.Current.NativeWindowHandle
    $previousForeground = [PreviewSmokeMouse]::GetForegroundWindow()
    if ($previousForeground -ne [IntPtr]::Zero -and $previousForeground -ne $windowHandle) {
        [PreviewSmokeMouse]::ShowWindow($previousForeground, 6) | Out-Null
    }

    function Open-VisibleTile {
        $current = $root.FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition)
        $candidate = $current | Where-Object {
            $_.Current.AutomationId -eq "TileRoot" -and
            -not $_.Current.IsOffscreen -and
            -not [string]::IsNullOrWhiteSpace($_.Current.Name)
        } | Select-Object -First 1
        if (-not $candidate) { throw "No visible file tile automation element found" }
        $tileBounds = $candidate.Current.BoundingRectangle
        if ($tileBounds.IsEmpty) { throw "Visible file tile has empty bounds" }
        $point = [System.Windows.Point]::new(
            $tileBounds.X + ($tileBounds.Width / 2),
            $tileBounds.Y + ($tileBounds.Height / 2))
        [PreviewSmokeMouse]::ShowWindow($windowHandle, 9) | Out-Null
        $shell = New-Object -ComObject WScript.Shell
        if (-not $shell.AppActivate($proc.Id)) {
            [PreviewSmokeMouse]::SetForegroundWindow($windowHandle) | Out-Null
        }
        Start-Sleep -Milliseconds 500
        [PreviewSmokeMouse]::SetCursorPos([int]$point.X, [int]$point.Y) | Out-Null
        foreach ($i in 1..2) {
            [PreviewSmokeMouse]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
            [PreviewSmokeMouse]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
            if ($i -eq 1) { Start-Sleep -Milliseconds 120 }
        }
        return $candidate.Current.Name
    }

    $tileName = Open-VisibleTile
    Start-Sleep -Seconds $PreviewSeconds
    $bounds = $root.Current.BoundingRectangle
    $bmp = [System.Drawing.Bitmap]::new([int]$bounds.Width, [int]$bounds.Height)
    $gfx = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $gfx.CopyFromScreen([int]$bounds.X, [int]$bounds.Y, 0, 0, $bmp.Size)
        $bmp.Save((Join-Path $OutputDir "preview.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $gfx.Dispose()
        $bmp.Dispose()
    }

    $after = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $afterRows = foreach ($el in $after) {
        [pscustomobject]@{
            Name = $el.Current.Name
            ControlType = $el.Current.ControlType.ProgrammaticName
            AutomationId = $el.Current.AutomationId
            IsOffscreen = $el.Current.IsOffscreen
        }
    }
    $afterRows | ConvertTo-Json -Depth 3 | Set-Content (Join-Path $OutputDir "after.json") -Encoding utf8

    $previewVisible = [bool]($afterRows | Where-Object {
        $_.Name -eq "File preview" -and -not $_.IsOffscreen
    })
    $placeholder = @($afterRows | Where-Object {
        $_.Name -match "couldn't be decoded|preview unavailable|No preview"
    } | Select-Object -ExpandProperty Name)
    if (-not $previewVisible) {
        throw "Preview image did not become visible. Screenshot: $(Join-Path $OutputDir 'preview.png')"
    }

    $close = $after | Where-Object {
        $_.Current.Name -eq "Close preview" -and -not $_.Current.IsOffscreen
    } | Select-Object -First 1
    if (-not $close) { throw "Preview close button not found" }
    $invoke = $close.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invoke.Invoke()

    $closed = $false
    $deadline = (Get-Date).AddSeconds(5)
    while (-not $closed -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        if ($proc.HasExited) { throw "FileID exited while closing the preview" }
        $current = $root.FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition)
        $closed = -not [bool]($current | Where-Object {
            $_.Current.Name -eq "Close preview" -and -not $_.Current.IsOffscreen
        })
    }
    if (-not $closed) { throw "Preview remained visible after invoking Close preview" }
    Start-Sleep -Milliseconds 750

    $reopenedTileName = Open-VisibleTile
    Start-Sleep -Seconds $PreviewSeconds
    $reopened = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $reopenedPreviewVisible = [bool]($reopened | Where-Object {
        $_.Current.Name -eq "File preview" -and -not $_.Current.IsOffscreen
    })
    if (-not $reopenedPreviewVisible) { throw "Preview did not render after close and reopen" }

    [pscustomobject]@{
        Pid = $proc.Id
        Tile = $tileName
        ReopenedTile = $reopenedTileName
        Screenshot = (Join-Path $OutputDir "preview.png")
        PreviewElement = $previewVisible
        ClosedWhileAppAlive = $closed
        ReopenedPreviewElement = $reopenedPreviewVisible
        Placeholder = $placeholder
    } | ConvertTo-Json -Depth 3
} finally {
    if (-not $proc.HasExited) {
        $proc.CloseMainWindow() | Out-Null
        if (-not $proc.WaitForExit(5000)) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
    if ($previousForeground -ne [IntPtr]::Zero -and $previousForeground -ne $windowHandle) {
        [PreviewSmokeMouse]::ShowWindow($previousForeground, 9) | Out-Null
    }
}

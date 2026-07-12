param(
    [ValidateSet("Bundle", "Msi")]
    [string]$Target = "Bundle",
    [string]$OutputDir = (Join-Path $PSScriptRoot "installer-smoke-out"),
    [int]$WaitSeconds = 5
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class InstallerSmokeWindow {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
}
"@

$platformDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$artifact = if ($Target -eq "Bundle") {
    Join-Path $platformDir "dist\installer\FileIDSetup.exe"
} else {
    Join-Path $platformDir "dist\installer\FileID-x64.msi"
}
if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
    throw "Installer artifact not found: $artifact"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$proc = if ($Target -eq "Bundle") {
    Start-Process -FilePath $artifact -PassThru
} else {
    Start-Process -FilePath "msiexec.exe" -ArgumentList @("/i", $artifact) -PassThru
}
$previousForeground = [IntPtr]::Zero
$windowHandle = [IntPtr]::Zero
$windowProcessId = 0
$root = $null
try {
    $requiredReadyNames = if ($Target -eq "Bundle") {
        @("Install FileID", "Cancel", "I agree to the Apache 2.0 license")
    } else {
        @("I accept the terms in the License Agreement", "Install", "Cancel")
    }
    $deadline = (Get-Date).AddSeconds([Math]::Max($WaitSeconds + 10, 15))
    do {
        Start-Sleep -Milliseconds 250
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition)
        $candidates = $windows | Where-Object {
            $_.Current.NativeWindowHandle -ne 0 -and
            ($_.Current.ProcessId -eq $proc.Id -or $_.Current.Name -eq "FileID Setup")
        }
        foreach ($candidate in $candidates) {
            $candidateElements = $candidate.FindAll(
                [System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition)
            $candidateNames = @($candidateElements | Where-Object {
                -not $_.Current.IsOffscreen -and -not [string]::IsNullOrWhiteSpace($_.Current.Name)
            } | ForEach-Object { $_.Current.Name })
            $ready = -not @($requiredReadyNames | Where-Object { $candidateNames -notcontains $_ }).Count
            if ($ready) {
                $root = $candidate
                break
            }
        }
    } while (-not $root -and (Get-Date) -lt $deadline)
    if (-not $root) {
        throw "$Target installer did not reach its interactive license surface within the timeout."
    }

    $windowHandle = [IntPtr]$root.Current.NativeWindowHandle
    $windowProcessId = $root.Current.ProcessId
    $previousForeground = [InstallerSmokeWindow]::GetForegroundWindow()
    if ($previousForeground -ne [IntPtr]::Zero -and $previousForeground -ne $windowHandle) {
        [InstallerSmokeWindow]::ShowWindow($previousForeground, 6) | Out-Null
    }
    [InstallerSmokeWindow]::ShowWindow($windowHandle, 9) | Out-Null
    [InstallerSmokeWindow]::SetForegroundWindow($windowHandle) | Out-Null
    Start-Sleep -Milliseconds 500

    $bounds = $root.Current.BoundingRectangle
    if ($bounds.IsEmpty) { throw "$Target installer window has empty bounds" }
    $bmp = [System.Drawing.Bitmap]::new([int]$bounds.Width, [int]$bounds.Height)
    $gfx = [System.Drawing.Graphics]::FromImage($bmp)
    $imagePath = Join-Path $OutputDir ("{0}.png" -f $Target.ToLowerInvariant())
    try {
        $hdc = $gfx.GetHdc()
        try {
            if (-not [InstallerSmokeWindow]::PrintWindow($windowHandle, $hdc, 2)) {
                throw "PrintWindow failed for $Target installer"
            }
        } finally { $gfx.ReleaseHdc($hdc) }
        $bmp.Save($imagePath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $gfx.Dispose()
        $bmp.Dispose()
    }

    $elements = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $rows = foreach ($element in $elements) {
        [pscustomobject]@{
            Name = $element.Current.Name
            ControlType = $element.Current.ControlType.ProgrammaticName
            AutomationId = $element.Current.AutomationId
            IsOffscreen = $element.Current.IsOffscreen
        }
    }
    $jsonPath = Join-Path $OutputDir ("{0}.json" -f $Target.ToLowerInvariant())
    $rows | ConvertTo-Json -Depth 3 | Set-Content $jsonPath -Encoding utf8

    $namedControls = @($rows | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_.Name) -and $_.AutomationId -ne ""
    } | Select-Object -ExpandProperty Name)
    foreach ($requiredName in $requiredReadyNames) {
        if ($namedControls -notcontains $requiredName) {
            throw "$Target accessibility tree is missing '$requiredName'."
        }
    }
    if ($Target -eq "Msi" -and -not ($namedControls | Where-Object { $_ -match "Apache License" })) {
        throw "MSI license text did not render in the accessibility tree."
    }
    [pscustomobject]@{
        Target = $Target
        Artifact = $artifact
        Screenshot = $imagePath
        WindowName = $root.Current.Name
        NamedControls = $namedControls
    } | ConvertTo-Json -Depth 4
} finally {
    if ($root) {
        try {
            $windowPattern = $root.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern)
            $windowPattern.Close()
        } catch { }
    }
    if (-not $proc.HasExited) {
        try { $proc.CloseMainWindow() | Out-Null } catch { }
        if (-not $proc.WaitForExit(2000)) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
    if ($windowProcessId -gt 0 -and $windowProcessId -ne $proc.Id) {
        Stop-Process -Id $windowProcessId -Force -ErrorAction SilentlyContinue
    }
    if ($previousForeground -ne [IntPtr]::Zero -and $previousForeground -ne $windowHandle) {
        [InstallerSmokeWindow]::ShowWindow($previousForeground, 9) | Out-Null
    }
}

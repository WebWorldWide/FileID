#requires -Version 5.1
<#
.SYNOPSIS
    End-to-end smoke test for FileIDEngine.exe.

.DESCRIPTION
    Builds the engine in release mode, spawns it, drives a minimal
    requestStatus + shutdown sequence, and asserts:
        * a `ready` event is emitted with the expected schema fields
        * the engine exits cleanly (exit code 0) within 10 seconds.

    Designed for CI (.github/workflows/windows-engine.yml workflow_dispatch
    or a future scheduled run). Local invocation is also supported.

.PARAMETER NoBuild
    Skip the `cargo build --release` step and use an existing
    FileIDEngine.exe under target\release\.

.PARAMETER EngineExe
    Override the path to FileIDEngine.exe. Defaults to
    platforms\windows\src\engine\target\release\FileIDEngine.exe.

.PARAMETER TimeoutSeconds
    Maximum time to wait for the ready event + clean exit. Default 10.

.EXAMPLE
    pwsh platforms\windows\build\engine-smoke.ps1

.EXAMPLE
    pwsh platforms\windows\build\engine-smoke.ps1 -NoBuild
#>

param(
    [switch]$NoBuild,
    [string]$EngineExe,
    [int]$TimeoutSeconds = 10
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path "$PSScriptRoot\..\..\..").Path
$engineDir = Join-Path $repoRoot 'platforms\windows\src\engine'

if (-not $EngineExe) {
    $EngineExe = Join-Path $engineDir 'target\release\FileIDEngine.exe'
}

if (-not $NoBuild) {
    Write-Host "[engine-smoke] Building release binary..." -ForegroundColor Cyan
    Push-Location $engineDir
    try {
        cargo build --release --bin FileIDEngine 2>&1 | Write-Host
        if ($LASTEXITCODE -ne 0) {
            Write-Error "[engine-smoke] cargo build failed (exit $LASTEXITCODE)"
            exit 1
        }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $EngineExe)) {
    Write-Error "[engine-smoke] FileIDEngine.exe not found at $EngineExe"
    exit 1
}

Write-Host "[engine-smoke] Spawning $EngineExe ..." -ForegroundColor Cyan

# Use System.Diagnostics.Process directly so we can attach to both
# stdin and stdout with explicit handles. PowerShell's Start-Process
# would not give us back the stdin pipe.
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $EngineExe
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true

$proc = [System.Diagnostics.Process]::Start($psi)
$stdin = $proc.StandardInput
$stdout = $proc.StandardOutput

$readyJson = $null
$exitCode = $null
$failures = @()

try {
    # The engine emits a `ready` event unsolicited on first frame
    # availability. We then send shutdown to confirm the exit path.
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline -and -not $readyJson) {
        if ($stdout.Peek() -ge 0 -or -not $stdout.EndOfStream) {
            $line = $stdout.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match '"ready"') {
                $readyJson = $line
                Write-Host "[engine-smoke] Ready: $line" -ForegroundColor Green
                break
            }
        }
        Start-Sleep -Milliseconds 50
    }

    if (-not $readyJson) {
        $failures += 'engine did not emit a ready event within timeout'
    } else {
        # Validate the ready event shape.
        try {
            $parsed = $readyJson | ConvertFrom-Json
            $payload = $parsed.payload
            if (-not $payload.ready) {
                $failures += 'ready event missing "ready" key'
            } else {
                $inner = $payload.ready._0
                if (-not $inner) {
                    $failures += 'ready._0 wrapper missing'
                } else {
                    foreach ($k in 'version', 'pid', 'workerCap', 'physicalMemoryGB') {
                        if (-not (Get-Member -InputObject $inner -Name $k -MemberType NoteProperty)) {
                            $failures += "ready._0 missing required field: $k"
                        }
                    }
                }
            }
        } catch {
            $failures += "ready event JSON parse failed: $_"
        }
    }

    # Send shutdown.
    Write-Host '[engine-smoke] Sending shutdown...' -ForegroundColor Cyan
    $stdin.WriteLine('{"id":"smoke-shutdown","payload":{"shutdown":{}}}')
    $stdin.Flush()
    $stdin.Close()

    # Wait for clean exit.
    if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
        $failures += 'engine did not exit within timeout after shutdown'
        try { $proc.Kill() } catch { }
    } else {
        $exitCode = $proc.ExitCode
        if ($exitCode -ne 0) {
            $failures += "engine exited with code $exitCode (expected 0)"
        }
    }
} finally {
    if (-not $proc.HasExited) {
        try { $proc.Kill() } catch { }
    }
}

if ($failures.Count -gt 0) {
    Write-Host '[engine-smoke] FAILED' -ForegroundColor Red
    foreach ($f in $failures) { Write-Host "  - $f" -ForegroundColor Red }
    exit 1
}

Write-Host "[engine-smoke] PASS (exit $exitCode)" -ForegroundColor Green
exit 0

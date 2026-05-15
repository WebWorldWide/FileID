# One-shot helper to install the missing `clip_text` model bundle on
# machines where the welcome sheet shipped before V14.9-V (which now
# installs both halves of CLIP automatically). Spawns the engine, sends
# a single `prewarmModel` IPC command for clip_text, waits for the
# sentinel to land, then shuts down.
#
# Safe to re-run: engine short-circuits at handle_prewarm_model when the
# sentinel already exists.
#
# Compatible with Windows PowerShell 5.1 (no event += syntax).

param([int]$TimeoutSeconds = 180)

$ErrorActionPreference = 'Stop'

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src\engine")
$EnginePath  = Join-Path $EngineDir "target\x86_64-pc-windows-msvc\release\FileIDEngine.exe"

if (-not (Test-Path $EnginePath)) {
    Write-Host "Building engine first..." -ForegroundColor Cyan
    Push-Location $EngineDir
    try { & cargo build --release --target x86_64-pc-windows-msvc 2>&1 | Select-Object -Last 3 }
    finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { Write-Host "engine build failed" -ForegroundColor Red; exit 1 }
}

$Sentinel = Join-Path $env:LOCALAPPDATA "FileID\Models\.sentinels\clip_text.installed"
if (Test-Path $Sentinel) {
    Write-Host "clip_text already installed; nothing to do." -ForegroundColor Green
    exit 0
}

$tempDir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_.FullName }
$eventLog = Join-Path $tempDir "events.jsonl"
"" | Set-Content $eventLog

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $EnginePath
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true
$psi.Environment["FILEID_LOG"] = "info"

$engineProc = New-Object System.Diagnostics.Process
$engineProc.StartInfo = $psi
[void]$engineProc.Start()

# Use Register-ObjectEvent (works on PS 5.1 and 7) to capture stdout.
$stdoutEvent = Register-ObjectEvent -InputObject $engineProc -EventName 'OutputDataReceived' -Action {
    if ($EventArgs.Data) { Add-Content -Path $event.MessageData -Value $EventArgs.Data }
} -MessageData $eventLog
$engineProc.BeginOutputReadLine()

# Wait for ready.
$deadline = (Get-Date).AddSeconds(30)
$ready = $false
while (-not $ready -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
    if (Test-Path $eventLog) {
        $tail = Get-Content $eventLog -Tail 20 -ErrorAction SilentlyContinue
        if ($tail | Where-Object { $_ -match '"ready"' }) { $ready = $true }
    }
}
if (-not $ready) {
    Write-Host "engine never emitted ready (30s timeout)" -ForegroundColor Red
    Write-Host "Event log tail:" -ForegroundColor Yellow
    Get-Content $eventLog -Tail 20 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }
    try { $engineProc.Kill() } catch {}
    Unregister-Event -SourceIdentifier $stdoutEvent.Name -ErrorAction SilentlyContinue
    exit 2
}
Write-Host "engine ready" -ForegroundColor Green

# Send prewarmModel for clip_text. Engine wire format is
# {"id":"...","payload":{<variant>:{...}}} — see ipc/mod.rs::IpcCommand.
$cmd = @{ id = "prewarm-clip-text"; payload = @{ prewarmModel = @{ modelKind = "clip_text" } } } | ConvertTo-Json -Compress -Depth 10
Write-Host "-> $cmd" -ForegroundColor DarkGray
$engineProc.StandardInput.WriteLine($cmd)
$engineProc.StandardInput.Flush()

# Wait for sentinel.
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$lastProgressReport = Get-Date
while (-not (Test-Path $Sentinel) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 1
    if ((Get-Date) - $lastProgressReport -gt [TimeSpan]::FromSeconds(10)) {
        $latest = Get-Content $eventLog -Tail 5 -ErrorAction SilentlyContinue | Select-String -Pattern '(progress|installed|downloadComplete|error)' | Select-Object -Last 1
        if ($latest) { Write-Host "  ... $latest" -ForegroundColor DarkGray }
        $lastProgressReport = Get-Date
    }
}

# Shutdown.
$shutdown = @{ id = "shutdown-1"; payload = @{ shutdown = @{} } } | ConvertTo-Json -Compress -Depth 10
try { $engineProc.StandardInput.WriteLine($shutdown); $engineProc.StandardInput.Flush() } catch {}
$engineProc.WaitForExit(10000) | Out-Null
if (-not $engineProc.HasExited) { try { $engineProc.Kill() } catch {} }
Unregister-Event -SourceIdentifier $stdoutEvent.Name -ErrorAction SilentlyContinue

if (Test-Path $Sentinel) {
    Write-Host "clip_text installed successfully (sentinel landed)." -ForegroundColor Green
    exit 0
} else {
    Write-Host "clip_text install did NOT complete within ${TimeoutSeconds}s." -ForegroundColor Red
    Write-Host "Event log tail:" -ForegroundColor Yellow
    Get-Content $eventLog -Tail 30 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }
    exit 1
}

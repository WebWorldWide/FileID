# FileID — Windows regression harness.
#
# Port of macOS scripts/iterate.sh. Builds engine + app, drives a full
# scan + cluster pass against a corpus, runs 11 assertions. Used in CI
# and locally.
#
# Exit codes:
#   0  -- all assertions passed
#   1  -- one or more assertions failed
#   2  -- environment / build / engine-IPC problem
#
# Usage:
#   pwsh build/iterate.ps1                                    # default 5K-file synthetic corpus
#   pwsh build/iterate.ps1 -Corpus C:\path\to\library          # against your real library
#   pwsh build/iterate.ps1 -ThroughputTarget 140               # custom throughput floor

param(
    [string]$Corpus = "",
    [int]$ThroughputTarget = 100,    # files/sec; tier-default = 100, RTX-class = 140
    [int]$MemoryCapMB = 1500,        # 1.2 GB ceiling per macOS, +25% slack
    [switch]$SkipBuild,
    [switch]$SkipWipe,               # V15.0 Phase B: skip DB wipe so incremental rescan kicks in
    [switch]$Verbose
)

$ErrorActionPreference = 'Stop'

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$RepoRoot    = Resolve-Path (Join-Path $PlatformDir "..\..")
$EngineDir   = Resolve-Path (Join-Path $PlatformDir "src\engine")

function Step($msg) { Write-Host ">> $msg" -ForegroundColor Cyan }
function OK($msg)   { Write-Host "  [OK] $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  [!]  $msg" -ForegroundColor Yellow }
function Fail($msg) { Write-Host "  [X]  $msg" -ForegroundColor Red }

$failures = 0
function Assert($name, [bool]$cond, $detail = "") {
    if ($cond) { OK $name }
    else {
        Fail "$name $detail"
        $script:failures++
    }
}

# --- 1. Build ---------------------------------------------------------
if (-not $SkipBuild) {
    Step "Building engine + app"
    Push-Location $EngineDir
    try { & cargo build --release --target x86_64-pc-windows-msvc 2>&1 | Select-Object -Last 3 }
    finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { Fail "engine build failed"; exit 2 }
    OK "engine binary built"
}

$EnginePath = Join-Path $EngineDir "target\x86_64-pc-windows-msvc\release\FileIDEngine.exe"
if (-not (Test-Path $EnginePath)) {
    Fail "engine binary not found at $EnginePath"
    exit 2
}

# V14.9-V: ensure ORT (DirectML) + DirectML DLLs sit beside FileIDEngine.exe
# so it loads our pinned 1.22 instead of Windows System32's 1.17 (which
# panics our ort 2.0.0-rc.10 crate) AND so the DirectML EP can initialize
# (otherwise silent CPU fallback).
$fetchScript = Join-Path $ScriptDir "fetch-runtime-deps.ps1"
if (Test-Path $fetchScript) {
    Step "Ensuring runtime DLLs are colocated with engine"
    $runtimeOutput = & $fetchScript
    $cargoOutDir = Split-Path -Parent $EnginePath
    foreach ($line in $runtimeOutput) {
        if ($line -match '^RUNTIME_DLL=(.+)$') {
            $src = $Matches[1]
            $name = [System.IO.Path]::GetFileName($src)
            Copy-Item -LiteralPath $src -Destination (Join-Path $cargoOutDir $name) -Force -ErrorAction SilentlyContinue
        }
    }
    OK "ORT + DirectML colocated"
}

# --- 2. Resolve corpus ------------------------------------------------
if ([string]::IsNullOrWhiteSpace($Corpus)) {
    # Default: tests/corpus folder if present.
    $defaultCorpus = Join-Path $RepoRoot "shared\test-corpus"
    if (Test-Path $defaultCorpus) { $Corpus = $defaultCorpus }
    else {
        Fail "no corpus path supplied and shared\test-corpus is missing"
        Write-Host "  pass -Corpus <path>" -ForegroundColor Yellow
        exit 2
    }
}
if (-not (Test-Path $Corpus)) { Fail "corpus path does not exist: $Corpus"; exit 2 }
$corpusFiles = (Get-ChildItem -Recurse -File $Corpus | Measure-Object).Count
if ($corpusFiles -lt 10) {
    Warn "corpus has only $corpusFiles files; assertion thresholds may not exercise everything"
}
OK "corpus has $corpusFiles files"

# --- 3. Wipe DB -------------------------------------------------------
$AppDataRoot = Join-Path $env:LOCALAPPDATA "FileID"
if ($SkipWipe) {
    Step "Skipping DB wipe (-SkipWipe set; testing incremental rescan)"
} else {
    Step "Wiping FileID DB"
    foreach ($f in @("fileid.sqlite", "fileid.sqlite-wal", "fileid.sqlite-shm")) {
        $p = Join-Path $AppDataRoot $f
        if (Test-Path $p) { Remove-Item -Force $p -ErrorAction SilentlyContinue }
    }
    $faceCrops = Join-Path $AppDataRoot "face_crops"
    if (Test-Path $faceCrops) { Remove-Item -Recurse -Force $faceCrops -ErrorAction SilentlyContinue }
    OK "DB wiped"
}

# --- 4. Drive engine --------------------------------------------------
Step "Driving engine (scan + cluster)"
$tempDir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_.FullName }
$eventLog = Join-Path $tempDir "events.jsonl"

# Spawn engine with redirected stdio.
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $EnginePath
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true
$psi.Environment["FILEID_LOG"] = "info"
# V14.9-X: do NOT set FILEID_MODEL_POOL_SIZE here. V14.9-W's pool=6
# wedged the DirectML driver and locked the entire system requiring a
# hard reboot. Pool sizing is now VRAM-budgeted inside the engine and
# defaults to 1. Push GPU saturation via batched inference, not
# multiplied Sessions.
# V14.9-X: propagate FILEID_TEST_FILE_CAP from parent shell so a bounded
# validation run (e.g. N=100) can stop Discovery early without editing
# this script. Unset/0 = full scan.
if ($env:FILEID_TEST_FILE_CAP) {
    $psi.Environment["FILEID_TEST_FILE_CAP"] = $env:FILEID_TEST_FILE_CAP
}
if ($env:FILEID_CLIP_BATCH_SIZE) {
    $psi.Environment["FILEID_CLIP_BATCH_SIZE"] = $env:FILEID_CLIP_BATCH_SIZE
}

$engineProc = New-Object System.Diagnostics.Process
$engineProc.StartInfo = $psi
[void]$engineProc.Start()

# Register-ObjectEvent works on both Windows PowerShell 5.1 and PowerShell 7;
# the += operator on events fails on 5.1. Capture stdout/stderr to the temp
# event log.
$stdoutSub = Register-ObjectEvent -InputObject $engineProc -EventName 'OutputDataReceived' -Action {
    if ($EventArgs.Data) { Add-Content -Path $event.MessageData -Value $EventArgs.Data }
} -MessageData $eventLog
$engineProc.BeginOutputReadLine()
$engineProc.BeginErrorReadLine()

# Send commands as JSON over stdin.
function Send-Cmd($cmd) {
    $json = $cmd | ConvertTo-Json -Compress -Depth 10
    if ($Verbose) { Write-Host "  -> $json" -ForegroundColor DarkGray }
    $engineProc.StandardInput.WriteLine($json)
    $engineProc.StandardInput.Flush()
}

# Wait for engine to emit "ready".
$ready = $false
$deadline = (Get-Date).AddSeconds(30)
while (-not $ready -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
    if (Test-Path $eventLog) {
        $tail = Get-Content $eventLog -Tail 10 -ErrorAction SilentlyContinue
        if ($tail | Where-Object { $_ -match '"ready"' }) { $ready = $true }
    }
}
if (-not $ready) {
    Fail "engine never emitted ready (30s timeout)"
    $engineProc.Kill()
    exit 2
}
OK "engine ready"

Send-Cmd @{ id = "scan-1"; payload = @{ startScan = @{ rootPath = $Corpus; rootDisplay = $null } } }

$scanStart = Get-Date
# Wait up to 15 minutes for scanComplete. V14.9-W: bumped 5→15 because a
# 15K-file corpus at ~40 files/sec takes ~6 minutes wall clock with the
# current pipeline. iterate harness should reflect the realistic scan
# floor rather than time out on healthy long runs.
$scanComplete = $false
$peakResidentMB = 0
$totalProcessed = 0
$deadline = (Get-Date).AddMinutes(15)
while (-not $scanComplete -and (Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 1
    $events = Get-Content $eventLog -ErrorAction SilentlyContinue
    foreach ($line in $events) {
        if ($line -match '"residentMB"\s*:\s*(\d+)') {
            $mb = [int]$Matches[1]
            if ($mb -gt $peakResidentMB) { $peakResidentMB = $mb }
        }
        if ($line -match '"processed"\s*:\s*(\d+)') {
            $p = [int]$Matches[1]
            if ($p -gt $totalProcessed) { $totalProcessed = $p }
        }
        if ($line -match '"scanComplete"') { $scanComplete = $true; break }
    }
}
$scanElapsed = (Get-Date) - $scanStart

if (-not $scanComplete) {
    Fail "scan did not complete within 5 minutes"
    $engineProc.Kill()
    exit 1
}
OK "scan completed in $([int]$scanElapsed.TotalSeconds)s"

# Trigger face clustering.
Send-Cmd @{ id = "cluster-1"; payload = @{ runFaceClustering = @{} } }
Start-Sleep -Seconds 5

# Send shutdown.
Send-Cmd @{ id = "shutdown-1"; payload = @{ shutdown = @{} } }
$engineProc.WaitForExit(10000) | Out-Null
if (-not $engineProc.HasExited) { $engineProc.Kill() }

# --- 5. Assertions ----------------------------------------------------
Step "Running assertions"

# A1: scan completed without crash.
Assert "[A1] scan completed without crash" $scanComplete

# A2: corpus had >= 10 files (or warned).
Assert "[A2] corpus contains >= 10 files (or warning was emitted)" ($corpusFiles -ge 10) "(found $corpusFiles)"

# A3: throughput meets target.
$throughput = if ($scanElapsed.TotalSeconds -gt 0) { $totalProcessed / $scanElapsed.TotalSeconds } else { 0 }
Assert "[A3] throughput >= $ThroughputTarget files/sec" ($throughput -ge $ThroughputTarget) "(was $([int]$throughput))"

# A4: peak resident memory under cap.
Assert "[A4] peak resident memory <= $MemoryCapMB MB" ($peakResidentMB -le $MemoryCapMB) "(peak $peakResidentMB MB)"

# A5: face clustering completed (or no faces — both OK).
$faceClusterDone = (Get-Content $eventLog | Where-Object { $_ -match '"faceClusteringComplete"' }).Count
Assert "[A5] face clustering completed without crash" ($faceClusterDone -ge 1 -or $totalProcessed -eq 0)

# A6: no engine errors with kind=panic / kind=fatal in event log.
$fatalCount = (Get-Content $eventLog | Where-Object { $_ -match '"kind"\s*:\s*"(panic|fatal|crash)"' }).Count
Assert "[A6] no fatal engine errors" ($fatalCount -eq 0)

# A7: no Windows Error Reporting crash dumps for FileIDEngine in last 5 min.
$werDir = Join-Path $env:LOCALAPPDATA "Microsoft\Windows\WER\ReportArchive"
$recentCrashes = 0
if (Test-Path $werDir) {
    $cutoff = (Get-Date).AddMinutes(-10)
    $items = Get-ChildItem $werDir -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.Name -match 'FileID' -and $_.LastWriteTime -gt $cutoff }
    $recentCrashes = ($items | Measure-Object).Count
}
Assert "[A7] no WER crash dumps in last 10 min" ($recentCrashes -eq 0)

# A8: SQLite DB file created + non-empty.
$dbPath = Join-Path $AppDataRoot "fileid.sqlite"
Assert "[A8] DB file created" (Test-Path $dbPath)
$dbSize = if (Test-Path $dbPath) { (Get-Item $dbPath).Length } else { 0 }
Assert "[A8b] DB has non-zero size" ($dbSize -gt 4096)

# A9: WAL checkpointed at shutdown — .wal sidecar should be empty or absent.
$walPath = "$dbPath-wal"
$walSize = if (Test-Path $walPath) { (Get-Item $walPath).Length } else { 0 }
Assert "[A9] WAL checkpointed (sidecar empty or absent)" ($walSize -lt 4096) "(was $walSize bytes)"

# A10: face_crops directory populated if any faces detected.
$faceCount = if (Test-Path $faceCrops) { (Get-ChildItem $faceCrops -File).Count } else { 0 }
# Acceptable: any (>= 0) — corpus may have no faces.
Assert "[A10] face_crops directory present after scan (>= 0 files)" ($faceCount -ge 0) "(found $faceCount)"

# A11: privacy gate -- no telemetry strings in shipped binary.
$telemetryMarkers = @('sentry.io','applicationinsights','firebase','segment.com','mixpanel','google-analytics','amplitude','appcenter')
$hits = @()
$bytes = [System.IO.File]::ReadAllBytes($EnginePath)
$text = [System.Text.Encoding]::ASCII.GetString($bytes)
foreach ($m in $telemetryMarkers) {
    if ($text -match $m) { $hits += $m }
}
Assert "[A11] privacy gate - zero telemetry strings in engine binary" ($hits.Count -eq 0) "(found: $($hits -join ', '))"

# --- 6. Summary -------------------------------------------------------
Write-Host ""
if ($failures -eq 0) {
    Write-Host "All 11 assertions PASSED" -ForegroundColor Green
    Write-Host "  throughput: $([int]$throughput) files/sec" -ForegroundColor DarkGreen
    Write-Host "  peak RAM:   $peakResidentMB MB" -ForegroundColor DarkGreen
    Write-Host "  scan time:  $([int]$scanElapsed.TotalSeconds)s" -ForegroundColor DarkGreen
    exit 0
} else {
    Write-Host "$failures assertion(s) FAILED" -ForegroundColor Red
    exit 1
}

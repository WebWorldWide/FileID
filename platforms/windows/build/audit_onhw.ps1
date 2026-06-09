<#
  Non-destructive on-hardware audit driver for the FileID Windows engine.

  Runs the FULL read-only pipeline (scan -> face clustering -> restructure PLAN
  -> merge suggestions) against a real corpus, capturing throughput, peak RSS,
  the bound execution provider, per-stage timings, and any panics/errors.

  SAFETY: the engine is pointed at an ISOLATED temp state dir via LOCALAPPDATA,
  with the real Models/ folder junctioned in. The user's real library DB
  (%LOCALAPPDATA%\FileID\fileid.sqlite, ~24k files) is NEVER touched. No
  destructive command (applyRestructure / renameFiles / trash / applyTags) is
  ever sent.
#>
[CmdletBinding()]
param(
    [string]$Corpus = "G:\TrueNAS\iMac Documents",
    [int]$ScanTimeoutMin = 25
)

$ErrorActionPreference = 'Stop'
function Step($m){ Write-Host ">> $m" -ForegroundColor Cyan }
function Info($m){ Write-Host "   $m" -ForegroundColor Gray }
function OK($m){ Write-Host "  [OK] $m" -ForegroundColor Green }
function Warn($m){ Write-Host "  [!]  $m" -ForegroundColor Yellow }
function Bad($m){ Write-Host "  [X]  $m" -ForegroundColor Red }

$RepoRoot   = "C:\Users\adamm\Desktop\Code\FileID"
$EngineDir  = Join-Path $RepoRoot "platforms\windows\src\engine"
$BuildDir   = Join-Path $RepoRoot "platforms\windows\build"
$EnginePath = Join-Path $EngineDir "target\x86_64-pc-windows-msvc\release\FileIDEngine.exe"
$RealRoot   = Join-Path $env:LOCALAPPDATA "FileID"
$RealModels = Join-Path $RealRoot "Models"

if (-not (Test-Path $EnginePath)) { Bad "engine not built: $EnginePath"; exit 2 }
if (-not (Test-Path $Corpus))     { Bad "corpus not found: $Corpus"; exit 2 }
if (-not (Test-Path $RealModels)) { Bad "real Models dir missing: $RealModels"; exit 2 }

# --- isolated state dir (preserves the user's real library) -----------
# The engine's root() = %LOCALAPPDATA%\FileID, so with LOCALAPPDATA=$Temp the
# real state dir is $Temp\FileID. The junction + DB must live THERE, not in $Temp.
$Temp  = Join-Path $env:TEMP "fileid_audit_state"
$State = Join-Path $Temp "FileID"
if (Test-Path $Temp) {
    $j = Join-Path $State "Models"
    if (Test-Path $j) { cmd /c rmdir "$j" 2>$null }   # remove junction, NOT its target
    Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Force -Path $State | Out-Null
New-Item -ItemType Junction -Path (Join-Path $State "Models") -Target $RealModels | Out-Null
# Carry the user's real app settings (EP override etc.) read-only so the run is representative.
$appSettings = Join-Path $RealRoot "app-settings.json"
if (Test-Path $appSettings) { Copy-Item $appSettings (Join-Path $State "app-settings.json") -Force }
OK "isolated state dir: $State  (Models junctioned; real library untouched)"

# --- colocate ORT + DirectML beside the engine -----------------------
$fetch = Join-Path $BuildDir "fetch-runtime-deps.ps1"
$outDir = Split-Path -Parent $EnginePath
if (Test-Path $fetch) {
    & $fetch | ForEach-Object {
        if ($_ -match '^RUNTIME_DLL=(.+)$') {
            $src = $Matches[1]
            Copy-Item -LiteralPath $src -Destination (Join-Path $outDir ([IO.Path]::GetFileName($src))) -Force -ErrorAction SilentlyContinue
        }
    }
}

$corpusCount = (Get-ChildItem -Recurse -File -LiteralPath $Corpus -ErrorAction SilentlyContinue | Measure-Object).Count
Info "corpus: $Corpus  ($corpusCount files)"

$eventLog = Join-Path $Temp "events.jsonl"
"" | Set-Content $eventLog

# --- spawn engine -----------------------------------------------------
Step "Launching engine (isolated LOCALAPPDATA)"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $EnginePath
$psi.UseShellExecute = $false
$psi.RedirectStandardInput  = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError  = $true
$psi.CreateNoWindow = $true
$psi.Environment["LOCALAPPDATA"]   = $Temp
$psi.Environment["FILEID_LOG"]     = "info"
$psi.Environment["FILEID_PERF_TRACE"] = "1"
$psi.Environment["ORT_DYLIB_PATH"] = Join-Path $outDir "onnxruntime.dll"
# Forward the RAM++ batch-size toggle so a measurement run can A/B single
# (unset/0) vs batched (>1) without editing this script.
if ($env:FILEID_RAMPLUS_BATCH_SIZE) { $psi.Environment["FILEID_RAMPLUS_BATCH_SIZE"] = $env:FILEID_RAMPLUS_BATCH_SIZE }

$proc = New-Object System.Diagnostics.Process
$proc.StartInfo = $psi
[void]$proc.Start()
$sub = Register-ObjectEvent -InputObject $proc -EventName 'OutputDataReceived' -Action {
    if ($EventArgs.Data) { Add-Content -Path $event.MessageData -Value $EventArgs.Data }
} -MessageData $eventLog
$subE = Register-ObjectEvent -InputObject $proc -EventName 'ErrorDataReceived' -Action {
    if ($EventArgs.Data) { Add-Content -Path $event.MessageData -Value $EventArgs.Data }
} -MessageData $eventLog
$proc.BeginOutputReadLine()
$proc.BeginErrorReadLine()

function Send-Cmd($cmd) {
    $json = $cmd | ConvertTo-Json -Compress -Depth 12
    $proc.StandardInput.WriteLine($json)
    $proc.StandardInput.Flush()
}
function Wait-For($pattern, $timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        $tail = Get-Content $eventLog -ErrorAction SilentlyContinue
        if ($tail | Where-Object { $_ -match $pattern }) { return $true }
        if ($proc.HasExited) { return $false }
    }
    return $false
}

try {
    if (-not (Wait-For '"ready"' 60)) { Bad "engine never emitted ready"; throw "no-ready" }
    OK "engine ready"
    $readyLine = (Get-Content $eventLog | Where-Object { $_ -match '"ready"' } | Select-Object -First 1)
    foreach ($k in @('executionProvider','gpuName','vramMB','ramTotalMB','npuPresent','activeProfile')) {
        if ($readyLine -match "`"$k`"\s*:\s*`"?([^,`"}]+)") { Info "$k = $($Matches[1])" }
    }

    # --- scan ---------------------------------------------------------
    Step "Scanning ($corpusCount files; timeout ${ScanTimeoutMin}m)"
    $scanStart = Get-Date
    Send-Cmd @{ id = "scan-1"; payload = @{ startScan = @{ rootPath = $Corpus; rootDisplay = $null; rescan = $true } } }
    $done = $false; $peakMB = 0; $processed = 0
    $deadline = (Get-Date).AddMinutes($ScanTimeoutMin)
    while (-not $done -and (Get-Date) -lt $deadline -and -not $proc.HasExited) {
        Start-Sleep -Seconds 2
        foreach ($line in (Get-Content $eventLog -ErrorAction SilentlyContinue)) {
            if ($line -match '"residentMB"\s*:\s*(\d+)') { $mb=[int]$Matches[1]; if ($mb -gt $peakMB){$peakMB=$mb} }
            if ($line -match '"processed"\s*:\s*(\d+)')  { $p=[int]$Matches[1];  if ($p -gt $processed){$processed=$p} }
            if ($line -match '"scanComplete"') { $done = $true }
        }
    }
    $scanSec = ((Get-Date) - $scanStart).TotalSeconds
    if ($done) { OK "scan complete in $([int]$scanSec)s  (peak RSS ${peakMB}MB, processed $processed)" }
    else { Bad "scan did NOT complete in ${ScanTimeoutMin}m (processed $processed)" }
    $tput = if ($scanSec -gt 0) { [math]::Round($processed / $scanSec, 1) } else { 0 }
    Info "throughput = $tput files/s"

    # --- face clustering ---------------------------------------------
    Step "Face clustering"
    Send-Cmd @{ id = "cluster-1"; payload = @{ runFaceClustering = @{} } }
    if (Wait-For '"faceClusteringComplete"' 300) { OK "clustering complete" } else { Warn "clustering did not report complete in 5m" }

    # --- restructure PLAN (non-destructive) --------------------------
    Step "Restructure plan (plan only — NOT applied)"
    Send-Cmd @{ id = "plan-1"; payload = @{ planRestructure = @{ libraryRoot = $Corpus } } }
    if (Wait-For '"restructurePlan"' 300) { OK "restructure plan produced" } else { Warn "no restructure plan in 5m" }

    # --- merge suggestions -------------------------------------------
    Step "Merge suggestions"
    Send-Cmd @{ id = "merge-1"; payload = @{ findMergeSuggestions = @{} } }
    Wait-For '"mergeSuggestions"' 120 | Out-Null

    Step "Shutdown"
    Send-Cmd @{ id = "stop-1"; payload = @{ shutdown = @{} } }
    $proc.WaitForExit(15000) | Out-Null
    if (-not $proc.HasExited) { $proc.Kill() }
}
finally {
    Unregister-Event -SourceIdentifier $sub.Name -ErrorAction SilentlyContinue
    Unregister-Event -SourceIdentifier $subE.Name -ErrorAction SilentlyContinue
    if (-not $proc.HasExited) { try { $proc.Kill() } catch {} }
}

# --- analysis ---------------------------------------------------------
Step "Telemetry"
$dbPath = Join-Path $State "fileid.sqlite"
Info "DB: $dbPath  ($([math]::Round((Get-Item $dbPath -ErrorAction SilentlyContinue).Length/1MB,2)) MB)"

$errLines = Get-Content $eventLog | Where-Object { $_ -match '"kind"\s*:\s*"(panic|fatal|crash|error)"' -or $_ -match 'panicked' }
if ($errLines) { Bad "ENGINE ERRORS:"; $errLines | Select-Object -First 20 | ForEach-Object { Write-Host "    $_" -ForegroundColor Red } }
else { OK "no panic/fatal/crash lines" }

Step "EP / perf / restructure lines from log"
Get-Content $eventLog | Where-Object { $_ -match '\[EP' -or $_ -match 'ExecutionProvider' -or $_ -match '\[PERF\]' -or $_ -match '\[STATS\]' -or $_ -match 'DirectML' -or $_ -match 'CUDA' -or $_ -match '"restructurePlan"' -or $_ -match '"mergeSuggestions"' } | Select-Object -First 40 | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }

Step "DB assertions (scan_assertions.py)"
$py = $null; foreach ($c in @('python','py')) { if (Get-Command $c -ErrorAction SilentlyContinue) { $py=$c; break } }
if ($py) { $env:ASSERT_MIN_FILES = "1"; & $py (Join-Path $BuildDir "scan_assertions.py") $dbPath }
else { Warn "python not found; skipping DB assertions" }

# --- cleanup ----------------------------------------------------------
Step "Cleanup (removing junction + temp; user library untouched)"
$j = Join-Path $State "Models"
if (Test-Path $j) { cmd /c rmdir "$j" 2>$null }
Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue
OK "done"

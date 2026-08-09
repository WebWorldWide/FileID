<#
  Repeatable A/B perf benchmark for the FileID Windows engine.

  Drives a bounded, NON-DESTRUCTIVE scan against a real corpus in an ISOLATED
  state dir (real Models junctioned in; the user's real library DB is NEVER
  touched), samples GPU telemetry at 4 Hz, and emits a single machine-parseable
  RESULT line so before/after runs can be diffed.

  Bounded via FILEID_TEST_FILE_CAP so a measurement run takes ~1 min, not ~36.

  Usage:
    pwsh build/perf_bench.ps1 -Label baseline -Cap 400
    pwsh build/perf_bench.ps1 -Label after-fix -Cap 400 -Corpus "G:\TrueNAS\Users"
#>
[CmdletBinding()]
param(
    [string]$Corpus = "G:\TrueNAS\Users",
    [int]$Cap = 400,
    [string]$Label = "run",
    [int]$ScanTimeoutMin = 20,
    [switch]$NoGpu
)
$ErrorActionPreference = 'Stop'
function Step($m){ Write-Host ">> $m" -ForegroundColor Cyan }
function Info($m){ Write-Host "   $m" -ForegroundColor Gray }

$RepoRoot   = "C:\Users\adamm\Desktop\Code\FileID"
$EngineDir  = Join-Path $RepoRoot "platforms\windows\src\engine"
$BuildDir   = Join-Path $RepoRoot "platforms\windows\build"
$EnginePath = Join-Path $EngineDir "target\x86_64-pc-windows-msvc\release\FileIDEngine.exe"
$RealRoot   = Join-Path $env:LOCALAPPDATA "FileID"
$RealModels = Join-Path $RealRoot "Models"
if (-not (Test-Path $EnginePath)) { Write-Host "engine not built: $EnginePath" -ForegroundColor Red; exit 2 }
if (-not (Test-Path $Corpus))     { Write-Host "corpus not found: $Corpus" -ForegroundColor Red; exit 2 }

# --- isolated state dir (preserves the user's real library) -----------
$Temp  = Join-Path $env:TEMP "fileid_perf_state"
$State = Join-Path $Temp "FileID"
if (Test-Path $Temp) {
    $j = Join-Path $State "Models"
    if (Test-Path $j) { cmd /c rmdir "$j" 2>$null }
    Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Force -Path $State | Out-Null
New-Item -ItemType Junction -Path (Join-Path $State "Models") -Target $RealModels | Out-Null
$appSettings = Join-Path $RealRoot "app-settings.json"
if (Test-Path $appSettings) { Copy-Item $appSettings (Join-Path $State "app-settings.json") -Force }

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

$eventLog = Join-Path $Temp "events.jsonl"
"" | Set-Content $eventLog
$gpuCsv = Join-Path $Temp "gpu.csv"

# --- GPU sampler ------------------------------------------------------
$smi = $null
if (-not $NoGpu -and (Get-Command nvidia-smi -ErrorAction SilentlyContinue)) {
    $smiArgs = "--query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits -lms 250 -f `"$gpuCsv`""
    $smi = Start-Process -FilePath "nvidia-smi" -ArgumentList $smiArgs -PassThru -WindowStyle Hidden
}

# --- spawn engine -----------------------------------------------------
Step "Bench '$Label': scanning <= $Cap files of $Corpus"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $EnginePath
$psi.UseShellExecute = $false
$psi.RedirectStandardInput  = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError  = $true
$psi.CreateNoWindow = $true
$psi.Environment["LOCALAPPDATA"]        = $Temp
$psi.Environment["FILEID_LOG"]          = "info"
$psi.Environment["FILEID_PERF_TRACE"]   = "1"
$psi.Environment["FILEID_TEST_FILE_CAP"]= "$Cap"
$psi.Environment["ORT_DYLIB_PATH"]      = Join-Path $outDir "onnxruntime.dll"
if ($env:FILEID_RAMPLUS_BATCH_SIZE) { $psi.Environment["FILEID_RAMPLUS_BATCH_SIZE"] = $env:FILEID_RAMPLUS_BATCH_SIZE }
if ($env:FILEID_CLIP_USE_BATCH)     { $psi.Environment["FILEID_CLIP_USE_BATCH"]     = $env:FILEID_CLIP_USE_BATCH }

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
    $proc.StandardInput.WriteLine($json); $proc.StandardInput.Flush()
}

# wait ready
$deadline = (Get-Date).AddSeconds(60); $ready = $false
while ((Get-Date) -lt $deadline -and -not $proc.HasExited) {
    Start-Sleep -Milliseconds 400
    if ((Get-Content $eventLog -ErrorAction SilentlyContinue) -match '"ready"') { $ready = $true; break }
}
if (-not $ready) { Write-Host "engine never readied" -ForegroundColor Red; if(-not $proc.HasExited){$proc.Kill()}; exit 2 }
$readyLine = (Get-Content $eventLog | Where-Object { $_ -match '"ready"' } | Select-Object -First 1)
$ep = if ($readyLine -match '"executionProvider"\s*:\s*"([^"]+)"') { $Matches[1] } else { "?" }
$gpuName = if ($readyLine -match '"gpuName"\s*:\s*"([^"]+)"') { $Matches[1] } else { "?" }
Info "EP=$ep  GPU=$gpuName"

# scan
$scanStart = Get-Date
Send-Cmd @{ id = "scan-1"; payload = @{ startScan = @{ rootPath = $Corpus; rootDisplay = $null; rescan = $true } } }
$done = $false; $peakMB = 0; $processed = 0; $fps = 0.0
$deadline = (Get-Date).AddMinutes($ScanTimeoutMin)
while (-not $done -and (Get-Date) -lt $deadline -and -not $proc.HasExited) {
    Start-Sleep -Seconds 1
    foreach ($line in (Get-Content $eventLog -ErrorAction SilentlyContinue)) {
        if ($line -match '"residentMB"\s*:\s*(\d+)') { $mb=[int]$Matches[1]; if ($mb -gt $peakMB){$peakMB=$mb} }
        if ($line -match '"processed"\s*:\s*(\d+)')  { $p=[int]$Matches[1];  if ($p -gt $processed){$processed=$p} }
        if ($line -match '"scanComplete"') { $done = $true }
    }
}
$scanSec = ((Get-Date) - $scanStart).TotalSeconds
Send-Cmd @{ id = "stop-1"; payload = @{ shutdown = @{} } }
$proc.WaitForExit(15000) | Out-Null
if (-not $proc.HasExited) { try { $proc.Kill() } catch {} }
Unregister-Event -SourceIdentifier $sub.Name -ErrorAction SilentlyContinue
Unregister-Event -SourceIdentifier $subE.Name -ErrorAction SilentlyContinue
if ($smi) { Start-Sleep -Milliseconds 400; Stop-Process -Id $smi.Id -Force -ErrorAction SilentlyContinue }

$tput = if ($scanSec -gt 0 -and $processed -gt 0) { [math]::Round($processed / $scanSec, 2) } else { 0 }

# --- last [STATS] line ------------------------------------------------
$statsLine = (Get-Content $eventLog | Where-Object { $_ -match '\[STATS\]' } | Select-Object -Last 1)
function StatOf($name) { if ($statsLine -match "$name\s*[=:]\s*(\d+)") { return [int]$Matches[1] } else { return 0 } }
$ramUs = StatOf 'ramplus_us'; $visUs = StatOf 'vision_us'; $clipUs = StatOf 'clip_us'
$ocrUs = StatOf 'ocr_us'; $totUs = StatOf 'total_us'; $vwaitUs = StatOf 'vision_wait_us'

# --- GPU summary ------------------------------------------------------
$gMean=0; $gP50=0; $gP90=0; $vramMax=0
if ((-not $NoGpu) -and (Test-Path $gpuCsv)) {
    $rows = @(Get-Content $gpuCsv | Where-Object { $_ -match ',' } | ForEach-Object {
        $p = $_ -split ',' | ForEach-Object { $_.Trim() }
        if ($p.Count -ge 2 -and $p[0] -match '^\d+$') { [pscustomobject]@{ g=[int]$p[0]; m=[int]$p[1] } } })
    if ($rows.Count -gt 0) {
        $gs = $rows.g | Sort-Object
        $gMean = [math]::Round(($rows.g | Measure-Object -Average).Average,1)
        $gP50 = $gs[[math]::Floor(($gs.Count-1)*0.5)]
        $gP90 = $gs[[math]::Floor(($gs.Count-1)*0.9)]
        $vramMax = ($rows.m | Measure-Object -Max).Maximum
    }
}

$errs = @(Get-Content $eventLog | Where-Object { $_ -match 'panicked' -or $_ -match '"kind"\s*:\s*"(panic|fatal|crash)"' }).Count

# cleanup
$j = Join-Path $State "Models"; if (Test-Path $j) { cmd /c rmdir "$j" 2>$null }
Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "================ PERF [$Label] ================" -ForegroundColor Magenta
Write-Host ("  throughput   : {0} files/s   ({1} files / {2}s)" -f $tput,$processed,[int]$scanSec)
Write-Host ("  peak RSS     : {0} MB" -f $peakMB)
Write-Host ("  per-file us  : total={0} ramplus={1} vision={2} clip={3} ocr={4} vision_wait={5}" -f $totUs,$ramUs,$visUs,$clipUs,$ocrUs,$vwaitUs)
Write-Host ("  GPU util %   : mean={0} p50={1} p90={2}   VRAM max={3} MB" -f $gMean,$gP50,$gP90,$vramMax)
Write-Host ("  EP={0}  panics={1}" -f $ep,$errs)
Write-Host ("RESULT label=$Label tput=$tput rss_mb=$peakMB processed=$processed sec=$([int]$scanSec) ramplus_us=$ramUs clip_us=$clipUs vision_us=$visUs vision_wait_us=$vwaitUs total_us=$totUs gpu_mean=$gMean gpu_p50=$gP50 gpu_p90=$gP90 vram_max=$vramMax ep=$ep panics=$errs") -ForegroundColor Green

<#
  Repeatable A/B perf benchmark for the FileID Windows engine.

  Drives a bounded, NON-DESTRUCTIVE scan against a real corpus in an ISOLATED
  state dir (real Models reused read-only; the user's real library DB is NEVER
  touched), samples GPU telemetry at 4 Hz, and emits a single machine-parseable
  RESULT line so before/after runs can be diffed.

  Bounded via FILEID_TEST_FILE_CAP so a measurement run takes ~1 min, not ~36.

  Usage:
    pwsh build/perf_bench.ps1 -Label baseline -Cap 400
    pwsh build/perf_bench.ps1 -Label after-fix -Cap 400 -Corpus "F:\TrueNAS\Users"
#>
[CmdletBinding()]
param(
    [string]$Corpus = "F:\TrueNAS\Users",
    [int]$Cap = 400,
    [string]$Label = "run",
    [int]$ScanTimeoutMin = 20,
    [switch]$NoGpu,
    [switch]$KeepState
)
$ErrorActionPreference = 'Stop'
function Step($m){ Write-Host ">> $m" -ForegroundColor Cyan }
function Info($m){ Write-Host "   $m" -ForegroundColor Gray }

$RepoRoot   = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\..\.."))
$EngineDir  = Join-Path $RepoRoot "platforms\windows\src\engine"
$BuildDir   = $PSScriptRoot
$EnginePath = Join-Path $EngineDir "target\x86_64-pc-windows-msvc\release\FileIDEngine.exe"
$RealRoot   = Join-Path $env:LOCALAPPDATA "FileID"
$RealModels = Join-Path $RealRoot "Models"
if (-not (Test-Path $EnginePath)) { Write-Host "engine not built: $EnginePath" -ForegroundColor Red; exit 2 }
if (-not (Test-Path $Corpus))     { Write-Host "corpus not found: $Corpus" -ForegroundColor Red; exit 2 }
if (-not (Test-Path $RealModels)) { Write-Host "models not found: $RealModels" -ForegroundColor Red; exit 2 }

# --- isolated state dir (preserves the user's real library) -----------
$Temp  = Join-Path $env:TEMP ("fileid_perf_state_" + [guid]::NewGuid().ToString("N"))
$State = Join-Path $Temp "FileID"
New-Item -ItemType Directory -Force -Path $State | Out-Null
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
    $smiArgs = "--query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits -lms 250"
    $smi = Start-Process -FilePath "nvidia-smi" -ArgumentList $smiArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput $gpuCsv
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
$psi.Environment["FILEID_DB"]           = Join-Path $State "fileid.sqlite"
$psi.Environment["FILEID_MODELS_DIR"]   = $RealModels
$psi.Environment["FILEID_LOG"]          = "info"
$psi.Environment["FILEID_PERF_TRACE"]   = "1"
$psi.Environment["FILEID_TEST_FILE_CAP"]= "$Cap"
[void]$psi.Environment.Remove("ORT_DYLIB_PATH")
if ($env:FILEID_RAMPLUS_BATCH_SIZE) { $psi.Environment["FILEID_RAMPLUS_BATCH_SIZE"] = $env:FILEID_RAMPLUS_BATCH_SIZE }
if ($env:FILEID_CLIP_USE_BATCH)     { $psi.Environment["FILEID_CLIP_USE_BATCH"]     = $env:FILEID_CLIP_USE_BATCH }
if ($env:FILEID_MODEL_POOL_SIZE)    { $psi.Environment["FILEID_MODEL_POOL_SIZE"]    = $env:FILEID_MODEL_POOL_SIZE }

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
if (-not $ready) {
    Write-Host "engine never readied; diagnostic state preserved at $Temp" -ForegroundColor Red
    if (-not $proc.HasExited) { $proc.Kill() }
    Unregister-Event -SourceIdentifier $sub.Name -ErrorAction SilentlyContinue
    Unregister-Event -SourceIdentifier $subE.Name -ErrorAction SilentlyContinue
    if ($smi -and -not $smi.HasExited) { Stop-Process -Id $smi.Id -Force -ErrorAction SilentlyContinue }
    exit 2
}
$readyLine = (Get-Content $eventLog | Where-Object { $_ -match '"ready"' } | Select-Object -First 1)
$ep = if ($readyLine -match '"executionProvider"\s*:\s*"([^"]+)"') { $Matches[1] } else { "?" }
$gpuName = if ($readyLine -match '"adapterName"\s*:\s*"([^"]+)"') { $Matches[1] } else { "?" }
Info "EP=$ep  GPU=$gpuName"

# scan
$scanStart = Get-Date
Send-Cmd @{ id = "scan-1"; payload = @{ startScan = @{ rootPath = $Corpus; rootDisplay = $null; rescan = $true } } }
$done = $false; $peakMB = 0; $processed = 0; $failed = 0; $engineSec = 0.0
$deadline = (Get-Date).AddMinutes($ScanTimeoutMin)
while (-not $done -and (Get-Date) -lt $deadline -and -not $proc.HasExited) {
    Start-Sleep -Seconds 1
    foreach ($line in (Get-Content $eventLog -Tail 200 -ErrorAction SilentlyContinue)) {
        if ($line -match '"residentMB"\s*:\s*(\d+)') { $mb=[int]$Matches[1]; if ($mb -gt $peakMB){$peakMB=$mb} }
        if ($line -match '"processed"\s*:\s*(\d+)')  { $p=[int]$Matches[1];  if ($p -gt $processed){$processed=$p} }
        if ($line -match '"processedFiles"\s*:\s*(\d+)') { $p=[int]$Matches[1]; if ($p -gt $processed){$processed=$p} }
        if ($line -match '"failed"\s*:\s*(\d+)') { $f=[int]$Matches[1]; if ($f -gt $failed){$failed=$f} }
        if ($line -match '"failedFiles"\s*:\s*(\d+)') { $f=[int]$Matches[1]; if ($f -gt $failed){$failed=$f} }
        if ($line -match '"totalSeconds"\s*:\s*([\d.]+)') { $engineSec=[double]$Matches[1] }
        if ($line -match '"scanComplete"') { $done = $true }
    }
}
$wallSec = ((Get-Date) - $scanStart).TotalSeconds
Send-Cmd @{ id = "stop-1"; payload = @{ shutdown = @{} } }
$proc.WaitForExit(15000) | Out-Null
if (-not $proc.HasExited) { try { $proc.Kill() } catch {} }
if ($proc.HasExited) { $proc.WaitForExit() }
Unregister-Event -SourceIdentifier $sub.Name -ErrorAction SilentlyContinue
Unregister-Event -SourceIdentifier $subE.Name -ErrorAction SilentlyContinue
if ($smi) {
    Start-Sleep -Milliseconds 400
    Stop-Process -Id $smi.Id -Force -ErrorAction SilentlyContinue
    $smi.WaitForExit(5000) | Out-Null
}

$eventLines = @(Get-Content $eventLog -ErrorAction SilentlyContinue)
$engineLogLines = @(Get-ChildItem -LiteralPath (Join-Path $State "logs") -Filter "engine.jsonl*" -File -ErrorAction SilentlyContinue |
    ForEach-Object { Get-Content -LiteralPath $_.FullName -ErrorAction SilentlyContinue })
$diagnosticLines = @($eventLines) + @($engineLogLines)
$providerBindCount = @($diagnosticLines | Where-Object { $_ -match 'Successfully registered `CUDAExecutionProvider`' }).Count
$providerFallbackCount = @($diagnosticLines | Where-Object {
    $_ -match 'No execution providers from session options registered successfully' -or
    $_ -match 'attempting to register `CUDAExecutionProvider`'
}).Count
if (-not $done) {
    throw "benchmark scan did not complete; diagnostic state preserved at $Temp"
}
if ($proc.ExitCode -ne 0) {
    throw "benchmark engine exited with code $($proc.ExitCode); diagnostic state preserved at $Temp"
}
if ($ep -eq 'cuda' -and ($providerBindCount -eq 0 -or $providerFallbackCount -gt 0)) {
    throw "CUDA was advertised but did not bind cleanly (binds=$providerBindCount fallbacks=$providerFallbackCount); diagnostic state preserved at $Temp"
}

$tput = if ($wallSec -gt 0 -and $processed -gt 0) { [math]::Round($processed / $wallSec, 2) } else { 0 }
$engineTput = if ($engineSec -gt 0 -and $processed -gt 0) { [math]::Round($processed / $engineSec, 2) } else { 0 }

# --- last [STATS] line ------------------------------------------------
$statsLine = ($diagnosticLines | Where-Object { $_ -match '\[STATS\]' } | Select-Object -Last 1)
function StatOf($name) { if ($statsLine -match ('"' + [regex]::Escape($name) + '"\s*:\s*(\d+)')) { return [int]$Matches[1] } else { return 0 } }
$ramUs = StatOf 'ramplus_us'; $visUs = StatOf 'vision_us'; $clipUs = StatOf 'clip_us'
$ocrUs = StatOf 'ocr_us'; $totUs = StatOf 'total_us'; $vwaitUs = StatOf 'vision_wait_us'
$ramDispatch = if ($diagnosticLines | Where-Object { $_ -match 'RAM\+\+.*model loaded \(batch-coordinator mode\)' }) {
    'batch'
} elseif ($diagnosticLines | Where-Object { $_ -match 'RAM\+\+.*does not expose a dynamic batch axis' }) {
    'pool-static-model'
} else {
    'pool'
}

# --- GPU summary ------------------------------------------------------
$gMean=0; $gP50=0; $gP90=0; $vramMax=0; $rows=@()
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
if (-not $NoGpu -and $ep -eq 'cuda' -and $rows.Count -eq 0) {
    throw "GPU telemetry was requested but nvidia-smi produced no samples; diagnostic state preserved at $Temp"
}

$errs = @($diagnosticLines | Where-Object { $_ -match 'panicked' -or $_ -match '"kind"\s*:\s*"(panic|fatal|crash)"' }).Count

# cleanup
$resolvedTemp = [IO.Path]::GetFullPath($Temp)
$resolvedSystemTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
if (-not $resolvedTemp.StartsWith($resolvedSystemTemp, [StringComparison]::OrdinalIgnoreCase)) {
    throw "refusing to remove benchmark state outside the system temp directory: $resolvedTemp"
}
if (-not $KeepState) {
    Remove-Item -LiteralPath $resolvedTemp -Recurse -Force -ErrorAction SilentlyContinue
}
$stateResult = if ($KeepState) { $resolvedTemp } else { "removed" }

Write-Host ""
Write-Host "================ PERF [$Label] ================" -ForegroundColor Magenta
Write-Host ("  cold throughput   : {0} files/s   ({1} files, {2} failed / {3:N1}s wall)" -f $tput,$processed,$failed,$wallSec)
Write-Host ("  engine throughput : {0} files/s   ({1:N1}s engine)" -f $engineTput,$engineSec)
Write-Host ("  peak RSS     : {0} MB" -f $peakMB)
Write-Host ("  per-file us  : total={0} ramplus={1} vision={2} clip={3} ocr={4} vision_wait={5}" -f $totUs,$ramUs,$visUs,$clipUs,$ocrUs,$vwaitUs)
Write-Host ("  RAM++ mode  : {0}" -f $ramDispatch)
Write-Host ("  GPU util %   : mean={0} p50={1} p90={2}   VRAM max={3} MB" -f $gMean,$gP50,$gP90,$vramMax)
Write-Host ("  EP={0}  binds={1}  fallbacks={2}  panics={3}" -f $ep,$providerBindCount,$providerFallbackCount,$errs)
if ($KeepState) { Write-Host ("  state         : {0}" -f $resolvedTemp) }
Write-Host ("RESULT label=$Label tput=$tput engine_tput=$engineTput rss_mb=$peakMB processed=$processed failed=$failed wall_sec=$([math]::Round($wallSec,2)) engine_sec=$([math]::Round($engineSec,2)) ramplus_us=$ramUs clip_us=$clipUs vision_us=$visUs vision_wait_us=$vwaitUs total_us=$totUs ramplus_mode=$ramDispatch gpu_mean=$gMean gpu_p50=$gP50 gpu_p90=$gP90 vram_max=$vramMax ep=$ep ep_binds=$providerBindCount ep_fallbacks=$providerFallbackCount panics=$errs state=$stateResult") -ForegroundColor Green

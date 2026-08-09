<#
  Profile the REAL RAM++ wall: sample nvidia-smi GPU utilization, VRAM, power,
  and SM clock at 4 Hz during a single-path (pool) scan, then summarize the
  distribution. Answers the premise directly:
    - GPU util LOW  (<30%) during the RAM++-heavy scan -> latency-bound, headroom
                          exists, batching/IO-binding could help (coordinator may
                          be the bug, not the idea).
    - GPU util HIGH (>80%) -> compute-bound; batching can't help; the win is a
                          faster inference (op fusion / EP / model), not batching.
  RAM++ is ~67% of per-file pipeline time, so scan-wide GPU util tracks RAM++.
#>
[CmdletBinding()]
param(
    [string]$Corpus = "G:\TrueNAS\iMac Documents"
)
$ErrorActionPreference = 'Stop'
$csv = Join-Path $env:TEMP "fileid_gpu_profile.csv"
$driver = "C:\Users\adamm\Desktop\Code\FileID\platforms\windows\build\audit_onhw.ps1"
Remove-Item Env:\FILEID_RAMPLUS_BATCH_SIZE -ErrorAction SilentlyContinue   # force single/pool path
if (Test-Path $csv) { Remove-Item $csv -Force }

Write-Host ">> Starting nvidia-smi sampler (4 Hz) -> $csv" -ForegroundColor Cyan
$smiArgs = "--query-gpu=utilization.gpu,utilization.memory,memory.used,power.draw,clocks.sm --format=csv,noheader,nounits -lms 250 -f `"$csv`""
$smi = Start-Process -FilePath "nvidia-smi" -ArgumentList $smiArgs -PassThru -WindowStyle Hidden

try {
    Write-Host ">> Running single-path scan ($Corpus)" -ForegroundColor Yellow
    & $driver -Corpus $Corpus 2>&1 | Select-String -Pattern 'scan complete|throughput = |executionProvider|RESULT:' | ForEach-Object { Write-Host "   $_" }
}
finally {
    Start-Sleep -Milliseconds 500
    Stop-Process -Id $smi.Id -Force -ErrorAction SilentlyContinue
}

if (-not (Test-Path $csv)) { Write-Host "NO SAMPLES — nvidia-smi -lms unsupported?" -ForegroundColor Red; exit 1 }
$rows = @(Get-Content $csv | Where-Object { $_ -match ',' } | ForEach-Object {
    $p = $_ -split ',' | ForEach-Object { $_.Trim() }
    if ($p.Count -ge 5 -and $p[0] -match '^\d+$') {
        [pscustomobject]@{ gpu=[int]$p[0]; mem=[int]$p[1]; memused=[int]$p[2]; power=[double]$p[3]; sm=[int]$p[4] }
    }
})
function Pctl($vals, $p) { if (-not $vals) { return 0 } $s = $vals | Sort-Object; $i = [math]::Floor(($s.Count - 1) * $p); return $s[$i] }
$g = $rows.gpu; $m = $rows.memused; $sm = $rows.sm; $pw = $rows.power
Write-Host ""
Write-Host "================ GPU PROFILE ($($rows.Count) samples @4Hz) ================" -ForegroundColor Magenta
Write-Host ("  GPU util %%:   p50=%-3s  p90=%-3s  max=%-3s  mean=%-5.1f" -f (Pctl $g 0.5),(Pctl $g 0.9),(($g|Measure-Object -Max).Maximum),(($g|Measure-Object -Average).Average))
Write-Host ("  SM clock MHz: p50=%-5s p90=%-5s max=%-5s" -f (Pctl $sm 0.5),(Pctl $sm 0.9),(($sm|Measure-Object -Max).Maximum))
Write-Host ("  VRAM used MB: p50=%-5s max=%-5s" -f (Pctl $m 0.5),(($m|Measure-Object -Max).Maximum))
Write-Host ("  Power W:      p50=%-5.1f max=%-5.1f" -f (Pctl $pw 0.5),(($pw|Measure-Object -Max).Maximum))
$busy = @($g | Where-Object { $_ -gt 50 }).Count
Write-Host ("  Samples >50%% GPU: %d / %d  (%.0f%%)" -f $busy,$rows.Count,(100.0*$busy/[math]::Max(1,$rows.Count)))
$verdict = if ((Pctl $g 0.9) -lt 30) { "LATENCY-BOUND (headroom exists)" } elseif ((Pctl $g 0.5) -gt 80) { "COMPUTE-BOUND (saturated)" } else { "MIXED/PIPELINE-BOUND" }
Write-Host "  VERDICT: $verdict" -ForegroundColor Magenta

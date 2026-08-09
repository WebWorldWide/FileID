<#
  Prove the batched-RAM++ throughput win on the RTX 2060. A/B the SAME batched
  ONNX: single (FILEID_RAMPLUS_BATCH_SIZE=0, pool path, batch=1) vs batched
  (=N, coordinator). The ratio is the win (precision-independent).

  SAFETY: backs up the real ram_plus.onnx, swaps in the batched one, and ALWAYS
  restores it in a finally block (the user's model is never lost).
#>
[CmdletBinding()]
param(
    [string]$Batched = "C:\Users\adamm\AppData\Local\Temp\ram_plus_batched\ram_plus.onnx",
    [int]$BatchN = 4,
    [string]$Corpus = "G:\TrueNAS\iMac Documents"
)
$ErrorActionPreference = 'Stop'
$real = Join-Path $env:LOCALAPPDATA "FileID\Models\ram_plus\ram_plus.onnx"
$bak = "$real.audit-bak"
$driver = "C:\Users\adamm\Desktop\Code\FileID\platforms\windows\build\audit_onhw.ps1"

function ThroughputOf($lines) {
    $m = $lines | Select-String -Pattern 'throughput = ([\d.]+)'
    if ($m) { return [double]$m.Matches[0].Groups[1].Value } else { return 0.0 }
}
function RamplusUs($lines) {
    $m = $lines | Select-String -Pattern 'ramplus_us=(\d+)'
    if ($m) { return [int]$m.Matches[-1].Groups[1].Value } else { return 0 }
}
function BatchAvg($lines) {
    $m = $lines | Select-String -Pattern 'rp_batch|RAMPLUS-BATCH'
    return ($m | Select-Object -First 2) -join ' | '
}

Write-Host ">> Backing up real ram_plus.onnx + swapping in the batched ONNX" -ForegroundColor Cyan
Copy-Item -LiteralPath $real -Destination $bak -Force
$realMB = [math]::Round((Get-Item $bak).Length / 1MB, 1)
Copy-Item -LiteralPath $Batched -Destination $real -Force
Write-Host "   real ($realMB MB) backed up -> .audit-bak ; batched ($([math]::Round((Get-Item $real).Length/1MB,1)) MB) swapped in"

$single = 0.0; $batched = 0.0; $singleRam = 0; $batchedRam = 0
try {
    Write-Host ">> RUN A: SINGLE (FILEID_RAMPLUS_BATCH_SIZE=0)" -ForegroundColor Yellow
    $env:FILEID_RAMPLUS_BATCH_SIZE = "0"
    $a = & $driver -Corpus $Corpus 2>&1
    $single = ThroughputOf $a; $singleRam = RamplusUs $a
    Write-Host "   SINGLE throughput=$single files/s  ramplus_us=$singleRam" -ForegroundColor Green

    Write-Host ">> RUN B: BATCHED (FILEID_RAMPLUS_BATCH_SIZE=$BatchN)" -ForegroundColor Yellow
    $env:FILEID_RAMPLUS_BATCH_SIZE = "$BatchN"
    $b = & $driver -Corpus $Corpus 2>&1
    $batched = ThroughputOf $b; $batchedRam = RamplusUs $b
    Write-Host "   BATCHED throughput=$batched files/s  ramplus_us=$batchedRam" -ForegroundColor Green
    Write-Host "   batch lines: $(BatchAvg $b)"
    ($b | Select-String -Pattern 'no panic|ENGINE ERRORS|out of memory|RESULT:') | Select-Object -First 4 | ForEach-Object { Write-Host "   $_" }
}
finally {
    Write-Host ">> Restoring real ram_plus.onnx" -ForegroundColor Cyan
    Copy-Item -LiteralPath $bak -Destination $real -Force
    Remove-Item -LiteralPath $bak -Force -ErrorAction SilentlyContinue
    $restMB = [math]::Round((Get-Item $real).Length / 1MB, 1)
    Write-Host "   restored ($restMB MB; expected $realMB MB) — match: $($restMB -eq $realMB)"
    Remove-Item Env:\FILEID_RAMPLUS_BATCH_SIZE -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "================ BATCHED RAM++ RESULT ================" -ForegroundColor Magenta
Write-Host "  single (batch=1):   $single files/s   (ramplus_us=$singleRam)"
Write-Host "  batched (batch=$BatchN): $batched files/s   (ramplus_us=$batchedRam)"
if ($single -gt 0) { Write-Host ("  SPEEDUP: {0:N2}x" -f ($batched / $single)) -ForegroundColor Magenta }

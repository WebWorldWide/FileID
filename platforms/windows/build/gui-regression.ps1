# FileID Windows — GUI regression harness.
#
# Drives the actual WinUI 3 app against a real corpus, watching for native
# fast-fails the existing iterate.ps1 (engine-headless) can never catch.
# This is the harness that would have caught V15.2 ThumbnailService /
# V15.4 SidebarQueueList before the user did — both Pattern B bugs that
# only surface when the 17 PropertyChanged subscribers run.
#
# Requires:
#   1. The app built with --auto-scan-folder support (V15.5+; Program.cs
#      parses the flag, App.xaml.cs dispatches the scan once Ready).
#   2. A corpus folder (use build/gen-corpus.ps1 to make a 50K synthetic one).
#
# Pass criteria:
#   - App reaches ScanCompleteEvent (Phase=Completed) within TimeoutMinutes
#   - App exits cleanly (last-session.txt: clean_exit=true)
#   - No new WER crash dumps written during the run
#   - No unmatched [APPLY:N] enter in app.log (would name the killer subscriber)
#
# Exit codes (match iterate.ps1):
#   0 — all assertions passed
#   1 — one or more assertions failed
#   2 — environment / build problem
#
# Usage:
#   pwsh build/gui-regression.ps1 -Corpus C:\path\to\library
#   pwsh build/gui-regression.ps1 -Corpus C:\path -TimeoutMinutes 90 -SkipBuild

param(
    [Parameter(Mandatory=$true)][string]$Corpus,
    [int]$TimeoutMinutes = 60,
    [string]$Configuration = "Debug",
    [switch]$SkipBuild,
    [switch]$SkipWipe
)

$ErrorActionPreference = 'Stop'

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = Resolve-Path (Join-Path $ScriptDir "..")
$AppDir      = Resolve-Path (Join-Path $PlatformDir "src/FileID.App")
$Solution    = Join-Path $PlatformDir "FileID.sln"
$AppTfm      = "net8.0-windows10.0.19041.0"

$LogsDir    = Join-Path $env:LOCALAPPDATA "FileID\logs"
$AppLog     = Join-Path $LogsDir "app.log"
$LastSess   = Join-Path $LogsDir "last-session.txt"
$WerDir     = Join-Path $env:LOCALAPPDATA "CrashDumps"
$StateDb    = Join-Path $env:LOCALAPPDATA "FileID\state.sqlite"

function Step($msg) { Write-Host ">> $msg" -ForegroundColor Cyan }
function OK($msg)   { Write-Host "  [OK] $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  [!]  $msg" -ForegroundColor Yellow }
function Fail($msg) { Write-Host "  [X]  $msg" -ForegroundColor Red }

$failures = 0
function Assert($name, [bool]$cond, $detail = "") {
    if ($cond) { OK $name }
    else { Fail "$name $detail"; $script:failures++ }
}

# --- 1. Pre-flight ----------------------------------------------------
Step "Pre-flight"
if (-not (Test-Path $Corpus)) {
    Fail "corpus path does not exist: $Corpus"
    exit 2
}
$corpusFileCount = (Get-ChildItem -Path $Corpus -Recurse -File -ErrorAction SilentlyContinue | Measure-Object).Count
OK "corpus: $Corpus ($corpusFileCount files)"

# --- 2. Build ---------------------------------------------------------
$AppExe = Join-Path $AppDir "bin\$Configuration\$AppTfm\FileID.exe"
if (-not $SkipBuild) {
    Step "Building app ($Configuration)"
    Push-Location $PlatformDir
    try {
        & dotnet build $Solution -c $Configuration --nologo -v minimal
        if ($LASTEXITCODE -ne 0) { Fail "dotnet build failed"; exit 2 }
    } finally { Pop-Location }
    OK "build complete"
}
if (-not (Test-Path $AppExe)) {
    Fail "app binary not found at: $AppExe"
    exit 2
}
OK "app exe: $AppExe"

# --- 3. Wipe state ----------------------------------------------------
if (-not $SkipWipe) {
    Step "Wiping prior state"
    if (Test-Path $StateDb)   { Remove-Item -LiteralPath $StateDb -Force -ErrorAction SilentlyContinue }
    foreach ($suffix in '-wal','-shm','-journal') {
        $sidecar = "$StateDb$suffix"
        if (Test-Path $sidecar) { Remove-Item -LiteralPath $sidecar -Force -ErrorAction SilentlyContinue }
    }
    if (Test-Path $AppLog)    { Remove-Item -LiteralPath $AppLog -Force -ErrorAction SilentlyContinue }
    if (Test-Path $LastSess)  { Remove-Item -LiteralPath $LastSess -Force -ErrorAction SilentlyContinue }
    OK "state wiped"
}

# --- 4. Snapshot WER dumps -------------------------------------------
$dumpsBefore = @()
if (Test-Path $WerDir) {
    $dumpsBefore = Get-ChildItem -Path $WerDir -Filter "FileID*.dmp" -ErrorAction SilentlyContinue | ForEach-Object Name
}

# --- 5. Spawn app -----------------------------------------------------
Step "Launching app with --auto-scan-folder"
$proc = Start-Process -FilePath $AppExe `
    -ArgumentList @("--auto-scan-folder", $Corpus, "--auto-exit-after-scan") `
    -PassThru
OK "spawned pid=$($proc.Id)"

# --- 6. Poll loop -----------------------------------------------------
$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
$scanStarted = $false
$scanEnded = $false
$scanOk = $false

while ((Get-Date) -lt $deadline) {
    if ($proc.HasExited) { break }
    if (Test-Path $AppLog) {
        $tail = Get-Content -Path $AppLog -Tail 200 -ErrorAction SilentlyContinue
        if (-not $scanStarted -and ($tail | Where-Object { $_ -match '\[AUTO-SCAN\] starting scan' })) {
            $scanStarted = $true
            Step "scan started"
        }
        $endLine = $tail | Where-Object { $_ -match '\[AUTO-SCAN\] scan ended ok=(\w+)' } | Select-Object -Last 1
        if ($endLine) {
            $scanEnded = $true
            $scanOk = ($endLine -match 'ok=True')
            break
        }
        if ($tail | Where-Object { $_ -match '\[AUTO-SCAN\] failed:' }) {
            $scanEnded = $true
            $scanOk = $false
            break
        }
    }
    Start-Sleep -Seconds 5
}

# --- 7. Wait briefly for graceful close ------------------------------
if (-not $proc.HasExited) {
    Step "Waiting up to 30s for app to close"
    $proc.WaitForExit(30000) | Out-Null
}
if (-not $proc.HasExited) {
    Warn "app did not exit gracefully; killing"
    try { Stop-Process -Id $proc.Id -Force } catch { }
}

# --- 8. Assertions ---------------------------------------------------
Step "Assertions"
Assert "scan started"   $scanStarted
Assert "scan ended"     $scanEnded
Assert "scan ok"        $scanOk

# clean_exit marker
$cleanExit = $false
if (Test-Path $LastSess) {
    $sess = Get-Content -Path $LastSess -Raw
    $cleanExit = $sess -match 'clean_exit=true'
}
Assert "clean_exit=true" $cleanExit "(see $LastSess)"

# WER dump delta
$dumpsAfter = @()
if (Test-Path $WerDir) {
    $dumpsAfter = Get-ChildItem -Path $WerDir -Filter "FileID*.dmp" -ErrorAction SilentlyContinue | ForEach-Object Name
}
$newDumps = $dumpsAfter | Where-Object { $_ -notin $dumpsBefore }
Assert "no new WER dumps" ($newDumps.Count -eq 0) "(new: $($newDumps -join ', '))"

# Forensic: scan app.log for unmatched [APPLY:N] enter (would name the killer)
$lastEnter = $null
$lastExitSeq = -1
if (Test-Path $AppLog) {
    foreach ($line in Get-Content -Path $AppLog) {
        if ($line -match '\[APPLY:(\d+)\] enter (\S+)') {
            $lastEnter = @{ Seq = [int]$Matches[1]; Event = $Matches[2] }
        } elseif ($line -match '\[APPLY:(\d+)\] exit') {
            $lastExitSeq = [int]$Matches[1]
        }
    }
}
$unmatchedApply = ($lastEnter -ne $null) -and ($lastEnter.Seq -gt $lastExitSeq)
Assert "no unmatched [APPLY:N] enter" (-not $unmatchedApply) `
    "(last enter seq=$($lastEnter.Seq) event=$($lastEnter.Event); last exit seq=$lastExitSeq)"

if ($unmatchedApply -and (Test-Path $AppLog)) {
    Step "Last subscriber called before death"
    Get-Content -Path $AppLog -Tail 30 `
        | Where-Object { $_ -match 'ENGINE-SUB:|APPLY:' } `
        | Select-Object -Last 10 `
        | ForEach-Object { Write-Host "  $_" -ForegroundColor Magenta }
}

# --- 9. Result -------------------------------------------------------
Write-Host ""
if ($failures -eq 0) {
    Write-Host "[PASS] GUI regression: scan completed cleanly." -ForegroundColor Green
    exit 0
} else {
    Write-Host "[FAIL] GUI regression: $failures assertion(s) failed." -ForegroundColor Red
    Write-Host "  app.log:        $AppLog"
    Write-Host "  last-session:   $LastSess"
    Write-Host "  WER dumps:      $WerDir"
    exit 1
}

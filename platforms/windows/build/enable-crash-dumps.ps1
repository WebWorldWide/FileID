# enable-crash-dumps.ps1 — turn on Windows Error Reporting LocalDumps for FileID.exe
# so the NEXT crash drops a full native minidump we can open in WinDbg/VS.
#
# Why: the >1h scan crash is a NATIVE fast-fail (RaiseFailFastException) — it
# bypasses every managed handler (Application.UnhandledException,
# AppDomain.UnhandledException) so app.log just stops with no exception. The
# only way to get the faulting native stack is a WER LocalDump.
#
# This writes under HKLM, so it needs an elevated shell. The script self-elevates.
# Dumps land in %LOCALAPPDATA%\FileID\crashdumps (DumpType=2 = full).
#
# Usage:   .\platforms\windows\build\enable-crash-dumps.ps1
# Disable: .\platforms\windows\build\enable-crash-dumps.ps1 -Disable

param([switch]$Disable)

$ErrorActionPreference = 'Stop'
$exe = 'FileID.exe'
$key = "HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\$exe"
# WER expands %LOCALAPPDATA% at crash time under the crashing user's profile.
$dumpFolder = '%LOCALAPPDATA%\FileID\crashdumps'

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-Admin)) {
    Write-Host 'Elevation required for HKLM — relaunching as admin...' -ForegroundColor Yellow
    $argList = @('-NoProfile','-ExecutionPolicy','Bypass','-File',"`"$PSCommandPath`"")
    if ($Disable) { $argList += '-Disable' }
    Start-Process pwsh -Verb RunAs -ArgumentList $argList
    return
}

if ($Disable) {
    if (Test-Path $key) { Remove-Item $key -Recurse -Force; Write-Host "Removed LocalDumps for $exe." -ForegroundColor Green }
    else { Write-Host "No LocalDumps entry for $exe." }
    return
}

New-Item -Path $key -Force | Out-Null
New-ItemProperty -Path $key -Name 'DumpFolder' -PropertyType ExpandString -Value $dumpFolder -Force | Out-Null
New-ItemProperty -Path $key -Name 'DumpType'   -PropertyType DWord       -Value 2           -Force | Out-Null
New-ItemProperty -Path $key -Name 'DumpCount'  -PropertyType DWord       -Value 10          -Force | Out-Null

Write-Host "WER LocalDumps enabled for $exe (full dumps)." -ForegroundColor Green
Write-Host "Dumps -> $dumpFolder  (resolves to $([Environment]::ExpandEnvironmentVariables($dumpFolder)))"
Write-Host ''
Write-Host 'Next: reproduce the crash (run a long scan + scroll through an audio folder).'
Write-Host 'Then share the newest .dmp from the crashdumps folder.'

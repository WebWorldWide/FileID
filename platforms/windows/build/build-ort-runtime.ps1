# Build the ONNX Runtime FileID ships on Windows: 1.22.0 with the DirectML and
# CPU execution providers, from Microsoft's source at the v1.22.0 commit, WITHOUT
# --use_telemetry. Microsoft's own DirectML NuGet and GitHub zips are built with
# that flag, which registers ONNX Runtime's ETW provider in Windows' telemetry
# provider group (DECISIONS 2026-11-10). Without it the provider keeps an
# all-zero group and nothing reaches Windows' diagnostic-data service.
#
# Writes onnxruntime.dll + onnxruntime_providers_shared.dll to -OutputDir, the
# folder fetch-runtime-deps.ps1 reads. CI runs this in ort-runtime.yml and
# caches the result; locally it takes about an hour on 12 cores.
#
# Needs Visual Studio 2022 (C++ desktop + ARM64 tools for arm64), CMake 3.28+,
# Python 3.10+ and git. The build itself downloads ORT's third-party sources
# (SHA-1 pinned in cmake/deps.txt), nuget.exe and the DirectML NuGet; the
# latter two are checked against the SHA-256 pins below before anything is
# written to -OutputDir.

[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64',
    [string]$OutputDir = '',
    # Kept short: ORT's build tree is deep enough to hit MAX_PATH.
    [string]$WorkDir = ''
)

$ErrorActionPreference = 'Stop'

$OrtVersion = '1.22.0'
$OrtCommit = 'f217402897f40ebba457e2421bc0a4702771968e'
$OrtRepository = 'https://github.com/microsoft/onnxruntime.git'
$NugetExeSha256 = '96a1dc5ad572716b2ab345a6830365cf0103661ec906b0e78fd04a064a7bb730'
$DirectMlNupkgSha256 = '4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9'
$Outputs = @('onnxruntime.dll', 'onnxruntime_providers_shared.dll')

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir '..\..\..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $ScriptDir "runtime-cache\onnxruntime-$OrtVersion-dml-notelemetry\$Architecture"
}
if ([string]::IsNullOrWhiteSpace($WorkDir)) {
    $WorkDir = Join-Path $env:SystemDrive 'fileid-ort'
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$WorkDir = [IO.Path]::GetFullPath($WorkDir)
$SourceDir = Join-Path $WorkDir 'src'
$BuildDir = Join-Path $WorkDir "b-$Architecture"

function Invoke-Native([string]$Executable, [string[]]$Arguments) {
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Executable $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-BuildDownload([string]$Filter, [string]$Expected) {
    $found = @(Get-ChildItem -LiteralPath $BuildDir -Recurse -File -Filter $Filter -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) {
        throw "The ONNX Runtime build did not leave $Filter under $BuildDir; cannot verify it."
    }
    foreach ($file in $found) {
        $actual = Get-Sha256 $file.FullName
        if ($actual -ne $Expected) {
            throw "SHA-256 mismatch for $($file.FullName): expected $Expected, got $actual."
        }
    }
}

function Get-PeMachine([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $reader = [IO.BinaryReader]::new($stream)
        $stream.Position = 0x3C
        $stream.Position = $reader.ReadUInt32() + 4
        return $reader.ReadUInt16()
    } finally {
        $stream.Dispose()
    }
}

New-Item -ItemType Directory -Force -Path $SourceDir | Out-Null
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir '.git'))) {
    Invoke-Native 'git' @('-C', $SourceDir, 'init', '--quiet')
}
$head = (& git -C $SourceDir rev-parse --verify --quiet HEAD) | Select-Object -First 1
if ($head -ne $OrtCommit) {
    Write-Host "Fetching ONNX Runtime $OrtVersion ($OrtCommit)..." -ForegroundColor Cyan
    Invoke-Native 'git' @('-C', $SourceDir, 'fetch', '--quiet', '--depth', '1', $OrtRepository, $OrtCommit)
    Invoke-Native 'git' @('-C', $SourceDir, 'checkout', '--quiet', '--force', 'FETCH_HEAD')
}
Invoke-Native 'git' @('-C', $SourceDir, 'reset', '--quiet', '--hard', $OrtCommit)
Invoke-Native 'git' @('-C', $SourceDir, 'clean', '-xdfq')
$head = (& git -C $SourceDir rev-parse HEAD).Trim()
if ($head -ne $OrtCommit) {
    throw "ONNX Runtime checkout is at $head, expected $OrtCommit."
}
# GitLab regenerated Eigen's archive, so the 1.22.0 SHA-1 pin no longer
# matches. Apply upstream's fix from v1.22.1 (its only deps.txt change): the
# same Eigen commit from the GitHub mirror, with that archive's SHA-1.
$depsFile = Join-Path $SourceDir 'cmake\deps.txt'
$eigenGitLab = 'eigen;https://gitlab.com/libeigen/eigen/-/archive/1d8b82b0740839c0de7f1242a3585e3390ff5f33/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip;5ea4d05e62d7f954a46b3213f9b2535bdd866803'
$eigenMirror = 'eigen;https://github.com/eigen-mirror/eigen/archive/1d8b82b0740839c0de7f1242a3585e3390ff5f33/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip;05b19b49e6fbb91246be711d801160528c135e34'
$deps = [IO.File]::ReadAllLines($depsFile)
if (@($deps | Where-Object { $_ -eq $eigenGitLab }).Count -ne 1) {
    throw "cmake/deps.txt does not carry the expected 1.22.0 Eigen line."
}
[IO.File]::WriteAllLines($depsFile, [string[]]@($deps | ForEach-Object { if ($_ -eq $eigenGitLab) { $eigenMirror } else { $_ } }))
$privateConfig = Join-Path $SourceDir 'include\onnxruntime\core\platform\windows\TraceLoggingConfigPrivate.h'
if (Test-Path -LiteralPath $privateConfig) {
    throw "TraceLoggingConfigPrivate.h exists; it would put ONNX Runtime in Windows' telemetry group."
}

$arguments = @(
    'tools\ci_build\build.py',
    '--build_dir', $BuildDir,
    '--config', 'Release',
    '--update', '--build', '--parallel',
    '--skip_submodule_sync', '--skip_tests',
    '--build_shared_lib',
    '--use_dml',
    '--use_binskim_compliant_compile_flags',
    '--compile_no_warning_as_error',
    '--cmake_generator', 'Visual Studio 17 2022',
    '--cmake_extra_defines', 'onnxruntime_BUILD_UNIT_TESTS=OFF', 'onnxruntime_USE_TELEMETRY=OFF'
)
if ($Architecture -eq 'arm64') {
    $arguments += '--arm64'
}
Write-Host "Building ONNX Runtime $OrtVersion (DirectML, $Architecture, no telemetry)..." -ForegroundColor Cyan
Push-Location $SourceDir
try {
    Invoke-Native 'python' $arguments
} finally {
    Pop-Location
}

Assert-BuildDownload 'nuget.exe' $NugetExeSha256
Assert-BuildDownload 'Microsoft.AI.DirectML.1.15.4.nupkg' $DirectMlNupkgSha256

$expectedMachine = if ($Architecture -eq 'arm64') { 0xAA64 } else { 0x8664 }
$binaries = Join-Path $BuildDir 'Release\Release'
foreach ($name in $Outputs) {
    $path = Join-Path $binaries $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "The ONNX Runtime build did not produce $path."
    }
    if ((Get-PeMachine $path) -ne $expectedMachine) {
        throw "$path is not a $Architecture binary."
    }
}
$version = (Get-Item -LiteralPath (Join-Path $binaries 'onnxruntime.dll')).VersionInfo.FileVersion
if ($version -notlike '1.22.*') {
    throw "onnxruntime.dll reports version '$version', expected 1.22.x."
}

$staging = "$OutputDir.tmp-$PID"
Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $staging | Out-Null
foreach ($name in $Outputs) {
    Copy-Item -LiteralPath (Join-Path $binaries $name) -Destination $staging
}
Invoke-Native 'python' @((Join-Path $RepoRoot 'shared\scripts\check_binary_privacy.py'), $staging)

$sums = foreach ($name in $Outputs) { "$(Get-Sha256 (Join-Path $staging $name))  $name" }
[IO.File]::WriteAllLines((Join-Path $staging 'SHA256SUMS'), [string[]]$sums)
[IO.File]::WriteAllLines((Join-Path $staging 'BUILDINFO.txt'), [string[]]@(
    "ONNX Runtime $OrtVersion ($OrtCommit), $Architecture, DirectML + CPU",
    "Built without --use_telemetry by platforms/windows/build/build-ort-runtime.ps1",
    "build.py $($arguments[1..($arguments.Count - 1)] -join ' ')"
))

if (Test-Path -LiteralPath $OutputDir) {
    Remove-Item -LiteralPath $OutputDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $OutputDir) | Out-Null
Move-Item -LiteralPath $staging -Destination $OutputDir
Write-Host "ONNX Runtime $OrtVersion ($Architecture, no telemetry): $OutputDir" -ForegroundColor Green
Get-Content -LiteralPath (Join-Path $OutputDir 'SHA256SUMS') | ForEach-Object { Write-Host "  $_" }

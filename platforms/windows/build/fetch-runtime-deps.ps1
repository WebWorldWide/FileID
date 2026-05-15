# Fetch ONNX Runtime (DirectML build) + DirectML runtime DLLs needed by
# the engine's CUDA/DirectML/CPU execution providers at LoadLibrary time.
#
# The `ort` Rust crate has a `download-binaries` Cargo feature that's
# meant to fetch these during build, but its build script silently falls
# through to "manual setup" without actually downloading anything (at
# least with the cuda + directml feature combo this project uses). So we
# fetch them ourselves from the canonical Microsoft channels:
#
#   - ORT 1.22.0 DirectML build → NuGet:Microsoft.ML.OnnxRuntime.DirectML
#   - DirectML 1.15.4 → NuGet:Microsoft.AI.DirectML
#
# Both are NuGet packages hosted on nuget.org and freely redistributable
# under Microsoft's own license — identical legal framing to the cuDNN
# fetch we do at runtime from NVIDIA's CDN. SHA pin is omitted only
# because NuGet immutable-by-version makes the pinned URL stable.
#
# Cached under `platforms/windows/build/runtime-cache/` so subsequent
# builds skip the download. Outputs full paths to the three DLLs the
# caller (build-all.ps1) needs to copy into the staging + install dirs.

param(
    [string]$OrtVersion = "1.22.0",
    [string]$DmlVersion = "1.15.4",
    [string]$CacheDir = ""
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $CacheDir = Join-Path $ScriptDir "runtime-cache"
}
New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null

$ortExtract = Join-Path $CacheDir "ort-directml-$OrtVersion"
$dmlExtract = Join-Path $CacheDir "directml-$DmlVersion"

function Fetch-NuGet {
    param([string]$Package, [string]$Version, [string]$Extract)
    if (Test-Path (Join-Path $Extract ".done")) { return }
    $tmpZip = Join-Path $env:TEMP "$Package.$Version.nupkg"
    $url = "https://www.nuget.org/api/v2/package/$Package/$Version"
    Write-Host "  fetching $Package $Version..." -ForegroundColor DarkGray
    Invoke-WebRequest -Uri $url -OutFile $tmpZip -UseBasicParsing
    if (Test-Path $Extract) { Remove-Item -Recurse -Force $Extract -ErrorAction SilentlyContinue }
    New-Item -ItemType Directory -Force -Path $Extract | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($tmpZip, $Extract)
    New-Item -ItemType File -Force -Path (Join-Path $Extract ".done") | Out-Null
    Remove-Item -Force $tmpZip -ErrorAction SilentlyContinue
}

Fetch-NuGet -Package "Microsoft.ML.OnnxRuntime.DirectML" -Version $OrtVersion -Extract $ortExtract
Fetch-NuGet -Package "Microsoft.AI.DirectML"            -Version $DmlVersion -Extract $dmlExtract

$ortNative = Join-Path $ortExtract "runtimes\win-x64\native"
$dmlBinX64 = Join-Path $dmlExtract "bin\x64-win"

$artifacts = @{
    "onnxruntime.dll"                  = Join-Path $ortNative "onnxruntime.dll"
    "onnxruntime_providers_shared.dll" = Join-Path $ortNative "onnxruntime_providers_shared.dll"
    "DirectML.dll"                     = Join-Path $dmlBinX64 "DirectML.dll"
}

foreach ($name in $artifacts.Keys) {
    if (-not (Test-Path $artifacts[$name])) {
        Write-Host "ERROR: missing $name at $($artifacts[$name])" -ForegroundColor Red
        exit 1
    }
}

# Print the resolved paths (caller parses these).
foreach ($name in $artifacts.Keys) {
    Write-Output "RUNTIME_DLL=$($artifacts[$name])"
}

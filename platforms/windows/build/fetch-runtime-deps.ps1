# Fetch ONNX Runtime (DirectML build) + DirectML runtime DLLs needed by
# the engine's CUDA/DirectML/CPU execution providers at LoadLibrary time,
# plus pdfium.dll for the `pdf-analyze` feature.
#
# The `ort` Rust crate has a `download-binaries` Cargo feature that's
# meant to fetch these during build, but its build script silently falls
# through to "manual setup" without actually downloading anything (at
# least with the cuda + directml feature combo this project uses). So we
# fetch them ourselves from the canonical channels:
#
#   - ORT 1.22.0 DirectML build → NuGet:Microsoft.ML.OnnxRuntime.DirectML
#   - DirectML 1.15.4 → NuGet:Microsoft.AI.DirectML
#   - pdfium 7857  → GitHub:bblanchon/pdfium-binaries (Apache 2.0; the
#                    upstream pdfium-render points users here)
#
# Microsoft NuGets are immutable-by-version so URL-pinning is sufficient.
# The pdfium tarball is content-addressed by GitHub release tag.
#
# Cached under `platforms/windows/build/runtime-cache/` so subsequent
# builds skip the download. Outputs full paths to the DLLs the caller
# (build-all.ps1) needs to copy into the staging + install dirs.

param(
    [string]$OrtVersion = "1.22.0",
    [string]$DmlVersion = "1.15.4",
    [string]$PdfiumVersion = "7857",
    [string]$CacheDir = ""
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($CacheDir)) {
    $ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $CacheDir = Join-Path $ScriptDir "runtime-cache"
}
New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null

$ortExtract    = Join-Path $CacheDir "ort-directml-$OrtVersion"
$dmlExtract    = Join-Path $CacheDir "directml-$DmlVersion"
$pdfiumExtract = Join-Path $CacheDir "pdfium-$PdfiumVersion"

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

# bblanchon/pdfium-binaries publishes a .tgz per chromium revision; tar.exe
# is built into Windows 10+ so we don't need 7-zip.
function Fetch-Pdfium {
    param([string]$Version, [string]$Extract)
    if (Test-Path (Join-Path $Extract ".done")) { return }
    $tmpTgz = Join-Path $env:TEMP "pdfium-win-x64-$Version.tgz"
    $url = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$Version/pdfium-win-x64.tgz"
    Write-Host "  fetching pdfium $Version..." -ForegroundColor DarkGray
    Invoke-WebRequest -Uri $url -OutFile $tmpTgz -UseBasicParsing
    if (Test-Path $Extract) { Remove-Item -Recurse -Force $Extract -ErrorAction SilentlyContinue }
    New-Item -ItemType Directory -Force -Path $Extract | Out-Null
    & tar -xzf $tmpTgz -C $Extract
    if ($LASTEXITCODE -ne 0) { throw "pdfium tar extraction failed (exit $LASTEXITCODE)" }
    New-Item -ItemType File -Force -Path (Join-Path $Extract ".done") | Out-Null
    Remove-Item -Force $tmpTgz -ErrorAction SilentlyContinue
}

Fetch-NuGet -Package "Microsoft.ML.OnnxRuntime.DirectML" -Version $OrtVersion -Extract $ortExtract
Fetch-NuGet -Package "Microsoft.AI.DirectML"            -Version $DmlVersion -Extract $dmlExtract
Fetch-Pdfium -Version $PdfiumVersion -Extract $pdfiumExtract

$ortNative = Join-Path $ortExtract "runtimes\win-x64\native"
$dmlBinX64 = Join-Path $dmlExtract "bin\x64-win"
$pdfiumBin = Join-Path $pdfiumExtract "bin"

$artifacts = @{
    "onnxruntime.dll"                  = Join-Path $ortNative "onnxruntime.dll"
    "onnxruntime_providers_shared.dll" = Join-Path $ortNative "onnxruntime_providers_shared.dll"
    "DirectML.dll"                     = Join-Path $dmlBinX64 "DirectML.dll"
    "pdfium.dll"                       = Join-Path $pdfiumBin "pdfium.dll"
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

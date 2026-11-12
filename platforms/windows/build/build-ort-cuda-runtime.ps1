# Build the ONNX Runtime CUDA pack FileID loads on NVIDIA cards: ONNX Runtime
# 1.22.0 with the CUDA and CPU execution providers, from Microsoft's source at
# the v1.22.0 commit, WITHOUT --use_telemetry, plus the NVIDIA runtime DLLs its
# provider imports. Microsoft's own onnxruntime-win-x64-gpu zip is built with
# that flag and the engine refuses it (DECISIONS 2026-11-10), so NVIDIA cards
# ran DirectML until this pack existed. Same approach as build-ort-runtime.ps1
# (which stays untouched so its CI cache keeps its key).
#
# Output (the layout the engine finds under Models\packs\cuda):
#   onnxruntime.dll, onnxruntime_providers_shared.dll,
#   onnxruntime_providers_cuda.dll        built here, no telemetry
#   cudart64_12 cublas64_12 cublasLt64_12 cufft64_11 curand64_10
#   nvrtc64_120_0 nvrtc-builtins64_128    CUDA 12.8.1, NVIDIA redistributables
#   cudnn*64_9.dll                        cuDNN 9.8.0, NVIDIA redistributable
#   licenses\                             the NVIDIA agreements, SHA256SUMS, BUILDINFO.txt
#
# NVIDIA's component archives (compiler, headers, import libraries and the DLLs)
# come from developer.download.nvidia.com/compute/{cuda,cudnn}/redist, each
# pinned by SHA-256 below. Only DLLs named in Attachment A of the CUDA EULA and
# the cuDNN supplement are copied into the pack. The pack is opt-in: the app
# shows the agreements before it installs (ModelLicenseGate).
#
# Needs Visual Studio 2022 (C++ desktop), CMake 3.28+, Python 3.10+ and git.
# Takes a few hours on a hosted runner (CUDA kernels).

[CmdletBinding()]
param(
    [string]$OutputDir = '',
    # Kept short: ORT's build tree is deep enough to hit MAX_PATH.
    [string]$WorkDir = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$OrtVersion = '1.22.0'
$OrtCommit = 'f217402897f40ebba457e2421bc0a4702771968e'
$OrtRepository = 'https://github.com/microsoft/onnxruntime.git'
$CudaVersion = '12.8.1'
$CudnnVersion = '9.8.0'
# SASS for Pascal, Turing, Ampere, Ada; Blackwell (RTX 50) as SASS plus PTX so
# newer cards JIT. ORT's default for CUDA 12.8 is "all", which doubles the build.
$CudaArchitectures = '61-real;75-real;86-real;89-real;120'

$CudaRedist = 'https://developer.download.nvidia.com/compute/cuda/redist/'
$CudnnRedist = 'https://developer.download.nvidia.com/compute/cudnn/redist/'
# Toolchain components (headers, import libraries, nvcc) and the DLLs shipped in the pack.
$CudaArchives = @(
    @{ Path = 'cuda_cccl/windows-x86_64/cuda_cccl-windows-x86_64-12.8.90-archive.zip'; Sha256 = 'bd8548fa1ae82f92910bebc3079e14bd58c5a92aa64596d46bd610a478cb39d7' },
    @{ Path = 'cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.8.90-archive.zip'; Sha256 = '4a39058fd8519444a81cfc7ae055d136f48d1a31ffa41ae255b35b2edd61e13b' },
    @{ Path = 'cuda_nvcc/windows-x86_64/cuda_nvcc-windows-x86_64-12.8.93-archive.zip'; Sha256 = '9fdc70b4271ed9aad4d64cd7076a7d96ec36512d074b9995fe638de669197391' },
    @{ Path = 'cuda_nvrtc/windows-x86_64/cuda_nvrtc-windows-x86_64-12.8.93-archive.zip'; Sha256 = 'a63302a077f0248a743a1a7caa7dbd80d0fac56c6cfa9c41fa05fac9b7e5eda5' },
    @{ Path = 'cuda_nvtx/windows-x86_64/cuda_nvtx-windows-x86_64-12.8.90-archive.zip'; Sha256 = '38dbd5941537fcb359137f26fe31b4e3528c504cc2a55fa10a9baaf0ff006187' },
    @{ Path = 'cuda_nvml_dev/windows-x86_64/cuda_nvml_dev-windows-x86_64-12.8.90-archive.zip'; Sha256 = '8504c77c1f28def5d66d7f1c71003bcf02083935c8d490e3538a1dda683222b7' },
    @{ Path = 'cuda_profiler_api/windows-x86_64/cuda_profiler_api-windows-x86_64-12.8.90-archive.zip'; Sha256 = '0a9d628d2582bd58c78ba92827cd9613d62790924cde1178719f677fc03b6ee2' },
    @{ Path = 'libcublas/windows-x86_64/libcublas-windows-x86_64-12.8.4.1-archive.zip'; Sha256 = '57a470112cec7e112c95253dde8b3c7184d795dbd92b0bde77a4cb7f8c94c8aa' },
    @{ Path = 'libcufft/windows-x86_64/libcufft-windows-x86_64-11.3.3.83-archive.zip'; Sha256 = 'cc6e0ba958cf23387b462017a24464c72bd901549046133f3d1ebcc3d7444c90' },
    @{ Path = 'libcurand/windows-x86_64/libcurand-windows-x86_64-10.3.9.90-archive.zip'; Sha256 = '46eba36c20748b21a5927b569b943337dd18485132119dfb39a5bcda4edb12e2' }
)
$VsIntegrationArchive = @{ Path = 'visual_studio_integration/windows-x86_64/visual_studio_integration-windows-x86_64-12.8.90-archive.zip'; Sha256 = 'f41d12a0e49b7848ed35e8a15b58926b83f635c723cab7e9952bc633e3c1f200' }
$CudnnArchive = @{ Path = 'cudnn/windows-x86_64/cudnn-windows-x86_64-9.8.0.87_cuda12-archive.zip'; Sha256 = 'd8a23705e3884b137b7e05449fb2b61bfa524e7cfc3fda80743d633f423c6ce4' }
$CudaLicense = @{ Url = "${CudaRedist}cuda_cudart/LICENSE.txt"; Sha256 = 'e2c71babfd18a8e69542dd7e9ca018f9caa438094001a58e6bc4d8c999bf0d07' }
$CudnnLicense = @{ Url = "${CudnnRedist}cudnn/LICENSE.txt"; Sha256 = '4fce5a3b1ecaf4d7d202478e969e4563c7220db2718a76067e286c0202e1195f' }
$CudaPackDlls = @(
    'cudart64_12.dll', 'cublas64_12.dll', 'cublasLt64_12.dll', 'cufft64_11.dll',
    'curand64_10.dll', 'nvrtc64_120_0.dll', 'nvrtc-builtins64_128.dll'
)
$OrtOutputs = @('onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'onnxruntime_providers_cuda.dll')

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir '..\..\..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $ScriptDir "runtime-cache\onnxruntime-$OrtVersion-cuda-notelemetry\x64"
}
if ([string]::IsNullOrWhiteSpace($WorkDir)) {
    $WorkDir = Join-Path $env:SystemDrive 'fileid-ort-cuda'
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$WorkDir = [IO.Path]::GetFullPath($WorkDir)
$SourceDir = Join-Path $WorkDir 'src'
$BuildDir = Join-Path $WorkDir 'b-x64'
$DownloadDir = Join-Path $WorkDir 'dl'
$CudaHome = Join-Path $WorkDir 'cuda'
$CudnnHome = Join-Path $WorkDir 'cudnn'

function Invoke-Native([string]$Executable, [string[]]$Arguments) {
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Executable $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
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

function Get-PinnedDownload([string]$Url, [string]$Sha256, [string]$Destination) {
    if (Test-Path -LiteralPath $Destination -PathType Leaf) {
        if ((Get-Sha256 $Destination) -eq $Sha256) { return }
        Remove-Item -LiteralPath $Destination -Force
    }
    $temporary = "$Destination.tmp-$PID"
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Write-Host "  fetching $Url (attempt $attempt/3)..." -ForegroundColor DarkGray
            Invoke-WebRequest -Uri $Url -OutFile $temporary -UseBasicParsing
            $actual = Get-Sha256 $temporary
            if ($actual -ne $Sha256) {
                throw "SHA-256 mismatch for ${Url}: expected $Sha256, got $actual."
            }
            Move-Item -LiteralPath $temporary -Destination $Destination -Force
            return
        } catch {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            if ($attempt -eq 3 -or $_.Exception.Message -like 'SHA-256 mismatch*') { throw }
            Start-Sleep -Seconds (5 * $attempt)
        }
    }
}

# Expands one NVIDIA component archive (a single top-level folder) into $Target.
function Expand-NvidiaArchive([string]$BaseUrl, [hashtable]$Archive, [string]$Target) {
    $zip = Join-Path $DownloadDir ([IO.Path]::GetFileName($Archive.Path))
    Get-PinnedDownload ($BaseUrl + $Archive.Path) $Archive.Sha256 $zip
    $scratch = Join-Path $DownloadDir ('x-' + [IO.Path]::GetFileNameWithoutExtension($zip))
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $scratch)
    $roots = @(Get-ChildItem -LiteralPath $scratch -Directory)
    if ($roots.Count -ne 1) {
        throw "$zip does not contain exactly one top-level folder."
    }
    New-Item -ItemType Directory -Force -Path $Target | Out-Null
    Copy-Item -Path (Join-Path $roots[0].FullName '*') -Destination $Target -Recurse -Force
    Remove-Item -LiteralPath $scratch -Recurse -Force
    Remove-Item -LiteralPath $zip -Force
}

New-Item -ItemType Directory -Force -Path $DownloadDir | Out-Null
foreach ($directory in @($CudaHome, $CudnnHome)) {
    Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "Fetching NVIDIA CUDA $CudaVersion and cuDNN $CudnnVersion (SHA-256 pinned)..." -ForegroundColor Cyan
foreach ($archive in $CudaArchives) {
    Expand-NvidiaArchive $CudaRedist $archive $CudaHome
}
Expand-NvidiaArchive $CudnnRedist $CudnnArchive $CudnnHome
$licenseDir = Join-Path $DownloadDir 'licenses'
New-Item -ItemType Directory -Force -Path $licenseDir | Out-Null
Get-PinnedDownload $CudaLicense.Url $CudaLicense.Sha256 (Join-Path $licenseDir 'NVIDIA-CUDA-LICENSE.txt')
Get-PinnedDownload $CudnnLicense.Url $CudnnLicense.Sha256 (Join-Path $licenseDir 'NVIDIA-cuDNN-LICENSE.txt')

# The Visual Studio generator needs NVIDIA's MSBuild integration (CUDA.props and
# friends) in the VC build customizations folder.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsRoot = (& $vswhere -latest -version '[17.0,18.0)' -products '*' -property installationPath | Select-Object -First 1)
if ([string]::IsNullOrWhiteSpace($vsRoot)) {
    throw 'Visual Studio 2022 was not found.'
}
$integration = Join-Path $DownloadDir 'vs-integration'
Expand-NvidiaArchive $CudaRedist $VsIntegrationArchive $integration
$customizations = Join-Path $vsRoot 'MSBuild\Microsoft\VC\v170\BuildCustomizations'
$integrationFiles = @(Get-ChildItem -LiteralPath $integration -Recurse -File | Where-Object { $_.Directory.Name -eq 'MSBuildExtensions' })
if ($integrationFiles.Count -eq 0) {
    throw 'The CUDA Visual Studio integration archive has no MSBuildExtensions folder.'
}
$integrationFiles | Copy-Item -Destination $customizations -Force
# CMake's "cuda=<path>" toolset also looks for them inside the toolkit folder.
$toolkitIntegration = Join-Path $CudaHome 'extras\visual_studio_integration\MSBuildExtensions'
New-Item -ItemType Directory -Force -Path $toolkitIntegration | Out-Null
$integrationFiles | Copy-Item -Destination $toolkitIntegration -Force

foreach ($required in @('bin\nvcc.exe', 'include\cuda_runtime.h', 'include\cublas_v2.h', 'include\cufft.h', 'lib\x64\cudart.lib', 'lib\x64\cublas.lib')) {
    if (-not (Test-Path -LiteralPath (Join-Path $CudaHome $required))) {
        throw "The CUDA toolkit assembled from NVIDIA's archives lacks $required."
    }
}
foreach ($required in @('include\cudnn.h', 'lib\x64\cudnn.lib', 'bin\cudnn64_9.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $CudnnHome $required))) {
        throw "The cuDNN archive lacks $required."
    }
}
$env:CUDA_PATH = $CudaHome
$env:CUDA_PATH_V12_8 = $CudaHome

New-Item -ItemType Directory -Force -Path $SourceDir | Out-Null
if (-not (Test-Path -LiteralPath (Join-Path $SourceDir '.git'))) {
    Invoke-Native 'git' @('-C', $SourceDir, 'init', '--quiet')
}
# Keep the commit's LF bytes: with Git for Windows' default autocrlf the patch
# files under cmake/patches become CRLF, and the patch.exe CMake finds first on
# hosted runners (Strawberry Perl's 2.5.9) crashes on them (arm64 cpuinfo patch).
Invoke-Native 'git' @('-C', $SourceDir, 'config', 'core.autocrlf', 'false')
Invoke-Native 'git' @('-C', $SourceDir, 'config', 'core.eol', 'lf')
$head = (& git -C $SourceDir rev-parse --verify --quiet HEAD) | Select-Object -First 1
if ($head -ne $OrtCommit) {
    Write-Host "Fetching ONNX Runtime $OrtVersion ($OrtCommit)..." -ForegroundColor Cyan
    Invoke-Native 'git' @('-C', $SourceDir, 'fetch', '--quiet', '--depth', '1', $OrtRepository, $OrtCommit)
    Invoke-Native 'git' @('-C', $SourceDir, 'checkout', '--quiet', '--force', 'FETCH_HEAD')
}
Invoke-Native 'git' @('-C', $SourceDir, 'reset', '--quiet', '--hard', $OrtCommit)
Invoke-Native 'git' @('-C', $SourceDir, 'clean', '-xdfq')
Invoke-Native 'git' @('-C', $SourceDir, 'checkout-index', '--quiet', '--force', '--all')
$head =(& git -C $SourceDir rev-parse HEAD).Trim()
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

# CMake's FindPatch picks Strawberry Perl's patch.exe 2.5.9 first on hosted
# runners, and it asserts on ORT's arm64 cpuinfo patch even with LF sources.
# Use Git for Windows' GNU patch (2.7+) instead.
$gitExe = (Get-Command git -ErrorAction Stop).Source
$gitRoot = Split-Path -Parent (Split-Path -Parent $gitExe)
$gnuPatch = Join-Path $gitRoot 'usr\bin\patch.exe'
if (-not (Test-Path -LiteralPath $gnuPatch)) {
    throw "GNU patch not found at $gnuPatch (expected beside Git for Windows)."
}
Write-Host "Using patch: $gnuPatch" -ForegroundColor Cyan


$cudaHomeArg = $CudaHome -replace '\\', '/'
$cudnnHomeArg = $CudnnHome -replace '\\', '/'
$arguments = @(
    'tools\ci_build\build.py',
    '--build_dir', $BuildDir,
    '--config', 'Release',
    '--update', '--build', '--parallel',
    '--skip_submodule_sync', '--skip_tests',
    '--build_shared_lib',
    '--use_cuda', '--cuda_home', $cudaHomeArg, '--cudnn_home', $cudnnHomeArg,
    '--use_binskim_compliant_compile_flags',
    '--compile_no_warning_as_error',
    '--cmake_generator', 'Visual Studio 17 2022',
    '--cmake_extra_defines', 'onnxruntime_BUILD_UNIT_TESTS=OFF', 'onnxruntime_USE_TELEMETRY=OFF',
    "CMAKE_CUDA_ARCHITECTURES=$CudaArchitectures",
    'onnxruntime_USE_FLASH_ATTENTION=OFF', 'onnxruntime_USE_LEAN_ATTENTION=OFF', 'onnxruntime_USE_MEMORY_EFFICIENT_ATTENTION=OFF',
    "Patch_EXECUTABLE=$($gnuPatch -replace '\\', '/')"
)
Write-Host "Building ONNX Runtime $OrtVersion (CUDA $CudaVersion, x64, no telemetry)..." -ForegroundColor Cyan
Push-Location $SourceDir
try {
    Invoke-Native 'python' $arguments
} finally {
    Pop-Location
}

$binaries = Join-Path $BuildDir 'Release\Release'
foreach ($name in $OrtOutputs) {
    $path = Join-Path $binaries $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "The ONNX Runtime build did not produce $path."
    }
    if ((Get-PeMachine $path) -ne 0x8664) {
        throw "$path is not an x64 binary."
    }
}
$version = (Get-Item -LiteralPath (Join-Path $binaries 'onnxruntime.dll')).VersionInfo.FileVersion
if ($version -notlike '1.22.*') {
    throw "onnxruntime.dll reports version '$version', expected 1.22.x."
}

$staging = "$OutputDir.tmp-$PID"
Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $staging | Out-Null
foreach ($name in $OrtOutputs) {
    Copy-Item -LiteralPath (Join-Path $binaries $name) -Destination $staging
}
foreach ($name in $CudaPackDlls) {
    $source = Join-Path $CudaHome "bin\$name"
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "NVIDIA's archives did not provide $name."
    }
    Copy-Item -LiteralPath $source -Destination $staging
}
$cudnnDlls = @(Get-ChildItem -LiteralPath (Join-Path $CudnnHome 'bin') -File -Filter 'cudnn*64_9.dll')
if (-not ($cudnnDlls | Where-Object { $_.Name -eq 'cudnn64_9.dll' })) {
    throw 'The cuDNN archive has no cudnn64_9.dll.'
}
$cudnnDlls | Copy-Item -Destination $staging
New-Item -ItemType Directory -Force -Path (Join-Path $staging 'licenses') | Out-Null
Copy-Item -Path (Join-Path $licenseDir '*') -Destination (Join-Path $staging 'licenses')

# Every DLL the CUDA provider imports must be in the pack or in Windows itself;
# a missing one makes ORT fall back to CPU without an error (the engine's
# CUDA_STACK_REQUIRED list is the same closure).
$dumpbin = (& $vswhere -latest -version '[17.0,18.0)' -products '*' -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1)
if ([string]::IsNullOrWhiteSpace($dumpbin)) {
    throw 'dumpbin.exe not found.'
}
$packNames = @(Get-ChildItem -LiteralPath $staging -File -Filter *.dll | ForEach-Object { $_.Name.ToLowerInvariant() })
$imports = & $dumpbin /nologo /dependents (Join-Path $staging 'onnxruntime_providers_cuda.dll')
if ($LASTEXITCODE -ne 0) { throw 'dumpbin /dependents failed.' }
$missing = @($imports | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^[\w.\-]+\.dll$' } | ForEach-Object { $_.ToLowerInvariant() } | Where-Object {
    $_ -notin $packNames -and -not (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32\$_")) -and $_ -notmatch '^(api-ms-win-|ext-ms-win-)'
})
if ($missing.Count -gt 0) {
    throw "onnxruntime_providers_cuda.dll imports DLLs that are neither in the pack nor in Windows: $($missing -join ', ')"
}

Invoke-Native 'python' @((Join-Path $RepoRoot 'shared\scripts\check_binary_privacy.py'), $staging)

$sums = foreach ($file in (Get-ChildItem -LiteralPath $staging -File -Recurse | Sort-Object FullName)) {
    $relative = $file.FullName.Substring($staging.Length + 1).Replace('\', '/')
    "$(Get-Sha256 $file.FullName)  $relative"
}
[IO.File]::WriteAllLines((Join-Path $staging 'SHA256SUMS'), [string[]]$sums)
[IO.File]::WriteAllLines((Join-Path $staging 'BUILDINFO.txt'), [string[]]@(
    "ONNX Runtime $OrtVersion ($OrtCommit), x64, CUDA $CudaVersion + cuDNN $CudnnVersion, CUDA architectures $CudaArchitectures",
    "Built without --use_telemetry by platforms/windows/build/build-ort-cuda-runtime.ps1",
    "build.py $($arguments[1..($arguments.Count - 1)] -join ' ')"
))

if (Test-Path -LiteralPath $OutputDir) {
    Remove-Item -LiteralPath $OutputDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $OutputDir) | Out-Null
Move-Item -LiteralPath $staging -Destination $OutputDir
Write-Host "ONNX Runtime $OrtVersion CUDA pack (x64, no telemetry): $OutputDir" -ForegroundColor Green
Get-Content -LiteralPath (Join-Path $OutputDir 'SHA256SUMS') | ForEach-Object { Write-Host "  $_" }

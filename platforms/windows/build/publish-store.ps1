[CmdletBinding()]
param(
    [ValidateSet('x64')]
    [string]$Architecture = 'x64',
    [switch]$SkipEngineBuild,
    [string]$EnginePath = '',
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$platformRoot = Split-Path $PSScriptRoot -Parent
$repoRoot = (Resolve-Path (Join-Path $platformRoot '../..')).Path
$engineRoot = Join-Path $platformRoot 'src/engine'
$version = (Get-Content (Join-Path $platformRoot 'VERSION') -Raw).Trim()
& (Join-Path $PSScriptRoot 'verify-version.ps1')
$packageVersion = "$version.0"
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $platformRoot 'dist/store'
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$stage = Join-Path $OutputDirectory ("stage-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null

function Invoke-Checked {
    param([string]$Executable, [string[]]$Arguments)
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Executable failed with exit code $LASTEXITCODE." }
}

$vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
if (-not (Test-Path $vswhere)) { throw 'Visual Studio with WinUI build tools is required.' }
$msbuild = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild/**/Bin/MSBuild.exe' |
    Select-Object -First 1
if (-not $msbuild) { throw 'MSBuild was not found by vswhere.' }
$sdkBin = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin" -Directory |
    Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' } |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64/MakeAppx.exe' } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $sdkBin) { throw 'MakeAppx.exe from the Windows SDK is required.' }

if (-not $SkipEngineBuild) {
    Push-Location $engineRoot
    try {
        Invoke-Checked 'cargo' @('build', '--locked', '--release', '--target', 'x86_64-pc-windows-msvc')
    } finally { Pop-Location }
}
if ([string]::IsNullOrWhiteSpace($EnginePath)) {
    $EnginePath = Join-Path $engineRoot 'target/x86_64-pc-windows-msvc/release/FileIDEngine.exe'
}
if (-not (Test-Path -LiteralPath $EnginePath -PathType Leaf)) { throw "Engine not found: $EnginePath" }

Invoke-Checked $msbuild @(
    (Join-Path $platformRoot 'src/FileID.App/FileID.App.csproj'),
    '/restore', '/t:Publish', '/p:Configuration=Release', "/p:Platform=$Architecture",
    "/p:RuntimeIdentifier=win-$Architecture", '/p:SelfContained=true',
    '/p:FileIDStoreBuild=true', "/p:PublishDir=$stage/", '/m', '/nologo'
)
Copy-Item -LiteralPath $EnginePath -Destination (Join-Path $stage 'FileIDEngine.exe')
$runtimeOutput = & (Join-Path $PSScriptRoot 'fetch-runtime-deps.ps1')
foreach ($line in $runtimeOutput) {
    if ([string]$line -match '^RUNTIME_DLL=(.+)$') {
        Copy-Item -LiteralPath $Matches[1] -Destination $stage
    }
}
foreach ($required in @('FileID.exe', 'FileIDEngine.exe', 'FileID.pri', 'onnxruntime.dll', 'DirectML.dll', 'pdfium.dll')) {
    if (-not (Test-Path (Join-Path $stage $required))) { throw "Missing package payload: $required" }
}
Copy-Item -Path (Join-Path $platformRoot 'store/Assets/*.png') -Destination (Join-Path $stage 'Assets')
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination (Join-Path $stage 'LICENSE.txt')

[xml]$manifest = Get-Content (Join-Path $platformRoot 'store/AppxManifest.xml') -Raw
$manifest.Package.Identity.Version = $packageVersion
$manifest.Package.Identity.ProcessorArchitecture = $Architecture
$manifest.Save((Join-Path $stage 'AppxManifest.xml'))
Invoke-Checked 'python' @((Join-Path $repoRoot 'shared/scripts/check_binary_privacy.py'), $stage)
$package = Join-Path $OutputDirectory "FileID-$version-$Architecture.msix"
Invoke-Checked $sdkBin @('pack', '/d', $stage, '/p', $package, '/o')
& (Join-Path $PSScriptRoot 'verify-store-package.ps1') -Path $package
$hash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$package.sha256", "$hash  $([IO.Path]::GetFileName($package))`n")
Write-Host "Store package: $package"
Write-Host 'Unsigned upload artifact; Microsoft signs the distributed Store package.'
Write-Host 'Complete the release blockers in shared/docs/WINDOWS_STORE.md before submission.'

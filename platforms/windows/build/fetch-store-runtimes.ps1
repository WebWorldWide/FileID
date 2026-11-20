[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot,

    [Parameter(Mandatory = $true)]
    [string]$CacheDir
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$CacheDir = [IO.Path]::GetFullPath($CacheDir)
New-Item -ItemType Directory -Force -Path $OutputRoot, $CacheDir | Out-Null

function Get-VerifiedFile([string]$Name, [string]$Uri, [string]$Sha256) {
    $path = Join-Path $CacheDir $Name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Invoke-WebRequest -Uri $Uri -OutFile $path -UseBasicParsing
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) {
        throw "SHA-256 mismatch for $Name; expected $Sha256, got $actual."
    }
    return $path
}

function Expand-VerifiedZip([string]$Name, [string]$Uri, [string]$Sha256, [string]$Destination) {
    $archive = Get-VerifiedFile $Name $Uri $Sha256
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    [IO.Compression.ZipFile]::ExtractToDirectory($archive, $Destination, $true)
}

$llamaRoot = Join-Path $OutputRoot 'llama.cpp'
$whisperRoot = Join-Path $OutputRoot 'whisper.cpp'

Expand-VerifiedZip `
    'llama-b9254-bin-win-vulkan-x64.zip' `
    'https://github.com/ggml-org/llama.cpp/releases/download/b9254/llama-b9254-bin-win-vulkan-x64.zip' `
    '45d276bdf73c80c795860e8421fe27ee4b1aa0a4d0916fe60dfc90dca1d4117b' `
    $llamaRoot
Copy-Item -LiteralPath (Get-VerifiedFile `
    'llama.cpp-b9254-LICENSE.txt' `
    'https://raw.githubusercontent.com/ggml-org/llama.cpp/b9254/LICENSE' `
    '94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d') `
    -Destination (Join-Path $llamaRoot 'LICENSE')

Expand-VerifiedZip `
    'whisper-bin-x64.zip' `
    'https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.0/whisper-bin-x64.zip' `
    '00c4304b6be363a224a4b69829df49009f74131df8c3ce6a5878b89a11cd26ef' `
    $whisperRoot
Copy-Item -LiteralPath (Get-VerifiedFile `
    'whisper.cpp-v1.9.0-LICENSE.txt' `
    'https://raw.githubusercontent.com/ggml-org/whisper.cpp/v1.9.0/LICENSE' `
    '94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d') `
    -Destination (Join-Path $whisperRoot 'LICENSE')

$notices = @(
    'FileID includes the following third-party runtime components:',
    '',
    'llama.cpp b9254 Windows Vulkan runtime',
    'Source: https://github.com/ggml-org/llama.cpp/tree/b9254',
    'License: MIT; see RuntimeBundles/llama.cpp/LICENSE.',
    '',
    'whisper.cpp v1.9.0 Windows runtime',
    'Source: https://github.com/ggml-org/whisper.cpp/tree/v1.9.0',
    'License: MIT; see RuntimeBundles/whisper.cpp/LICENSE.'
)
[IO.File]::WriteAllLines(
    (Join-Path $OutputRoot 'THIRD-PARTY-NOTICES.txt'),
    $notices,
    [Text.UTF8Encoding]::new($false))

Write-Host 'Verified and staged llama.cpp Vulkan and whisper.cpp runtimes.'

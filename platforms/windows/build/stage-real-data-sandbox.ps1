#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$OutputRoot = (
        Join-Path `
            ([IO.Path]::GetTempPath()) `
            ("FileID-real-data-sandbox-" + [DateTime]::UtcNow.ToString("yyyyMMddTHHmmssZ"))
    ),
    [string]$CorpusPath,
    [switch]$AllowAdlonDrive,
    [string]$CorpusProbeRelativePath,
    [Parameter(Mandatory)]
    [string]$EnginePath,
    [Parameter(Mandatory)]
    [ValidateRange(1, [long]::MaxValue)]
    [Alias("ExpectedEngineBytes")]
    [long]$ExpectedEngineSize,
    [Parameter(Mandatory)]
    [ValidatePattern("^[0-9A-Fa-f]{64}$")]
    [string]$ExpectedEngineSha256,
    [string]$OrtRuntimeDirectory,
    [string]$DirectMLPath,
    [Parameter(Mandatory)]
    [string]$LlamaRuntimePath,
    [string]$ModelsPath,
    [string]$SeedDbPath,
    [string]$FaceCropsPath,
    [string]$PythonHome,
    [switch]$Launch,
    [switch]$WaitForPreflight,
    [switch]$AutoClose,
    [ValidateRange(60, 3600)]
    [int]$TimeoutSeconds = 600
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if (-not [string]::IsNullOrWhiteSpace($SeedDbPath)) {
    throw (
        "SeedDbPath is prohibited. Validation must create a fresh catalog " +
        "inside the writable Sandbox validation mount."
    )
}

if ([string]::IsNullOrWhiteSpace($OrtRuntimeDirectory)) {
    $OrtRuntimeDirectory = Join-Path `
        $PSScriptRoot `
        "runtime-cache\ort-directml-1.22.0\runtimes\win-x64\native"
}
if ([string]::IsNullOrWhiteSpace($DirectMLPath)) {
    $DirectMLPath = Join-Path `
        $PSScriptRoot `
        "runtime-cache\directml-1.15.4\bin\x64-win\DirectML.dll"
}

function Get-UtcTimestamp {
    return [DateTime]::UtcNow.ToString("o")
}

function Write-JsonAtomic {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [object]$Value
    )

    $parent = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    $temporary = "$Path.tmp"
    $json = $Value | ConvertTo-Json -Depth 30
    [IO.File]::WriteAllText(
        $temporary,
        "$json`n",
        [Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Get-Sha256 {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    )
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString(
            $algorithm.ComputeHash($stream)
        ).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
        $stream.Dispose()
    }
}

function Resolve-ExistingFile {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Label
    )

    $full = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
        throw "$Label does not exist: $full"
    }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label cannot be a reparse point: $full"
    }
    return $item.FullName
}

function Resolve-ExistingDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Label
    )

    $full = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $full -PathType Container)) {
        throw "$Label does not exist: $full"
    }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label cannot be a reparse point: $full"
    }
    return $item.FullName.TrimEnd("\")
}

function Resolve-RelativeChild {
    param(
        [Parameter(Mandatory)]
        [string]$Root,
        [Parameter(Mandatory)]
        [string]$RelativePath
    )

    if ([IO.Path]::IsPathRooted($RelativePath)) {
        throw "Expected a relative path, got: $RelativePath"
    }
    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd("\")
    $candidate = [IO.Path]::GetFullPath((Join-Path $rootFull $RelativePath))
    if (-not $candidate.StartsWith(
        "$rootFull\",
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Path escapes its configured root: $RelativePath"
    }
    return $candidate
}

function Get-RelativePath {
    param(
        [Parameter(Mandatory)]
        [string]$Root,
        [Parameter(Mandatory)]
        [string]$Path
    )

    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd("\")
    $pathFull = [IO.Path]::GetFullPath($Path)
    if (-not $pathFull.StartsWith(
        "$rootFull\",
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Path is not inside the requested root: $pathFull"
    }
    return $pathFull.Substring($rootFull.Length + 1)
}

function Test-PathsOverlap {
    param(
        [Parameter(Mandatory)]
        [string]$Left,
        [Parameter(Mandatory)]
        [string]$Right
    )

    $leftFull = [IO.Path]::GetFullPath($Left).TrimEnd("\")
    $rightFull = [IO.Path]::GetFullPath($Right).TrimEnd("\")
    return (
        $leftFull.Equals($rightFull, [StringComparison]::OrdinalIgnoreCase) -or
        $leftFull.StartsWith(
            "$rightFull\",
            [StringComparison]::OrdinalIgnoreCase
        ) -or
        $rightFull.StartsWith(
            "$leftFull\",
            [StringComparison]::OrdinalIgnoreCase
        )
    )
}

function Get-DirectoryCopyPlan {
    param(
        [Parameter(Mandatory)]
        [string]$Source
    )

    $sourceFull = [IO.Path]::GetFullPath($Source).TrimEnd("\")
    $sourceItem = Get-Item -LiteralPath $sourceFull -Force
    if (-not $sourceItem.PSIsContainer) {
        throw "Directory copy source is not a directory: $sourceFull"
    }
    if (
        ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        throw "Source tree root is a reparse point: $sourceFull"
    }
    $plan = [Collections.Generic.List[object]]::new()
    $pending = [Collections.Generic.Queue[string]]::new()
    $pending.Enqueue($sourceFull)
    while ($pending.Count -gt 0) {
        $current = $pending.Dequeue()
        foreach ($item in Get-ChildItem -LiteralPath $current -Force) {
            if (
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
            ) {
                throw "Source tree contains a reparse point: $($item.FullName)"
            }
            $relativePath = Get-RelativePath `
                -Root $sourceFull `
                -Path $item.FullName
            $plan.Add([pscustomobject]@{
                Source = $item.FullName
                RelativePath = $relativePath
                IsDirectory = [bool]$item.PSIsContainer
            })
            if ($item.PSIsContainer) {
                $pending.Enqueue($item.FullName)
            }
        }
    }
    return $plan.ToArray()
}

function Assert-DirectoryTreeHasNoReparsePoint {
    param(
        [Parameter(Mandatory)]
        [string]$Source
    )

    $null = @(Get-DirectoryCopyPlan -Source $Source)
}

function Copy-DirectoryContents {
    param(
        [Parameter(Mandatory)]
        [string]$Source,
        [Parameter(Mandatory)]
        [string]$Destination
    )

    if (Test-Path -LiteralPath $Destination) {
        throw "Staging destination already exists: $Destination"
    }
    $sourceFull = [IO.Path]::GetFullPath($Source).TrimEnd("\")
    $destinationFull = [IO.Path]::GetFullPath($Destination).TrimEnd("\")
    $plan = @(Get-DirectoryCopyPlan -Source $sourceFull)
    New-Item -ItemType Directory -Path $destinationFull | Out-Null
    foreach ($entry in $plan) {
        $sourceItem = Get-Item -LiteralPath $entry.Source -Force
        if (
            ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "Source entry became a reparse point: $($entry.Source)"
        }
        if ([bool]$sourceItem.PSIsContainer -ne [bool]$entry.IsDirectory) {
            throw "Source entry type changed while staging: $($entry.Source)"
        }
        $target = Resolve-RelativeChild `
            -Root $destinationFull `
            -RelativePath ([string]$entry.RelativePath)
        if ($entry.IsDirectory) {
            New-Item -ItemType Directory -Path $target | Out-Null
        } else {
            Copy-Item `
                -LiteralPath $entry.Source `
                -Destination $target `
                -Force
        }
    }
}

function Get-TreeSummary {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $files = @(Get-ChildItem -LiteralPath $Path -Recurse -File -Force)
    $bytes = [int64]0
    foreach ($file in $files) {
        $bytes += [int64]$file.Length
    }
    return [ordered]@{
        files = $files.Count
        bytes = $bytes
    }
}

function Get-CriticalFile {
    param(
        [Parameter(Mandatory)]
        [string]$ToolsRoot,
        [Parameter(Mandatory)]
        [string]$Path
    )

    $relative = Get-RelativePath -Root $ToolsRoot -Path $Path
    $item = Get-Item -LiteralPath $Path -Force
    return [ordered]@{
        relativePath = $relative
        bytes = [int64]$item.Length
        sha256 = Get-Sha256 -Path $Path
    }
}

function ConvertTo-XmlText {
    param(
        [Parameter(Mandatory)]
        [string]$Value
    )

    return [Security.SecurityElement]::Escape($Value)
}

$startedAt = Get-UtcTimestamp
$packageId = [Guid]::NewGuid().ToString("D")
$scriptPath = Resolve-ExistingFile `
    -Path (Join-Path $PSScriptRoot "sandbox-real-data-preflight.ps1") `
    -Label "sandbox preflight script"
$ortProbeSource = Resolve-ExistingFile `
    -Path (Join-Path $PSScriptRoot "ort_provider_probe.py") `
    -Label "ORT provider probe"
$harnessSource = Resolve-ExistingFile `
    -Path (Join-Path $PSScriptRoot "real_data_validation.py") `
    -Label "real-data harness"
$engineCandidate = [IO.Path]::GetFullPath($EnginePath)
$defaultTargetEngine = [IO.Path]::GetFullPath(
    (Join-Path $PSScriptRoot "..\src\engine\target\release\FileIDEngine.exe")
)
if ($engineCandidate.Equals(
    $defaultTargetEngine,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw (
        "EnginePath cannot use the repository fallback target. Supply the " +
        "explicit independently built and audited release path."
    )
}
$engineSource = Resolve-ExistingFile `
    -Path $engineCandidate `
    -Label "FileID engine"
$engineSourceItem = Get-Item -LiteralPath $engineSource -Force
$engineSourceSha256 = Get-Sha256 -Path $engineSource
if ([int64]$engineSourceItem.Length -ne [int64]$ExpectedEngineSize) {
    throw (
        "FileID engine size mismatch: expected $ExpectedEngineSize, got " +
        "$($engineSourceItem.Length)"
    )
}
if (-not $engineSourceSha256.Equals(
    $ExpectedEngineSha256,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw (
        "FileID engine SHA256 mismatch: expected " +
        "$($ExpectedEngineSha256.ToLowerInvariant()), got $engineSourceSha256"
    )
}
$ortRuntimeSource = Resolve-ExistingDirectory `
    -Path $OrtRuntimeDirectory `
    -Label "ONNX Runtime directory"
$ortDllSource = Resolve-ExistingFile `
    -Path (Join-Path $ortRuntimeSource "onnxruntime.dll") `
    -Label "ONNX Runtime DLL"
$directMLSource = Resolve-ExistingFile `
    -Path $DirectMLPath `
    -Label "DirectML DLL"
$llamaRuntimeSource = Resolve-ExistingDirectory `
    -Path $LlamaRuntimePath `
    -Label "llama.cpp runtime"
$llamaCliSource = Resolve-ExistingFile `
    -Path (Join-Path $llamaRuntimeSource "llama-mtmd-cli.exe") `
    -Label "llama-mtmd-cli"

$outputFull = [IO.Path]::GetFullPath($OutputRoot).TrimEnd("\")
$outputDriveRoot = [IO.Path]::GetPathRoot($outputFull)
if ($outputDriveRoot -eq $outputFull -or $outputFull.Length -le 3) {
    throw "OutputRoot must be a dedicated child directory, not a drive root"
}
if ($outputDriveRoot.Equals("F:\", [StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputRoot cannot use F:; tools and validation state must stay on C:"
}
if (Test-Path -LiteralPath $outputFull) {
    throw "OutputRoot already exists; use a new empty path: $outputFull"
}
New-Item -ItemType Directory -Path $outputFull | Out-Null

$toolsRoot = Join-Path $outputFull "tools"
$validationRoot = Join-Path $outputFull "validation"
$artifactsRoot = Join-Path $validationRoot "artifacts"
$stateRoot = Join-Path $validationRoot "state"
New-Item -ItemType Directory -Path $toolsRoot | Out-Null
New-Item -ItemType Directory -Path $validationRoot | Out-Null
New-Item -ItemType Directory -Path $artifactsRoot | Out-Null
New-Item -ItemType Directory -Path $stateRoot | Out-Null

$corpusMode = "fixture"
$operationProbeMode = "fixture-actual"
$overwriteProbeRelativePath = "boundary-overwrite.txt"
$deleteProbeRelativePath = "boundary-delete.txt"
$identityRelativePaths = @(
    "boundary-overwrite.txt"
    "boundary-delete.txt"
    "nested\identity-sample.txt"
)

if ([string]::IsNullOrWhiteSpace($CorpusPath)) {
    $corpusRoot = Join-Path $outputFull "fixture-corpus"
    New-Item -ItemType Directory -Path $corpusRoot | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $corpusRoot "nested") | Out-Null
    [IO.File]::WriteAllText(
        (Join-Path $corpusRoot $overwriteProbeRelativePath),
        "FileID sandbox overwrite boundary fixture`n",
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllText(
        (Join-Path $corpusRoot $deleteProbeRelativePath),
        "FileID sandbox delete boundary fixture`n",
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllText(
        (Join-Path $corpusRoot "nested\identity-sample.txt"),
        "FileID sandbox identity fixture`n",
        [Text.UTF8Encoding]::new($false)
    )
} else {
    $corpusCandidate = [IO.Path]::GetFullPath($CorpusPath).TrimEnd("\")
    $corpusDriveRoot = [IO.Path]::GetPathRoot($corpusCandidate)
    if ($corpusDriveRoot.Equals(
        "F:\",
        [StringComparison]::OrdinalIgnoreCase
    )) {
        if (-not $AllowAdlonDrive) {
            throw "F: is fail-closed. Pass -AllowAdlonDrive with an explicit F:\Adlon Drive corpus path."
        }
        $isExactAdlonRoot = $corpusCandidate.Equals(
            "F:\Adlon Drive",
            [StringComparison]::OrdinalIgnoreCase
        )
        $isAdlonChild = $corpusCandidate.StartsWith(
            "F:\Adlon Drive\",
            [StringComparison]::OrdinalIgnoreCase
        )
        if (-not ($isExactAdlonRoot -or $isAdlonChild)) {
            throw "-AllowAdlonDrive permits only F:\Adlon Drive and its children"
        }
        $corpusMode = "adlon"
    } elseif ($AllowAdlonDrive) {
        throw "-AllowAdlonDrive is valid only for an F:\Adlon Drive corpus"
    } else {
        $corpusMode = "external"
    }
    $corpusRoot = Resolve-ExistingDirectory `
        -Path $corpusCandidate `
        -Label "mapped corpus"
    if ([string]::IsNullOrWhiteSpace($CorpusProbeRelativePath)) {
        throw "External corpora require -CorpusProbeRelativePath; automatic corpus traversal is disabled"
    }
    $probePath = Resolve-RelativeChild `
        -Root $corpusRoot `
        -RelativePath $CorpusProbeRelativePath
    $probePath = Resolve-ExistingFile -Path $probePath -Label "corpus probe file"
    $overwriteProbeRelativePath = Get-RelativePath `
        -Root $corpusRoot `
        -Path $probePath
    $deleteProbeRelativePath = $overwriteProbeRelativePath
    $identityRelativePaths = @($overwriteProbeRelativePath)
    $operationProbeMode = "access-only"
}
if (
    (Test-PathsOverlap -Left $corpusRoot -Right $toolsRoot) -or
    (Test-PathsOverlap -Left $corpusRoot -Right $validationRoot) -or
    (Test-PathsOverlap -Left $toolsRoot -Right $validationRoot)
) {
    throw "Tools, corpus, and validation roots must be pairwise disjoint"
}

$modelsSource = $null
$modelsMappedReadOnly = -not [string]::IsNullOrWhiteSpace($ModelsPath)
if ($modelsMappedReadOnly) {
    $modelsSource = Resolve-ExistingDirectory `
        -Path $ModelsPath `
        -Label "models directory"
    Assert-DirectoryTreeHasNoReparsePoint -Source $modelsSource
    if (
        (Test-PathsOverlap -Left $modelsSource -Right $toolsRoot) -or
        (Test-PathsOverlap -Left $modelsSource -Right $validationRoot) -or
        (Test-PathsOverlap -Left $modelsSource -Right $corpusRoot)
    ) {
        throw "Models, tools, corpus, and validation roots must be disjoint"
    }
}

$pythonSourceRoot = $null
if ([string]::IsNullOrWhiteSpace($PythonHome)) {
    $pythonCommand = Get-Command "python.exe" -ErrorAction Stop
    $basePrefix = & $pythonCommand.Source `
        -I `
        -c `
        "import sys;print(sys.base_prefix)"
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($basePrefix)) {
        throw "Could not resolve the base CPython runtime"
    }
    $basePrefixPath = [IO.Path]::GetFullPath(([string]$basePrefix).Trim())
    $basePrefixItem = Get-Item -LiteralPath $basePrefixPath -Force
    if (
        ($basePrefixItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        $targets = @($basePrefixItem.Target)
        if ($targets.Count -ne 1) {
            throw "The base CPython reparse point must have one explicit target"
        }
        $basePrefixPath = [IO.Path]::GetFullPath([string]$targets[0])
    }
    $pythonSourceRoot = Resolve-ExistingDirectory `
        -Path $basePrefixPath `
        -Label "base CPython runtime"
} else {
    $pythonSourceRoot = Resolve-ExistingDirectory `
        -Path $PythonHome `
        -Label "portable Python runtime"
}
$pythonSourceExe = Resolve-ExistingFile `
    -Path (Join-Path $pythonSourceRoot "python.exe") `
    -Label "portable Python executable"
$faceCropsSource = $null
if (-not [string]::IsNullOrWhiteSpace($FaceCropsPath)) {
    $faceCropsSource = Resolve-ExistingDirectory `
        -Path $FaceCropsPath `
        -Label "face crops directory"
}
if (
    $modelsMappedReadOnly -and
    (
        (Test-PathsOverlap -Left $modelsSource -Right $pythonSourceRoot) -or
        (Test-PathsOverlap -Left $modelsSource -Right $llamaRuntimeSource) -or
        (Test-PathsOverlap `
            -Left $modelsSource `
            -Right (Split-Path -Parent $engineSource)) -or
        (Test-PathsOverlap -Left $modelsSource -Right $ortRuntimeSource) -or
        (Test-PathsOverlap `
            -Left $modelsSource `
            -Right (Split-Path -Parent $directMLSource))
    )
) {
    throw (
        "Models must be disjoint from every staged runtime source so VLM " +
        "weights are never copied into the tools package"
    )
}
if (
    $null -ne $faceCropsSource -and
    $modelsMappedReadOnly -and
    (Test-PathsOverlap -Left $modelsSource -Right $faceCropsSource)
) {
    throw "Face crops cannot overlap the read-only models source"
}
Assert-DirectoryTreeHasNoReparsePoint -Source $pythonSourceRoot
Assert-DirectoryTreeHasNoReparsePoint -Source $llamaRuntimeSource
if ($null -ne $faceCropsSource) {
    Assert-DirectoryTreeHasNoReparsePoint -Source $faceCropsSource
}

$pythonDestination = Join-Path $toolsRoot "python"
Copy-DirectoryContents `
    -Source $pythonSourceRoot `
    -Destination $pythonDestination
$stagedPython = Resolve-ExistingFile `
    -Path (Join-Path $pythonDestination "python.exe") `
    -Label "staged Python executable"

$harnessDestination = Join-Path $toolsRoot "harness"
New-Item -ItemType Directory -Path $harnessDestination | Out-Null
Copy-Item `
    -LiteralPath $harnessSource `
    -Destination (Join-Path $harnessDestination "real_data_validation.py")
Copy-Item `
    -LiteralPath $scriptPath `
    -Destination (Join-Path $harnessDestination "sandbox-real-data-preflight.ps1")
Copy-Item `
    -LiteralPath $ortProbeSource `
    -Destination (Join-Path $harnessDestination "ort_provider_probe.py")

$engineDestination = Join-Path $toolsRoot "engine"
New-Item -ItemType Directory -Path $engineDestination | Out-Null
$stagedEngine = Join-Path $engineDestination "FileIDEngine.exe"
Copy-Item `
    -LiteralPath $engineSource `
    -Destination $stagedEngine
$stagedEngineItem = Get-Item -LiteralPath $stagedEngine -Force
$stagedEngineSha256 = Get-Sha256 -Path $stagedEngine
if (
    [int64]$stagedEngineItem.Length -ne [int64]$ExpectedEngineSize -or
    -not $stagedEngineSha256.Equals(
        $ExpectedEngineSha256,
        [StringComparison]::OrdinalIgnoreCase
    )
) {
    throw (
        "Staged engine does not match the authoritative engine: " +
        "$($stagedEngineItem.Length) bytes, $stagedEngineSha256"
    )
}
foreach ($dll in Get-ChildItem `
    -LiteralPath (Split-Path -Parent $engineSource) `
    -Filter "*.dll" `
    -File `
    -Force) {
    $dllSource = Resolve-ExistingFile `
        -Path $dll.FullName `
        -Label "engine runtime DLL"
    Copy-Item -LiteralPath $dllSource -Destination $engineDestination -Force
}
foreach ($dll in Get-ChildItem `
    -LiteralPath $ortRuntimeSource `
    -Filter "*.dll" `
    -File `
    -Force) {
    $dllSource = Resolve-ExistingFile `
        -Path $dll.FullName `
        -Label "ONNX Runtime DLL"
    Copy-Item -LiteralPath $dllSource -Destination $engineDestination -Force
}
Copy-Item `
    -LiteralPath $directMLSource `
    -Destination (Join-Path $engineDestination "DirectML.dll") `
    -Force

$stagedLlamaRuntime = Join-Path $toolsRoot "llama-runtime"
Copy-DirectoryContents `
    -Source $llamaRuntimeSource `
    -Destination $stagedLlamaRuntime
$stagedLlamaCli = Resolve-ExistingFile `
    -Path (Join-Path $stagedLlamaRuntime "llama-mtmd-cli.exe") `
    -Label "staged llama-mtmd-cli"
$nativeRuntimeNames = @(
    "concrt140.dll"
    "msvcp140.dll"
    "msvcp140_1.dll"
    "msvcp140_2.dll"
    "msvcp140_atomic_wait.dll"
    "msvcp140_codecvt_ids.dll"
    "vcruntime140.dll"
    "vcruntime140_1.dll"
    "vcruntime140_threads.dll"
)
foreach ($nativeRuntimeName in $nativeRuntimeNames) {
    $nativeRuntimeSource = Resolve-ExistingFile `
        -Path (Join-Path "$env:SystemRoot\System32" $nativeRuntimeName) `
        -Label "native runtime $nativeRuntimeName"
    Copy-Item `
        -LiteralPath $nativeRuntimeSource `
        -Destination (Join-Path $engineDestination $nativeRuntimeName) `
        -Force
    Copy-Item `
        -LiteralPath $nativeRuntimeSource `
        -Destination (Join-Path $stagedLlamaRuntime $nativeRuntimeName) `
        -Force
}
$vulkanLoaderSource = Resolve-ExistingFile `
    -Path (Join-Path "$env:SystemRoot\System32" "vulkan-1.dll") `
    -Label "Vulkan loader"
Copy-Item `
    -LiteralPath $vulkanLoaderSource `
    -Destination (Join-Path $stagedLlamaRuntime "vulkan-1.dll") `
    -Force

$faceCropsDestination = $null
if ($null -ne $faceCropsSource) {
    $faceCropsDestination = Join-Path $toolsRoot "face-crops"
    Copy-DirectoryContents `
        -Source $faceCropsSource `
        -Destination $faceCropsDestination
}

$pythonVersionOutput = & $stagedPython -I --version 2>&1
if ($LASTEXITCODE -ne 0 -or "$pythonVersionOutput" -notmatch "Python 3\.") {
    throw "The staged Python runtime failed its host smoke test"
}
$stagedHarness = Join-Path $harnessDestination "real_data_validation.py"
$harnessHelpOutput = & $stagedPython -I $stagedHarness --help 2>&1
if ($LASTEXITCODE -ne 0 -or "$harnessHelpOutput" -notmatch "--corpus") {
    throw "The staged Python runtime could not load real_data_validation.py"
}

$criticalPaths = @(
    $stagedPython
    $stagedHarness
    (Join-Path $harnessDestination "sandbox-real-data-preflight.ps1")
    (Join-Path $harnessDestination "ort_provider_probe.py")
    $stagedEngine
    (Join-Path $engineDestination "onnxruntime.dll")
    (Join-Path $engineDestination "DirectML.dll")
    $stagedLlamaCli
)
$pythonDlls = @(
    Get-ChildItem `
        -LiteralPath $pythonDestination `
        -Filter "python3*.dll" `
        -File `
        -Force
)
foreach ($pythonDll in $pythonDlls) {
    $criticalPaths += $pythonDll.FullName
}
$llamaDlls = @(
    Get-ChildItem `
        -LiteralPath $stagedLlamaRuntime `
        -Filter "*.dll" `
        -File `
        -Force
)
foreach ($llamaDll in $llamaDlls) {
    $criticalPaths += $llamaDll.FullName
}
$criticalFiles = @(
    foreach ($criticalPath in $criticalPaths | Select-Object -Unique) {
        Get-CriticalFile -ToolsRoot $toolsRoot -Path $criticalPath
    }
)

$treeSummaries = [ordered]@{
    python = Get-TreeSummary -Path $pythonDestination
    harness = Get-TreeSummary -Path $harnessDestination
    engine = Get-TreeSummary -Path $engineDestination
    llamaRuntime = Get-TreeSummary -Path $stagedLlamaRuntime
}
if ($null -ne $faceCropsDestination) {
    $treeSummaries["faceCrops"] = Get-TreeSummary -Path $faceCropsDestination
}
$stagedReparsePoints = @(
    Get-ChildItem -LiteralPath $toolsRoot -Recurse -Force |
        Where-Object {
            ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        } |
        ForEach-Object { $_.FullName }
)
if ($stagedReparsePoints.Count -gt 0) {
    throw "Staged tools contain reparse points: $($stagedReparsePoints[0])"
}
$filesManifest = @(
    foreach ($file in Get-ChildItem `
        -LiteralPath $toolsRoot `
        -Recurse `
        -File `
        -Force) {
        Get-CriticalFile -ToolsRoot $toolsRoot -Path $file.FullName
    }
)

$packageManifest = [ordered]@{
    schemaVersion = 1
    packageId = $packageId
    createdAt = Get-UtcTimestamp
    modelsMappedReadOnly = $modelsMappedReadOnly
    modelsStaged = $false
    seedDatabaseStaged = $false
    faceCropsStaged = $null -ne $faceCropsDestination
    engine = [ordered]@{
        explicitSourcePath = $engineSource
        expectedBytes = [int64]$ExpectedEngineSize
        actualBytes = [int64]$engineSourceItem.Length
        stagedBytes = [int64]$stagedEngineItem.Length
        expectedSha256 = $ExpectedEngineSha256.ToLowerInvariant()
        sourceSha256 = $engineSourceSha256
        stagedSha256 = $stagedEngineSha256
    }
    portablePython = [ordered]@{
        version = "$pythonVersionOutput".Trim()
        sourceExecutableSha256 = Get-Sha256 -Path $pythonSourceExe
        stagedExecutableSha256 = Get-Sha256 -Path $stagedPython
    }
    treeSummaries = $treeSummaries
    files = $filesManifest
    criticalFiles = $criticalFiles
}
$packageManifestPath = Join-Path $toolsRoot "package-manifest.json"
Write-JsonAtomic -Path $packageManifestPath -Value $packageManifest
$verificationFiles = @(
    $filesManifest
    Get-CriticalFile -ToolsRoot $toolsRoot -Path $packageManifestPath
)

$sandboxTools = "C:\FileIDTools"
$sandboxCorpus = "C:\FileIDCorpus"
$sandboxValidation = "C:\FileIDValidation"
$sandboxModels = if ($modelsMappedReadOnly) {
    "C:\FileIDModels"
} else {
    $null
}
$llamaRelativePath = Get-RelativePath `
    -Root $toolsRoot `
    -Path $stagedLlamaCli
$configuration = [ordered]@{
    schemaVersion = 1
    packageId = $packageId
    createdAt = Get-UtcTimestamp
    corpusMode = $corpusMode
    operationProbeMode = $operationProbeMode
    expectedNetworking = "Disable"
    autoClose = [bool]$AutoClose
    harnessLaunchPermitted = $false
    catalogPolicy = "fresh-writable-validation-only"
    expectedEngine = [ordered]@{
        bytes = [int64]$ExpectedEngineSize
        sha256 = $ExpectedEngineSha256.ToLowerInvariant()
    }
    harnessLaunchBlocker = (
        "Preflight-only package, not an accepted real-run configuration: " +
        "the mapped corpus is C:\FileIDCorpus and no exact catalog relocation " +
        "or sandbox F:\ path-equivalence proof exists. The engine harness " +
        "must remain unlaunched."
    )
    mounts = [ordered]@{
        tools = $sandboxTools
        corpus = $sandboxCorpus
        validation = $sandboxValidation
        models = $sandboxModels
    }
    paths = [ordered]@{
        python = "python\python.exe"
        harness = "harness\real_data_validation.py"
        ortProbe = "harness\ort_provider_probe.py"
        engineDirectory = "engine"
        engine = "engine\FileIDEngine.exe"
        llamaMtmdCli = $llamaRelativePath
        models = $sandboxModels
        catalog = "state\fileid-validation.db"
        seedDatabase = $null
        faceCrops = if ($null -eq $faceCropsDestination) {
            $null
        } else {
            "face-crops"
        }
    }
    overwriteProbeRelativePath = $overwriteProbeRelativePath
    deleteProbeRelativePath = $deleteProbeRelativePath
    identityRelativePaths = $identityRelativePaths
    criticalFiles = $verificationFiles
}
$configurationPath = Join-Path $validationRoot "preflight-config.json"
Write-JsonAtomic -Path $configurationPath -Value $configuration
$validationMarkerPath = Join-Path `
    $validationRoot `
    ".fileid-sandbox-validation.json"
Write-JsonAtomic `
    -Path $validationMarkerPath `
    -Value ([ordered]@{
        schemaVersion = 1
        packageId = $packageId
        createdAt = Get-UtcTimestamp
        purpose = "FileID Windows Sandbox writable validation mount"
    })

$toolsXml = ConvertTo-XmlText -Value $toolsRoot
$corpusXml = ConvertTo-XmlText -Value $corpusRoot
$validationXml = ConvertTo-XmlText -Value $validationRoot
$modelsMappingXml = if ($modelsMappedReadOnly) {
    $modelsXml = ConvertTo-XmlText -Value $modelsSource
    @"
    <MappedFolder>
      <HostFolder>$modelsXml</HostFolder>
      <SandboxFolder>C:\FileIDModels</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
"@
} else {
    ""
}
$command = (
    'powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass ' +
    '-File C:\FileIDTools\harness\sandbox-real-data-preflight.ps1 ' +
    '-ConfigurationPath C:\FileIDValidation\preflight-config.json'
)
$commandXml = ConvertTo-XmlText -Value $command
$wsbContent = @"
<Configuration>
  <VGpu>Enable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <MemoryInMB>8192</MemoryInMB>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$toolsXml</HostFolder>
      <SandboxFolder>C:\FileIDTools</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$corpusXml</HostFolder>
      <SandboxFolder>C:\FileIDCorpus</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
$modelsMappingXml
    <MappedFolder>
      <HostFolder>$validationXml</HostFolder>
      <SandboxFolder>C:\FileIDValidation</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>$commandXml</Command>
  </LogonCommand>
</Configuration>
"@
$wsbPath = Join-Path $outputFull "FileID-real-data-preflight.wsb"
[IO.File]::WriteAllText(
    $wsbPath,
    $wsbContent,
    [Text.UTF8Encoding]::new($false)
)

[xml]$wsbXml = Get-Content -LiteralPath $wsbPath -Raw
$mappedFolders = @($wsbXml.Configuration.MappedFolders.MappedFolder)
$toolsMapping = @(
    $mappedFolders |
        Where-Object { $_.SandboxFolder -eq "C:\FileIDTools" }
)
$corpusMapping = @(
    $mappedFolders |
        Where-Object { $_.SandboxFolder -eq "C:\FileIDCorpus" }
)
$validationMapping = @(
    $mappedFolders |
        Where-Object { $_.SandboxFolder -eq "C:\FileIDValidation" }
)
$modelsMapping = @(
    $mappedFolders |
        Where-Object { $_.SandboxFolder -eq "C:\FileIDModels" }
)
$wsbText = Get-Content -LiteralPath $wsbPath -Raw
$hostChecks = [ordered]@{
    outputOutsideF = -not $outputDriveRoot.Equals(
        "F:\",
        [StringComparison]::OrdinalIgnoreCase
    )
    fixtureDefaultDidNotResolveF = (
        $corpusMode -ne "fixture" -or
        $wsbText -notmatch "(?i)F:\\"
    )
    networkingDisabled = $wsbXml.Configuration.Networking -eq "Disable"
    toolsMappedExactlyOnce = $toolsMapping.Count -eq 1
    toolsMappedReadOnly = (
        $toolsMapping.Count -eq 1 -and
        $toolsMapping[0].ReadOnly -eq "true"
    )
    corpusMappedExactlyOnce = $corpusMapping.Count -eq 1
    corpusMappedReadOnly = (
        $corpusMapping.Count -eq 1 -and
        $corpusMapping[0].ReadOnly -eq "true"
    )
    validationMappedExactlyOnce = $validationMapping.Count -eq 1
    validationMappedWritable = (
        $validationMapping.Count -eq 1 -and
        $validationMapping[0].ReadOnly -eq "false"
    )
    modelsMappingMatchesRequest = (
        (
            -not $modelsMappedReadOnly -and
            $modelsMapping.Count -eq 0
        ) -or (
            $modelsMappedReadOnly -and
            $modelsMapping.Count -eq 1 -and
            $modelsMapping[0].ReadOnly -eq "true"
        )
    )
    modelsNeverStaged = -not (Test-Path `
        -LiteralPath (Join-Path $toolsRoot "models"))
    liveCatalogNeverStaged = (
        -not [bool]$packageManifest.seedDatabaseStaged -and
        $null -eq $configuration.paths.seedDatabase -and
        [string]$configuration.catalogPolicy -eq
            "fresh-writable-validation-only"
    )
    portablePythonStaged = Test-Path -LiteralPath $stagedPython -PathType Leaf
    harnessPinned = Test-Path -LiteralPath $stagedHarness -PathType Leaf
    engineStaged = Test-Path `
        -LiteralPath $stagedEngine `
        -PathType Leaf
    engineStagedBytesExact = (
        [int64]$stagedEngineItem.Length -eq [int64]$ExpectedEngineSize
    )
    engineStagedSha256Exact = $stagedEngineSha256.Equals(
        $ExpectedEngineSha256,
        [StringComparison]::OrdinalIgnoreCase
    )
    ortStaged = Test-Path `
        -LiteralPath (Join-Path $engineDestination "onnxruntime.dll") `
        -PathType Leaf
    llamaStaged = Test-Path -LiteralPath $stagedLlamaCli -PathType Leaf
    engineLaunchDisabled = -not [bool]$configuration.harnessLaunchPermitted
}
$hostFailedChecks = @(
    foreach ($entry in $hostChecks.GetEnumerator()) {
        if (-not [bool]$entry.Value) {
            $entry.Key
        }
    }
)
$hostSummary = [ordered]@{
    schemaVersion = 1
    packageId = $packageId
    startedAt = $startedAt
    finishedAt = Get-UtcTimestamp
    result = if ($hostFailedChecks.Count -eq 0) { "GREEN" } else { "RED" }
    failedChecks = $hostFailedChecks
    corpusMode = $corpusMode
    outputRoot = $outputFull
    paths = [ordered]@{
        tools = $toolsRoot
        corpus = $corpusRoot
        validation = $validationRoot
        configuration = $configurationPath
        wsb = $wsbPath
        preflightSummary = Join-Path $artifactsRoot "preflight-summary.json"
    }
    package = $packageManifest
    wsb = [ordered]@{
        sha256 = Get-Sha256 -Path $wsbPath
        networking = [string]$wsbXml.Configuration.Networking
        mappedFolders = @(
            foreach ($mapping in $mappedFolders) {
                [ordered]@{
                    sandboxFolder = [string]$mapping.SandboxFolder
                    readOnly = [string]$mapping.ReadOnly
                }
            }
        )
    }
    checks = $hostChecks
    launch = [ordered]@{
        requested = [bool]$Launch
        waitRequested = [bool]$WaitForPreflight
        processId = $null
        launcherExited = $false
        launcherExitCode = $null
        completed = $false
        preflightResult = $null
        error = $null
    }
}
$hostSummaryPath = Join-Path $artifactsRoot "host-package-summary.json"
Write-JsonAtomic -Path $hostSummaryPath -Value $hostSummary

if ($hostSummary.result -ne "GREEN") {
    throw "Sandbox package failed host checks: $($hostFailedChecks -join ', ')"
}
if ($WaitForPreflight -and -not $Launch) {
    throw "-WaitForPreflight requires -Launch"
}

if ($Launch) {
    $sandboxExecutable = Resolve-ExistingFile `
        -Path (Join-Path $env:SystemRoot "System32\WindowsSandbox.exe") `
        -Label "Windows Sandbox"
    try {
        $sandboxProcess = Start-Process `
            -FilePath $sandboxExecutable `
            -ArgumentList @($wsbPath) `
            -PassThru `
            -WindowStyle Hidden
        $hostSummary.launch.processId = $sandboxProcess.Id
        if ($WaitForPreflight) {
            $preflightSummaryPath = Join-Path $artifactsRoot "preflight-summary.json"
            $preflightStartedPath = Join-Path $artifactsRoot "preflight-started.json"
            $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
            $startupDeadline = [DateTime]::UtcNow.AddSeconds(
                [Math]::Min(120, $TimeoutSeconds)
            )
            while (
                [DateTime]::UtcNow -lt $deadline -and
                -not (Test-Path -LiteralPath $preflightSummaryPath -PathType Leaf)
            ) {
                $sandboxProcess.Refresh()
                if (
                    $sandboxProcess.HasExited -and
                    [DateTime]::UtcNow -ge $startupDeadline -and
                    -not (Test-Path `
                        -LiteralPath $preflightStartedPath `
                        -PathType Leaf)
                ) {
                    break
                }
                Start-Sleep -Seconds 2
            }
            $sandboxProcess.Refresh()
            $hostSummary.launch.launcherExited = $sandboxProcess.HasExited
            if ($sandboxProcess.HasExited) {
                $hostSummary.launch.launcherExitCode = $sandboxProcess.ExitCode
            }
            if (Test-Path -LiteralPath $preflightSummaryPath -PathType Leaf) {
                Start-Sleep -Seconds 2
                $preflightSummary = Get-Content `
                    -LiteralPath $preflightSummaryPath `
                    -Raw |
                    ConvertFrom-Json
                $hostSummary.launch.completed = $true
                $hostSummary.launch.preflightResult = [string]$preflightSummary.result
            } else {
                $hostSummary.launch.error = (
                    "Preflight summary was not written before the wait deadline"
                )
            }
        }
    } catch {
        $hostSummary.launch.error = (
            "$($_.Exception.GetType().Name): $($_.Exception.Message)"
        )
    }
    if (
        $WaitForPreflight -and
        (
            -not $hostSummary.launch.completed -or
            $hostSummary.launch.preflightResult -ne "GREEN"
        )
    ) {
        $hostSummary.result = "RED"
        $hostSummary.failedChecks += "sandboxPreflightGreen"
    }
    if ($null -ne $hostSummary.launch.error) {
        $hostSummary.result = "RED"
        $hostSummary.failedChecks += "sandboxLaunchSucceeded"
    }
    $hostSummary.finishedAt = Get-UtcTimestamp
    Write-JsonAtomic -Path $hostSummaryPath -Value $hostSummary
}

$output = [ordered]@{
    result = $hostSummary.result
    corpusMode = $corpusMode
    outputRoot = $outputFull
    wsb = $wsbPath
    hostSummary = $hostSummaryPath
    preflightSummary = Join-Path $artifactsRoot "preflight-summary.json"
    harnessLaunchPermitted = $false
}
$output | ConvertTo-Json -Depth 10

if ($hostSummary.result -ne "GREEN") {
    exit 1
}

#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$ConfigurationPath = "C:\FileIDValidation\preflight-config.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

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

function Resolve-ChildPath {
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
    $prefix = "$rootFull\"
    if (-not $candidate.StartsWith(
        $prefix,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Path escapes its configured root: $RelativePath"
    }
    return $candidate
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
            $relativePath = $item.FullName.Substring($sourceFull.Length + 1)
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

function Copy-DirectoryContentsNoReparse {
    param(
        [Parameter(Mandatory)]
        [string]$Source,
        [Parameter(Mandatory)]
        [string]$Destination
    )

    if (Test-Path -LiteralPath $Destination) {
        throw "Copy destination already exists: $Destination"
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
            throw "Source entry type changed while copying: $($entry.Source)"
        }
        $target = Resolve-ChildPath `
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

function Get-FileSnapshot {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $item = Get-Item -LiteralPath $Path -Force
    $hash = $null
    if (-not $item.PSIsContainer) {
        $hash = Get-Sha256 -Path $Path
    }
    return [ordered]@{
        path = $Path
        isDirectory = [bool]$item.PSIsContainer
        length = if ($item.PSIsContainer) { $null } else { [int64]$item.Length }
        modifiedUtc = $item.LastWriteTimeUtc.ToString("o")
        attributes = [string]$item.Attributes
        isReparsePoint = (
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        )
        sha256 = $hash
    }
}

function Invoke-ChildProcess {
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory)]
        [string]$WorkingDirectory,
        [int]$TimeoutMilliseconds = 30000
    )

    foreach ($argument in $Arguments) {
        if ($argument -match '[\s"]') {
            throw "Sandbox process arguments must not contain whitespace or quotes: $argument"
        }
    }

    $started = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = [Diagnostics.ProcessStartInfo]@{
        FileName = $FilePath
        Arguments = ($Arguments -join " ")
        WorkingDirectory = $WorkingDirectory
        UseShellExecute = $false
        RedirectStandardOutput = $true
        RedirectStandardError = $true
        CreateNoWindow = $true
    }
    $processTempRoot = "C:\FileIDValidation\state\process-temp"
    $localAppDataRoot = "C:\FileIDValidation\state\local-app-data"
    $roamingAppDataRoot = "C:\FileIDValidation\state\roaming-app-data"
    foreach ($directory in @(
        $processTempRoot
        $localAppDataRoot
        $roamingAppDataRoot
    )) {
        if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
            New-Item -ItemType Directory -Path $directory -Force | Out-Null
        }
    }
    $process.StartInfo.EnvironmentVariables.Clear()
    $sanitizedEnvironment = [ordered]@{
        SystemRoot = $env:SystemRoot
        WINDIR = $env:SystemRoot
        ComSpec = "$env:SystemRoot\System32\cmd.exe"
        PATH = "$env:SystemRoot\System32;$env:SystemRoot"
        TEMP = $processTempRoot
        TMP = $processTempRoot
        USERPROFILE = "C:\FileIDValidation\state"
        LOCALAPPDATA = $localAppDataRoot
        APPDATA = $roamingAppDataRoot
        PYTHONDONTWRITEBYTECODE = "1"
        PYTHONNOUSERSITE = "1"
        PYTHONIOENCODING = "utf-8"
    }
    foreach ($entry in $sanitizedEnvironment.GetEnumerator()) {
        $process.StartInfo.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }
    $exitCode = $null
    $stdout = ""
    $stderr = ""
    $timedOut = $false
    $errorText = $null
    try {
        if (-not $process.Start()) {
            throw "Process.Start returned false"
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            $timedOut = $true
            $process.Kill()
            $process.WaitForExit()
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
    } catch {
        $errorText = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
    } finally {
        $started.Stop()
        $process.Dispose()
    }

    return [ordered]@{
        file = $FilePath
        arguments = @($Arguments)
        exitCode = $exitCode
        timedOut = $timedOut
        wallMilliseconds = [int64]$started.ElapsedMilliseconds
        stdout = $stdout
        stderr = $stderr
        error = $errorText
    }
}

if (-not ("FileIdSandbox.NativeFileProbe" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace FileIdSandbox
{
    public sealed class IdentitySnapshot
    {
        public string Path { get; set; }
        public uint VolumeSerialNumber { get; set; }
        public ulong FileIndex { get; set; }
        public uint NumberOfLinks { get; set; }
        public uint Attributes { get; set; }
        public bool IsReparsePoint { get; set; }
    }

    public sealed class AccessSnapshot
    {
        public string Path { get; set; }
        public uint DesiredAccess { get; set; }
        public bool Granted { get; set; }
        public int ErrorCode { get; set; }
    }

    public static class NativeFileProbe
    {
        private const uint FileReadAttributes = 0x00000080;
        private const uint FileFlagBackupSemantics = 0x02000000;
        private const uint FileFlagOpenReparsePoint = 0x00200000;
        private const uint OpenExisting = 3;
        private const uint ReparsePointAttribute = 0x00000400;

        [StructLayout(LayoutKind.Sequential)]
        private struct FileTime
        {
            public uint Low;
            public uint High;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ByHandleFileInformation
        {
            public uint FileAttributes;
            public FileTime CreationTime;
            public FileTime LastAccessTime;
            public FileTime LastWriteTime;
            public uint VolumeSerialNumber;
            public uint FileSizeHigh;
            public uint FileSizeLow;
            public uint NumberOfLinks;
            public uint FileIndexHigh;
            public uint FileIndexLow;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(
            string fileName,
            uint desiredAccess,
            FileShare shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle file,
            out ByHandleFileInformation information);

        private static SafeFileHandle Open(string path, uint desiredAccess)
        {
            return CreateFileW(
                path,
                desiredAccess,
                FileShare.Read | FileShare.Write | FileShare.Delete,
                IntPtr.Zero,
                OpenExisting,
                FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                IntPtr.Zero);
        }

        public static IdentitySnapshot ReadIdentity(string path)
        {
            using (SafeFileHandle handle = Open(path, FileReadAttributes))
            {
                if (handle.IsInvalid)
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "CreateFileW failed for " + path);
                }
                ByHandleFileInformation information;
                if (!GetFileInformationByHandle(handle, out information))
                {
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "GetFileInformationByHandle failed for " + path);
                }
                return new IdentitySnapshot
                {
                    Path = path,
                    VolumeSerialNumber = information.VolumeSerialNumber,
                    FileIndex = ((ulong)information.FileIndexHigh << 32)
                        | information.FileIndexLow,
                    NumberOfLinks = information.NumberOfLinks,
                    Attributes = information.FileAttributes,
                    IsReparsePoint =
                        (information.FileAttributes & ReparsePointAttribute) != 0
                };
            }
        }

        public static AccessSnapshot ProbeAccess(string path, uint desiredAccess)
        {
            using (SafeFileHandle handle = Open(path, desiredAccess))
            {
                bool granted = !handle.IsInvalid;
                return new AccessSnapshot
                {
                    Path = path,
                    DesiredAccess = desiredAccess,
                    Granted = granted,
                    ErrorCode = granted ? 0 : Marshal.GetLastWin32Error()
                };
            }
        }
    }
}
'@
}

function Get-IdentityEvidence {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $first = [FileIdSandbox.NativeFileProbe]::ReadIdentity($Path)
    Start-Sleep -Milliseconds 20
    $second = [FileIdSandbox.NativeFileProbe]::ReadIdentity($Path)
    $stable = (
        $first.VolumeSerialNumber -eq $second.VolumeSerialNumber -and
        $first.FileIndex -eq $second.FileIndex
    )
    return [ordered]@{
        path = $Path
        first = [ordered]@{
            volumeSerialNumber = [uint64]$first.VolumeSerialNumber
            fileIndex = [uint64]$first.FileIndex
            numberOfLinks = [uint64]$first.NumberOfLinks
            attributes = [uint64]$first.Attributes
            isReparsePoint = [bool]$first.IsReparsePoint
        }
        second = [ordered]@{
            volumeSerialNumber = [uint64]$second.VolumeSerialNumber
            fileIndex = [uint64]$second.FileIndex
            numberOfLinks = [uint64]$second.NumberOfLinks
            attributes = [uint64]$second.Attributes
            isReparsePoint = [bool]$second.IsReparsePoint
        }
        checks = [ordered]@{
            stable = $stable
            volumeSerialNonzero = $first.VolumeSerialNumber -ne 0
            fileIndexNonzero = $first.FileIndex -ne 0
            notReparsePoint = -not $first.IsReparsePoint
        }
    }
}

function Invoke-ActualOperation {
    param(
        [Parameter(Mandatory)]
        [string]$Name,
        [Parameter(Mandatory)]
        [scriptblock]$Operation,
        [scriptblock]$Restore
    )

    $allowed = $false
    $errorType = $null
    $errorMessage = $null
    $hresult = $null
    $restoreError = $null
    try {
        & $Operation
        $allowed = $true
    } catch {
        $errorType = $_.Exception.GetType().FullName
        $errorMessage = $_.Exception.Message
        $hresult = $_.Exception.HResult
    } finally {
        if ($allowed -and $null -ne $Restore) {
            try {
                & $Restore
            } catch {
                $restoreError = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            }
        }
    }
    return [ordered]@{
        name = $Name
        attempted = $true
        allowed = $allowed
        denied = -not $allowed
        errorType = $errorType
        errorMessage = $errorMessage
        hresult = $hresult
        restoreError = $restoreError
    }
}

function Invoke-BoundaryProbes {
    param(
        [Parameter(Mandatory)]
        [object]$Configuration,
        [Parameter(Mandatory)]
        [string]$CorpusRoot
    )

    $overwritePath = Resolve-ChildPath `
        -Root $CorpusRoot `
        -RelativePath ([string]$Configuration.overwriteProbeRelativePath)
    $deletePath = Resolve-ChildPath `
        -Root $CorpusRoot `
        -RelativePath ([string]$Configuration.deleteProbeRelativePath)
    $createPath = Join-Path $CorpusRoot "__fileid_create_denial_probe.tmp"
    $renamePath = Join-Path $CorpusRoot "__fileid_rename_denial_probe.tmp"
    $before = [ordered]@{
        overwrite = Get-FileSnapshot -Path $overwritePath
        delete = Get-FileSnapshot -Path $deletePath
    }

    if ([string]$Configuration.operationProbeMode -eq "fixture-actual") {
        $overwriteBytes = [IO.File]::ReadAllBytes($overwritePath)
        $deleteBytes = [IO.File]::ReadAllBytes($deletePath)
        $create = Invoke-ActualOperation `
            -Name "create" `
            -Operation {
                [IO.File]::WriteAllText(
                    $createPath,
                    "sandbox boundary create probe",
                    [Text.UTF8Encoding]::new($false)
                )
            } `
            -Restore {
                if (Test-Path -LiteralPath $createPath) {
                    Remove-Item -LiteralPath $createPath -Force
                }
            }
        $overwrite = Invoke-ActualOperation `
            -Name "overwrite" `
            -Operation {
                [IO.File]::WriteAllText(
                    $overwritePath,
                    "sandbox boundary overwrite probe",
                    [Text.UTF8Encoding]::new($false)
                )
            } `
            -Restore {
                [IO.File]::WriteAllBytes($overwritePath, $overwriteBytes)
            }
        $rename = Invoke-ActualOperation `
            -Name "rename" `
            -Operation {
                [IO.File]::Move($overwritePath, $renamePath)
            } `
            -Restore {
                if (Test-Path -LiteralPath $renamePath) {
                    [IO.File]::Move($renamePath, $overwritePath)
                }
            }
        $delete = Invoke-ActualOperation `
            -Name "delete" `
            -Operation {
                [IO.File]::Delete($deletePath)
            } `
            -Restore {
                if (-not (Test-Path -LiteralPath $deletePath)) {
                    [IO.File]::WriteAllBytes($deletePath, $deleteBytes)
                }
            }
    } elseif ([string]$Configuration.operationProbeMode -eq "access-only") {
        $genericWrite = [uint32]0x40000000
        $deleteAccess = [uint32]0x00010000
        $createAccess = [FileIdSandbox.NativeFileProbe]::ProbeAccess(
            $CorpusRoot,
            $genericWrite
        )
        $overwriteAccess = [FileIdSandbox.NativeFileProbe]::ProbeAccess(
            $overwritePath,
            $genericWrite
        )
        $renameAccess = [FileIdSandbox.NativeFileProbe]::ProbeAccess(
            $overwritePath,
            $deleteAccess
        )
        $deleteAccessResult = [FileIdSandbox.NativeFileProbe]::ProbeAccess(
            $deletePath,
            $deleteAccess
        )
        $create = [ordered]@{
            name = "create"
            attempted = $true
            accessOnly = $true
            allowed = [bool]$createAccess.Granted
            denied = -not $createAccess.Granted
            errorCode = [int]$createAccess.ErrorCode
        }
        $overwrite = [ordered]@{
            name = "overwrite"
            attempted = $true
            accessOnly = $true
            allowed = [bool]$overwriteAccess.Granted
            denied = -not $overwriteAccess.Granted
            errorCode = [int]$overwriteAccess.ErrorCode
        }
        $rename = [ordered]@{
            name = "rename"
            attempted = $true
            accessOnly = $true
            allowed = [bool]$renameAccess.Granted
            denied = -not $renameAccess.Granted
            errorCode = [int]$renameAccess.ErrorCode
        }
        $delete = [ordered]@{
            name = "delete"
            attempted = $true
            accessOnly = $true
            allowed = [bool]$deleteAccessResult.Granted
            denied = -not $deleteAccessResult.Granted
            errorCode = [int]$deleteAccessResult.ErrorCode
        }
    } else {
        throw "Unsupported operation probe mode: $($Configuration.operationProbeMode)"
    }

    $after = [ordered]@{
        overwrite = Get-FileSnapshot -Path $overwritePath
        delete = Get-FileSnapshot -Path $deletePath
    }
    $identities = @(
        Get-IdentityEvidence -Path $CorpusRoot
        foreach ($relativePath in @($Configuration.identityRelativePaths)) {
            $identityPath = Resolve-ChildPath `
                -Root $CorpusRoot `
                -RelativePath ([string]$relativePath)
            Get-IdentityEvidence -Path $identityPath
        }
    )
    $topLevelReparse = @(
        foreach ($item in Get-ChildItem -LiteralPath $CorpusRoot -Force) {
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                $item.FullName
            }
        }
    )
    $identityChecks = @(
        foreach ($identity in $identities) {
            @($identity.checks.Values) -notcontains $false
        }
    )
    $checks = [ordered]@{
        createDenied = [bool]$create.denied
        overwriteDenied = [bool]$overwrite.denied
        renameDenied = [bool]$rename.denied
        deleteDenied = [bool]$delete.denied
        overwriteUnchanged = (
            $before.overwrite.sha256 -eq $after.overwrite.sha256 -and
            $before.overwrite.length -eq $after.overwrite.length
        )
        deleteUnchanged = (
            $before.delete.sha256 -eq $after.delete.sha256 -and
            $before.delete.length -eq $after.delete.length
        )
        createProbeAbsent = -not (Test-Path -LiteralPath $createPath)
        renameProbeAbsent = -not (Test-Path -LiteralPath $renamePath)
        identitiesStableNonzero = $identityChecks -notcontains $false
        topLevelContainsNoReparsePoints = $topLevelReparse.Count -eq 0
    }
    return [ordered]@{
        schemaVersion = 1
        startedAt = Get-UtcTimestamp
        mode = [string]$Configuration.operationProbeMode
        corpusRoot = $CorpusRoot
        before = $before
        operations = [ordered]@{
            create = $create
            overwrite = $overwrite
            rename = $rename
            delete = $delete
        }
        after = $after
        identities = $identities
        topLevelReparsePoints = $topLevelReparse
        checks = $checks
        result = if (@($checks.Values) -contains $false) { "RED" } else { "GREEN" }
        finishedAt = Get-UtcTimestamp
    }
}

function Invoke-NetworkProbe {
    $adapters = @()
    $adapterError = $null
    try {
        $adapters = @(
            [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() |
                ForEach-Object {
                    [ordered]@{
                        name = $_.Name
                        type = [string]$_.NetworkInterfaceType
                        status = [string]$_.OperationalStatus
                    }
                }
        )
    } catch {
        $adapterError = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
    }
    $activeNonLoopback = @(
        $adapters |
            Where-Object {
                $_.status -eq "Up" -and
                $_.type -ne "Loopback"
            }
    )

    $defaultGateways = @()
    $routeError = $null
    try {
        $defaultGateways = @(
            foreach ($networkInterface in (
                [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()
            )) {
                foreach (
                    $gateway in $networkInterface.GetIPProperties().GatewayAddresses
                ) {
                    if (
                        $gateway.Address.ToString() -ne "0.0.0.0" -and
                        $gateway.Address.ToString() -ne "::"
                    ) {
                        [ordered]@{
                            interfaceName = $networkInterface.Name
                            interfaceType = [string]$networkInterface.NetworkInterfaceType
                            interfaceStatus = [string]$networkInterface.OperationalStatus
                            address = $gateway.Address.ToString()
                        }
                    }
                }
            }
        )
    } catch {
        $routeError = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
    }

    $checks = [ordered]@{
        adapterEnumerationCompleted = $null -eq $adapterError
        noActiveNonLoopbackAdapter = $activeNonLoopback.Count -eq 0
        routeEnumerationCompleted = $null -eq $routeError
        noDefaultIpv4OrIpv6Route = $defaultGateways.Count -eq 0
    }
    return [ordered]@{
        schemaVersion = 1
        startedAt = Get-UtcTimestamp
        adapters = $adapters
        activeNonLoopbackAdapters = $activeNonLoopback
        adapterError = $adapterError
        defaultGateways = $defaultGateways
        routeError = $routeError
        activeNetworkProbeAttempted = $false
        checks = $checks
        result = if (@($checks.Values) -contains $false) { "RED" } else { "GREEN" }
        finishedAt = Get-UtcTimestamp
    }
}

function Invoke-PackageVerification {
    param(
        [Parameter(Mandatory)]
        [object]$Configuration,
        [Parameter(Mandatory)]
        [string]$ToolsRoot,
        [Parameter(Mandatory)]
        [string]$ValidationRoot,
        [AllowNull()]
        [string]$ModelsRoot
    )

    $criticalFiles = @()
    foreach ($entry in @($Configuration.criticalFiles)) {
        $path = Resolve-ChildPath `
            -Root $ToolsRoot `
            -RelativePath ([string]$entry.relativePath)
        $snapshot = Get-FileSnapshot -Path $path
        $snapshot["relativePath"] = [string]$entry.relativePath
        $snapshot["expectedLength"] = [int64]$entry.bytes
        $snapshot["expectedSha256"] = [string]$entry.sha256
        $snapshot["lengthMatches"] = (
            [int64]$snapshot.length -eq [int64]$entry.bytes
        )
        $snapshot["hashMatches"] = (
            $snapshot.sha256 -eq ([string]$entry.sha256).ToLowerInvariant()
        )
        $criticalFiles += $snapshot
    }
    $packageManifestPath = Resolve-ChildPath `
        -Root $ToolsRoot `
        -RelativePath "package-manifest.json"
    $packageManifest = Get-Content -LiteralPath $packageManifestPath -Raw |
        ConvertFrom-Json

    $toolsWriteProbe = Join-Path $ToolsRoot "__fileid_tools_write_probe.tmp"
    $toolsWrite = Invoke-ActualOperation `
        -Name "tools-create" `
        -Operation {
            [IO.File]::WriteAllText(
                $toolsWriteProbe,
                "tools write probe",
                [Text.UTF8Encoding]::new($false)
            )
        } `
        -Restore {
            if (Test-Path -LiteralPath $toolsWriteProbe) {
                Remove-Item -LiteralPath $toolsWriteProbe -Force
            }
        }

    $modelsWrite = $null
    $modelsWriteProbe = $null
    $modelsIdentity = $null
    if (-not [string]::IsNullOrWhiteSpace($ModelsRoot)) {
        $modelsWriteProbe = Join-Path `
            $ModelsRoot `
            "__fileid_models_write_probe.tmp"
        $modelsWrite = Invoke-ActualOperation `
            -Name "models-create" `
            -Operation {
                [IO.File]::WriteAllText(
                    $modelsWriteProbe,
                    "models write probe",
                    [Text.UTF8Encoding]::new($false)
                )
            } `
            -Restore {
                if (Test-Path -LiteralPath $modelsWriteProbe) {
                    Remove-Item -LiteralPath $modelsWriteProbe -Force
                }
            }
        $modelsIdentity = Get-IdentityEvidence -Path $ModelsRoot
    }

    $validationWriteProbe = Join-Path $ValidationRoot "__fileid_validation_write_probe.tmp"
    $validationRenameProbe = Join-Path `
        $ValidationRoot `
        "__fileid_validation_rename_probe.tmp"
    $validationWritable = $false
    $validationRenameWorked = $false
    $validationError = $null
    try {
        [IO.File]::WriteAllText(
            $validationWriteProbe,
            "validation write probe",
            [Text.UTF8Encoding]::new($false)
        )
        $validationWritable = Test-Path -LiteralPath $validationWriteProbe -PathType Leaf
        [IO.File]::Move($validationWriteProbe, $validationRenameProbe)
        $validationRenameWorked = (
            -not (Test-Path -LiteralPath $validationWriteProbe) -and
            (Test-Path -LiteralPath $validationRenameProbe -PathType Leaf)
        )
    } catch {
        $validationError = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
    } finally {
        if (Test-Path -LiteralPath $validationWriteProbe) {
            Remove-Item -LiteralPath $validationWriteProbe -Force
        }
        if (Test-Path -LiteralPath $validationRenameProbe) {
            Remove-Item -LiteralPath $validationRenameProbe -Force
        }
    }

    $markerPath = Join-Path $ValidationRoot ".fileid-sandbox-validation.json"
    $marker = if (Test-Path -LiteralPath $markerPath -PathType Leaf) {
        Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
    } else {
        $null
    }
    $toolsIdentity = Get-IdentityEvidence -Path $ToolsRoot
    $validationIdentity = Get-IdentityEvidence -Path $ValidationRoot
    $checks = [ordered]@{
        criticalFilesPresentAndVerified = (
            @(
                $criticalFiles |
                    Where-Object {
                        -not $_.hashMatches -or
                        -not $_.lengthMatches -or
                        $_.isReparsePoint
                    }
            ).Count -eq 0
        )
        packageManifestMatchesPackage = (
            [string]$packageManifest.packageId -eq
                [string]$Configuration.packageId
        )
        packageManifestEngineAttestationMatches = (
            [int64]$packageManifest.engine.expectedBytes -eq
                [int64]$Configuration.expectedEngine.bytes -and
            [int64]$packageManifest.engine.actualBytes -eq
                [int64]$Configuration.expectedEngine.bytes -and
            [int64]$packageManifest.engine.stagedBytes -eq
                [int64]$Configuration.expectedEngine.bytes -and
            [string]$packageManifest.engine.expectedSha256 -eq
                ([string]$Configuration.expectedEngine.sha256).ToLowerInvariant() -and
            [string]$packageManifest.engine.sourceSha256 -eq
                ([string]$Configuration.expectedEngine.sha256).ToLowerInvariant() -and
            [string]$packageManifest.engine.stagedSha256 -eq
                ([string]$Configuration.expectedEngine.sha256).ToLowerInvariant()
        )
        packageManifestDoesNotStageModelsOrDatabase = (
            -not [bool]$packageManifest.modelsStaged -and
            -not [bool]$packageManifest.seedDatabaseStaged
        )
        toolsCreateDenied = [bool]$toolsWrite.denied
        toolsProbeAbsent = -not (Test-Path -LiteralPath $toolsWriteProbe)
        modelsCreateDeniedWhenMapped = (
            $null -eq $modelsWrite -or [bool]$modelsWrite.denied
        )
        modelsProbeAbsent = (
            $null -eq $modelsWrite -or
            -not (Test-Path -LiteralPath $modelsWriteProbe)
        )
        modelsNotReparsePointWhenMapped = (
            $null -eq $modelsIdentity -or
            [bool]$modelsIdentity.checks.notReparsePoint
        )
        validationWritable = $validationWritable
        validationRenameWorked = $validationRenameWorked
        validationCanaryRemoved = (
            -not (Test-Path -LiteralPath $validationWriteProbe) -and
            -not (Test-Path -LiteralPath $validationRenameProbe)
        )
        validationMarkerMatchesPackage = (
            $null -ne $marker -and
            [string]$marker.packageId -eq [string]$Configuration.packageId
        )
        harnessLaunchDisabled = -not [bool]$Configuration.harnessLaunchPermitted
        toolsIdentityStable = [bool]$toolsIdentity.checks.stable
        toolsIdentityNonzero = (
            [bool]$toolsIdentity.checks.volumeSerialNonzero -and
            [bool]$toolsIdentity.checks.fileIndexNonzero
        )
        toolsNotReparsePoint = [bool]$toolsIdentity.checks.notReparsePoint
        validationIdentityStable = [bool]$validationIdentity.checks.stable
        validationIdentityNonzero = (
            [bool]$validationIdentity.checks.volumeSerialNonzero -and
            [bool]$validationIdentity.checks.fileIndexNonzero
        )
        validationNotReparsePoint = [bool]$validationIdentity.checks.notReparsePoint
        toolsAndValidationHaveDistinctIdentities = -not (
            $toolsIdentity.first.volumeSerialNumber -eq
                $validationIdentity.first.volumeSerialNumber -and
            $toolsIdentity.first.fileIndex -eq
                $validationIdentity.first.fileIndex
        )
    }
    return [ordered]@{
        schemaVersion = 1
        startedAt = Get-UtcTimestamp
        criticalFiles = $criticalFiles
        packageManifest = $packageManifest
        toolsWriteProbe = $toolsWrite
        modelsWriteProbe = $modelsWrite
        modelsIdentity = $modelsIdentity
        validationWriteProbe = [ordered]@{
            writable = $validationWritable
            renameWorked = $validationRenameWorked
            error = $validationError
        }
        validationMarker = $marker
        toolsIdentity = $toolsIdentity
        validationIdentity = $validationIdentity
        checks = $checks
        result = if (@($checks.Values) -contains $false) { "RED" } else { "GREEN" }
        finishedAt = Get-UtcTimestamp
    }
}

$configurationFullPath = [IO.Path]::GetFullPath($ConfigurationPath)
$validationRoot = Split-Path -Parent $configurationFullPath
$artifactsRoot = Join-Path $validationRoot "artifacts"
$stateRoot = Join-Path $validationRoot "state"
New-Item -ItemType Directory -Path $artifactsRoot -Force | Out-Null
New-Item -ItemType Directory -Path $stateRoot -Force | Out-Null

$startedAt = Get-UtcTimestamp
$fatalError = $null
$configuration = $null
$package = $null
$boundary = $null
$network = $null
$python = $null
$engineCopy = $null
$ort = $null
$llama = $null

Write-JsonAtomic `
    -Path (Join-Path $artifactsRoot "preflight-started.json") `
    -Value ([ordered]@{
        schemaVersion = 1
        startedAt = $startedAt
        configurationPath = $configurationFullPath
        processId = $PID
    })

try {
    $configuration = Get-Content -LiteralPath $configurationFullPath -Raw |
        ConvertFrom-Json
    if ([int]$configuration.schemaVersion -ne 1) {
        throw "Unsupported preflight configuration schema: $($configuration.schemaVersion)"
    }
    $toolsRoot = [IO.Path]::GetFullPath([string]$configuration.mounts.tools)
    $corpusRoot = [IO.Path]::GetFullPath([string]$configuration.mounts.corpus)
    $expectedValidationRoot = [IO.Path]::GetFullPath(
        [string]$configuration.mounts.validation
    )
    $modelsRoot = if ([string]::IsNullOrWhiteSpace(
        [string]$configuration.mounts.models
    )) {
        $null
    } else {
        [IO.Path]::GetFullPath([string]$configuration.mounts.models)
    }
    if ($toolsRoot -ne "C:\FileIDTools") {
        throw "Unexpected tools mount: $toolsRoot"
    }
    if ($corpusRoot -ne "C:\FileIDCorpus") {
        throw "Unexpected corpus mount: $corpusRoot"
    }
    if ($expectedValidationRoot -ne "C:\FileIDValidation") {
        throw "Unexpected validation mount: $expectedValidationRoot"
    }
    if ($validationRoot -ne $expectedValidationRoot) {
        throw "Configuration was not loaded from the writable validation mount"
    }
    if ($null -ne $modelsRoot -and $modelsRoot -ne "C:\FileIDModels") {
        throw "Unexpected models mount: $modelsRoot"
    }
    if ([string]$configuration.paths.models -ne [string]$modelsRoot) {
        throw "Models path does not match the configured read-only mount"
    }
    if ([string]$configuration.expectedNetworking -ne "Disable") {
        throw "Sandbox configuration does not require disabled networking"
    }
    $expectedEngineBytes = [int64]$configuration.expectedEngine.bytes
    $expectedEngineSha256 = (
        [string]$configuration.expectedEngine.sha256
    ).ToLowerInvariant()
    if (
        $expectedEngineBytes -le 0 -or
        $expectedEngineSha256 -notmatch "^[0-9a-f]{64}$"
    ) {
        throw "Configuration does not contain a valid explicit engine attestation"
    }
    if (
        [string]$configuration.catalogPolicy -ne
            "fresh-writable-validation-only" -or
        $null -ne $configuration.paths.seedDatabase
    ) {
        throw "Configuration permits a staged or live catalog database"
    }
    $catalogPath = Resolve-ChildPath `
        -Root $validationRoot `
        -RelativePath ([string]$configuration.paths.catalog)
    if (Test-Path -LiteralPath $catalogPath) {
        throw "Fresh validation catalog already exists: $catalogPath"
    }

    try {
        $package = Invoke-PackageVerification `
            -Configuration $configuration `
            -ToolsRoot $toolsRoot `
            -ValidationRoot $validationRoot `
            -ModelsRoot $modelsRoot
    } catch {
        $package = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "package-verification.json") `
        -Value $package

    try {
        $boundary = Invoke-BoundaryProbes `
            -Configuration $configuration `
            -CorpusRoot $corpusRoot
    } catch {
        $boundary = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "boundary-probes.json") `
        -Value $boundary

    try {
        $network = Invoke-NetworkProbe
    } catch {
        $network = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "network-probe.json") `
        -Value $network

    $pythonPath = Resolve-ChildPath `
        -Root $toolsRoot `
        -RelativePath ([string]$configuration.paths.python)
    $harnessPath = Resolve-ChildPath `
        -Root $toolsRoot `
        -RelativePath ([string]$configuration.paths.harness)
    try {
        $versionProbe = Invoke-ChildProcess `
            -FilePath $pythonPath `
            -Arguments @("-I", "--version") `
            -WorkingDirectory $validationRoot `
            -TimeoutMilliseconds 30000
        $harnessProbe = Invoke-ChildProcess `
            -FilePath $pythonPath `
            -Arguments @("-I", $harnessPath, "--help") `
            -WorkingDirectory $validationRoot `
            -TimeoutMilliseconds 30000
        $pythonChecks = [ordered]@{
            versionExitedZero = $versionProbe.exitCode -eq 0
            versionReported = (
                "$($versionProbe.stdout)`n$($versionProbe.stderr)" -match "Python 3\."
            )
            harnessLoaded = $harnessProbe.exitCode -eq 0
            harnessHelpReported = $harnessProbe.stdout -match "--corpus"
            noTimeout = (
                -not $versionProbe.timedOut -and
                -not $harnessProbe.timedOut
            )
        }
        $python = [ordered]@{
            schemaVersion = 1
            version = $versionProbe
            harnessHelp = $harnessProbe
            checks = $pythonChecks
            result = if (@($pythonChecks.Values) -contains $false) {
                "RED"
            } else {
                "GREEN"
            }
        }
    } catch {
        $python = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "python-probe.json") `
        -Value $python

    try {
        $sourceEngineDirectory = Resolve-ChildPath `
            -Root $toolsRoot `
            -RelativePath ([string]$configuration.paths.engineDirectory)
        $sourceEngine = Resolve-ChildPath `
            -Root $toolsRoot `
            -RelativePath ([string]$configuration.paths.engine)
        $writableEngineDirectory = Join-Path $stateRoot "engine"
        if (Test-Path -LiteralPath $writableEngineDirectory) {
            throw "Writable engine state already exists: $writableEngineDirectory"
        }
        Copy-DirectoryContentsNoReparse `
            -Source $sourceEngineDirectory `
            -Destination $writableEngineDirectory
        $writableEngine = Join-Path $writableEngineDirectory "FileIDEngine.exe"
        $sourceSnapshot = Get-FileSnapshot -Path $sourceEngine
        $writableSnapshot = Get-FileSnapshot -Path $writableEngine
        $sourceIdentity = Get-IdentityEvidence -Path $sourceEngine
        $writableIdentity = Get-IdentityEvidence -Path $writableEngine
        $engineChecks = [ordered]@{
            sourceExists = Test-Path -LiteralPath $sourceEngine -PathType Leaf
            writableCopyExists = Test-Path -LiteralPath $writableEngine -PathType Leaf
            sourceBytesExact = (
                [int64]$sourceSnapshot.length -eq $expectedEngineBytes
            )
            writableCopyBytesExact = (
                [int64]$writableSnapshot.length -eq $expectedEngineBytes
            )
            sourceSha256Exact = ([string]$sourceSnapshot.sha256).Equals(
                $expectedEngineSha256,
                [StringComparison]::OrdinalIgnoreCase
            )
            writableCopySha256Exact = (
                [string]$writableSnapshot.sha256
            ).Equals(
                $expectedEngineSha256,
                [StringComparison]::OrdinalIgnoreCase
            )
            hashesMatch = $sourceSnapshot.sha256 -eq $writableSnapshot.sha256
            sourceNotReparsePoint = -not [bool]$sourceSnapshot.isReparsePoint
            writableCopyNotReparsePoint = (
                -not [bool]$writableSnapshot.isReparsePoint
            )
            sourceAndDestinationDiffer = $sourceEngine -ne $writableEngine
            destinationInsideWritableState = $writableEngine.StartsWith(
                "$stateRoot\",
                [StringComparison]::OrdinalIgnoreCase
            )
            sourceIdentityStableNonzero = (
                [bool]$sourceIdentity.checks.stable -and
                [bool]$sourceIdentity.checks.volumeSerialNonzero -and
                [bool]$sourceIdentity.checks.fileIndexNonzero
            )
            destinationIdentityStableNonzero = (
                [bool]$writableIdentity.checks.stable -and
                [bool]$writableIdentity.checks.volumeSerialNonzero -and
                [bool]$writableIdentity.checks.fileIndexNonzero
            )
            sourceAndDestinationIdentitiesDiffer = -not (
                $sourceIdentity.first.volumeSerialNumber -eq
                    $writableIdentity.first.volumeSerialNumber -and
                $sourceIdentity.first.fileIndex -eq
                    $writableIdentity.first.fileIndex
            )
        }
        $engineCopy = [ordered]@{
            schemaVersion = 1
            expected = [ordered]@{
                bytes = $expectedEngineBytes
                sha256 = $expectedEngineSha256
            }
            source = $sourceSnapshot
            destination = $writableSnapshot
            sourceIdentity = $sourceIdentity
            destinationIdentity = $writableIdentity
            checks = $engineChecks
            result = if (@($engineChecks.Values) -contains $false) {
                "RED"
            } else {
                "GREEN"
            }
        }
    } catch {
        $engineCopy = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "engine-copy.json") `
        -Value $engineCopy

    try {
        $ortProbePath = Resolve-ChildPath `
            -Root $toolsRoot `
            -RelativePath ([string]$configuration.paths.ortProbe)
        $writableOrtPath = Join-Path $stateRoot "engine\onnxruntime.dll"
        $ortOutput = Join-Path $artifactsRoot "ort-providers.json"
        $ortProcess = Invoke-ChildProcess `
            -FilePath $pythonPath `
            -Arguments @(
                "-I",
                $ortProbePath,
                "--dll",
                $writableOrtPath,
                "--output",
                $ortOutput,
                "--allowed-python-root",
                $toolsRoot,
                "--require-provider",
                "DmlExecutionProvider"
            ) `
            -WorkingDirectory $stateRoot `
            -TimeoutMilliseconds 30000
        $ortPayload = if (Test-Path -LiteralPath $ortOutput -PathType Leaf) {
            Get-Content -LiteralPath $ortOutput -Raw | ConvertFrom-Json
        } else {
            $null
        }
        $ortChecks = [ordered]@{
            probeExitedZero = $ortProcess.exitCode -eq 0
            resultWritten = $null -ne $ortPayload
            runtimeGreen = (
                $null -ne $ortPayload -and
                [string]$ortPayload.result -eq "GREEN"
            )
            providersReported = (
                $null -ne $ortPayload -and
                @($ortPayload.providers).Count -gt 0
            )
            cpuProviderAvailable = (
                $null -ne $ortPayload -and
                @($ortPayload.providers) -contains "CPUExecutionProvider"
            )
            directMlProviderAvailable = (
                $null -ne $ortPayload -and
                @($ortPayload.providers) -contains "DmlExecutionProvider"
            )
        }
        $ort = [ordered]@{
            schemaVersion = 1
            process = $ortProcess
            payload = $ortPayload
            checks = $ortChecks
            result = if (@($ortChecks.Values) -contains $false) {
                "RED"
            } else {
                "GREEN"
            }
        }
        Write-JsonAtomic -Path $ortOutput -Value $ort
    } catch {
        $ort = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
        Write-JsonAtomic `
            -Path (Join-Path $artifactsRoot "ort-providers.json") `
            -Value $ort
    }

    try {
        $llamaPath = Resolve-ChildPath `
            -Root $toolsRoot `
            -RelativePath ([string]$configuration.paths.llamaMtmdCli)
        $llamaWorkingDirectory = Split-Path -Parent $llamaPath
        $llamaVersionProcess = Invoke-ChildProcess `
            -FilePath $llamaPath `
            -Arguments @("--version") `
            -WorkingDirectory $llamaWorkingDirectory `
            -TimeoutMilliseconds 20000
        $llamaProcess = Invoke-ChildProcess `
            -FilePath $llamaPath `
            -Arguments @("--list-devices") `
            -WorkingDirectory $llamaWorkingDirectory `
            -TimeoutMilliseconds 15000
        $combinedOutput = "$($llamaProcess.stdout)`n$($llamaProcess.stderr)"
        $deviceLines = @(
            $combinedOutput -split "\r?\n" |
                Where-Object { $_ -match "^\s+\S+\d+:" }
        )
        $llamaChecks = [ordered]@{
            versionExitedZero = $llamaVersionProcess.exitCode -eq 0
            versionNoTimeout = -not $llamaVersionProcess.timedOut
            exitedZero = $llamaProcess.exitCode -eq 0
            noTimeout = -not $llamaProcess.timedOut
            listDevicesRecognized = $combinedOutput -match "Available devices:"
        }
        $llama = [ordered]@{
            schemaVersion = 1
            versionProcess = $llamaVersionProcess
            process = $llamaProcess
            deviceLines = $deviceLines
            acceleratorDevicePresent = $deviceLines.Count -gt 0
            checks = $llamaChecks
            result = if (@($llamaChecks.Values) -contains $false) {
                "RED"
            } else {
                "GREEN"
            }
        }
    } catch {
        $llama = [ordered]@{
            result = "RED"
            error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
            checks = [ordered]@{ completed = $false }
        }
    }
    Write-JsonAtomic `
        -Path (Join-Path $artifactsRoot "llama-devices.json") `
        -Value $llama
} catch {
    $fatalError = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
}

$componentChecks = [ordered]@{
    configurationLoaded = $null -ne $configuration
    packageVerificationGreen = (
        $null -ne $package -and [string]$package.result -eq "GREEN"
    )
    corpusBoundaryGreen = (
        $null -ne $boundary -and [string]$boundary.result -eq "GREEN"
    )
    networkIsolationGreen = (
        $null -ne $network -and [string]$network.result -eq "GREEN"
    )
    portablePythonGreen = (
        $null -ne $python -and [string]$python.result -eq "GREEN"
    )
    writableEngineCopyGreen = (
        $null -ne $engineCopy -and [string]$engineCopy.result -eq "GREEN"
    )
    ortProviderProbeGreen = (
        $null -ne $ort -and [string]$ort.result -eq "GREEN"
    )
    llamaDeviceProbeGreen = (
        $null -ne $llama -and [string]$llama.result -eq "GREEN"
    )
    noFatalError = $null -eq $fatalError
}
$failedChecks = @(
    foreach ($entry in $componentChecks.GetEnumerator()) {
        if (-not [bool]$entry.Value) {
            $entry.Key
        }
    }
)
$result = if ($failedChecks.Count -eq 0) { "GREEN" } else { "RED" }
$summary = [ordered]@{
    schemaVersion = 1
    packageId = if ($null -eq $configuration) {
        $null
    } else {
        [string]$configuration.packageId
    }
    startedAt = $startedAt
    finishedAt = Get-UtcTimestamp
    result = $result
    failedChecks = $failedChecks
    fatalError = $fatalError
    processId = $PID
    sandbox = [ordered]@{
        computerName = $env:COMPUTERNAME
        userName = $env:USERNAME
        operatingSystem = [Environment]::OSVersion.VersionString
        is64BitProcess = [Environment]::Is64BitProcess
    }
    checks = $componentChecks
    artifacts = @(
        "boundary-probes.json"
        "engine-copy.json"
        "llama-devices.json"
        "network-probe.json"
        "ort-providers.json"
        "package-verification.json"
        "python-probe.json"
    )
}
Write-JsonAtomic `
    -Path (Join-Path $artifactsRoot "preflight-summary.json") `
    -Value $summary
[IO.File]::WriteAllText(
    (Join-Path $artifactsRoot "preflight-exit-code.txt"),
    "$(if ($result -eq "GREEN") { 0 } else { 1 })`n",
    [Text.UTF8Encoding]::new($false)
)

if ($null -ne $configuration -and [bool]$configuration.autoClose) {
    Start-Process `
        -FilePath "$env:SystemRoot\System32\shutdown.exe" `
        -ArgumentList @(
            "/s"
            "/f"
            "/t"
            "0"
        ) `
        -WindowStyle Hidden
}

if ($result -eq "GREEN") {
    exit 0
}
exit 1

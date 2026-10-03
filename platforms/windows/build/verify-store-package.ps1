[CmdletBinding()]
param([Parameter(Mandatory)][string]$Path)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$packagePath = (Resolve-Path -LiteralPath $Path).Path
$archive = [IO.Compression.ZipFile]::OpenRead($packagePath)
try {
    $entry = $archive.GetEntry('AppxManifest.xml')
    if (-not $entry) { throw 'Package manifest is missing.' }
    $reader = [IO.StreamReader]::new($entry.Open())
    try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
    $identity = $manifest.Package.Identity
    if ($identity.Name -ne 'AdamNolle.FileID' -or
        $identity.Publisher -ne 'CN=B6BC6354-0217-4C63-8B82-7040B465A25E' -or
        $manifest.Package.Properties.PublisherDisplayName -ne 'Adam Nolle') {
        throw 'Package identity does not match the reserved Partner Center listing.'
    }
    $version = (Get-Content (Join-Path $PSScriptRoot '../VERSION') -Raw).Trim() + '.0'
    if ($identity.Version -ne $version -or $identity.ProcessorArchitecture -ne 'x64') {
        throw 'Package version or architecture does not match the supported release.'
    }
    foreach ($required in @('FileID.exe', 'FileIDEngine.exe', 'FileID.pri', 'onnxruntime.dll',
            'DirectML.dll', 'pdfium.dll', 'Assets/StoreLogo.png', 'Assets/Square44x44Logo.png',
            'Assets/Square150x150Logo.png', 'LICENSE.txt')) {
        if (-not $archive.GetEntry($required)) { throw "Package payload missing: $required" }
    }
    foreach ($name in @('Microsoft.WindowsAppRuntime.1.7', 'Microsoft.VCLibs.140.00.UWPDesktop')) {
        if ($name -notin @($manifest.Package.Dependencies.PackageDependency.Name)) {
            throw "Store-managed runtime dependency missing: $name"
        }
    }
    if ($manifest.Package.Capabilities.Capability.Name -ne 'runFullTrust') {
        throw 'The native app requires the declared runFullTrust capability.'
    }
    if ($manifest.Package.Applications.Application.Executable -ne 'FileID.exe') {
        throw 'Unexpected application entry point.'
    }
    Write-Host "Store identity and payload verified: $($identity.Name) $version"
} finally { $archive.Dispose() }

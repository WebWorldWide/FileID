param()

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = (Resolve-Path (Join-Path $ScriptDir "..")).Path
$RepoRoot = (Resolve-Path (Join-Path $PlatformDir "..\..")).Path
$StoreProject = Join-Path $PlatformDir "installer/FileID.StorePackage/FileID.StorePackage.wapproj"
$PackageDir = Join-Path $PlatformDir "dist/store-packages"
$StoreAppPublishDir = Join-Path $PlatformDir "dist/store-app-publish"
$AppPublishDir = Join-Path $PlatformDir "src/FileID.App/bin/x64/Release/net8.0-windows10.0.19041.0/win-x64/publish"

if (-not (Get-Command msbuild -ErrorAction SilentlyContinue)) {
    throw "MSBuild was not found. Install the Visual Studio MSBuild and Windows App Packaging components."
}

Push-Location $PlatformDir
try {
    & .\build\build-all.ps1 -Release
    if ($LASTEXITCODE -ne 0) {
        throw "The Windows release build failed with exit code $LASTEXITCODE."
    }

    $requiredPayload = @(
        "FileIDEngine.exe",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "DirectML.dll",
        "pdfium.dll"
    )
    foreach ($name in $requiredPayload) {
        $staged = Join-Path $PlatformDir "dist/x64/FileID/$name"
        if (-not (Test-Path $staged -PathType Leaf)) {
            throw "Required Store package payload is missing: $staged"
        }
    }

    & python (Join-Path $RepoRoot "shared/scripts/check_binary_privacy.py") $AppPublishDir
    if ($LASTEXITCODE -ne 0) {
        throw "The Store package payload failed the binary privacy gate."
    }

    if (Test-Path $PackageDir) {
        Remove-Item -LiteralPath $PackageDir -Recurse -Force
    }
    if (Test-Path $StoreAppPublishDir) {
        Remove-Item -LiteralPath $StoreAppPublishDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $PackageDir | Out-Null

    & msbuild $StoreProject `
        /t:Build `
        /p:Configuration=Release `
        /p:Platform=x64 `
        /p:RuntimeIdentifier=win-x64 `
        /p:UapAppxPackageBuildMode=StoreUpload `
        /p:AppxBundle=Never `
        /p:AppxPackageSigningEnabled=false `
        /p:GenerateAppInstallerFile=false `
        /p:AppxPackageDir="$PackageDir\" `
        /restore `
        /m `
        /nologo
    if ($LASTEXITCODE -ne 0) {
        throw "The Store MSIX packaging build failed with exit code $LASTEXITCODE."
    }

    $upload = @(Get-ChildItem -Path $PackageDir -Recurse -Filter "*.msixupload" -File)
    if ($upload.Count -ne 1 -or $upload[0].Length -eq 0) {
        throw "Expected one non-empty Store upload package under $PackageDir; found $($upload.Count)."
    }

    $msix = Get-ChildItem -Path $PackageDir -Recurse -Filter "*.msix" -File |
        Where-Object { $_.Name -notmatch "resources" } |
        Select-Object -First 1
    if (-not $msix) {
        throw "The Store build produced an upload archive but no inspectable x64 MSIX package."
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $packageZip = [System.IO.Compression.ZipFile]::OpenRead($msix.FullName)
    try {
        $entries = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $packageZip.Entries) {
            [void]$entries.Add($entry.FullName.Replace('/', '\'))
        }
        $storeProjectXml = [xml](Get-Content -LiteralPath $StoreProject -Raw)
        $versionNode = $storeProjectXml.SelectSingleNode("//*[local-name()='AppxPackageVersion']")
        if (-not $versionNode) {
            throw "The Store packaging project is missing AppxPackageVersion."
        }
        $expectedPackageVersion = $versionNode.InnerText.Trim()
        if ($expectedPackageVersion -notmatch '^[1-9][0-9]{0,4}\.(?:0|[1-9][0-9]{0,4})\.(?:0|[1-9][0-9]{0,4})\.0$') {
            throw "AppxPackageVersion must have a non-zero major, four numeric parts, fourth part zero, and each part at most 65535."
        }
        foreach ($part in $expectedPackageVersion.Split('.')) {
            if ([int]$part -gt 65535) {
                throw "Each AppxPackageVersion component must be at most 65535."
            }
        }

        $sourceManifestPath = Join-Path (Split-Path -Parent $StoreProject) "Package.appxmanifest"
        $sourceManifest = [xml](Get-Content -LiteralPath $sourceManifestPath -Raw)
        $sourceIdentity = $sourceManifest.SelectSingleNode("/*[local-name()='Package']/*[local-name()='Identity']")
        if (-not $sourceIdentity -or
            $sourceIdentity.GetAttribute("Name") -ne "AdamNolle.FileID" -or
            $sourceIdentity.GetAttribute("Publisher") -ne "CN=B6BC6354-0217-4C63-8B82-7040B465A25E" -or
            $sourceIdentity.GetAttribute("Version") -ne $expectedPackageVersion) {
            throw "The source manifest must match the reserved Store identity and AppxPackageVersion $expectedPackageVersion."
        }

    $expectedExecutable = "FileID.App\FileID.exe"
    $sourceApplication = $sourceManifest.SelectSingleNode("/*[local-name()='Package']/*[local-name()='Applications']/*[local-name()='Application']")
    if (-not $sourceApplication -or $sourceApplication.GetAttribute("Executable") -ne $expectedExecutable) {
        throw "The source manifest must launch '$expectedExecutable'."
    }

    $manifestEntry = $packageZip.GetEntry("AppxManifest.xml")
        if (-not $manifestEntry) {
            throw "The MSIX package is missing AppxManifest.xml."
        }
        $manifestStream = $manifestEntry.Open()
        try {
            $packageManifest = [xml]::new()
            $packageManifest.Load($manifestStream)
        }
        finally {
            $manifestStream.Dispose()
        }
        $identityNode = $packageManifest.SelectSingleNode("/*[local-name()='Package']/*[local-name()='Identity']")
        if (-not $identityNode -or
            $identityNode.GetAttribute("Name") -ne "AdamNolle.FileID" -or
            $identityNode.GetAttribute("Publisher") -ne "CN=B6BC6354-0217-4C63-8B82-7040B465A25E" -or
            $identityNode.GetAttribute("Version") -ne $expectedPackageVersion) {
            throw "The packaged identity does not match the reserved Store identity and AppxPackageVersion $expectedPackageVersion."
        }

        $packageApplication = $packageManifest.SelectSingleNode("/*[local-name()='Package']/*[local-name()='Applications']/*[local-name()='Application']")
        if (-not $packageApplication -or $packageApplication.GetAttribute("Executable") -ne $expectedExecutable) {
            throw "The packaged manifest must launch '$expectedExecutable'."
        }
        foreach ($name in @("FileID.exe", "FileIDEngine.exe", "onnxruntime.dll", "onnxruntime_providers_shared.dll", "DirectML.dll", "pdfium.dll")) {
            $name = "FileID.App\$name"
            if (-not $entries.Contains($name)) {
                throw "The MSIX package is missing required app payload '$name'."
            }
        }
        foreach ($name in @("Images\Square44x44Logo.png", "Images\Square150x150Logo.png")) {
            if (-not $entries.Contains($name)) {
                throw "The MSIX package is missing required Store image '$name'."
            }
        }
    }
    finally {
        $packageZip.Dispose()
    }

    Write-Host "Store package ready: $($upload[0].FullName)"
    Write-Host "MSIX payload verified: x64 app, engine, inference/runtime DLLs, and Store logos."
}
finally {
    Pop-Location
}

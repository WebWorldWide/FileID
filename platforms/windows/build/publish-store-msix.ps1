param()

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$PlatformDir = (Resolve-Path (Join-Path $ScriptDir "..")).Path
$RepoRoot = (Resolve-Path (Join-Path $PlatformDir "..\..")).Path
$StoreProject = Join-Path $PlatformDir "installer/FileID.StorePackage/FileID.StorePackage.wapproj"
$PackageDir = Join-Path $PlatformDir ("dist/store-packages/local-" + (Get-Date -Format "yyyyMMdd-HHmmss") + "-$PID")
$StoreAppPublishDir = Join-Path $PlatformDir "dist/store-app-publish"
$AppPublishDir = Join-Path $PlatformDir "src/FileID.App/bin/x64/Release/net8.0-windows10.0.19041.0/win-x64/publish"

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
if (Test-Path -LiteralPath $vswhere) {
    $preferredInstalls = @(& $vswhere -version '[17.0,18.0)' -products * -requires Microsoft.Component.MSBuild -property installationPath)
    $fallbackInstalls = @(& $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath)
    foreach ($vsInstall in @($preferredInstalls) + @($fallbackInstalls) | Select-Object -Unique) {
        $msbuildDir = Join-Path $vsInstall "MSBuild/Current/Bin"
        $packagingTasks = Get-ChildItem -Path (Join-Path $vsInstall "MSBuild/Microsoft/VisualStudio") -Filter "Microsoft.Build.Packaging.Pri.Tasks.dll" -Recurse -File -ErrorAction SilentlyContinue | Select-Object -First 1
        if ((Test-Path -LiteralPath (Join-Path $msbuildDir "MSBuild.exe")) -and $packagingTasks) {
            $env:PATH = "$msbuildDir;$env:PATH"
            Write-Host "Store packaging MSBuild: $(Join-Path $msbuildDir 'MSBuild.exe')"
            break
        }
    }
}
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

    if (Test-Path $StoreAppPublishDir) {
        $distRoot = [System.IO.Path]::GetFullPath((Join-Path $PlatformDir "dist")) + [System.IO.Path]::DirectorySeparatorChar
        $resolvedPublishDir = [System.IO.Path]::GetFullPath($StoreAppPublishDir)
        if (-not $resolvedPublishDir.StartsWith($distRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Store publish cleanup path is outside the workspace dist directory."
        }
        if ((Get-Item -LiteralPath $StoreAppPublishDir).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw "Store publish cleanup path is a reparse point."
        }
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

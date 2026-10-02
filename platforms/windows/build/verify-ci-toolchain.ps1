$ErrorActionPreference = 'Stop'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw 'Visual Studio Installer/vswhere is required.'
}
$installs = @(& $vswhere -version '[17.0,18.0)' -products * -requires Microsoft.Component.MSBuild -property installationPath)
if ($LASTEXITCODE -ne 0 -or $installs.Count -eq 0) {
    throw 'Install Visual Studio 2022 MSBuild and WinUI/MSIX tooling on the Windows runner.'
}
$usable = @($installs | Where-Object {
    $tasksRoot = Join-Path $_ 'MSBuild/Microsoft/VisualStudio'
    (Test-Path -LiteralPath (Join-Path $_ 'MSBuild/Current/Bin/MSBuild.exe')) -and
    (Test-Path -LiteralPath (Join-Path $_ 'Common7/IDE/Extensions/TestPlatform/vstest.console.exe')) -and
    @(Get-ChildItem -LiteralPath $tasksRoot -Filter Microsoft.Build.Packaging.Pri.Tasks.dll -Recurse -File -ErrorAction SilentlyContinue).Count -gt 0
})
if ($usable.Count -eq 0) {
    throw 'The runner needs VS 2022 packaging/PriGen tasks and VSTest.'
}
$sdkBin = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
$makeAppx = @(Get-ChildItem -LiteralPath $sdkBin -Filter makeappx.exe -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq 'x64' })
if ($makeAppx.Count -eq 0) {
    throw 'The Windows SDK x64 MakeAppx tool is missing.'
}
Write-Host "VS 2022 WinUI/MSIX and VSTest tooling: $($usable[0])"
Write-Host "Windows SDK MakeAppx: $($makeAppx[0].FullName)"
foreach ($tool in @('dotnet', 'python')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Required CI tool is missing: $tool"
    }
    & $tool --version
    if ($LASTEXITCODE -ne 0) { throw "Could not run $tool." }
}

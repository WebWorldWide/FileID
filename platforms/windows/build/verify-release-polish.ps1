param(
    [switch]$SkipPackage
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$platformDir = Resolve-Path (Join-Path $scriptDir "..")
$repoRoot = Resolve-Path (Join-Path $platformDir "..\..")

function Invoke-Checked([scriptblock]$Command, [string]$Label) {
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE."
    }
}

Push-Location $repoRoot
try {
    & "$platformDir\build\verify-version.ps1"

    Push-Location "$platformDir\src\engine"
    try {
        Invoke-Checked { rustup run 1.90 cargo clippy --all-targets -- -D warnings } "engine clippy"
        Invoke-Checked { rustup run 1.90 cargo test --all-targets } "engine tests"
        Invoke-Checked {
            rustup run 1.90 cargo test --release --lib `
                million_row_reconciliation_uses_set_based_update -- --ignored
        } "million-row reconciliation gate"
    } finally {
        Pop-Location
    }

    Invoke-Checked {
        dotnet build "$platformDir\FileID.sln" -c Debug -p:Platform=x64 --no-restore --nologo
    } ".NET build"
    Invoke-Checked {
        dotnet test "$platformDir\Tests\FileID.App.Tests\FileID.App.Tests.csproj" --no-restore --nologo
    } "app tests"
    Invoke-Checked {
        dotnet test "$platformDir\Tests\FileID.IpcSchema.Tests\FileID.IpcSchema.Tests.csproj" --no-restore --nologo
    } "IPC schema tests"
    Invoke-Checked {
        dotnet format "$platformDir\FileID.sln" --verify-no-changes --no-restore
    } ".NET format"

    if (-not $SkipPackage) {
        & "$platformDir\build\publish-bundle.ps1" -SkipSign -SkipArm64
        $wix = Get-ChildItem "$env:USERPROFILE\.nuget\packages\wixtoolset.sdk\*\tools\net6.0\wix.dll" -File |
            Sort-Object { [version]$_.Directory.Parent.Parent.Name } -Descending |
            Select-Object -ExpandProperty FullName -First 1
        if (-not $wix) { throw "WiX CLI not found after package build." }
        Invoke-Checked {
            dotnet $wix msi validate "$platformDir\dist\installer\FileID-x64.msi"
        } "WiX MSI validation"
    }

    git diff --check
    if ($LASTEXITCODE -ne 0) { throw "git diff --check failed." }
    Write-Host "Windows release-polish verification PASSED." -ForegroundColor Green
} finally {
    Pop-Location
}

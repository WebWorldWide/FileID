param(
    [string]$VersionFile = (Join-Path $PSScriptRoot "..\VERSION"),
    [string]$CargoToml = (Join-Path $PSScriptRoot "..\src\engine\Cargo.toml")
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $VersionFile -PathType Leaf)) {
    throw "VERSION file not found: $VersionFile"
}
if (-not (Test-Path -LiteralPath $CargoToml -PathType Leaf)) {
    throw "Engine Cargo.toml not found: $CargoToml"
}

$productVersion = (Get-Content -LiteralPath $VersionFile -Raw).Trim()
if ($productVersion -notmatch '^\d+\.\d+\.\d+$') {
    throw "Invalid product version '$productVersion' in $VersionFile; expected x.y.z."
}

$cargoText = Get-Content -LiteralPath $CargoToml -Raw
$packageBlock = [regex]::Match($cargoText, '(?ms)^\[package\]\s*(.*?)(?=^\[|\z)')
if (-not $packageBlock.Success) {
    throw "Could not find [package] in $CargoToml."
}
$cargoVersionMatch = [regex]::Match($packageBlock.Groups[1].Value, '(?m)^version\s*=\s*"([^"]+)"\s*$')
if (-not $cargoVersionMatch.Success) {
    throw "Could not parse the engine package version from $CargoToml."
}
$cargoVersion = $cargoVersionMatch.Groups[1].Value
if ($cargoVersion -ne $productVersion) {
    throw "Version drift: VERSION is '$productVersion' but engine Cargo.toml is '$cargoVersion'."
}

Write-Host "Version contract OK: $productVersion" -ForegroundColor Green

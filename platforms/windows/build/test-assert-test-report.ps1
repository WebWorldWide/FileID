$ErrorActionPreference = 'Stop'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fileid-test-report-" + [guid]::NewGuid().ToString('N'))
$assertReport = Join-Path $PSScriptRoot 'assert-test-report.ps1'
$caseCount = 0

function Assert-RejectedReport([string]$ReportPath) {
    $rejected = $false
    try {
        & $assertReport -Path $ReportPath
    } catch {
        $rejected = $true
    }
    if (-not $rejected) { throw "Invalid report was accepted: $ReportPath" }
}

try {
    New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
    $valid = Join-Path $fixtureRoot 'valid.trx'
    Set-Content -LiteralPath $valid -Value '<TestRun xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010"><ResultSummary><Counters total="2" executed="2" passed="2" failed="0" notExecuted="0" /></ResultSummary></TestRun>'
    & $assertReport -Path $valid
    $caseCount++

    Assert-RejectedReport (Join-Path $fixtureRoot 'missing.trx')
    $caseCount++

    $invalidReports = @(
        '<TestRun><ResultSummary><Counters total="0" executed="0" /></ResultSummary></TestRun>',
        '<TestRun><ResultSummary><Counters total="2" executed="0" notExecuted="2" /></ResultSummary></TestRun>',
        '<TestRun><ResultSummary /></TestRun>',
        '<TestRun><ResultSummary><Counters executed="invalid" /></ResultSummary></TestRun>',
        '<TestRun>'
    )
    foreach ($contents in $invalidReports) {
        $invalid = Join-Path $fixtureRoot 'invalid.trx'
        Set-Content -LiteralPath $invalid -Value $contents
        Assert-RejectedReport $invalid
        $caseCount++
    }
    Write-Host "Test-report gate regression checks passed: $caseCount cases."
} finally {
    if (Test-Path -LiteralPath $fixtureRoot) {
        Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
    }
}

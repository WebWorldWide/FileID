param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'
[xml]$results = Get-Content -LiteralPath $Path -Raw
$counters = $results.TestRun.ResultSummary.Counters
if ([int]$counters.executed -lt 1) {
    throw "Test report '$Path' contains no executed tests."
}
Write-Host "Test report '$Path': executed=$($counters.executed), passed=$($counters.passed), failed=$($counters.failed), skipped=$($counters.notExecuted)."

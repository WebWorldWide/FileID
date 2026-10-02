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
if ([int]$counters.failed -ne 0 -or [int]$counters.passed -ne [int]$counters.executed) {
    throw "Test report '$Path' does not confirm every executed test passed."
}
Write-Host "Test report '$Path': executed=$($counters.executed), passed=$($counters.passed), failed=$($counters.failed), skipped=$($counters.notExecuted)."

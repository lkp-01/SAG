param(
  [string]$TransportSummaryJson = ".\artifacts\k6-transport-summary.json",
  [string]$WorkloadSummaryJson = ".\artifacts\k6-workload-summary.json",
  [string]$FullChainSummaryJson = ".\artifacts\k6-full-chain-summary.json",
  [string]$UserPoolFile = "",
  [switch]$SkipPrecheck
)

$ErrorActionPreference = "Stop"

function Invoke-Scenario {
  param([string]$Name, [string[]]$Args)
  Write-Host ""
  Write-Host "=== Scenario: $Name ===" -ForegroundColor Cyan
  & ".\scripts\ops\run-load-dataplane.ps1" @Args
  $code = $LASTEXITCODE
  if ($code -eq 0) {
    Write-Host "Scenario $Name PASS" -ForegroundColor Green
  } else {
    Write-Host "Scenario $Name FAIL (exit=$code)" -ForegroundColor Red
  }
  return $code
}

function Add-CommonArgs {
  param([string[]]$Args)
  if ($SkipPrecheck) { $Args += "-SkipPrecheck" }
  if (-not [string]::IsNullOrWhiteSpace($UserPoolFile)) {
    $Args += @("-UserPoolFile", $UserPoolFile)
  }
  return $Args
}

$transportArgs = Add-CommonArgs @(
  "-Scenario", "transport", "-RunMode", "dataplane_only",
  "-ConstantRps", "100", "-TestDuration", "2m",
  "-PreAllocatedVUs", "100", "-MaxVUs", "500",
  "-SummaryJson", $TransportSummaryJson
)
$workloadArgs = Add-CommonArgs @(
  "-Scenario", "workload", "-RunMode", "strict",
  "-ConstantRps", "100", "-TestDuration", "2m",
  "-ExpectedDataplaneStatus", "200",
  "-PreAllocatedVUs", "100", "-MaxVUs", "500",
  "-SummaryJson", $WorkloadSummaryJson
)
$fullChainArgs = Add-CommonArgs @(
  "-Scenario", "full_chain", "-RunMode", "strict",
  "-ConstantRps", "20", "-TestDuration", "2m",
  "-ExpectedDataplaneStatus", "200", "-MutationMode", "-RequireRedisQueue",
  "-PreAllocatedVUs", "100", "-MaxVUs", "500",
  "-SummaryJson", $FullChainSummaryJson
)

$transportCode = Invoke-Scenario "transport" $transportArgs
$workloadCode = Invoke-Scenario "workload" $workloadArgs
$fullChainCode = Invoke-Scenario "full_chain" $fullChainArgs

Write-Host ""
Write-Host "=== Regression Gate Result ===" -ForegroundColor Yellow
Write-Host "transport:  exit=$transportCode (reachability only; never capacity-qualified)"
Write-Host "workload:   exit=$workloadCode (exact status/body)"
Write-Host "full_chain: exit=$fullChainCode (Auth/Policy/idempotency/audit/Redis/APISIX/workload)"

if ($transportCode -eq 0 -and $workloadCode -eq 0 -and $fullChainCode -eq 0) { exit 0 }
exit 1

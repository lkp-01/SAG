$ErrorActionPreference = "Stop"

$powerShellGate = Join-Path $PSScriptRoot "run-production-fault-gate.ps1"
$shellGate = Join-Path $PSScriptRoot "run-production-fault-gate.sh"

if (-not (Test-Path -LiteralPath $powerShellGate)) {
  throw "missing PowerShell production fault gate"
}
if (-not (Test-Path -LiteralPath $shellGate)) {
  throw "missing shell production fault gate"
}

& $powerShellGate -SelfTest
if ($LASTEXITCODE -ne 0) {
  throw "PowerShell production fault gate self-test failed"
}

$shellText = Get-Content -LiteralPath $shellGate -Raw
foreach ($required in "kill_bridge", "kill_agent", "kill_connector", "auth_policy_replica", "postgres_failover", "redis_failover", "apisix_workload", "network_impairment") {
  if ($shellText -notmatch [regex]::Escape($required)) {
    throw "shell production fault gate is missing scenario $required"
  }
}

Write-Host "production fault gate contract passed" -ForegroundColor Green

param(
  [ValidateSet("all", "kill_bridge", "kill_agent", "kill_connector", "auth_policy_replica", "postgres_failover", "redis_failover", "apisix_workload", "network_impairment")]
  [string]$Scenario = "all",
  [ValidateRange(1, 100000)][int]$TrafficRps = 350,
  [string]$RunnerScript = $env:SAG_FAULT_SCENARIO_RUNNER,
  [string]$OutputDir = ".\artifacts\production-fault-gate",
  [string]$EnvironmentName = $env:SAG_PERF_ENVIRONMENT,
  [string]$ValidateArtifact = "",
  [switch]$SelfTest
)

$ErrorActionPreference = "Stop"

$allScenarios = @(
  "kill_bridge",
  "kill_agent",
  "kill_connector",
  "auth_policy_replica",
  "postgres_failover",
  "redis_failover",
  "apisix_workload",
  "network_impairment"
)

$scenarioAssertions = @{
  kill_bridge = @("lb_stopped_unready_traffic", "alternate_complete_path_takeover", "accepted_mutation_redispatch_total")
  kill_agent = @("bridge_stopped_unready_traffic", "connector_new_epoch_observed", "old_epoch_response_accepted_total")
  kill_connector = @("agent_became_unready", "unsafe_retry_total", "absolute_deadline_violation_total")
  auth_policy_replica = @("ready_replica_takeover", "unauthorized_allow_total", "fail_open_total")
  postgres_failover = @("auth_policy_fail_closed", "pool_wait_bounded", "audit_buffer_bounded", "connection_storm_total")
  redis_failover = @("bounded_sync_fallback", "pel_recovered", "lost_job_total")
  apisix_workload = @("connector_error_semantics_correct", "response_memory_bounded", "unknown_response_total")
  network_impairment = @("absolute_deadline_preserved", "cancellation_release_slo_met", "deadline_violation_total")
}

function Add-FaultError {
  param([System.Collections.Generic.List[string]]$Errors, [string]$Message)
  $Errors.Add($Message)
}

function Test-AtLeast {
  param($Value, [double]$Minimum)
  return $null -ne $Value -and [double]$Value -ge $Minimum
}

function Test-FaultArtifact {
  param([Parameter(Mandatory = $true)][object]$Artifact)
  $errors = New-Object 'System.Collections.Generic.List[string]'
  if ($Artifact.schema_version -ne "sag.production-fault-gate/v1") { Add-FaultError $errors "unsupported or missing schema_version" }
  if ($allScenarios -notcontains $Artifact.scenario) { Add-FaultError $errors "unknown fault scenario" }
  if ($Artifact.run.runner_exit_code -ne 0) { Add-FaultError $errors "scenario runner exited non-zero" }
  if ($Artifact.run.git_sha -notmatch '^[0-9a-fA-F]{40}$') { Add-FaultError $errors "real Git SHA is required" }
  if ([string]::IsNullOrWhiteSpace([string]$Artifact.run.environment) -or $Artifact.run.environment -eq "unspecified") { Add-FaultError $errors "named isolated environment is required" }
  if ($Artifact.run.fault_injected -ne $true -or $Artifact.run.service_restored -ne $true) { Add-FaultError $errors "fault injection/restore evidence is incomplete" }

  $submitted = [long]$Artifact.traffic.submitted_total
  $classified = [long]$Artifact.traffic.classified_total
  $classificationSum = 0L
  if ($null -eq $Artifact.traffic.final_classifications) {
    Add-FaultError $errors "per-request final classifications are missing"
  } else {
    foreach ($property in $Artifact.traffic.final_classifications.PSObject.Properties) {
      $classificationSum += [long]$property.Value
    }
  }
  if ($submitted -le 0 -or $classified -ne $submitted -or $classificationSum -ne $submitted) {
    Add-FaultError $errors "submitted, classified, and classification totals must match and be non-zero"
  }

  foreach ($field in "unknown_job_total", "duplicate_side_effect_total", "incorrect_authorization_total", "unready_accept_total", "permanent_pel_total") {
    if ($null -eq $Artifact.results.$field -or [long]$Artifact.results.$field -ne 0) {
      Add-FaultError $errors "$field must be present and zero"
    }
  }
  if ($Artifact.results.business_slo_met -ne $true) { Add-FaultError $errors "business SLO was not met" }
  if ($null -eq $Artifact.results.rto_ms -or $null -eq $Artifact.results.rto_limit_ms -or [double]$Artifact.results.rto_ms -gt [double]$Artifact.results.rto_limit_ms) {
    Add-FaultError $errors "RTO exceeded or evidence is missing"
  }

  foreach ($field in "expected_business_status_rate", "correct_response_body_rate", "auth_participation_rate", "policy_participation_rate", "audit_completion_rate") {
    if (-not (Test-AtLeast $Artifact.evidence.$field 0.99)) { Add-FaultError $errors "$field below 0.99 or missing" }
  }
  if ([double]$Artifact.evidence.audit_completion_rate -ne 1.0) { Add-FaultError $errors "audit completion evidence must be exactly 1.0" }
  if ($Artifact.evidence.tls_verified -ne $true) { Add-FaultError $errors "TLS verification evidence is missing" }

  $resources = $Artifact.resources
  if ($null -eq $resources.hard_permits_peak -or $null -eq $resources.hard_permits_limit -or [double]$resources.hard_permits_peak -gt [double]$resources.hard_permits_limit) {
    Add-FaultError $errors "hard permit limit exceeded or missing"
  }
  if ($null -eq $resources.pg_connections_peak -or $null -eq $resources.pg_connections_budget -or [double]$resources.pg_connections_peak -gt [double]$resources.pg_connections_budget) {
    Add-FaultError $errors "PostgreSQL connection budget exceeded or missing"
  }
  if ($resources.response_memory_bounded -ne $true) { Add-FaultError $errors "response memory bound is not proven" }

  if ($allScenarios -contains $Artifact.scenario) {
    foreach ($assertionName in $scenarioAssertions[$Artifact.scenario]) {
      $value = $Artifact.assertions.$assertionName
      if ($assertionName -match '_total$') {
        if ($null -eq $value -or [long]$value -ne 0) { Add-FaultError $errors "$assertionName must be present and zero" }
      } elseif ($value -ne $true) {
        Add-FaultError $errors "$assertionName must be true"
      }
    }
  }
  return $errors
}

function Assert-FaultArtifactFile {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) { throw "artifact not found: $Path" }
  $artifact = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
  $errors = Test-FaultArtifact $artifact
  if ($errors.Count -gt 0) {
    Write-Host "FAIL $Path" -ForegroundColor Red
    $errors | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    return $false
  }
  Write-Host "PASS $Path" -ForegroundColor Green
  return $true
}

function New-SelfTestFixture {
  [pscustomobject]@{
    schema_version = "sag.production-fault-gate/v1"
    scenario = "kill_bridge"
    run = [pscustomobject]@{ runner_exit_code = 0; git_sha = ("a" * 40); environment = "isolated-test"; fault_injected = $true; service_restored = $true }
    traffic = [pscustomobject]@{ submitted_total = 100; classified_total = 100; final_classifications = [pscustomobject]@{ business_success = 98; expected_unavailable = 2 } }
    results = [pscustomobject]@{ unknown_job_total = 0; duplicate_side_effect_total = 0; incorrect_authorization_total = 0; unready_accept_total = 0; permanent_pel_total = 0; business_slo_met = $true; rto_ms = 500; rto_limit_ms = 1000 }
    evidence = [pscustomobject]@{ expected_business_status_rate = 1; correct_response_body_rate = 1; auth_participation_rate = 1; policy_participation_rate = 1; audit_completion_rate = 1; tls_verified = $true }
    resources = [pscustomobject]@{ hard_permits_peak = 10; hard_permits_limit = 20; pg_connections_peak = 8; pg_connections_budget = 16; response_memory_bounded = $true }
    assertions = [pscustomobject]@{ lb_stopped_unready_traffic = $true; alternate_complete_path_takeover = $true; accepted_mutation_redispatch_total = 0 }
  }
}

function Invoke-SelfTest {
  $fixture = New-SelfTestFixture
  if ((Test-FaultArtifact $fixture).Count -ne 0) { throw "valid fault fixture was rejected" }
  $fixture.traffic.classified_total = 99
  if ((Test-FaultArtifact $fixture).Count -eq 0) { throw "unclassified requests were accepted" }
  $fixture.traffic.classified_total = 100
  $fixture.results.duplicate_side_effect_total = 1
  if ((Test-FaultArtifact $fixture).Count -eq 0) { throw "duplicate mutation side effect was accepted" }
  $fixture.results.duplicate_side_effect_total = 0
  $fixture.evidence.audit_completion_rate = 0
  if ((Test-FaultArtifact $fixture).Count -eq 0) { throw "missing audit evidence was accepted" }
  $fixture.evidence.audit_completion_rate = 1
  $fixture.assertions.alternate_complete_path_takeover = $false
  if ((Test-FaultArtifact $fixture).Count -eq 0) { throw "missing alternate path takeover was accepted" }
  Write-Host "production fault gate self-test passed" -ForegroundColor Green
}

if ($SelfTest) { Invoke-SelfTest; exit 0 }
if (-not [string]::IsNullOrWhiteSpace($ValidateArtifact)) {
  if (Assert-FaultArtifactFile $ValidateArtifact) { exit 0 }
  exit 1
}

if ($env:SAG_FAULT_GATE_ACK -ne "AUTHORIZED_ISOLATED_ENVIRONMENT") {
  throw "set SAG_FAULT_GATE_ACK=AUTHORIZED_ISOLATED_ENVIRONMENT only for an approved destructive test environment"
}
if ([string]::IsNullOrWhiteSpace($EnvironmentName)) { throw "set -EnvironmentName or SAG_PERF_ENVIRONMENT" }
if ([string]::IsNullOrWhiteSpace($RunnerScript) -or -not (Test-Path -LiteralPath $RunnerScript)) {
  throw "an environment-specific -RunnerScript or SAG_FAULT_SCENARIO_RUNNER is required"
}
$gitSha = (& git rev-parse HEAD 2>$null)
if ($LASTEXITCODE -ne 0 -or $gitSha -notmatch '^[0-9a-fA-F]{40}$') { throw "a recognized Git worktree and real commit SHA are required for an external fault gate" }

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$runId = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
$selectedScenarios = if ($Scenario -eq "all") { $allScenarios } else { @($Scenario) }
$artifacts = @()
foreach ($fault in $selectedScenarios) {
  $artifactPath = Join-Path $OutputDir "$runId-$fault.json"
  & $RunnerScript -Scenario $fault -TrafficRps $TrafficRps -ArtifactPath $artifactPath -EnvironmentName $EnvironmentName -GitSha $gitSha
  if ($LASTEXITCODE -ne 0) { throw "scenario runner failed for $fault" }
  if (-not (Assert-FaultArtifactFile $artifactPath)) { exit 1 }
  $artifacts += $artifactPath
}

$result = [ordered]@{
  schema_version = "sag.production-fault-gate-result/v1"
  qualification = "passed"
  environment = $EnvironmentName
  traffic_rps = $TrafficRps
  scenarios = $selectedScenarios
  artifacts = $artifacts
}
$resultPath = Join-Path $OutputDir "$runId-result.json"
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $resultPath -Encoding UTF8
Write-Host "Production fault gate passed: $resultPath" -ForegroundColor Green

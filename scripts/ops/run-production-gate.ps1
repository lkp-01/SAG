param(
  [ValidateSet("transport", "workload", "full_chain")][string]$Scenario = "full_chain",
  [int]$TargetRps = 500,
  [ValidateRange(3, 20)][int]$Repeats = 3,
  [ValidateRange(120, 1440)][int]$SoakMinutes = 120,
  [ValidateRange(10, 15)][int]$SteadyMinutes = 10,
  [string]$OutputDir = ".\artifacts\production-gate",
  [string]$EnvironmentName = $env:SAG_PERF_ENVIRONMENT,
  [double]$MinBusinessSuccessRate = 0.99,
  [double]$MinCompletedRpsRatio = 0.98,
  [double]$MaxP95Ms = 2500,
  [double]$MaxP99Ms = 5000,
  [double]$MaxPgPoolWaitP95Ms = 50,
  [double]$MaxRedisPelAgeMs = 1000,
  [string]$ValidateArtifact = "",
  [switch]$SelfTest
)

$ErrorActionPreference = "Stop"

function Add-ArtifactError {
  param([System.Collections.Generic.List[string]]$Errors, [string]$Message)
  $Errors.Add($Message)
}

function Test-NumberAtLeast {
  param($Value, [double]$Minimum)
  return $null -ne $Value -and [double]$Value -ge $Minimum
}

function Test-ProductionArtifact {
  param([Parameter(Mandatory = $true)][object]$Artifact)
  $errors = New-Object 'System.Collections.Generic.List[string]'
  if ($Artifact.schema_version -ne "sag.production-gate/v1") { Add-ArtifactError $errors "unsupported or missing schema_version" }
  if ($Artifact.scenario -ne "full_chain") { Add-ArtifactError $errors "capacity qualification requires scenario=full_chain" }
  if ($Artifact.run.k6_exit_code -ne 0) { Add-ArtifactError $errors "k6 exited non-zero" }
  if ($Artifact.run.git_sha -notmatch '^[0-9a-fA-F]{40}$') { Add-ArtifactError $errors "a real Git SHA is required" }
  if ($null -eq $Artifact.run.image_digests -or @($Artifact.run.image_digests).Count -eq 0) { Add-ArtifactError $errors "image digests are required" }
  foreach ($digest in @($Artifact.run.image_digests)) {
    if ([string]$digest -notmatch 'sha256:[0-9a-fA-F]{64}') { Add-ArtifactError $errors "invalid or mutable image digest" }
  }
  if ([string]::IsNullOrWhiteSpace([string]$Artifact.run.environment) -or $Artifact.run.environment -eq "unspecified") { Add-ArtifactError $errors "named test environment is required" }
  if ($Artifact.config_snapshot.insecure_skip_tls_verify -ne $false) { Add-ArtifactError $errors "TLS verification was disabled" }
  if ($Artifact.config_snapshot.mutation -ne $true) { Add-ArtifactError $errors "mutation/idempotency path did not participate" }
  if ($Artifact.config_snapshot.require_redis_queue -ne $true) { Add-ArtifactError $errors "Redis queue path was not required" }

  if (-not (Test-NumberAtLeast $Artifact.results.business_success_rate $MinBusinessSuccessRate)) { Add-ArtifactError $errors "business_success_rate below threshold or missing" }
  if ($null -eq $Artifact.results.dropped_iterations -or [double]$Artifact.results.dropped_iterations -ne 0) { Add-ArtifactError $errors "generator dropped iterations or evidence missing" }
  if ($null -eq $Artifact.results.latency_ms.p95 -or [double]$Artifact.results.latency_ms.p95 -gt $MaxP95Ms) { Add-ArtifactError $errors "p95 exceeds threshold or is missing" }
  if ($null -eq $Artifact.results.latency_ms.p99 -or [double]$Artifact.results.latency_ms.p99 -gt $MaxP99Ms) { Add-ArtifactError $errors "p99 exceeds threshold or is missing" }
  $target = [double]$Artifact.results.target_rps
  $actual = [double]$Artifact.results.actual_completed_rps
  if ($target -le 0 -or $actual -lt ($target * $MinCompletedRpsRatio)) { Add-ArtifactError $errors "completed RPS did not reach target ratio" }

  foreach ($errorProperty in $Artifact.results.business_error_distribution.PSObject.Properties) {
    if ([double]$errorProperty.Value -ne 0) { Add-ArtifactError $errors "business error evidence is non-zero: $($errorProperty.Name)" }
  }
  foreach ($statusProperty in $Artifact.results.http_status_distribution.PSObject.Properties) {
    if ($statusProperty.Name -ne [string]$Artifact.config_snapshot.expected_status -and [double]$statusProperty.Value -gt 0) {
      Add-ArtifactError $errors "unexpected business HTTP status $($statusProperty.Name)"
    }
  }

  foreach ($name in "auth_rate", "policy_rate", "audit_rate", "redis_queue_rate", "idempotency_rate", "workload_rate") {
    if (-not (Test-NumberAtLeast $Artifact.evidence.$name $MinBusinessSuccessRate)) { Add-ArtifactError $errors "missing/failed $name evidence" }
  }
  if ([double]$Artifact.evidence.audit_rate -ne 1.0) { Add-ArtifactError $errors "sampled audit evidence is not complete" }

  $resources = $Artifact.evidence.resources
  if ($resources.status -ne "complete") { Add-ArtifactError $errors "resource watermark evidence is missing" }
  if ($resources.process_rss_within_budget -ne $true) { Add-ArtifactError $errors "process RSS exceeded or lacks budget evidence" }
  if ($null -eq $resources.load_generator_cpu_pct -or [double]$resources.load_generator_cpu_pct -gt 85) { Add-ArtifactError $errors "load generator CPU lacks headroom" }
  if ($null -eq $resources.load_generator_network_utilization_pct -or [double]$resources.load_generator_network_utilization_pct -gt 80) { Add-ArtifactError $errors "load generator network lacks headroom" }

  $dependencies = $Artifact.evidence.dependencies
  if ($dependencies.status -ne "complete") { Add-ArtifactError $errors "dependency evidence is missing" }
  if ($null -eq $dependencies.apisix_requests_delta -or [double]$dependencies.apisix_requests_delta -le 0) { Add-ArtifactError $errors "APISIX participation is not proven" }
  if ($null -eq $dependencies.pg_pool_wait_p95_ms -or [double]$dependencies.pg_pool_wait_p95_ms -gt $MaxPgPoolWaitP95Ms) { Add-ArtifactError $errors "PG pool wait exceeds threshold or is missing" }
  if ($null -eq $dependencies.redis_pel_oldest_ms -or [double]$dependencies.redis_pel_oldest_ms -gt $MaxRedisPelAgeMs) { Add-ArtifactError $errors "Redis PEL age exceeds threshold or is missing" }
  if ($null -eq $dependencies.audit_dropped_total -or [double]$dependencies.audit_dropped_total -ne 0) { Add-ArtifactError $errors "audit drops exceeded zero budget or evidence missing" }
  if ($null -eq $dependencies.authorization_errors_total -or [double]$dependencies.authorization_errors_total -ne 0) { Add-ArtifactError $errors "incorrect authorization was observed or evidence missing" }

  return $errors
}

function Assert-ArtifactFile {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) { throw "artifact not found: $Path" }
  $artifact = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
  $errors = Test-ProductionArtifact $artifact
  if ($errors.Count -gt 0) {
    Write-Host "FAIL $Path" -ForegroundColor Red
    $errors | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    return $false
  }
  Write-Host "PASS $Path" -ForegroundColor Green
  return $true
}

function Invoke-SelfTest {
  $fixture = [pscustomobject]@{
    schema_version = "sag.production-gate/v1"; scenario = "full_chain"
    run = [pscustomobject]@{ k6_exit_code = 0; git_sha = ("a" * 40); image_digests = @("sha256:" + ("b" * 64)); environment = "test" }
    config_snapshot = [pscustomobject]@{ insecure_skip_tls_verify = $false; mutation = $true; require_redis_queue = $true; expected_status = 200 }
    results = [pscustomobject]@{
      target_rps = 100; actual_completed_rps = 100; business_success_rate = 1; dropped_iterations = 0
      latency_ms = [pscustomobject]@{ p95 = 10; p99 = 20 }
      business_error_distribution = [pscustomobject]@{}
      http_status_distribution = [pscustomobject]@{ "200" = 1000 }
    }
    evidence = [pscustomobject]@{
      auth_rate = 1; policy_rate = 1; audit_rate = 1; redis_queue_rate = 1; idempotency_rate = 1; workload_rate = 1
      resources = [pscustomobject]@{ status = "complete"; process_rss_within_budget = $true; load_generator_cpu_pct = 20; load_generator_network_utilization_pct = 30 }
      dependencies = [pscustomobject]@{ status = "complete"; apisix_requests_delta = 1000; pg_pool_wait_p95_ms = 1; redis_pel_oldest_ms = 0; audit_dropped_total = 0; authorization_errors_total = 0 }
    }
  }
  if ((Test-ProductionArtifact $fixture).Count -ne 0) { throw "self-test valid fixture was rejected" }
  $fixture.results.http_status_distribution = [pscustomobject]@{ "200" = 999; "500" = 1 }
  if ((Test-ProductionArtifact $fixture).Count -eq 0) { throw "self-test accepted HTTP 500" }
  $fixture.results.http_status_distribution = [pscustomobject]@{ "200" = 1000 }
  $fixture.evidence.audit_rate = 0
  if ((Test-ProductionArtifact $fixture).Count -eq 0) { throw "self-test accepted missing audit evidence" }
  $fixture.evidence.audit_rate = 1
  $fixture.evidence.workload_rate = 0
  if ((Test-ProductionArtifact $fixture).Count -eq 0) { throw "self-test accepted wrong workload body/correlation" }
  $fixture.evidence.workload_rate = 1
  $fixture.results.dropped_iterations = 1
  if ((Test-ProductionArtifact $fixture).Count -eq 0) { throw "self-test accepted dropped iteration" }
  Write-Host "production gate self-test passed" -ForegroundColor Green
}

if ($SelfTest) { Invoke-SelfTest; exit 0 }
if (-not [string]::IsNullOrWhiteSpace($ValidateArtifact)) {
  if (Assert-ArtifactFile $ValidateArtifact) { exit 0 }
  exit 1
}
if ($Scenario -ne "full_chain") { throw "Only full_chain can run the production capacity gate" }
if ([string]::IsNullOrWhiteSpace($EnvironmentName)) { throw "set -EnvironmentName or SAG_PERF_ENVIRONMENT" }
foreach ($required in "SAG_IMAGE_DIGESTS_JSON", "SAG_RESOURCE_EVIDENCE_JSON", "SAG_DEPENDENCY_EVIDENCE_JSON") {
  $value = [Environment]::GetEnvironmentVariable($required)
  if ([string]::IsNullOrWhiteSpace($value) -or -not (Test-Path -LiteralPath $value)) { throw "$required must name a captured evidence JSON file" }
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$runId = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
$artifacts = @()
for ($i = 1; $i -le $Repeats; $i++) {
  $summaryPath = Join-Path $OutputDir "$runId-repeat-$i-summary.json"
  $artifactPath = Join-Path $OutputDir "$runId-repeat-$i-artifact.json"
  & "$PSScriptRoot\run-load-dataplane.ps1" -Scenario full_chain -RunMode strict `
    -ConstantRps $TargetRps -TestDuration "${SteadyMinutes}m" -ProductionGate `
    -MutationMode -RequireRedisQueue -DisableInsecureSkipTlsVerify `
    -EnvironmentName $EnvironmentName -SummaryJson $summaryPath -ArtifactJson $artifactPath
  if ($LASTEXITCODE -ne 0 -or -not (Assert-ArtifactFile $artifactPath)) { exit 1 }
  $artifacts += $artifactPath
}

$soakSummary = Join-Path $OutputDir "$runId-soak-summary.json"
$soakArtifact = Join-Path $OutputDir "$runId-soak-artifact.json"
& "$PSScriptRoot\run-load-dataplane.ps1" -Scenario full_chain -RunMode strict `
  -ConstantRps $TargetRps -TestDuration "${SoakMinutes}m" -ProductionGate `
  -MutationMode -RequireRedisQueue -DisableInsecureSkipTlsVerify `
  -EnvironmentName $EnvironmentName -SummaryJson $soakSummary -ArtifactJson $soakArtifact
if ($LASTEXITCODE -ne 0 -or -not (Assert-ArtifactFile $soakArtifact)) { exit 1 }
$artifacts += $soakArtifact

$result = [ordered]@{
  schema_version = "sag.production-gate-result/v1"
  qualification = "passed"
  scenario = "full_chain"
  target_rps = $TargetRps
  repeats = $Repeats
  steady_minutes = $SteadyMinutes
  soak_minutes = $SoakMinutes
  artifacts = $artifacts
}
$resultPath = Join-Path $OutputDir "$runId-gate-result.json"
$result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $resultPath -Encoding UTF8
Write-Host "Production gate passed: $resultPath" -ForegroundColor Green

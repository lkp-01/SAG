param(
  # 若指定，则覆盖下方四个 Base/Dataplane URL（便于在 test_192.168.8.87 上跑 k6 仍压新 Edge）
  [string]$EdgeHost = "",
  [string]$DataplaneUrl = "https://172.16.9.107:10080/dev/",
  [string]$AuthBaseUrl = "http://172.16.9.107:8080",
  [string]$PolicyBaseUrl = "http://172.16.9.107:8081",
  [string]$ControlBaseUrl = "http://172.16.9.107:8090",
  [string]$AppId = "app-001",
  [string]$Username = "admin",
  [string]$Password = "Admin@123",
  [ValidateSet("strict", "capacity", "dataplane_only")][string]$RunMode = "strict",
  [Alias("ScenarioType")][ValidateSet("transport", "workload", "full_chain", "mixed_fullchain", "policy_only", "dataplane_only", "auth_login_verify")][string]$Scenario = "full_chain",
  [int]$LoginEveryN = 1,
  [int]$ControlEveryN = 1,
  [int]$PolicyListEveryN = 1,
  [switch]$ControlPlaneBlocking,
  [int]$LoginRetries = 0,
  [int]$LoginRetryBackoffMs = 50,
  [string]$UserPoolFile = "",
  [string]$SharedToken = "",
  # Default 90s aligns with Zentinel route timeout (90s) and bridge forward (60s); 20s causes k6 status-0 while server still waits.
  [string]$RequestTimeout = "90s",
  [ValidateSet("strict", "capacity", "dataplane", "dataplane_routed", "auth")][string]$GateProfile = "",
  [int]$StartQps = 100,
  [int]$Stage1Qps = 200,
  [int]$Stage2Qps = 500,
  [int]$Stage3Qps = 800,
  [int]$Stage4Qps = 1000,
  [string]$Stage1Duration = "2m",
  [string]$Stage2Duration = "3m",
  [string]$Stage3Duration = "3m",
  [string]$Stage4Duration = "5m",
  [int]$PreAllocatedVUs = 3000,
  [int]$MaxVUs = 20000,
  [string]$SummaryJson = ".\artifacts\k6-fullchain-summary.json",
  [string]$ArtifactJson = "",
  [string]$EnvironmentName = "unspecified",
  [int]$ExpectedDataplaneStatus = 200,
  [string]$TestDuration = "",
  [switch]$ProductionGate,
  [switch]$RequireRedisQueue,
  [switch]$MutationMode,
  [int]$AuditSampleEveryN = 100,
  [int]$AuditLagTimeoutMs = 5000,
  [switch]$DisableInsecureSkipTlsVerify,
  [int]$ExtraApisEveryN = 10,
  [switch]$IncludeIdentityApis,
  [switch]$IncludeUsersApis,
  [switch]$IncludeControlAppsApis,
  [switch]$SkipVerifyAfterLogin,
  [switch]$SkipPrecheck,
  # When set, k6 treats HTTP 202 (bridge Redis queue) as success for dataplane_get metrics.
  [switch]$AcceptDataplane202,
  # After 202, poll GET /__sag/queue/{id}/status until done (same origin as dataplane). High-QPS dataplane runs should pass -AcceptDataplane202 -PollDataplane202 together (see tunnel-capacity-bootstrap.md).
  [switch]$PollDataplane202,
  # When set, k6 treats HTTP 429 (queue full / hard reject) as an expected shed response under overload.
  [switch]$AcceptDataplane429Shed,
  # Full-chain load: fewer logins, no control/policy-list/extra APIs per iter (reduces auth/policy storms). Use with mixed_fullchain.
  [switch]$SteadyFullchain,
  # If > 0: overrides StartQps and all stage QPS to this value (flat arrival rate). See k6 script header for what "RPS" means per scenario.
  [int]$ConstantRps = 0,
  [ValidateSet("strict", "apisix_routed")][string]$DataplaneSuccessMode = "strict",
  [switch]$NoCapacityVuCap
)

$ErrorActionPreference = "Stop"

if ($Scenario -eq "mixed_fullchain") { $Scenario = "full_chain" }
if ($Scenario -eq "dataplane_only") { $Scenario = "transport" }
if ([string]::IsNullOrWhiteSpace($ArtifactJson)) {
  $ArtifactJson = if ($SummaryJson.EndsWith(".json")) {
    $SummaryJson.Substring(0, $SummaryJson.Length - 5) + ".artifact.json"
  } else {
    $SummaryJson + ".artifact.json"
  }
}
if ($ExpectedDataplaneStatus -lt 200 -or $ExpectedDataplaneStatus -ge 300) {
  throw "ExpectedDataplaneStatus must be a specified 2xx code"
}
if ($ProductionGate -and $Scenario -ne "full_chain") {
  throw "ProductionGate only qualifies Scenario=full_chain"
}

if (-not [string]::IsNullOrWhiteSpace($EdgeHost)) {
  $h = $EdgeHost.Trim() -replace "^https?://", "" -replace "/$", ""
  if ($h -eq "..." -or $h -eq ".." -or $h -eq "." -or ($h.Length -ge 2 -and $h -match '^\.+$')) {
    Write-Host "Invalid -EdgeHost: '$EdgeHost'. The literal '...' in docs is a placeholder, not a hostname. Example: -EdgeHost 172.16.9.107" -ForegroundColor Red
    exit 1
  }
  $DataplaneUrl = "https://${h}:10080/dev/"
  $AuthBaseUrl = "http://${h}:8080"
  $PolicyBaseUrl = "http://${h}:8081"
  $ControlBaseUrl = "http://${h}:8090"
}

function Assert-SagServiceUrl {
  param(
    [Parameter(Mandatory = $true)][string]$Label,
    [Parameter(Mandatory = $true)][string]$Url
  )
  try {
    $u = [System.Uri]::new($Url)
  }
  catch {
    Write-Host "Invalid ${Label} URL: $Url -> $($_.Exception.Message)" -ForegroundColor Red
    exit 1
  }
  $hn = $u.Host
  if ([string]::IsNullOrWhiteSpace($hn)) {
    Write-Host "Invalid ${Label} URL (missing host): $Url" -ForegroundColor Red
    exit 1
  }
  if ($hn -eq "..." -or ($hn.Length -ge 2 -and $hn -match '^\.+$')) {
    Write-Host "Invalid ${Label} host '$hn': do not paste '...' from documentation. Set -EdgeHost to your Edge IP or pass explicit -DataplaneUrl / -AuthBaseUrl." -ForegroundColor Red
    exit 1
  }
}

Assert-SagServiceUrl "auth" $AuthBaseUrl
Assert-SagServiceUrl "policy" $PolicyBaseUrl
Assert-SagServiceUrl "control" $ControlBaseUrl
Assert-SagServiceUrl "dataplane" $DataplaneUrl

if (-not (Get-Command k6 -ErrorAction SilentlyContinue)) {
  Write-Host "k6 未安装，请先安装：https://k6.io/docs/get-started/installation/" -ForegroundColor Yellow
  exit 1
}

$scriptPath = Join-Path $PSScriptRoot "load-dataplane-k6.js"
if (-not (Test-Path $scriptPath)) {
  Write-Host "未找到脚本: $scriptPath" -ForegroundColor Red
  exit 1
}

$summaryDir = Split-Path -Parent $SummaryJson
if ([string]::IsNullOrWhiteSpace($summaryDir)) {
  $summaryDir = "."
}
$summaryPath = Resolve-Path $summaryDir -ErrorAction SilentlyContinue
if (-not $summaryPath) {
  New-Item -ItemType Directory -Path $summaryDir -Force | Out-Null
}

function Test-TcpEndpoint {
  param(
    [Parameter(Mandatory = $true)][string]$Url,
    [int]$TimeoutMs = 2500
  )
  try {
    $uri = [System.Uri]$Url
    $hostName = $uri.Host
    $port = $uri.Port
    if ([string]::IsNullOrWhiteSpace($hostName)) {
      return @{ ok = $false; target = $Url; reason = "URL has no host" }
    }
    if ($hostName -eq "..." -or ($hostName.Length -ge 2 -and $hostName -match '^\.+$')) {
      return @{ ok = $false; target = $Url; reason = "invalid host '$hostName' (placeholder?)" }
    }
    $client = New-Object System.Net.Sockets.TcpClient
    $iar = $client.BeginConnect($hostName, $port, $null, $null)
    $ok = $iar.AsyncWaitHandle.WaitOne($TimeoutMs, $false)
    if (-not $ok) {
      $client.Close()
      return @{ ok = $false; target = "${hostName}:$port"; reason = "connect timeout (${TimeoutMs}ms)" }
    }
    $client.EndConnect($iar)
    $client.Close()
    return @{ ok = $true; target = "${hostName}:$port"; reason = "ok" }
  }
  catch {
    return @{ ok = $false; target = $Url; reason = $_.Exception.Message }
  }
}

if (-not $SkipPrecheck) {
  Write-Host "Precheck: endpoint TCP connectivity" -ForegroundColor Yellow
  $checks = @(
    @{ name = "auth"; url = $AuthBaseUrl },
    @{ name = "policy"; url = $PolicyBaseUrl },
    @{ name = "control"; url = $ControlBaseUrl },
    @{ name = "dataplane"; url = $DataplaneUrl }
  )
  $precheckFailed = $false
  foreach ($c in $checks) {
    $r = Test-TcpEndpoint -Url $c.url
    if ($r.ok) {
      Write-Host "  PASS [$($c.name)] $($r.target)" -ForegroundColor Green
    }
    else {
      $precheckFailed = $true
      Write-Host "  FAIL [$($c.name)] $($r.target) -> $($r.reason)" -ForegroundColor Red
    }
  }
  if ($precheckFailed) {
    Write-Host "Precheck failed: please verify Linux VM service bind and routing/firewall." -ForegroundColor Red
    Write-Host "Use -SkipPrecheck to bypass this gate." -ForegroundColor DarkGray
    exit 2
  }
}

if ($RunMode -eq "capacity" -and $ConstantRps -le 0 -and -not $NoCapacityVuCap.IsPresent) {
  if ($LoginEveryN -le 1) { $LoginEveryN = 20 }
  if ($ControlEveryN -le 1) { $ControlEveryN = 0 }
  if ($PolicyListEveryN -le 1) { $PolicyListEveryN = 0 }
  if ($ExtraApisEveryN -le 10) { $ExtraApisEveryN = 0 }
  if ($LoginRetries -le 0) { $LoginRetries = 1 }
  if ($Scenario -eq "full_chain") {
    if ($PreAllocatedVUs -ge 500) { $PreAllocatedVUs = 300 }
    if ($MaxVUs -ge 5000) { $MaxVUs = 900 }
    if ($StartQps -ge 100) { $StartQps = 80 }
    if ($Stage1Qps -ge 200) { $Stage1Qps = 120 }
    if ($Stage2Qps -ge 500) { $Stage2Qps = 220 }
    if ($Stage3Qps -ge 800) { $Stage3Qps = 320 }
    if ($Stage4Qps -ge 1000) { $Stage4Qps = 450 }
  }
  elseif ($Scenario -eq "policy_only") {
    if ($PreAllocatedVUs -ge 500) { $PreAllocatedVUs = 250 }
    if ($MaxVUs -ge 5000) { $MaxVUs = 700 }
    if ($StartQps -ge 100) { $StartQps = 100 }
    if ($Stage1Qps -ge 200) { $Stage1Qps = 200 }
    if ($Stage2Qps -ge 500) { $Stage2Qps = 350 }
    if ($Stage3Qps -ge 800) { $Stage3Qps = 500 }
    if ($Stage4Qps -ge 1000) { $Stage4Qps = 650 }
  }
}
if ($RunMode -eq "dataplane_only") {
  $LoginEveryN = 0
  $ControlEveryN = 0
  $PolicyListEveryN = 0
  $LoginRetries = 0
  $Scenario = "transport"
}
if ($Scenario -eq "auth_login_verify") {
  $ControlEveryN = 0
  $PolicyListEveryN = 0
  $ExtraApisEveryN = 0
  $LoginRetries = 0
}
if ($SteadyFullchain.IsPresent -and $Scenario -eq "full_chain") {
  $ControlEveryN = 0
  $PolicyListEveryN = 0
  $ExtraApisEveryN = 0
}
if ($Scenario -eq "full_chain") {
  # Capacity qualification requires Auth and token verification in every iteration.
  $LoginEveryN = 1
}
if ($ConstantRps -gt 0) {
  $StartQps = $ConstantRps
  $Stage1Qps = $ConstantRps
  $Stage2Qps = $ConstantRps
  $Stage3Qps = $ConstantRps
  $Stage4Qps = $ConstantRps
}
if ($RunMode -eq "strict" -and $LoginRetries -lt 0) { $LoginRetries = 0 }
$blockingValue = if ($ControlPlaneBlocking.IsPresent) { "1" } elseif ($RunMode -eq "strict") { "1" } else { "0" }
if ([string]::IsNullOrWhiteSpace($GateProfile)) {
  if ($Scenario -eq "auth_login_verify") { $GateProfile = "auth" }
  elseif ($DataplaneSuccessMode -eq "apisix_routed") { $GateProfile = "dataplane_routed" }
  elseif ($RunMode -eq "capacity") { $GateProfile = "capacity" }
  elseif ($RunMode -eq "dataplane_only") { $GateProfile = "dataplane" }
  else { $GateProfile = "strict" }
}

$userPoolJson = ""
if (-not [string]::IsNullOrWhiteSpace($UserPoolFile)) {
  if (-not (Test-Path -LiteralPath $UserPoolFile)) {
    Write-Host "用户池文件不存在: $UserPoolFile" -ForegroundColor Red
    exit 3
  }
  $userPoolJson = Get-Content -LiteralPath $UserPoolFile -Raw -Encoding UTF8
}

$env:DATAPLANE_URL = $DataplaneUrl
$env:AUTH_BASE_URL = $AuthBaseUrl
$env:POLICY_BASE_URL = $PolicyBaseUrl
$env:CONTROL_BASE_URL = $ControlBaseUrl
try {
  $env:SAG_EDGE_HOST = ([Uri]$DataplaneUrl).Host
}
catch {
  $env:SAG_EDGE_HOST = "172.16.9.107"
}
$env:SAG_APP_ID = $AppId
$env:SAG_AUTH_USERNAME = $Username
$env:SAG_AUTH_PASSWORD = $Password
$env:SAG_RUN_MODE = $RunMode
$env:SAG_SCENARIO_TYPE = $Scenario
$env:SAG_LOGIN_EVERY_N = "$LoginEveryN"
$env:SAG_CONTROL_EVERY_N = "$ControlEveryN"
$env:SAG_POLICY_LIST_EVERY_N = "$PolicyListEveryN"
$env:SAG_CONTROL_PLANE_BLOCKING = $blockingValue
$env:SAG_LOGIN_RETRIES = "$LoginRetries"
$env:SAG_LOGIN_RETRY_BACKOFF_MS = "$LoginRetryBackoffMs"
$env:SAG_USER_POOL_JSON = $userPoolJson
$env:SAG_SHARED_TOKEN = $SharedToken
$env:SAG_REQ_TIMEOUT = $RequestTimeout
$env:SAG_INSECURE_SKIP_TLS_VERIFY = if ($DisableInsecureSkipTlsVerify.IsPresent -or $ProductionGate.IsPresent) { "0" } else { "1" }
$env:SAG_EXTRA_APIS_EVERY_N = "$ExtraApisEveryN"
$env:SAG_INCLUDE_IDENTITY_APIS = if ($IncludeIdentityApis.IsPresent) { "1" } else { "0" }
$env:SAG_INCLUDE_USERS_APIS = if ($IncludeUsersApis.IsPresent) { "1" } else { "0" }
$env:SAG_INCLUDE_CONTROL_APPS_APIS = if ($IncludeControlAppsApis.IsPresent) { "1" } else { "0" }
$env:SAG_SKIP_VERIFY_AFTER_LOGIN = if ($Scenario -eq "auth_login_verify" -or $Scenario -eq "full_chain") {
  "0"
} elseif ($SkipVerifyAfterLogin.IsPresent -or $RunMode -eq "capacity") {
  "1"
} else {
  "0"
}
$env:SAG_GATE_PROFILE = $GateProfile
$env:SAG_START_QPS = "$StartQps"
$env:SAG_STAGE1_QPS = "$Stage1Qps"
$env:SAG_STAGE2_QPS = "$Stage2Qps"
$env:SAG_STAGE3_QPS = "$Stage3Qps"
$env:SAG_STAGE4_QPS = "$Stage4Qps"
$env:SAG_STAGE1_DURATION = $Stage1Duration
$env:SAG_STAGE2_DURATION = $Stage2Duration
$env:SAG_STAGE3_DURATION = $Stage3Duration
$env:SAG_STAGE4_DURATION = $Stage4Duration
$env:SAG_PRE_ALLOCATED_VUS = "$PreAllocatedVUs"
$env:SAG_MAX_VUS = "$MaxVUs"
$env:SAG_DP_ACCEPT_202 = if ($AcceptDataplane202) { "1" } else { "0" }
$env:SAG_DP_POLL_202 = if ($PollDataplane202 -or $Scenario -eq "full_chain") { "1" } else { "0" }
$env:SAG_DP_ACCEPT_429_SHED = if ($AcceptDataplane429Shed) { "1" } else { "0" }
$env:SAG_DP_SUCCESS_MODE = $DataplaneSuccessMode
$env:SAG_EXPECT_DATAPLANE_STATUS = "$ExpectedDataplaneStatus"
$env:SAG_PRODUCTION_GATE = if ($ProductionGate) { "1" } else { "0" }
$env:SAG_REQUIRE_REDIS_QUEUE = if ($RequireRedisQueue -or $Scenario -eq "full_chain") { "1" } else { "0" }
$env:SAG_MUTATION_MODE = if ($MutationMode -or $Scenario -eq "full_chain") { "1" } else { "0" }
$env:SAG_AUDIT_SAMPLE_EVERY_N = "$AuditSampleEveryN"
$env:SAG_AUDIT_LAG_TIMEOUT_MS = "$AuditLagTimeoutMs"
$env:SAG_TEST_DURATION = $TestDuration
$env:SAG_TARGET_RPS = if ($ConstantRps -gt 0) { "$ConstantRps" } else { "$Stage4Qps" }

Write-Host "开始压测 (k6 executor=ramping-arrival-rate, timeUnit=1s)" -ForegroundColor Cyan
if ($DataplaneSuccessMode -eq "apisix_routed") {
  Write-Host "Dataplane success mode: apisix_routed (tunnel+APISIX routed OK; upstream 5xx counts success; 404/403/tunnel-miss still fail)" -ForegroundColor Green
}
if ($Scenario -eq "transport") {
  Write-Host "口径: 每迭代 1 次 dataplane GET —— 配置的 QPS = 数据面请求 RPS (若 VU/超时足够)" -ForegroundColor Green
}
elseif ($Scenario -eq "workload") {
  Write-Host "口径: 精确 2xx + workload JSON correlation/identity 断言；不含 Auth/Policy/audit，不能作为生产容量" -ForegroundColor Green
}
elseif ($Scenario -eq "policy_only") {
  Write-Host "口径: 每迭代 1 次 policy evaluate —— 配置的 QPS = evaluate 请求 RPS" -ForegroundColor Green
}
elseif ($Scenario -eq "auth_login_verify") {
  Write-Host "口径: 每迭代 login + verify（无会话缓存）—— QPS=2000 时 Auth HTTP 约 4000/s" -ForegroundColor Green
}
else {
  Write-Host "口径: 每迭代 = 整条混合链路一次 —— 配置的 QPS = 迭代/秒，不是单独数据面 RPS (每迭代多次 HTTP)" -ForegroundColor Yellow
}
if ($ConstantRps -gt 0) {
  Write-Host "ConstantRps=$ConstantRps (各阶段目标 arrival 相同)" -ForegroundColor Cyan
}
Write-Host "DATAPLANE_URL=$DataplaneUrl"
Write-Host "AUTH_BASE_URL=$AuthBaseUrl"
Write-Host "POLICY_BASE_URL=$PolicyBaseUrl"
Write-Host "CONTROL_BASE_URL=$ControlBaseUrl"
Write-Host "SAG_APP_ID=$AppId"
Write-Host "SAG_AUTH_USERNAME=$Username"
Write-Host "RUN_MODE=$RunMode"
Write-Host "SCENARIO_TYPE=$Scenario"
Write-Host "LOGIN_EVERY_N=$LoginEveryN  CONTROL_EVERY_N=$ControlEveryN  POLICY_LIST_EVERY_N=$PolicyListEveryN"
Write-Host "CONTROL_PLANE_BLOCKING=$blockingValue  LOGIN_RETRIES=$LoginRetries  LOGIN_RETRY_BACKOFF_MS=$LoginRetryBackoffMs"
Write-Host "REQUEST_TIMEOUT=$RequestTimeout"
Write-Host "GATE_PROFILE=$GateProfile"
if (-not [string]::IsNullOrWhiteSpace($UserPoolFile)) {
  Write-Host "USER_POOL_FILE=$UserPoolFile"
}
if (-not [string]::IsNullOrWhiteSpace($SharedToken)) {
  Write-Host "SHARED_TOKEN=provided" -ForegroundColor DarkGray
}
Write-Host "阶梯QPS: $StartQps -> $Stage1Qps -> $Stage2Qps -> $Stage3Qps -> $Stage4Qps"
Write-Host "阶段时长: $Stage1Duration / $Stage2Duration / $Stage3Duration / $Stage4Duration"
Write-Host "VUs: preAllocated=$PreAllocatedVUs, max=$MaxVUs"
Write-Host "Summary: $SummaryJson"
Write-Host "SAG_DP_SUCCESS_MODE=$DataplaneSuccessMode"
Write-Host "SAG_DP_ACCEPT_202=$($env:SAG_DP_ACCEPT_202)  SAG_DP_POLL_202=$($env:SAG_DP_POLL_202)  SAG_DP_ACCEPT_429_SHED=$($env:SAG_DP_ACCEPT_429_SHED)  (bridge queue / shed-aware k6)"
if ($ConstantRps -gt 0 -and ($NoCapacityVuCap -or $RunMode -ne "capacity")) {
  Write-Host "VU cap: PreAllocated=$PreAllocatedVUs Max=$MaxVUs (capacity 900-cap disabled)" -ForegroundColor DarkGray
}
if ($SteadyFullchain.IsPresent) {
  Write-Host "STEADY_FULLCHAIN: LoginEveryN=$LoginEveryN  CONTROL_EVERY_N=$ControlEveryN  POLICY_LIST_EVERY_N=$PolicyListEveryN  EXTRA_APIS_EVERY_N=$ExtraApisEveryN"
}
Write-Host "建议同步观察: http://<host>:3001/ops/workflow 和 Prometheus/Grafana" -ForegroundColor DarkGray

$runStartedAt = (Get-Date).ToUniversalTime()
k6 run --summary-export "$SummaryJson" $scriptPath
$k6ExitCode = $LASTEXITCODE

if (Test-Path -LiteralPath $SummaryJson) {
  $summary = Get-Content -LiteralPath $SummaryJson -Raw -Encoding UTF8 | ConvertFrom-Json
  function Get-K6MetricField {
    param([object]$Summary, [string]$Name, [string]$Field)
    $property = $Summary.metrics.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    $fieldProperty = $property.Value.PSObject.Properties[$Field]
    if ($null -eq $fieldProperty) { return $null }
    return $fieldProperty.Value
  }

  $gitSha = "unavailable:not-a-git-repository"
  $gitOutput = (& git rev-parse HEAD 2>$null)
  if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($gitOutput)) {
    $gitSha = $gitOutput.Trim()
  }

  $imageDigests = @()
  if (-not [string]::IsNullOrWhiteSpace($env:SAG_IMAGE_DIGESTS_JSON) -and (Test-Path -LiteralPath $env:SAG_IMAGE_DIGESTS_JSON)) {
    $imageDigests = @(Get-Content -LiteralPath $env:SAG_IMAGE_DIGESTS_JSON -Raw -Encoding UTF8 | ConvertFrom-Json)
  }

  $resourceEvidence = @{ status = "missing"; reason = "SAG_RESOURCE_EVIDENCE_JSON not provided" }
  if (-not [string]::IsNullOrWhiteSpace($env:SAG_RESOURCE_EVIDENCE_JSON) -and (Test-Path -LiteralPath $env:SAG_RESOURCE_EVIDENCE_JSON)) {
    $resourceEvidence = Get-Content -LiteralPath $env:SAG_RESOURCE_EVIDENCE_JSON -Raw -Encoding UTF8 | ConvertFrom-Json
  }
  $dependencyEvidence = @{ status = "missing"; reason = "SAG_DEPENDENCY_EVIDENCE_JSON not provided" }
  if (-not [string]::IsNullOrWhiteSpace($env:SAG_DEPENDENCY_EVIDENCE_JSON) -and (Test-Path -LiteralPath $env:SAG_DEPENDENCY_EVIDENCE_JSON)) {
    $dependencyEvidence = Get-Content -LiteralPath $env:SAG_DEPENDENCY_EVIDENCE_JSON -Raw -Encoding UTF8 | ConvertFrom-Json
  }

  $businessErrors = [ordered]@{}
  $httpStatuses = [ordered]@{}
  foreach ($metricProperty in $summary.metrics.PSObject.Properties) {
    if ($metricProperty.Name -match '^sag_(api_business_reject|api_system_failure|correlation_mismatch|stale_result|mutation_side_effect_mismatch|unexpected_business_status)_total') {
      $countProperty = $metricProperty.Value.PSObject.Properties['count']
      if ($null -ne $countProperty) { $businessErrors[$metricProperty.Name] = $countProperty.Value }
    }
    if ($metricProperty.Name -match '^sag_dataplane_bridge_status_total\{status:([^,}]+)') {
      $countProperty = $metricProperty.Value.PSObject.Properties['count']
      if ($null -ne $countProperty) { $httpStatuses[$Matches[1]] = $countProperty.Value }
    }
  }

  $artifact = [ordered]@{
    schema_version = "sag.production-gate/v1"
    qualification = "unqualified-run"
    scenario = $Scenario
    run = [ordered]@{
      started_at_utc = $runStartedAt.ToString("o")
      finished_at_utc = (Get-Date).ToUniversalTime().ToString("o")
      git_sha = $gitSha
      image_digests = $imageDigests
      environment = $EnvironmentName
      raw_k6_summary = (Resolve-Path -LiteralPath $SummaryJson).Path
      k6_exit_code = $k6ExitCode
    }
    config_snapshot = [ordered]@{
      dataplane_url = $DataplaneUrl
      auth_base_url = $AuthBaseUrl
      policy_base_url = $PolicyBaseUrl
      control_base_url = $ControlBaseUrl
      app_id = $AppId
      target_rps = if ($ConstantRps -gt 0) { $ConstantRps } else { $Stage4Qps }
      duration = $TestDuration
      expected_status = $ExpectedDataplaneStatus
      mutation = ($env:SAG_MUTATION_MODE -eq "1")
      require_redis_queue = ($env:SAG_REQUIRE_REDIS_QUEUE -eq "1")
      audit_sample_every_n = $AuditSampleEveryN
      insecure_skip_tls_verify = ($env:SAG_INSECURE_SKIP_TLS_VERIFY -eq "1")
    }
    results = [ordered]@{
      target_rps = if ($ConstantRps -gt 0) { $ConstantRps } else { $Stage4Qps }
      actual_completed_rps = Get-K6MetricField $summary "iterations" "rate"
      business_success_rate = Get-K6MetricField $summary "sag_business_success_rate" "value"
      dropped_iterations = Get-K6MetricField $summary "dropped_iterations" "count"
      latency_ms = [ordered]@{
        p50 = Get-K6MetricField $summary "http_req_duration" "med"
        p95 = Get-K6MetricField $summary "http_req_duration" "p(95)"
        p99 = Get-K6MetricField $summary "http_req_duration" "p(99)"
      }
      business_error_distribution = $businessErrors
      http_status_distribution = $httpStatuses
    }
    evidence = [ordered]@{
      auth_rate = Get-K6MetricField $summary "sag_auth_evidence_rate" "value"
      policy_rate = Get-K6MetricField $summary "sag_policy_evidence_rate" "value"
      audit_rate = Get-K6MetricField $summary "sag_audit_evidence_rate" "value"
      redis_queue_rate = Get-K6MetricField $summary "sag_redis_queue_evidence_rate" "value"
      idempotency_rate = Get-K6MetricField $summary "sag_idempotency_evidence_rate" "value"
      workload_rate = Get-K6MetricField $summary "sag_workload_evidence_rate" "value"
      resources = $resourceEvidence
      dependencies = $dependencyEvidence
    }
  }
  $artifactDir = Split-Path -Parent $ArtifactJson
  if (-not [string]::IsNullOrWhiteSpace($artifactDir) -and -not (Test-Path -LiteralPath $artifactDir)) {
    New-Item -ItemType Directory -Path $artifactDir -Force | Out-Null
  }
  $artifact | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $ArtifactJson -Encoding UTF8
  Write-Host "Evidence artifact: $ArtifactJson"
}

exit $k6ExitCode

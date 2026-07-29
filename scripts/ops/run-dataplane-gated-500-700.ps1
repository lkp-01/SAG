<#
.SYNOPSIS
  Dataplane-only k6：先固定 500 iter/s；若 dataplane_get 成功率 >= 0.90，冷却后再跑 700。
.EXAMPLE
  cd sag-cloud
  .\scripts\ops\run-dataplane-gated-500-700.ps1 -EdgeHost 172.16.9.107 -SkipPrecheck -CooldownSeconds 120
#>
param(
  [string]$EdgeHost = "",
  [string]$DataplaneUrl = "https://172.16.9.107:10080/dev/",
  [int]$FirstRps = 500,
  [int]$SecondRps = 700,
  [double]$SuccessThreshold = 0.90,
  [int]$CooldownSeconds = 120,
  [string]$TierStageDuration = "3m",
  [string]$RequestTimeout = "90s",
  [int]$PreAllocatedVUs = 3000,
  [int]$MaxVUs = 20000,
  [switch]$SkipPrecheck,
  [string]$ArtifactsDir = ""
)

if (-not [string]::IsNullOrWhiteSpace($EdgeHost)) {
  $h = $EdgeHost.Trim() -replace "^https?://", "" -replace "/$", ""
  $DataplaneUrl = "https://${h}:10080/dev/"
}

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$root = Split-Path (Split-Path $here -Parent) -Parent
if ([string]::IsNullOrWhiteSpace($ArtifactsDir)) {
  $ArtifactsDir = Join-Path $root "artifacts"
}
if (-not (Test-Path -LiteralPath $ArtifactsDir)) {
  New-Item -ItemType Directory -Path $ArtifactsDir -Force | Out-Null
}

$runId = Get-Date -Format "yyyyMMdd-HHmmss"
$runner = Join-Path $here "run-load-dataplane.ps1"
$report = Join-Path $ArtifactsDir "k6-gated-500-700-$runId-report.txt"
$out500 = Join-Path $ArtifactsDir "k6-gated-500-700-$runId-dp-$FirstRps.json"
$out700 = Join-Path $ArtifactsDir "k6-gated-500-700-$runId-dp-$SecondRps.json"

$Utf8BomEnc = New-Object System.Text.UTF8Encoding $true
function Write-ReportLine([string]$Line) {
  [System.IO.File]::AppendAllText($report, $Line + [Environment]::NewLine, $Utf8BomEnc)
}

function Read-DataplaneSuccessRate([string]$JsonPath) {
  if (-not (Test-Path -LiteralPath $JsonPath)) { return 0.0 }
  $j = Get-Content -LiteralPath $JsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
  $m = $j.metrics.PSObject.Properties["sag_api_success_rate{api:dataplane_get}"]
  if ($null -eq $m) { return 0.0 }
  $v = $m.Value.value
  if ($null -eq $v) { return 0.0 }
  return [double]$v
}

function Append-DataplaneBottleneckMetrics([string]$JsonPath) {
  if (-not (Test-Path -LiteralPath $JsonPath)) { return }
  $j = Get-Content -LiteralPath $JsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
  function MetricCount([string]$name) {
    $p = $j.metrics.PSObject.Properties[$name]
    if ($null -eq $p) { return [long]0 }
    $val = $p.Value
    if ($null -ne $val.count) { return [long]$val.count }
    return [long]0
  }
  $req = MetricCount "http_reqs"
  if ($req -eq 0) { $req = MetricCount "iterations" }
  $b200 = MetricCount "sag_dataplane_bridge_status_total{status:200}"
  $b500 = MetricCount "sag_dataplane_bridge_status_total{status:500}"
  $b0 = MetricCount "sag_dataplane_bridge_status_total{status:0}"
  $up5 = MetricCount "sag_dataplane_failure_cause_total{cause:upstream_5xx}"
  $net = MetricCount "sag_dataplane_failure_cause_total{cause:network}"
  $to = MetricCount "sag_dataplane_failure_cause_total{cause:timeout}"
  Write-ReportLine ""
  Write-ReportLine "---- bottleneck_metric_counts ($(Split-Path -Leaf $JsonPath)) ----"
  Write-ReportLine "http_reqs=$req  bridge_status_200=$b200  bridge_status_500=$b500  bridge_status_0=$b0"
  Write-ReportLine "failure_cause  upstream_5xx=$up5  network=$net  timeout=$to"
  if ($req -gt 0) {
    $p500 = [math]::Round(100.0 * [double]$b500 / [double]$req, 2)
    $p0 = [math]::Round(100.0 * [double]$b0 / [double]$req, 2)
    Write-ReportLine "share_of_requests  bridge_500=${p500}%  bridge_0=${p0}%"
    if ($b500 -gt 0 -and [math]::Abs($b500 - $up5) -le 2) {
      Write-ReportLine "hint: bridge_500 ~= upstream_5xx -> connector/upstream HTTP (e.g. Intra mock-workload)."
    }
  }
}

function Append-Analysis([string]$JsonPath, [string]$Label) {
  Write-ReportLine ""
  Write-ReportLine "---- $Label ----"
  $r = Read-DataplaneSuccessRate $JsonPath
  Write-ReportLine "dataplane_get success_rate: $r"
  Append-DataplaneBottleneckMetrics $JsonPath
}

$header = @"
SAG gated dataplane run id: $runId
gate: success_rate >= $SuccessThreshold @ ${FirstRps} iter/s -> run ${SecondRps}
DATAPLANE_URL=$DataplaneUrl
$(Get-Date -Format u)
"@
[System.IO.File]::WriteAllText($report, $header + [Environment]::NewLine, $Utf8BomEnc)

$invokeBase = @{
  DataplaneUrl     = $DataplaneUrl
  RunMode          = "dataplane_only"
  ScenarioType     = "dataplane_only"
  ConstantRps      = $FirstRps
  RequestTimeout   = $RequestTimeout
  PreAllocatedVUs  = $PreAllocatedVUs
  MaxVUs           = $MaxVUs
  Stage1Duration   = $TierStageDuration
  Stage2Duration   = $TierStageDuration
  Stage3Duration   = $TierStageDuration
  Stage4Duration   = $TierStageDuration
  SummaryJson      = $out500
}
if ($SkipPrecheck) { $invokeBase.SkipPrecheck = $true }

Write-Host "========== gate tier $FirstRps -> $out500 ==========" -ForegroundColor Cyan
& $runner @invokeBase -PollDataplane202 -AcceptDataplane202 -AcceptDataplane429Shed
Append-Analysis $out500 "tier $FirstRps"

$rateFirst = Read-DataplaneSuccessRate $out500
$runSecond = $rateFirst -ge $SuccessThreshold
Write-ReportLine ""
if ($runSecond) {
  Write-ReportLine "PASS gate: $rateFirst >= $SuccessThreshold ; will run $SecondRps after ${CooldownSeconds}s."
  Write-Host "Rate $rateFirst >= $SuccessThreshold : running $SecondRps after cooldown." -ForegroundColor Green
  Start-Sleep -Seconds $CooldownSeconds
  $invoke2 = @{}
  foreach ($k in @($invokeBase.Keys)) { $invoke2[$k] = $invokeBase[$k] }
  $invoke2["ConstantRps"] = $SecondRps
  $invoke2["SummaryJson"] = $out700
  Write-Host "========== tier $SecondRps -> $out700 ==========" -ForegroundColor Cyan
  & $runner @invoke2 -PollDataplane202 -AcceptDataplane202 -AcceptDataplane429Shed
  Append-Analysis $out700 "tier $SecondRps"
  $rateSecond = Read-DataplaneSuccessRate $out700
  Write-ReportLine "tier ${SecondRps} dataplane_get success_rate: $rateSecond"
}
else {
  Write-ReportLine "SKIP ${SecondRps}: $rateFirst < $SuccessThreshold"
  Write-Host "Skip $SecondRps : rate $rateFirst < $SuccessThreshold (need >= $SuccessThreshold on $FirstRps)" -ForegroundColor Yellow
}

Write-ReportLine ""
Write-ReportLine "---- triage_cheatsheet ----"
Write-ReportLine "403/forbidden -> policy / auth / agent gRPC"
Write-ReportLine "503/policy -> sag-policy overload or agent policy HTTP timeout"
Write-ReportLine "502/gateway -> bridge / connector / poll failure"
Write-ReportLine "timeout / bridge_status_0 -> deadline chain bridge / agent / connector"
Write-ReportLine "many 202 -> bridge queue SOFT_INFLIGHT / worker backlog"

Write-Host "Report: $report" -ForegroundColor Green

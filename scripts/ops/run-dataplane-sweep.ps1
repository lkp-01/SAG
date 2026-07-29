<#
.SYNOPSIS
  连续多档 dataplane_only 压测，档间冷却，避免同 shell 连跑导致资源叠加不可比。

.EXAMPLE
  .\run-dataplane-sweep.ps1 -RpsList "500" -CooldownSeconds 0 `
    -RequestTimeout 90s -MaxVUs 12000 -PreAllocatedVUs 2000 `
    -SkipPrecheck -AcceptDataplane202 -PollDataplane202 -AcceptDataplane429Shed

.EXAMPLE
  .\run-dataplane-sweep.ps1 -RpsList "300,500,700,900" -CooldownSeconds 120 -EdgeHost "172.16.9.107"
#>
param(
  [string]$EdgeHost = "",
  [string]$RpsList = "500,700,900",
  [int]$CooldownSeconds = 120,
  [string]$DataplaneUrl = "https://172.16.9.107:10080/dev/",
  [string]$AuthBaseUrl = "http://172.16.9.107:8080",
  [string]$PolicyBaseUrl = "http://172.16.9.107:8081",
  [string]$ControlBaseUrl = "http://172.16.9.107:8090",
  [string]$AppId = "app-001",
  [string]$RequestTimeout = "90s",
  [int]$PreAllocatedVUs = 3000,
  [int]$MaxVUs = 20000,
  [string]$StageDuration = "25s",
  [switch]$SkipPrecheck,
  [switch]$AcceptDataplane202,
  [switch]$PollDataplane202,
  [switch]$AcceptDataplane429Shed,
  [string]$ArtifactsDir = ""
)

if (-not [string]::IsNullOrWhiteSpace($EdgeHost)) {
  $h = $EdgeHost.Trim() -replace "^https?://", "" -replace "/$", ""
  $DataplaneUrl = "https://${h}:10080/dev/"
  $AuthBaseUrl = "http://${h}:8080"
  $PolicyBaseUrl = "http://${h}:8081"
  $ControlBaseUrl = "http://${h}:8090"
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

$runs = @($RpsList -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ -match "^\d+$" })
if ($runs.Count -eq 0) {
  Write-Host "RpsList 无有效整数，例如: 300,500,700" -ForegroundColor Red
  exit 1
}

$sweepId = Get-Date -Format "yyyyMMdd-HHmmss"
$runner = Join-Path $here "run-load-dataplane.ps1"
if (-not (Test-Path -LiteralPath $runner)) {
  Write-Host "缺少: $runner" -ForegroundColor Red
  exit 1
}

$n = 0
foreach ($rps in $runs) {
  $n++
  $out = Join-Path $ArtifactsDir "k6-sweep-${sweepId}-dp-${rps}.json"
  Write-Host ""
  Write-Host "========== sweep [$n/$($runs.Count)] ConstantRps=$rps -> $out ==========" -ForegroundColor Cyan

  $rpsInt = [int]$rps
  $invoke = @{
    DataplaneUrl    = $DataplaneUrl
    AuthBaseUrl     = $AuthBaseUrl
    PolicyBaseUrl   = $PolicyBaseUrl
    ControlBaseUrl  = $ControlBaseUrl
    AppId           = $AppId
    RunMode         = "dataplane_only"
    ScenarioType    = "dataplane_only"
    ConstantRps     = $rpsInt
    RequestTimeout  = $RequestTimeout
    PreAllocatedVUs = $PreAllocatedVUs
    MaxVUs          = $MaxVUs
    Stage1Duration  = $StageDuration
    Stage2Duration  = $StageDuration
    Stage3Duration  = $StageDuration
    Stage4Duration  = $StageDuration
    SummaryJson     = $out
  }
  if ($SkipPrecheck) { $invoke.SkipPrecheck = $true }
  if ($AcceptDataplane202) { $invoke.AcceptDataplane202 = $true }
  if ($PollDataplane202) { $invoke.PollDataplane202 = $true }
  if ($AcceptDataplane429Shed) { $invoke.AcceptDataplane429Shed = $true }
  & $runner @invoke
  $code = $LASTEXITCODE
  Write-Host "k6 exit_code=$code (99=k6 threshold fail，可接受)" -ForegroundColor $(if ($code -eq 0) { "Green" } else { "Yellow" })

  if ($n -lt $runs.Count -and $CooldownSeconds -gt 0) {
    Write-Host "Cooldown ${CooldownSeconds}s ..." -ForegroundColor DarkGray
    Start-Sleep -Seconds $CooldownSeconds
  }
}

Write-Host ""
Write-Host "Sweep done. Artifacts: $ArtifactsDir\\k6-sweep-${sweepId}-dp-*.json" -ForegroundColor Green
Write-Host "Edge 指标: .\\snapshot-bridge-metrics.ps1 -BridgeBaseUrl http://<edge>:9000" -ForegroundColor DarkGray

<#
.SYNOPSIS
  数据面阶梯压测：1000 RPS（apisix_routed 口径）-> 成功率>=90% 则休息 300s -> 1500 RPS。

.EXAMPLE
  cd sag-cloud
  .\scripts\ops\run-dataplane-gated-1000-1500.ps1 -EdgeHost 172.16.9.107
#>
param(
  [string]$EdgeHost = "172.16.9.107",
  [int]$FirstRps = 1000,
  [int]$SecondRps = 1500,
  [double]$SuccessThreshold = 0.90,
  [int]$CooldownSec = 300,
  [string]$TierDuration = "2m",
  [int]$PreAllocatedVUs = 2500,
  [int]$MaxVUs = 12000,
  [string]$AppId = "app-001"
)

$ErrorActionPreference = "Stop"
$scriptDir = $PSScriptRoot
$root = Split-Path (Split-Path $scriptDir -Parent) -Parent
$artifacts = Join-Path $root "artifacts"
if (-not (Test-Path $artifacts)) {
  New-Item -ItemType Directory -Path $artifacts -Force | Out-Null
}
$ts = Get-Date -Format "yyyyMMdd-HHmmss"
$report = Join-Path $artifacts "k6-gated-${ts}-report.txt"
$out1 = Join-Path $artifacts "k6-gated-${ts}-dp-${FirstRps}.json"
$out2 = Join-Path $artifacts "k6-gated-${ts}-dp-${SecondRps}.json"
$runner = Join-Path $scriptDir "run-load-dataplane.ps1"

function Read-DataplaneRate {
  param([string]$JsonPath)
  if (-not (Test-Path $JsonPath)) { return $null }
  $raw = Get-Content -LiteralPath $JsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
  $m = $raw.metrics.'sag_api_success_rate{api:dataplane_get}'
  if ($null -ne $m.value) { return [double]$m.value }
  if ($m.values.rate -ne $null) { return [double]$m.values.rate }
  return $null
}

function Invoke-Tier {
  param([int]$Rps, [string]$SummaryJson)
  $prevEap = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  & $runner `
    -EdgeHost $EdgeHost `
    -RunMode dataplane_only `
    -ScenarioType dataplane_only `
    -DataplaneSuccessMode apisix_routed `
    -NoCapacityVuCap `
    -ConstantRps $Rps `
    -PreAllocatedVUs $PreAllocatedVUs `
    -MaxVUs $MaxVUs `
    -RequestTimeout 90s `
    -Stage1Duration $TierDuration `
    -Stage2Duration $TierDuration `
    -Stage3Duration $TierDuration `
    -Stage4Duration $TierDuration `
    -AcceptDataplane202 `
    -PollDataplane202 `
    -AcceptDataplane429Shed `
    -SkipPrecheck `
    -SummaryJson $SummaryJson
  $code = $LASTEXITCODE
  $ErrorActionPreference = $prevEap
  return $code
}

@"
SAG gated dataplane run: $ts
EdgeHost=$EdgeHost  AppId=$AppId
FirstRps=$FirstRps  SecondRps=$SecondRps  threshold=$SuccessThreshold  cooldown=${CooldownSec}s
Success mode: apisix_routed  VUs: pre=$PreAllocatedVUs max=$MaxVUs  tier=$TierDuration x4
"@ | Set-Content -LiteralPath $report -Encoding UTF8

Write-Host "=== Tier ${FirstRps} RPS (apisix_routed) ===" -ForegroundColor Cyan
$code1 = Invoke-Tier -Rps $FirstRps -SummaryJson $out1
$rate1 = Read-DataplaneRate $out1
Add-Content -LiteralPath $report -Value "tier $FirstRps exit=$code1 dataplane_get_success_rate=$rate1 json=$out1"

if ($null -eq $rate1) {
  Write-Host "无法读取 $out1 中的 dataplane 成功率" -ForegroundColor Red
  exit 2
}

Write-Host "Tier ${FirstRps} dataplane_get success_rate = $([math]::Round($rate1 * 100, 2))%" -ForegroundColor Yellow

if ($rate1 -lt $SuccessThreshold) {
  Add-Content -LiteralPath $report -Value ("SKIP tier ${SecondRps}: rate $rate1 below $SuccessThreshold")
  Write-Host "未达到 $SuccessThreshold，跳过 ${SecondRps} RPS。" -ForegroundColor Red
  Write-Host "报告: $report"
  exit 1
}

Write-Host "达到阈值，休息 ${CooldownSec}s ..." -ForegroundColor Green
Add-Content -LiteralPath $report -Value "COOLDOWN ${CooldownSec}s before $SecondRps"
Start-Sleep -Seconds $CooldownSec

Write-Host "=== Tier ${SecondRps} RPS ===" -ForegroundColor Cyan
$code2 = Invoke-Tier -Rps $SecondRps -SummaryJson $out2
$rate2 = Read-DataplaneRate $out2
Add-Content -LiteralPath $report -Value "tier $SecondRps exit=$code2 dataplane_get_success_rate=$rate2 json=$out2"

Write-Host "Tier ${SecondRps} dataplane_get success_rate = $([math]::Round($rate2 * 100, 2))%" -ForegroundColor Yellow
Write-Host "报告: $report" -ForegroundColor Cyan
Write-Host "JSON: $out1" -ForegroundColor DarkGray
if (Test-Path $out2) { Write-Host "JSON: $out2" -ForegroundColor DarkGray }

if ($rate2 -lt $SuccessThreshold) { exit 1 }
exit 0

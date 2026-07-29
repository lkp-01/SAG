<#
.SYNOPSIS
  从 Windows 主机对远端执行多轮冒烟，并汇总延迟。

.EXAMPLE
  # 单机（管理面 + 数据面都在同一 VM）
  .\scripts\smoke-remote-windows.ps1 -VmHost 192.168.9.26 -Rounds 5

.EXAMPLE
  # 双机：Edge 上新 IP + Intra 上 APISIX/mock（默认 Intra 192.168.9.26）
  .\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -Rounds 3

.EXAMPLE
  # 双机且 Intra 不是默认地址
  .\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
#>
param(
    # 单机模式：所有探测指向该主机（8090/8080/8081/9000/10080/9080/18080）
    [string]$VmHost = "",
    # 双机模式：仅 Edge 侧端口；配合 IntraHost 设置 INTRA_APISIX + mock
    [string]$EdgeHost = "",
    [string]$IntraHost = "192.168.9.26",
    [int]$Rounds = 3
)

$ErrorActionPreference = "Stop"
if ($Rounds -lt 1) {
    throw "Rounds must be >= 1"
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$smokeScript = Join-Path $scriptDir "smoke-dataplane.ps1"
if (-not (Test-Path -LiteralPath $smokeScript)) {
    throw "Missing script: $smokeScript"
}

# Avoid stale env from a prior run mixing single-host + dual-host.
$clearKeys = @(
    "EDGE_BASE_URL", "INTRA_APISIX_DATA_BASE_URL", "SMOKE_CONTROL_PLANE_BASE", "SMOKE_AUTH_BASE", "SMOKE_POLICY_BASE",
    "BRIDGE_URL", "ZENTINEL_URL", "APISIX_DATA_BASE_URL", "MOCK_BASE_URL"
)

if ($EdgeHost) {
    foreach ($k in $clearKeys + @("HDR_APP")) {
        Remove-Item "Env:$k" -ErrorAction SilentlyContinue
    }
    $eb = $EdgeHost.Trim().TrimEnd('/')
    if ($eb -notmatch '^https?://') {
        $eb = "http://$eb"
    }
    $env:EDGE_BASE_URL = $eb
    $ih = $IntraHost.Trim()
    $env:INTRA_APISIX_DATA_BASE_URL = "http://${ih}:9080"
    $env:MOCK_BASE_URL = "http://${ih}:18080"
    # Must match control-plane bootstrap + connector default (SAG_APP_ID / tunnel_routes demo).
    $env:HDR_APP = "app-001"
    # APP_CASES use app-ci, app-finance, ... — no tunnel route unless seeded. Default skip; pre-set SMOKE_SKIP_MULTI_APP=0 to run all.
    if (-not $env:SMOKE_SKIP_MULTI_APP) {
        $env:SMOKE_SKIP_MULTI_APP = "1"
    }
    Write-Host "Dual-host smoke: EDGE_BASE_URL=$($env:EDGE_BASE_URL)  INTRA=$ih (9080/18080)  HDR_APP=$($env:HDR_APP)  SMOKE_SKIP_MULTI_APP=$($env:SMOKE_SKIP_MULTI_APP)" -ForegroundColor Yellow
}
elseif ($VmHost) {
    foreach ($k in $clearKeys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    $env:SMOKE_CONTROL_PLANE_BASE = "http://${VmHost}:8090"
    $env:SMOKE_AUTH_BASE = "http://${VmHost}:8080"
    $env:SMOKE_POLICY_BASE = "http://${VmHost}:8081"
    $env:BRIDGE_URL = "http://${VmHost}:9000"
    $env:ZENTINEL_URL = "https://${VmHost}:10080"
    $env:APISIX_DATA_BASE_URL = "http://${VmHost}:9080"
    $env:MOCK_BASE_URL = "http://${VmHost}:18080"
    Write-Host "Single-host smoke: VmHost=$VmHost" -ForegroundColor Yellow
}
else {
    throw "Specify -VmHost <IP> for single-machine smoke, or -EdgeHost <Edge IP or URL> for dual-host (optional -IntraHost, default 192.168.9.26)."
}

$allMs = New-Object System.Collections.Generic.List[int]
$passCount = 0
$failCount = 0

Write-Host "Remote smoke rounds=$Rounds" -ForegroundColor Yellow
for ($i = 1; $i -le $Rounds; $i++) {
    Write-Host ""
    Write-Host "######## Round $i/$Rounds ########" -ForegroundColor Cyan
    $out = & $smokeScript 2>&1
    $out | ForEach-Object { $_ }
    if ($LASTEXITCODE -eq 0) { $passCount++ } else { $failCount++ }

    foreach ($line in $out) {
        if ($line -match "PASS\s+HTTP\s+\d+\s+(\d+)\s+ms") {
            [void]$allMs.Add([int]$Matches[1])
        }
    }
}

Write-Host ""
Write-Host "=== REMOTE LATENCY SUMMARY ===" -ForegroundColor Yellow
Write-Host "rounds_passed=$passCount rounds_failed=$failCount"
if ($allMs.Count -gt 0) {
    $avg = [math]::Round((($allMs | Measure-Object -Average).Average), 2)
    $min = ($allMs | Measure-Object -Minimum).Minimum
    $max = ($allMs | Measure-Object -Maximum).Maximum
    Write-Host "all_layers_samples=$($allMs.Count) avg_ms=$avg min_ms=$min max_ms=$max"
} else {
    Write-Host "No latency samples parsed from output."
}

if ($failCount -gt 0) {
    exit 1
}

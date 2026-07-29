<#
.SYNOPSIS
  抓取 Edge 上 http-tunnel-bridge /metrics 中与队列、同步并发相关的行，便于与 k6 JSON 对照。
  过滤包含 bridge_ 与部分 HTTP 汇总行；重点对齐 dataplane-optimization-plan §3 P0 / §4：
  bridge_sync_inflight、bridge_soft_gate_entered_total、bridge_queue_202_total、
  bridge_soft_fallback_total、bridge_tunnel_try_saturated_total、bridge_tunnel_shed_to_queue_total 等。
  若无匹配行，会额外写入 *.raw.txt（前 120 行）便于排查「过滤过严」或「非 bridge 响应」。

.EXAMPLE
  .\snapshot-bridge-metrics.ps1 -BridgeBaseUrl "http://172.16.9.107:9000"
#>
param(
  [Parameter(Mandatory = $true)][string]$BridgeBaseUrl,
  [string]$OutFile = ""
)

$ErrorActionPreference = "Stop"
$url = ($BridgeBaseUrl.TrimEnd("/")) + "/metrics"
if ([string]::IsNullOrWhiteSpace($OutFile)) {
  $here = $PSScriptRoot
  $root = Split-Path (Split-Path $here -Parent) -Parent
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $OutFile = Join-Path $root ("artifacts\\metrics-bridge-{0}.txt" -f $stamp)
}
$dir = Split-Path -Parent $OutFile
if ($dir -and -not (Test-Path $dir)) {
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
}

Write-Host "GET $url" -ForegroundColor Cyan
try {
  $raw = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 15
  $body = $raw.Content
  Write-Host "HTTP $($raw.StatusCode) body_len=$($body.Length)" -ForegroundColor DarkGray
}
catch {
  Write-Host "请求失败: $_" -ForegroundColor Red
  exit 1
}

$norm = $body -replace "`r`n", "`n" -replace "`r", "`n"
$allLines = $norm -split "`n"
$lines = $allLines | Where-Object {
  $_ -match '(?i)bridge_' -or
  $_ -match '(?i)http_requests_total' -or
  $_ -match '(?i)http_request_duration' -or
  $_ -match '^#\s*(HELP|TYPE)\s+bridge_'
}

$text = ($lines -join "`n").TrimEnd()
if ($text.Length -gt 0) { $text += "`n" }
Set-Content -LiteralPath $OutFile -Value $text -Encoding utf8

if ($lines.Count -eq 0) {
  $rawPath = [System.IO.Path]::ChangeExtension($OutFile, "raw.txt")
  $head = $allLines | Select-Object -First 120
  Set-Content -LiteralPath $rawPath -Value (($head -join "`n") + "`n") -Encoding utf8
  Write-Host "No bridge/http filter matches; wrote raw head -> $rawPath" -ForegroundColor Yellow
}

Write-Host "Wrote $OutFile ($($lines.Count) filtered lines)" -ForegroundColor Green

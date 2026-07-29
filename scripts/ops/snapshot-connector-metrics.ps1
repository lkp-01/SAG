<#
.SYNOPSIS
  抓取 Intra 上 sag-connector /metrics 子集（经 metrics-gateway 或直连 9103），与 dataplane-optimization-plan §3 P0 对齐。
.EXAMPLE
  # 在 Intra 本机（metrics-gateway 监听 19090）
  .\snapshot-connector-metrics.ps1 -MetricsUrl "http://127.0.0.1:19090/connector/metrics"
.EXAMPLE
  # 在 Windows 笔记本上拉 Intra 指标（把 192.168.9.26 换成你的 Intra IP）
  .\snapshot-connector-metrics.ps1 -MetricsUrl "http://192.168.9.26:19090/connector/metrics"
.EXAMPLE
  # 或 SSH 端口转发后仍用 localhost: ssh -L 19090:127.0.0.1:19090 user@<intra>
  .\snapshot-connector-metrics.ps1 -MetricsUrl "http://127.0.0.1:19090/connector/metrics"
.EXAMPLE
  # 容器内直连 connector
  docker exec sag-connector curl -sS http://127.0.0.1:9103/metrics > .\artifacts\connector.raw.txt
#>
param(
  [string]$MetricsUrl = "http://127.0.0.1:19090/connector/metrics",
  [string]$OutFile = ""
)

$ErrorActionPreference = "Stop"
$url = $MetricsUrl.TrimEnd("/")
if ([string]::IsNullOrWhiteSpace($OutFile)) {
  $here = $PSScriptRoot
  $root = Split-Path (Split-Path $here -Parent) -Parent
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $OutFile = Join-Path $root ("artifacts\\metrics-connector-{0}.txt" -f $stamp)
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
  Write-Host "Request failed: $_" -ForegroundColor Red
  Write-Host "Hint: metrics-gateway :19090 runs on the Intra host, not your laptop." -ForegroundColor Yellow
  Write-Host "  Use -MetricsUrl 'http://<INTRA_IP>:19090/connector/metrics'" -ForegroundColor Yellow
  Write-Host "  Or: ssh -L 19090:127.0.0.1:19090 user@<INTRA>  then default localhost URL works." -ForegroundColor Yellow
  exit 1
}

$norm = $body -replace "`r`n", "`n" -replace "`r", "`n"
$allLines = $norm -split "`n"
$lines = $allLines | Where-Object {
  $_ -match '(?i)^connector_' -or
  $_ -match '(?i)^#\s*(HELP|TYPE)\s+connector_'
}

$text = ($lines -join "`n").TrimEnd()
if ($text.Length -gt 0) { $text += "`n" }
Set-Content -LiteralPath $OutFile -Value $text -Encoding utf8

if ($lines.Count -eq 0) {
  $rawPath = [System.IO.Path]::ChangeExtension($OutFile, "raw.txt")
  $head = $allLines | Select-Object -First 120
  Set-Content -LiteralPath $rawPath -Value (($head -join "`n") + "`n") -Encoding utf8
  Write-Host "No connector_ filter matches; wrote raw head -> $rawPath" -ForegroundColor Yellow
}

Write-Host "Wrote $OutFile ($($lines.Count) filtered lines)" -ForegroundColor Green

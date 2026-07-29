<#
.SYNOPSIS
  抓取 Intra 上 mock-workload /metrics 子集（经 metrics-gateway /mock/metrics 或直连 :18080）。
.EXAMPLE
  .\snapshot-mock-metrics.ps1 -MetricsUrl "http://127.0.0.1:19090/mock/metrics"
.EXAMPLE
  # Windows -> Intra
  .\snapshot-mock-metrics.ps1 -MetricsUrl "http://192.168.9.26:19090/mock/metrics"
#>
param(
  [string]$MetricsUrl = "http://127.0.0.1:19090/mock/metrics",
  [string]$OutFile = ""
)

$ErrorActionPreference = "Stop"
$url = $MetricsUrl.TrimEnd("/")
if ([string]::IsNullOrWhiteSpace($OutFile)) {
  $here = $PSScriptRoot
  $root = Split-Path (Split-Path $here -Parent) -Parent
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $OutFile = Join-Path $root ("artifacts\\metrics-mock-{0}.txt" -f $stamp)
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
  Write-Host "Hint: use -MetricsUrl 'http://<INTRA_IP>:19090/mock/metrics' or SSH -L 19090:127.0.0.1:19090 ..." -ForegroundColor Yellow
  exit 1
}

$norm = $body -replace "`r`n", "`n" -replace "`r", "`n"
$allLines = $norm -split "`n"
$lines = $allLines | Where-Object {
  $_ -match '(?i)^mock_' -or
  $_ -match '(?i)^#\s*(HELP|TYPE)\s+mock_'
}

$text = ($lines -join "`n").TrimEnd()
if ($text.Length -gt 0) { $text += "`n" }
Set-Content -LiteralPath $OutFile -Value $text -Encoding utf8

if ($lines.Count -eq 0) {
  $rawPath = [System.IO.Path]::ChangeExtension($OutFile, "raw.txt")
  $head = $allLines | Select-Object -First 120
  Set-Content -LiteralPath $rawPath -Value (($head -join "`n") + "`n") -Encoding utf8
  Write-Host "No mock_ filter matches; wrote raw head -> $rawPath" -ForegroundColor Yellow
}

Write-Host "Wrote $OutFile ($($lines.Count) filtered lines)" -ForegroundColor Green

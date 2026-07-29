<#
.SYNOPSIS
  压测前路径预检：从本机确认 TCP 可达 + Zentinel HTTPS 一次 GET（与 smoke N1 同口径），并打印与 k6 一致的 URL。
.EXAMPLE
  cd sag-cloud
  .\scripts\ops\preflight-dataplane-edge.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26
#>
param(
  [string]$EdgeHost = "172.16.9.107",
  [string]$IntraHost = "192.168.9.26",
  [string]$Path = "/dev/",
  [string]$AppId = "app-001"
)

$ErrorActionPreference = "Continue"
$h = $EdgeHost.Trim() -replace '^https?://', '' -replace '/$', ''
$p = $Path.Trim()
if (-not $p.StartsWith("/")) { $p = "/$p" }
$dp = "https://${h}:10080$p"

function Test-TcpQuick([string]$targetHost, [int]$port, [int]$timeoutMs = 2500) {
  try {
    $c = New-Object System.Net.Sockets.TcpClient
    $iar = $c.BeginConnect($targetHost, $port, $null, $null)
    if (-not $iar.AsyncWaitHandle.WaitOne($timeoutMs)) {
      $c.Close()
      return $false
    }
    $c.EndConnect($iar)
    $c.Close()
    return $true
  }
  catch { return $false }
}

Write-Host "=== preflight (same host:port as k6 DATAPLANE_URL) ===" -ForegroundColor Cyan
Write-Host "DATAPLANE_URL=$dp"
Write-Host "Edge TCP 10080 (Zentinel):" -ForegroundColor Yellow
$ok10080 = Test-TcpQuick $h 10080 3000
Write-Host "  TcpOk=$ok10080" -ForegroundColor $(if ($ok10080) { "Green" } else { "Red" })
if (-not $ok10080) {
  Write-Host "BLOCKER: TCP to ${h}:10080 failed from this host (same ingress as k6 DATAPLANE_URL and smoke N1)." -ForegroundColor Red
  Write-Host "Fix: publish/bind Zentinel on 10080, open firewall, or run k6 from a host that can reach Edge :10080." -ForegroundColor Yellow
  exit 1
}

Write-Host "Intra TCP 9080 / 18080 (optional south checks):" -ForegroundColor Yellow
$ih = $IntraHost.Trim()
foreach ($port in @(9080, 18080)) {
  $okp = Test-TcpQuick $ih $port 3000
  Write-Host "  ${ih}:${port} TcpOk=$okp" -ForegroundColor $(if ($okp) { "Green" } else { "Red" })
}

$hdr = @(
  "-H", "x-sag-app-id: $AppId",
  "-H", "x-sag-user-id: admin",
  "-H", "x-sag-user-roles: admin"
)
Write-Host "curl.exe -k (one GET, expect 200 after full chain):" -ForegroundColor Yellow
& curl.exe -k -sS -o $env:TEMP\sag-preflight-body.txt -w "http_code=%{http_code} time_total=%{time_total}s`n" @hdr $dp
$ec = $LASTEXITCODE
if (Test-Path $env:TEMP\sag-preflight-body.txt) {
  $snippet = Get-Content $env:TEMP\sag-preflight-body.txt -Raw -ErrorAction SilentlyContinue
  if ($snippet) {
    $one = $snippet.Substring(0, [Math]::Min(180, $snippet.Length)) -replace "[\r\n]+", " "
    Write-Host "body_snippet: $one" -ForegroundColor DarkGray
  }
}
if ($ec -ne 0) {
  Write-Host "curl exit=$ec" -ForegroundColor Red
  exit $ec
}
Write-Host "OK. Next: .\scripts\smoke-remote-windows.ps1 -EdgeHost $EdgeHost -IntraHost $IntraHost -Rounds 1" -ForegroundColor Green

<#
.SYNOPSIS
  Intra 上绕过隧道：直连 mock 与 APISIX:9080，用于 dataplane-optimization-plan §3 P0「定责是否在 APISIX→mock」。
  mock: python:slim → Python urllib。APISIX: 须带 **x-sag-app-id**（与 control-plane `apisix.rs` route vars 一致），再 curl / wget / 侧车。
.EXAMPLE
  .\quick-check-intra-dataplane.ps1 -ApisixTestHost "app.internal.com" -AppId "app-001"
#>
param(
  [string]$MockContainer = "sag-mock",
  [string]$ApisixContainer = "sag-apisix",
  [string]$ApisixTestHost = "app.internal.com",
  [string]$AppId = "app-001",
  [string]$TestPath = "/dev/",
  [string]$CurlSidecarImage = "curlimages/curl:8.11.1"
)

$ErrorActionPreference = "Stop"

Write-Host "=== mock direct (container $MockContainer; python:slim -> urllib) ===" -ForegroundColor Yellow
$pyMock = "import os,urllib.request as u;p=os.environ['MOCK_TC_PATH'];r=u.urlopen('http://127.0.0.1:18080'+p,timeout=20);print('mock_http_code=',r.status)"
& docker @("exec", "-e", "MOCK_TC_PATH=$TestPath", $MockContainer, "python", "-c", $pyMock)
if ($LASTEXITCODE -ne 0) { Write-Host "docker exit=$LASTEXITCODE" -ForegroundColor Red }

Write-Host "=== APISIX data plane (container $ApisixContainer, Host: $ApisixTestHost, x-sag-app-id: $AppId) ===" -ForegroundColor Yellow
$apisixUrl = "http://127.0.0.1:9080$TestPath"

$null = & docker @("exec", $ApisixContainer, "sh", "-lc", "command -v curl >/dev/null 2>&1")
if ($LASTEXITCODE -eq 0) {
  & docker @("exec", $ApisixContainer, "sh", "-lc", "curl -sS -o /dev/null -w 'apisix_http_code=%{http_code}\n' '$apisixUrl' -H 'Host: $ApisixTestHost' -H 'x-sag-app-id: $AppId'")
  if ($LASTEXITCODE -ne 0) {
    Write-Host "curl exit=$LASTEXITCODE" -ForegroundColor Red
    exit $LASTEXITCODE
  }
}
else {
  $null = & docker @("exec", $ApisixContainer, "sh", "-lc", "command -v wget >/dev/null 2>&1")
  if ($LASTEXITCODE -eq 0) {
    $inner = "wget -qS --spider --header='Host: $ApisixTestHost' --header='x-sag-app-id: $AppId' '$apisixUrl' 2>&1"
    $out = & docker @("exec", $ApisixContainer, "sh", "-lc", $inner) 2>&1 | Out-String
    $m = [regex]::Match($out, 'HTTP/\d+\.\d+\s+(\d{3})')
    if ($m.Success) {
      Write-Host "apisix_http_code=$($m.Groups[1].Value) (wget)" -ForegroundColor Green
    }
    else {
      Write-Host "WARN: wget output could not parse HTTP status:" -ForegroundColor Yellow
      if ($out.Length -gt 0) {
        Write-Host $out.Substring([Math]::Max(0, $out.Length - 800)) -ForegroundColor DarkGray
      }
      exit 3
    }
  }
  else {
    Write-Host "No curl/wget in $ApisixContainer; docker run sidecar $CurlSidecarImage ..." -ForegroundColor Yellow
    & docker @(
      "run", "--rm", "--network", "container:$ApisixContainer",
      $CurlSidecarImage,
      "-sS", "-o", "/dev/null", "-w", "apisix_http_code=%{http_code}`n",
      "-H", "Host: $ApisixTestHost",
      "-H", "x-sag-app-id: $AppId",
      $apisixUrl
    )
    if ($LASTEXITCODE -ne 0) {
      Write-Host "Sidecar failed (docker pull $CurlSidecarImage ?). exit=$LASTEXITCODE" -ForegroundColor Red
      exit 2
    }
  }
}

Write-Host "Done. If apisix still 404: reconcile routes to APISIX (Edge control-plane-admin + SAG_APISIX_ADMIN_* on Intra). Else: docker logs $ApisixContainer --tail 80" -ForegroundColor DarkGray

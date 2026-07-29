<#
.SYNOPSIS
  向 control-plane-admin 写入与默认 smoke/connector 一致的隧道路由，供 stealth-tunnel-agent 同步。

.DESCRIPTION
  默认: app_id=app-001, connector_endpoint=connector-local-001:stream（与 sag-connector 默认 SAG_CONNECTOR_ID 一致）。
  执行后等待数秒再跑 smoke；若 agent 同步仍失败，检查 SAG_CONTROL_PLANE_SYNC_ENDPOINT。

  用法（在 sag-cloud 或任意目录）:
    .\scripts\seed-demo-tunnel-route.ps1
#>
param(
    [string]$AdminBase = "http://127.0.0.1:8090",
    [string]$HostName = "app.internal.com",
    [string]$AppId = "app-001",
    [string]$ConnectorEndpoint = "connector-local-001:stream",
    [bool]$RequireHealthyTunnel = $true
)

$uri = "$($AdminBase.TrimEnd('/'))/api/v1/agent/routes"
$body = @{
    host                     = $HostName
    app_id                   = $AppId
    connector_endpoint       = $ConnectorEndpoint
    require_healthy_tunnel   = $RequireHealthyTunnel
} | ConvertTo-Json

Write-Host "POST $uri" -ForegroundColor Cyan
Write-Host $body
try {
    Invoke-RestMethod -Method Post -Uri $uri -ContentType "application/json; charset=utf-8" -Body $body
    Write-Host "OK — 路由已写入。若 agent 已跑，约 5s 内会从 $AdminBase 同步；可查询:" -ForegroundColor Green
    Write-Host "  Invoke-RestMethod `"$($AdminBase.TrimEnd('/'))/api/v1/agent/routes?app_id=$AppId`"" -ForegroundColor DarkGray
}
catch {
    Write-Host "FAIL: $_" -ForegroundColor Red
    exit 1
}

$ErrorActionPreference = "Continue"
Set-Location (Join-Path $PSScriptRoot "..\..")

Write-Host "=== smoke: management + dataplane ==="
.\scripts\smoke-dataplane.ps1

Write-Host "=== smoke: route snapshot ==="
Invoke-RestMethod "http://127.0.0.1:8090/api/v1/agent/routes" | ConvertTo-Json -Depth 6
$ErrorActionPreference = "Continue"
Set-Location (Join-Path $PSScriptRoot "..\..")

Write-Host "[diag] admin routes"
try { Invoke-RestMethod "http://127.0.0.1:8090/api/v1/agent/routes?app_id=app-001" | ConvertTo-Json -Depth 6 } catch { Write-Host $_ }

Write-Host "[diag] bridge direct"
try {
  Invoke-RestMethod "http://127.0.0.1:9000/dev/" -Headers @{
    "x-sag-app-id"="app-001";
    "x-sag-user-id"="u-admin";
    "x-sag-user-roles"="admin";
  } | ConvertTo-Json -Depth 6
} catch { Write-Host $_ }

Write-Host "[diag] zentinel ingress"
try {
  & curl.exe -k -sS -i "https://127.0.0.1:10080/dev/" -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin"
} catch { Write-Host $_ }
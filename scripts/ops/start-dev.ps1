$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")
$Root = (Get-Location).Path
$RuntimeDir = Join-Path $Root ".runtime"
if (!(Test-Path $RuntimeDir)) { New-Item -ItemType Directory -Path $RuntimeDir | Out-Null }

Write-Host "[1/4] start docker compose core stack"
docker compose up -d postgres etcd apisix mock-workload control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge

Write-Host "[2/4] seed demo route"
.\scripts\seed-demo-tunnel-route.ps1

Write-Host "[3/4] start docker-log -> audit ingestion (background)"
$EnableIngest = if ($env:SAG_AUDIT_INGEST_ENABLE) { $env:SAG_AUDIT_INGEST_ENABLE } else { "1" }
if ($EnableIngest -eq "1") {
  $AdminUser = if ($env:SAG_AUDIT_INGEST_USER) { $env:SAG_AUDIT_INGEST_USER } else { "admin" }
  $AdminPass = if ($env:SAG_AUDIT_INGEST_PASSWORD) { $env:SAG_AUDIT_INGEST_PASSWORD } else { "Admin@123" }
  $ControlBase = if ($env:SAG_AUDIT_CONTROL_BASE) { $env:SAG_AUDIT_CONTROL_BASE } else { "http://127.0.0.1:8090" }
  $Services = if ($env:SAG_AUDIT_INGEST_SERVICES) { $env:SAG_AUDIT_INGEST_SERVICES } else { "zentinel,http-tunnel-bridge,stealth-tunnel-agent,sag-connector,public-edge,apisix" }
  try {
    $login = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:8080/api/v1/auth/login" -ContentType "application/json" -Body (@{
      username = $AdminUser
      password = $AdminPass
    } | ConvertTo-Json)
    if ($login.token) {
      $PidFile = Join-Path $RuntimeDir "audit-ingest.pid"
      $LogFile = Join-Path $RuntimeDir "audit-ingest.log"
      $AlreadyRunning = $false
      if (Test-Path $PidFile) {
        $OldPid = Get-Content $PidFile -ErrorAction SilentlyContinue
        if ($OldPid) {
          $proc = Get-Process -Id $OldPid -ErrorAction SilentlyContinue
          if ($proc) { $AlreadyRunning = $true }
        }
      }
      if ($AlreadyRunning) {
        Write-Host "audit ingestion already running, skip restart"
      } else {
        $cmd = "TOKEN='$($login.token)' CONTROL_BASE='$ControlBase' SERVICES='$Services' bash '$Root/scripts/ops/ingest-docker-logs-to-audit.sh' >> '$LogFile' 2>&1"
        $p = Start-Process -FilePath "bash" -ArgumentList "-lc", $cmd -PassThru -WindowStyle Hidden
        Set-Content -Path $PidFile -Value $p.Id
        Write-Host "audit ingestion started pid=$($p.Id)"
      }
    } else {
      Write-Host "skip audit ingestion: login token empty"
    }
  } catch {
    Write-Host "skip audit ingestion: admin login failed ($AdminUser)"
  }
} else {
  Write-Host "skip audit ingestion: SAG_AUDIT_INGEST_ENABLE=$EnableIngest"
}

Write-Host "[4/4] run smoke"
.\scripts\smoke-dataplane.ps1
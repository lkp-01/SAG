param(
  [string]$ComposeEdge = "docker-compose.edge.yml",
  [string]$ComposeIntra = "docker-compose.intra.yml"
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Set-Location $RepoRoot

function Get-ContainerEnv {
  param([string]$Service, [string]$Key, [string[]]$ComposeArgs)
  try {
    $value = docker compose @ComposeArgs exec -T $Service sh -c "printenv $Key" 2>$null
    if ($LASTEXITCODE -eq 0 -and $value) { return [int]$value.Trim() }
  } catch {}
  return $null
}

$edge = @("-f", $ComposeEdge)
$intra = @("-f", $ComposeIntra)
$bridgeFwd = Get-ContainerEnv "http-tunnel-bridge" "SAG_BRIDGE_FORWARD_TIMEOUT_MS" $edge
$grpcRpc = Get-ContainerEnv "http-tunnel-bridge" "SAG_GRPC_RPC_TIMEOUT_MS" $edge
$agentFwd = Get-ContainerEnv "stealth-tunnel-agent" "SAG_FORWARD_TIMEOUT_MS" $edge
$connectorHttp = Get-ContainerEnv "sag-connector" "SAG_CONNECTOR_HTTP_TIMEOUT_MS" $intra

Write-Host "=== Request deadline chain ==="
Write-Host "SAG_CONNECTOR_HTTP_TIMEOUT_MS     = $connectorHttp"
Write-Host "SAG_FORWARD_TIMEOUT_MS (Agent)    = $agentFwd"
Write-Host "SAG_BRIDGE_FORWARD_TIMEOUT_MS     = $bridgeFwd"
Write-Host "SAG_GRPC_RPC_TIMEOUT_MS           = $grpcRpc"

$issues = [System.Collections.Generic.List[string]]::new()
if (-not $connectorHttp) { $issues.Add("missing Connector HTTP timeout") }
if (-not $agentFwd) { $issues.Add("missing Agent forward timeout") }
if (-not $bridgeFwd) { $issues.Add("missing Bridge forward timeout") }
if (-not $grpcRpc) { $issues.Add("missing Bridge gRPC RPC timeout") }
if ($connectorHttp -and $agentFwd -and $connectorHttp -ge $agentFwd) {
  $issues.Add("connector_http >= agent_forward")
}
if ($agentFwd -and $bridgeFwd -and $agentFwd -ge $bridgeFwd) {
  $issues.Add("agent_forward >= bridge_forward")
}
if ($bridgeFwd -and $grpcRpc -and $bridgeFwd -gt $grpcRpc) {
  $issues.Add("bridge_forward > grpc_rpc")
}

$apisixSource = Get-Content "services/control-plane-admin/src/apisix.rs" -Raw
if ($apisixSource -notmatch '"retries"\s*:\s*0') {
  $issues.Add("APISIX route does not explicitly disable retries")
}
$bridgeSource = Get-Content "proxy/http-tunnel-bridge/src/main.rs" -Raw
if ($bridgeSource -match 'for\s+attempt\s+in\s+0\.\.2') {
  $issues.Add("Bridge still contains the old two-attempt retry loop")
}

if ($issues.Count -gt 0) {
  $issues | ForEach-Object { Write-Host "FAIL: $_" -ForegroundColor Red }
  exit 1
}

Write-Host "OK: connector < agent < bridge <= grpc; APISIX and Bridge retries are disabled" -ForegroundColor Green
exit 0

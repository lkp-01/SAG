$ErrorActionPreference = "Stop"

$requiredEnvironment = @{
    SAG_POSTGRES_PASSWORD = "test-only"
    SAG_REDIS_PASSWORD = "test-only"
    SAG_FOURA_CLIENT_SECRET = "test-only"
    SAG_POSTGRES_DSN = "postgres://sag:test@postgres-ha.internal/sag"
    SAG_JWT_SECRET = "test-only"
    SAG_PUBLIC_READONLY_TOKEN = "test-only"
    SAG_AGENT_SYNC_TOKEN = "test-only"
    SAG_APISIX_ADMIN_API_KEY = "test-only"
    SAG_BOOTSTRAP_ADMIN_PASSWORD = "test-only"
    SAG_SESSION_REDIS_URL = "rediss://sag:test@redis-ha.internal/0"
    SAG_POLICY_INTERNAL_TOKEN = "test-only"
    SAG_POLICY_CACHE_REDIS_URL = "rediss://sag:test@redis-ha.internal/0"
    SAG_GRPC_TLS_CERT = "/run/secrets/agent.crt"
    SAG_GRPC_TLS_KEY = "/run/secrets/agent.key"
    SAG_GRPC_TLS_CLIENT_CA = "/run/secrets/connector-ca.crt"
    SAG_CONNECTOR_CERT_BINDINGS = "connector-1=/run/secrets/connector.crt"
    SAG_BRIDGE_REDIS_URL = "rediss://sag:test@redis-ha.internal/2"
    SAG_GRPC_TLS_CLIENT_CERT = "/run/secrets/bridge.crt"
    SAG_GRPC_TLS_CLIENT_KEY = "/run/secrets/bridge.key"
    SAG_GRPC_TLS_CA = "/run/secrets/agent-ca.crt"
    SAG_GRPC_TLS_SERVER_NAME = "agent.internal"
    SAG_GRAFANA_ADMIN_PASSWORD = "test-only"
    SAG_TUNNEL_ENDPOINT = "https://agent-1.internal:50051"
    SAG_TUNNEL_ENDPOINTS = "https://agent-1.internal:50051,https://agent-2.internal:50051"
}
foreach ($entry in $requiredEnvironment.GetEnumerator()) {
    Set-Item -Path "Env:$($entry.Key)" -Value $entry.Value
}

function Get-ComposeJson([string[]]$Files) {
    $arguments = @("compose")
    foreach ($file in $Files) {
        $arguments += @("-f", $file)
    }
    $arguments += @("config", "--format", "json")
    $json = & docker @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "docker compose config failed: $($Files -join ', ')"
    }
    return $json | ConvertFrom-Json
}

function Assert-ServiceContract($Config, [string[]]$Names) {
    foreach ($name in $Names) {
        $property = $Config.services.PSObject.Properties[$name]
        if ($null -eq $property) { throw "missing HA service: $name" }
        $service = $property.Value
        if ($null -eq $service.healthcheck) { throw "$name has no healthcheck" }
        if ($service.restart -ne "unless-stopped") { throw "$name has no restart policy" }
        if ([int64]$service.mem_limit -le 0) { throw "$name has no memory limit" }
    }
}

$edge = Get-ComposeJson @(
    "docker-compose.edge.yml",
    "docker-compose.release.edge.yml",
    "docker-compose.hscale-edge.yml",
    "docker-compose.hscale-auth.yml"
)
Assert-ServiceContract $edge @(
    "http-tunnel-bridge", "http-tunnel-bridge-2",
    "stealth-tunnel-agent", "stealth-tunnel-agent-2",
    "sag-auth", "sag-auth-2", "sag-policy", "sag-policy-2"
)
if ($edge.services.'http-tunnel-bridge'.environment.SAG_TUNNEL_GRPC_ENDPOINT -eq
    $edge.services.'http-tunnel-bridge-2'.environment.SAG_TUNNEL_GRPC_ENDPOINT) {
    throw "both Bridge replicas target the same Agent path"
}

$intra = Get-ComposeJson @("docker-compose.intra.yml", "docker-compose.release.intra.yml")
Assert-ServiceContract $intra @("etcd", "etcd-2", "etcd-3", "apisix", "apisix-2", "apisix-lb")
if ($intra.services.'sag-connector'.environment.SAG_TUNNEL_ENDPOINTS -notmatch ',') {
    throw "Connector is not configured with both Agent endpoints"
}

$alertFile = "infra/observability/alerts/production-hardening.yml"
if (-not (Test-Path $alertFile)) { throw "missing production hardening alert rules" }
$alertText = Get-Content -Raw $alertFile
foreach ($alertName in @(
    "ReadyReplicaInsufficient", "PostgresPoolWaitHigh", "AuditDropDetected",
    "RedisPelOldestAgeHigh", "DeadLetterQueueGrowing", "QueueSaturationHigh",
    "AgentWithoutConnector", "RouteSyncStale", "AuthInvalidationLagHigh",
    "IndeterminateIdempotencyTooOld", "ServiceRestartLoop"
)) {
    if ($alertText -notmatch "alert:\s+$alertName") {
        throw "missing alert rule: $alertName"
    }
}

Write-Output "HA topology static contract passed"

#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

EDGE_COMPOSE=(docker compose -f docker-compose.edge.yml)
INTRA_COMPOSE=(docker compose -f docker-compose.intra.yml)

env_value() {
  local service="$1" key="$2"
  "${EDGE_COMPOSE[@]}" exec -T "$service" sh -c "printenv $key 2>/dev/null" 2>/dev/null \
    || "${INTRA_COMPOSE[@]}" exec -T "$service" sh -c "printenv $key 2>/dev/null" 2>/dev/null \
    || echo ""
}

bridge_fwd="$(env_value http-tunnel-bridge SAG_BRIDGE_FORWARD_TIMEOUT_MS)"
grpc_rpc="$(env_value http-tunnel-bridge SAG_GRPC_RPC_TIMEOUT_MS)"
agent_fwd="$(env_value stealth-tunnel-agent SAG_FORWARD_TIMEOUT_MS)"
connector_http="$(env_value sag-connector SAG_CONNECTOR_HTTP_TIMEOUT_MS)"

echo "=== Request deadline chain ==="
echo "SAG_CONNECTOR_HTTP_TIMEOUT_MS     = ${connector_http:-<missing>}"
echo "SAG_FORWARD_TIMEOUT_MS (Agent)    = ${agent_fwd:-<missing>}"
echo "SAG_BRIDGE_FORWARD_TIMEOUT_MS     = ${bridge_fwd:-<missing>}"
echo "SAG_GRPC_RPC_TIMEOUT_MS           = ${grpc_rpc:-<missing>}"

failures=0
for value_name in connector_http agent_fwd bridge_fwd grpc_rpc; do
  if [[ -z "${!value_name}" || "${!value_name}" -le 0 ]]; then
    echo "FAIL: ${value_name} is missing from running containers"
    failures=$((failures + 1))
  fi
done

if [[ -n "$connector_http" && -n "$agent_fwd" && "$connector_http" -ge "$agent_fwd" ]]; then
  echo "FAIL: connector_http >= agent_forward"
  failures=$((failures + 1))
fi
if [[ -n "$agent_fwd" && -n "$bridge_fwd" && "$agent_fwd" -ge "$bridge_fwd" ]]; then
  echo "FAIL: agent_forward >= bridge_forward"
  failures=$((failures + 1))
fi
if [[ -n "$bridge_fwd" && -n "$grpc_rpc" && "$bridge_fwd" -gt "$grpc_rpc" ]]; then
  echo "FAIL: bridge_forward > grpc_rpc"
  failures=$((failures + 1))
fi
if ! grep -Eq '"retries"[[:space:]]*:[[:space:]]*0' services/control-plane-admin/src/apisix.rs; then
  echo "FAIL: APISIX route does not explicitly disable retries"
  failures=$((failures + 1))
fi
if grep -Eq 'for[[:space:]]+attempt[[:space:]]+in[[:space:]]+0\.\.2' proxy/http-tunnel-bridge/src/main.rs; then
  echo "FAIL: Bridge still contains the old two-attempt retry loop"
  failures=$((failures + 1))
fi

if [[ "$failures" -ne 0 ]]; then
  echo "=== failed checks: $failures ==="
  exit 1
fi
echo "OK: connector < agent < bridge <= grpc; APISIX and Bridge retries are disabled"

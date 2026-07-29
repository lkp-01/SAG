#!/usr/bin/env bash
# Post cpuset: Intra APISIX + connector + tunnel (run on Intra host in REPO_ROOT).
set -euo pipefail

APP_ID="${APP_ID:-app-001}"
PATH_REQ="${PATH_REQ:-/dev/}"
EDGE_IP="${EDGE_IP:-172.16.9.107}"

say() { printf '\n== %s ==\n' "$*"; }

say "Containers"
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml ps

say "Cpuset"
for c in sag-connector sag-apisix sag-mock sag-etcd; do
  if docker inspect "$c" &>/dev/null; then
    printf '%s -> %s\n' "$c" "$(docker inspect "$c" --format '{{.HostConfig.CpusetCpus}}')"
  fi
done

say "Tunnel endpoint (must be current Edge)"
docker exec sag-connector printenv SAG_TUNNEL_ENDPOINT || true

say "Connector tunnel + forward metrics"
curl -sS http://127.0.0.1:9103/metrics 2>/dev/null | grep -E 'connector_tunnel_up|connector_forward_' | head -n 12 || echo "WARN: metrics :9103"

say "APISIX direct (bypass tunnel)"
code="$(curl -sS -o /dev/null -w '%{http_code}' -H "x-sag-app-id: ${APP_ID}" -H "x-sag-user-id: ui-admin" -H "x-sag-user-roles: admin" \
  "http://127.0.0.1:9080${PATH_REQ}" || echo 000)"
echo "apisix ${PATH_REQ} => ${code}"

say "Mock upstream"
curl -sS -o /dev/null -w "mock health => %{http_code}\n" http://127.0.0.1:18080/health || true

say "Optional: TLS reach Edge agent from Intra"
echo | timeout 5 openssl s_client -connect "${EDGE_IP}:50051" -servername localhost 2>&1 | tail -n 5 || echo "WARN: cannot reach Edge :50051"

say "Done — apisix app-001 expect 200; tunnel_up should be 1"

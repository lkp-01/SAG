#!/usr/bin/env bash
# Post hscale + cpuset: Edge local connectivity (run on Edge host in REPO_ROOT).
set -euo pipefail

APP_ID="${APP_ID:-app-001}"
PATH_REQ="${PATH_REQ:-/dev/}"
EDGE_IP="${EDGE_IP:-127.0.0.1}"

say() { printf '\n== %s ==\n' "$*"; }

say "Compose: bridge replicas"
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml ps http-tunnel-bridge http-tunnel-bridge-2 zentinel stealth-tunnel-agent 2>/dev/null || true

say "Cpuset (28c profile: bridge 12-14, bridge-2 15-17, zentinel 18-25 — see cpuset-edge-28.env)"
for c in sag-zentinel secure_access_gateway_sag-http-tunnel-bridge-1 sag-stealth-agent; do
  if docker inspect "$c" &>/dev/null; then
    printf '%s -> %s\n' "$c" "$(docker inspect "$c" --format '{{.HostConfig.CpusetCpus}}')"
  fi
done
bid="$(docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml ps -q http-tunnel-bridge-2 2>/dev/null | head -1)"
if [ -n "${bid}" ]; then
  printf 'bridge-2 -> %s\n' "$(docker inspect "$bid" --format '{{.HostConfig.CpusetCpus}}')"
fi

say "Zentinel -> both bridges (in-cluster)"
docker exec sag-zentinel sh -c 'wget -qO- --timeout=3 http://http-tunnel-bridge:9000/metrics 2>/dev/null | head -n 2' || echo "WARN: bridge:9000 from zentinel"
docker exec sag-zentinel sh -c 'wget -qO- --timeout=3 http://http-tunnel-bridge-2:9000/metrics 2>/dev/null | head -n 2' || echo "WARN: bridge-2:9000 from zentinel"

say "Northbound HTTPS (app-001)"
code="$(curl -sS -k -o /dev/null -w '%{http_code}' -H "x-sag-app-id: ${APP_ID}" -H "x-sag-user-id: ui-admin" -H "x-sag-user-roles: admin" \
  "https://${EDGE_IP}:10080${PATH_REQ}" || echo 000)"
echo "zentinel ${PATH_REQ} => ${code}"

say "Bridge host port (first replica only)"
code="$(curl -sS -o /dev/null -w '%{http_code}' -H "x-sag-app-id: ${APP_ID}" "http://${EDGE_IP}:9000${PATH_REQ}" || echo 000)"
echo "bridge :9000 ${PATH_REQ} => ${code}"

say "Agent / bridge grpc metrics snippet"
curl -sS "http://${EDGE_IP}:9000/metrics" 2>/dev/null | grep -E 'bridge_grpc_channel_forward_total|bridge_sync_inflight' | head -n 6 || true

say "Auth LB (when hscale-auth.yml is active)"
if docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-auth.yml ps -q sag-auth-lb 2>/dev/null | grep -q .; then
  a1="$(docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
    -f docker-compose.hscale-auth.yml ps -q sag-auth 2>/dev/null | head -1)"
  if [ -n "${a1}" ]; then
    printf 'sag-auth cpuset -> %s\n' "$(docker inspect "$a1" --format '{{.HostConfig.CpusetCpus}}')"
  fi
  aid="$(docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
    -f docker-compose.hscale-auth.yml ps -q sag-auth-2 2>/dev/null | head -1)"
  if [ -n "${aid}" ]; then
    printf 'sag-auth-2 cpuset -> %s\n' "$(docker inspect "$aid" --format '{{.HostConfig.CpusetCpus}}')"
  fi
  code="$(curl -sS -o /dev/null -w '%{http_code}' -X POST "http://${EDGE_IP}:8080/api/v1/auth/login" \
    -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123"}' || echo 000)"
  echo "auth login via :8080 LB => ${code}"
  curl -sS "http://${EDGE_IP}:9104/metrics" 2>/dev/null | grep -E 'sag_auth_login_memo_(hit|miss)_total' | head -n 4 || echo "WARN: auth metrics :9104"
else
  echo "skip (sag-auth-lb not running — single sag-auth only)"
fi

say "Done — expect zentinel/bridge 2xx for app-001; 403 acceptable for policy deny"

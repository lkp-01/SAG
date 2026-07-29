#!/usr/bin/env bash
# Roll back Auth horizontal scale: stop LB + sag-auth-2, restore single sag-auth on host :8080.
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

COMPOSE_HSCALE=(
  -f docker-compose.edge.yml
  -f docker-compose.release.edge.yml
  -f docker-compose.hscale-edge.yml
  -f docker-compose.hscale-auth.yml
  -f docker-compose.edge.perf.yml
  -f docker-compose.hscale-edge.perf.yml
  -f docker-compose.hscale-auth.perf.yml
  --env-file scripts/ops/cpuset-edge-28.env
)

COMPOSE_SINGLE=(
  -f docker-compose.edge.yml
  -f docker-compose.release.edge.yml
  -f docker-compose.hscale-edge.yml
  -f docker-compose.edge.perf.yml
  -f docker-compose.hscale-edge.perf.yml
  --env-file scripts/ops/cpuset-edge-28.env
)

echo "== Stop auth hscale (LB + replica) =="
docker compose "${COMPOSE_HSCALE[@]}" stop sag-auth-lb sag-auth-2 2>/dev/null || true
docker rm -f sag-auth-lb sag-auth-2 2>/dev/null || true
docker ps -aq --filter name=sag-auth-lb | xargs -r docker rm -f
docker ps -aq --filter name=sag-auth-2 | xargs -r docker rm -f

echo "== Recreate single sag-auth (no hscale-auth compose) =="
docker compose "${COMPOSE_SINGLE[@]}" up -d --force-recreate sag-auth

echo "== Verify :8080 (expect no nginx Server header) =="
curl -sS -D- -o /dev/null -X POST "http://127.0.0.1:8080/api/v1/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123"}' | grep -iE '^HTTP|^Server' || true

docker inspect sag-auth --format 'sag-auth cpuset={{.HostConfig.CpusetCpus}} ports={{json .HostConfig.PortBindings}}' 2>/dev/null || \
  docker ps --format '{{.Names}} {{.Ports}}' | grep -i auth

echo "== Done. Run k6 from loadgen against ${EDGE_IP:-172.16.9.107}:8080 =="

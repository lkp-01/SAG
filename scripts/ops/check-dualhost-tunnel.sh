#!/usr/bin/env bash
set -euo pipefail

APP_ID="${APP_ID:-app-001}"
PATH_REQ="${PATH_REQ:-/dev/}"
EDGE_BASE="${EDGE_BASE:-http://127.0.0.1:3001}"

say() {
  printf '\n[%s] %s\n' "$(date '+%H:%M:%S')" "$*"
}

probe() {
  local name="$1"
  local url="$2"
  local out
  out="$(mktemp)"
  local code
  code="$(curl -sS -L -o "$out" -w "%{http_code}" \
    -H "x-sag-app-id: ${APP_ID}" \
    -H "x-sag-user-id: ui-admin" \
    -H "x-sag-user-roles: admin" \
    "$url" || true)"
  local body
  body="$(head -c 220 "$out" | tr '\n' ' ')"
  rm -f "$out"
  printf '%s status=%s body=%s\n' "$name" "$code" "$body"
}

say "Dual-host quick checks"
printf 'APP_ID=%s PATH_REQ=%s EDGE_BASE=%s\n' "$APP_ID" "$PATH_REQ" "$EDGE_BASE"

say "Prometheus readiness"
curl -i -sS "${EDGE_BASE}/api-prom/-/ready" | sed -n '1,8p'

say "Northbound probes"
probe "N1 zentinel" "${EDGE_BASE}/api-zentinel${PATH_REQ}"
probe "T1 bridge" "${EDGE_BASE}/api-bridge${PATH_REQ}"

say "Expected pass criteria"
echo "- N1/T1 final status should be 2xx or policy 403"
echo "- Must NOT contain: connector tunnel is unhealthy"

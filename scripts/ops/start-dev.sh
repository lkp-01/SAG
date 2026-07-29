#!/usr/bin/env bash
set -eu
set -o pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

echo "[1/4] start docker compose core stack"
docker compose up -d postgres etcd apisix mock-workload control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge

echo "[2/4] seed demo route via admin API"
curl -sS -X POST "http://127.0.0.1:8090/api/v1/agent/routes" \
  -H "Content-Type: application/json" \
  -d '{"host":"app.internal.com","app_id":"app-001","connector_endpoint":"connector-local-001:stream","require_healthy_tunnel":true}' || true

echo "[3/4] start docker-log -> audit ingestion (background)"
if [[ "${SAG_AUDIT_INGEST_ENABLE:-1}" == "1" ]]; then
  ADMIN_USER="${SAG_AUDIT_INGEST_USER:-admin}"
  ADMIN_PASS="${SAG_AUDIT_INGEST_PASSWORD:-Admin@123}"
  CONTROL_BASE="${SAG_AUDIT_CONTROL_BASE:-http://127.0.0.1:8090}"
  SERVICES="${SAG_AUDIT_INGEST_SERVICES:-zentinel,http-tunnel-bridge,stealth-tunnel-agent,sag-connector,public-edge,apisix}"
  mkdir -p "$ROOT/.runtime"
  LOGIN_JSON="$(curl -sS -X POST "http://127.0.0.1:8080/api/v1/auth/login" \
    -H "Content-Type: application/json" \
    -d "{\"username\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASS}\"}" || true)"
  TOKEN="$(printf '%s' "$LOGIN_JSON" | jq -r '.token // empty' 2>/dev/null || true)"
  if [[ -n "${TOKEN}" ]]; then
    if [[ -f "$ROOT/.runtime/audit-ingest.pid" ]] && kill -0 "$(cat "$ROOT/.runtime/audit-ingest.pid")" 2>/dev/null; then
      echo "audit ingestion already running, skip restart"
    else
      TOKEN="$TOKEN" CONTROL_BASE="$CONTROL_BASE" SERVICES="$SERVICES" \
      nohup bash "$ROOT/scripts/ops/ingest-docker-logs-to-audit.sh" \
        > "$ROOT/.runtime/audit-ingest.log" 2>&1 &
      INJ_PID=$!
      # shellcheck disable=SC2009
      sleep 1
      if kill -0 "$INJ_PID" 2>/dev/null; then
        echo "$INJ_PID" > "$ROOT/.runtime/audit-ingest.pid"
        echo "audit ingestion started pid=$INJ_PID"
      else
        echo "audit ingestion failed to stay alive, see .runtime/audit-ingest.log"
      fi
    fi
  else
    echo "skip audit ingestion: failed to login admin user (${ADMIN_USER})"
  fi
else
  echo "skip audit ingestion: SAG_AUDIT_INGEST_ENABLE=${SAG_AUDIT_INGEST_ENABLE}"
fi

echo "[4/4] run smoke"
bash ./scripts/smoke-dataplane-wsl.sh
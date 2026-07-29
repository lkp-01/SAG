#!/usr/bin/env bash
set -euo pipefail

# Ingest Docker container logs into SAG audit center.
# Usage:
#   TOKEN=<admin_jwt> CONTROL_BASE=http://127.0.0.1:8090 \
#   SERVICES="zentinel,apisix" ./scripts/ops/ingest-docker-logs-to-audit.sh

CONTROL_BASE="${CONTROL_BASE:-http://127.0.0.1:8090}"
SERVICES="${SERVICES:-zentinel,http-tunnel-bridge,stealth-tunnel-agent,sag-connector,public-edge,apisix}"
POLL_SEC="${POLL_SEC:-5}"
TAIL_SEC="${TAIL_SEC:-8}"
TOKEN="${TOKEN:-}"

if [[ -z "${TOKEN}" ]]; then
  echo "TOKEN is required (admin JWT)." >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "docker command not found." >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "curl command not found." >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq command not found." >&2
  exit 1
fi

echo "ingesting logs to ${CONTROL_BASE}/api/v1/audit/logs"
echo "services: ${SERVICES}"

while true; do
  IFS=',' read -r -a svc_list <<< "${SERVICES}"
  now_ms=$(date +%s%3N)
  for svc in "${svc_list[@]}"; do
    svc_trim="$(echo "${svc}" | xargs)"
    [[ -z "${svc_trim}" ]] && continue
    docker logs --since "${TAIL_SEC}s" "${svc_trim}" 2>&1 | while IFS= read -r line; do
      [[ -z "${line}" ]] && continue
      payload="$(jq -nc \
        --arg id "docker-${svc_trim}-${now_ms}-$RANDOM" \
        --argjson ts_ms "${now_ms}" \
        --arg service "${svc_trim}" \
        --arg path "docker://logs/${svc_trim}" \
        --arg method "LOG" \
        --arg result "docker-log" \
        --arg trace_id "docker-${svc_trim}" \
        --arg msg "${line}" \
        '{
          id: $id,
          ts_ms: $ts_ms,
          service: $service,
          user_id: "",
          app_id: "",
          path: $path,
          method: $method,
          latency_ms: 0,
          decision: "observe",
          result: $result,
          trace_id: $trace_id,
          extra_json: ({source:"docker-logs",message:$msg} | tostring)
        }')"
      curl -sS -X POST "${CONTROL_BASE}/api/v1/audit/logs" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d "${payload}" >/dev/null || true
    done
  done
  sleep "${POLL_SEC}"
done


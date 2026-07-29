#!/usr/bin/env bash
# 数据面南北向分层冒烟（WSL/Linux）；每层单独输出，便于定位。
# 用法: cd sag-cloud && bash ./scripts/smoke-dataplane-wsl.sh
# 环境变量见同目录 smoke-dataplane.ps1 顶部注释（同名）。
# Zentinel 首次编译较慢时: SMOKE_ZENTINEL_WAIT_SEC=600 bash ./scripts/smoke-dataplane-wsl.sh

set -eu
set -o pipefail

PATH_REQ="${PATH_REQ:-/dev/}"
HDR_APP="${HDR_APP:-app-001}"
HDR_USER="${HDR_USER:-u-admin}"
HDR_ROLES="${HDR_ROLES:-admin}"
BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:9000}"
ZENTINEL_URL="${ZENTINEL_URL:-https://127.0.0.1:10080}"
MOCK_BASE_URL="${MOCK_BASE_URL:-http://127.0.0.1:18080}"
APISIX_DATA_BASE_URL="${APISIX_DATA_BASE_URL:-http://127.0.0.1:9080}"
# Dual-host convenience:
# - EDGE_BASE_URL: e.g. http://192.168.8.87 (override bridge + management + zentinel defaults)
# - INTRA_APISIX_DATA_BASE_URL: e.g. http://192.168.9.26:9080 (south direct check)
# - INTRA_MOCK_BASE_URL: e.g. http://192.168.9.26:18080 (south direct upstream check)
EDGE_BASE_URL="${EDGE_BASE_URL:-}"
if [[ -n "${EDGE_BASE_URL}" ]]; then
  eb="${EDGE_BASE_URL%/}"
  if [[ "${BRIDGE_URL}" == "http://127.0.0.1:9000" ]]; then BRIDGE_URL="${eb}:9000"; fi
  if [[ "${ZENTINEL_URL}" == "https://127.0.0.1:10080" ]]; then
    host="${eb#http://}"; host="${host#https://}"
    ZENTINEL_URL="https://${host}:10080"
  fi
fi
if [[ -n "${INTRA_APISIX_DATA_BASE_URL:-}" ]]; then
  APISIX_DATA_BASE_URL="${INTRA_APISIX_DATA_BASE_URL}"
fi
if [[ -n "${INTRA_MOCK_BASE_URL:-}" ]]; then
  MOCK_BASE_URL="${INTRA_MOCK_BASE_URL}"
fi
SMOKE_CONTROL_PLANE_BASE="${SMOKE_CONTROL_PLANE_BASE:-http://127.0.0.1:8090}"
SMOKE_AUTH_BASE="${SMOKE_AUTH_BASE:-http://127.0.0.1:8080}"
SMOKE_POLICY_BASE="${SMOKE_POLICY_BASE:-http://127.0.0.1:8081}"
APP_CASES="${APP_CASES:-app-dev:/dev/,app-ci:/ci/,app-finance:/finance/,app-oa:/oa/,app-hr:/hr/,app-bi:/bi/,app-vendor:/vendor/}"

HDR_ARGS=(
  -H "x-sag-app-id: ${HDR_APP}"
  -H "x-sag-user-id: ${HDR_USER}"
  -H "x-sag-user-roles: ${HDR_ROLES}"
)

# First `cargo run` of zentinel on a new host can take many minutes. Optional wait (seconds):
#   SMOKE_ZENTINEL_WAIT_SEC=600 bash ./scripts/smoke-dataplane-wsl.sh
SMOKE_ZENTINEL_WAIT_SEC="${SMOKE_ZENTINEL_WAIT_SEC:-0}"

failures=0

layer_ok() {
  echo "    PASS  HTTP $1"
  if [[ -n "${2:-}" ]]; then echo "    body  $2"; fi
}

layer_fail() {
  echo "    FAIL  $1"
  failures=$((failures + 1))
}

# $1=id $2=title $3=url  (SAG tunnel headers)
probe_http() {
  local id="$1" title="$2" url="$3"
  echo ""
  echo "=== [${id}] ${title} ==="
  echo "    ${url}"
  local code
  code=$(curl -sS -o /tmp/sag-smoke-body.$$ -w "%{http_code}" "${HDR_ARGS[@]}" "$url" || true)
  if [[ "$code" =~ ^2[0-9][0-9]$ ]]; then
    local body
    body=$(head -c 220 /tmp/sag-smoke-body.$$ 2>/dev/null | tr '\r\n' '  ')
    layer_ok "$code" "$body"
  else
    layer_fail "HTTP ${code:-000} (body: /tmp/sag-smoke-body.$$)"
  fi
  rm -f /tmp/sag-smoke-body.$$
}

probe_http_for_app() {
  local id="$1" title="$2" url="$3" app_id="$4"
  echo ""
  echo "=== [${id}] ${title} ==="
  echo "    ${url} (app=${app_id})"
  local code
  local curl_k=()
  if [[ "$url" == https://* ]]; then
    curl_k=(-k)
  fi
  code=$(curl -sS "${curl_k[@]}" -o /tmp/sag-smoke-body.$$ -w "%{http_code}" \
    -H "x-sag-app-id: ${app_id}" \
    -H "x-sag-user-id: ${HDR_USER}" \
    -H "x-sag-user-roles: ${HDR_ROLES}" \
    "$url" || true)
  if [[ "$code" =~ ^2[0-9][0-9]$ ]]; then
    local body
    body=$(head -c 220 /tmp/sag-smoke-body.$$ 2>/dev/null | tr '\r\n' '  ')
    layer_ok "$code" "$body"
  else
    layer_fail "HTTP ${code:-000} (body: /tmp/sag-smoke-body.$$)"
  fi
  rm -f /tmp/sag-smoke-body.$$
}

probe_http_no_hdr() {
  local id="$1" title="$2" url="$3"
  echo ""
  echo "=== [${id}] ${title} ==="
  echo "    ${url}"
  local code
  code=$(curl -sS -o /tmp/sag-smoke-body.$$ -w "%{http_code}" "$url" || true)
  if [[ "$code" =~ ^2[0-9][0-9]$ ]]; then
    local body
    body=$(head -c 220 /tmp/sag-smoke-body.$$ 2>/dev/null | tr '\r\n' '  ')
    layer_ok "$code" "$body"
  else
    layer_fail "HTTP ${code:-000}"
  fi
  rm -f /tmp/sag-smoke-body.$$
}

echo "smoke-dataplane-wsl.sh — north-to-south + management probes"
echo "PATH_REQ=${PATH_REQ} app=${HDR_APP}"

if [[ -z "${SMOKE_SKIP_MANAGEMENT:-}" ]]; then
  probe_http_no_hdr "M1" "control-plane-admin /health" "${SMOKE_CONTROL_PLANE_BASE}/health"
  probe_http_no_hdr "M2" "sag-auth /health" "${SMOKE_AUTH_BASE}/health"
  probe_http_no_hdr "M3" "sag-policy /health" "${SMOKE_POLICY_BASE}/health"
  if [[ -n "${SMOKE_ADMIN_BEARER_TOKEN:-}" ]]; then
    echo ""
    echo "=== [M4] verify control-plane route model ==="
    body="$(curl -sS -H "Authorization: Bearer ${SMOKE_ADMIN_BEARER_TOKEN}" "${SMOKE_CONTROL_PLANE_BASE}/api/v1/agent/routes" || true)"
    if echo "${body}" | grep -q "\"app_id\":\"app-dev\"" && echo "${body}" | grep -q "\"app_id\":\"app-vendor\""; then
      echo "    PASS  app route rows visible"
    else
      layer_fail "route rows missing (expected app-dev..app-vendor)"
    fi
  fi
else
  echo ""
  echo "=== [M*] management skipped (SMOKE_SKIP_MANAGEMENT=1) ==="
fi

echo ""
echo "=== [N1] north Zentinel HTTPS ingress + full tunnel chain ==="
echo "    ${ZENTINEL_URL}${PATH_REQ}"
if [[ "${SMOKE_ZENTINEL_WAIT_SEC}" =~ ^[0-9]+$ ]] && [[ "${SMOKE_ZENTINEL_WAIT_SEC}" -gt 0 ]]; then
  echo "    (waiting up to ${SMOKE_ZENTINEL_WAIT_SEC}s for zentinel to listen; set to 0 to disable)"
fi
code="000"
start_ts=$(date +%s)
while true; do
  code=$(curl -sS -k -o /tmp/sag-smoke-body.$$ -w "%{http_code}" "${HDR_ARGS[@]}" "${ZENTINEL_URL}${PATH_REQ}" 2>/dev/null || true)
  if [[ "$code" =~ ^2[0-9][0-9]$ ]]; then
    body=$(head -c 220 /tmp/sag-smoke-body.$$ 2>/dev/null | tr '\r\n' '  ')
    echo "    PASS  HTTP $code"
    echo "    body  $body"
    break
  fi
  now_ts=$(date +%s)
  elapsed=$((now_ts - start_ts))
  if [[ "${SMOKE_ZENTINEL_WAIT_SEC}" =~ ^[0-9]+$ ]] && [[ "$elapsed" -lt "${SMOKE_ZENTINEL_WAIT_SEC}" ]]; then
    echo "    WAIT  HTTP ${code:-000} (${elapsed}s / ${SMOKE_ZENTINEL_WAIT_SEC}s) — zentinel may still be compiling; retry in 5s..."
    sleep 5
    continue
  fi
  echo "    FAIL  HTTP ${code:-000}"
  echo "    HINT  docker compose logs --tail=80 zentinel ; test -f proxy/core/Cargo.toml || git submodule update --init --recursive"
  failures=$((failures + 1))
  break
done
rm -f /tmp/sag-smoke-body.$$

probe_http "T1" "http-tunnel-bridge (gRPC to agent path)" "${BRIDGE_URL}${PATH_REQ}"

if [[ -z "${SMOKE_SKIP_APISIX_DIRECT:-}" ]]; then
  probe_http "S1" "south APISIX data plane (direct)" "${APISIX_DATA_BASE_URL}${PATH_REQ}"
else
  echo ""
  echo "=== [S1] APISIX direct skipped (SMOKE_SKIP_APISIX_DIRECT=1) ==="
fi

if [[ -z "${SMOKE_SKIP_MOCK_DIRECT:-}" ]]; then
  probe_http_no_hdr "S2" "south mock workload /health (upstream only)" "${MOCK_BASE_URL}/health"
else
  echo ""
  echo "=== [S2] mock direct skipped (SMOKE_SKIP_MOCK_DIRECT=1) ==="
fi

if [[ -n "${PUBLIC_EDGE_BASE_URL:-}" ]]; then
  pe="${PUBLIC_EDGE_BASE_URL%/}"
  probe_http "P1" "public-edge ingress" "${pe}${PATH_REQ}"
fi

if [[ -z "${SMOKE_SKIP_MULTI_APP:-}" ]]; then
  echo ""
  echo "=== [V*] verify 7 app real paths ==="
  IFS=',' read -r -a pairs <<< "${APP_CASES}"
  idx=1
  for pair in "${pairs[@]}"; do
    app_id="${pair%%:*}"
    app_path="${pair#*:}"
    probe_http_for_app "V${idx}N" "zentinel real path" "${ZENTINEL_URL}${app_path}" "${app_id}"
    probe_http_for_app "V${idx}T" "bridge real path" "${BRIDGE_URL}${app_path}" "${app_id}"
    if [[ -z "${SMOKE_SKIP_APISIX_DIRECT:-}" ]]; then
      probe_http_for_app "V${idx}S" "apisix real path" "${APISIX_DATA_BASE_URL}${app_path}" "${app_id}"
    fi
    idx=$((idx + 1))
  done
fi

echo ""
echo "=== SUMMARY ==="
if [[ "$failures" -eq 0 ]]; then
  echo "All executed layers passed."
  exit 0
else
  echo "Failed layers: $failures"
  exit 1
fi

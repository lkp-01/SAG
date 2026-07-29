#!/usr/bin/env bash
# Tiered dataplane-only load: constant 700 iter/s first; if dataplane_get success rate >= threshold, run 900.
# Requires: k6, jq. Run from anywhere; resolves repo root from this script location.
#
# Usage:
#   ./scripts/ops/run-dataplane-tiered-700-900.sh
#   SAG_TIER_DURATION=5m FIRST_RPS=700 SECOND_RPS=900 SUCCESS_THRESHOLD=0.80 ./scripts/ops/run-dataplane-tiered-700-900.sh
#   SAG_EDGE_HOST=172.16.9.107 ./scripts/ops/run-dataplane-tiered-700-900.sh
#   DATAPLANE_URL=https://edge:10080/dev/ ./scripts/ops/run-dataplane-tiered-700-900.sh
#
# Recommended (aligns with bridge queue / 202 semantics):
#   SAG_DP_ACCEPT_202=1 SAG_DP_POLL_202=1 SAG_DP_ACCEPT_429_SHED=1 ./scripts/ops/run-dataplane-tiered-700-900.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SAG_CLOUD_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
K6_SCRIPT="${SCRIPT_DIR}/load-dataplane-k6.js"
ARTIFACTS_DIR="${SAG_ARTIFACTS_DIR:-${SAG_CLOUD_ROOT}/artifacts}"
TS="$(date +%Y%m%d-%H%M%S)"
RUN_ID="${SAG_RUN_ID:-${TS}}"

FIRST_RPS="${FIRST_RPS:-700}"
SECOND_RPS="${SECOND_RPS:-900}"
SUCCESS_THRESHOLD="${SUCCESS_THRESHOLD:-0.80}"
COOLDOWN_SEC="${COOLDOWN_SEC:-60}"
SAG_TIER_DURATION="${SAG_TIER_DURATION:-3m}"

SAG_EDGE_HOST="${SAG_EDGE_HOST:-172.16.9.107}"
H="${SAG_EDGE_HOST#http://}"
H="${H#https://}"
H="${H%/}"

DATAPLANE_URL="${DATAPLANE_URL:-https://${H}:10080/dev/}"
AUTH_BASE_URL="${AUTH_BASE_URL:-http://${H}:8080}"
POLICY_BASE_URL="${POLICY_BASE_URL:-http://${H}:8081}"
CONTROL_BASE_URL="${CONTROL_BASE_URL:-http://${H}:8090}"
export SAG_EDGE_HOST="$H"
SAG_APP_ID="${SAG_APP_ID:-app-001}"
SAG_AUTH_USERNAME="${SAG_AUTH_USERNAME:-admin}"
SAG_AUTH_PASSWORD="${SAG_AUTH_PASSWORD:-Admin@123}"
SAG_REQ_TIMEOUT="${SAG_REQ_TIMEOUT:-90s}"
SAG_PRE_ALLOCATED_VUS="${SAG_PRE_ALLOCATED_VUS:-3000}"
SAG_MAX_VUS="${SAG_MAX_VUS:-20000}"
SAG_INSECURE_SKIP_TLS_VERIFY="${SAG_INSECURE_SKIP_TLS_VERIFY:-1}"
SAG_GRACEFUL_STOP="${SAG_GRACEFUL_STOP:-20s}"

# Default: treat 202+poll as success (recommended when SOFT_INFLIGHT triggers queue).
SAG_DP_ACCEPT_202="${SAG_DP_ACCEPT_202:-1}"
SAG_DP_POLL_202="${SAG_DP_POLL_202:-1}"
SAG_DP_POLL_MAX_MS="${SAG_DP_POLL_MAX_MS:-120000}"
SAG_DP_POLL_INTERVAL_MS="${SAG_DP_POLL_INTERVAL_MS:-100}"
SAG_DP_ACCEPT_429_SHED="${SAG_DP_ACCEPT_429_SHED:-1}"

for cmd in k6 jq; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "ERROR: missing '$cmd' in PATH (install k6 and jq)." >&2
    exit 1
  fi
done
if [[ ! -f "$K6_SCRIPT" ]]; then
  echo "ERROR: k6 script not found: $K6_SCRIPT" >&2
  exit 1
fi
mkdir -p "$ARTIFACTS_DIR"

export DATAPLANE_URL AUTH_BASE_URL POLICY_BASE_URL CONTROL_BASE_URL
export SAG_APP_ID SAG_AUTH_USERNAME SAG_AUTH_PASSWORD
export SAG_RUN_MODE="dataplane_only"
export SAG_SCENARIO_TYPE="dataplane_only"
export SAG_LOGIN_EVERY_N="0"
export SAG_CONTROL_EVERY_N="0"
export SAG_POLICY_LIST_EVERY_N="0"
export SAG_CONTROL_PLANE_BLOCKING="0"
export SAG_LOGIN_RETRIES="0"
export SAG_GATE_PROFILE="dataplane"
export SAG_REQ_TIMEOUT SAG_INSECURE_SKIP_TLS_VERIFY SAG_GRACEFUL_STOP
export SAG_PRE_ALLOCATED_VUS SAG_MAX_VUS
export SAG_DP_ACCEPT_202 SAG_DP_POLL_202 SAG_DP_POLL_MAX_MS SAG_DP_POLL_INTERVAL_MS SAG_DP_ACCEPT_429_SHED

run_tier() {
  local rps="$1"
  local out_json="$2"
  export SAG_START_QPS="$rps"
  export SAG_STAGE1_QPS="$rps"
  export SAG_STAGE2_QPS="$rps"
  export SAG_STAGE3_QPS="$rps"
  export SAG_STAGE4_QPS="$rps"
  export SAG_STAGE1_DURATION="$SAG_TIER_DURATION"
  export SAG_STAGE2_DURATION="$SAG_TIER_DURATION"
  export SAG_STAGE3_DURATION="$SAG_TIER_DURATION"
  export SAG_STAGE4_DURATION="$SAG_TIER_DURATION"

  echo ""
  echo "========== k6 dataplane_only constant ${rps} iter/s, ${SAG_TIER_DURATION} x4 stages -> ${out_json} =========="
  (cd "$SAG_CLOUD_ROOT" && k6 run --summary-export "$out_json" "$K6_SCRIPT") || true
}

read_dataplane_success_rate() {
  local json="$1"
  jq -r '.metrics["sag_api_success_rate{api:dataplane_get}"].value // empty' "$json"
}

append_analysis() {
  local json="$1"
  local label="$2"
  {
    echo ""
    echo "---- ${label} ----"
    echo "dataplane_get success_rate (k6 Rate value): $(read_dataplane_success_rate "$json")"
    echo "check dataplane status acceptable:"
    jq -r '.root_group.checks["dataplane status acceptable"] // empty | "  passes=\(.passes // 0) fails=\(.fails // 0)"' "$json" 2>/dev/null || true
    echo "failure_cause (non-zero count):"
    jq -r '
      .metrics | to_entries[]
      | select(.key|test("^sag_dataplane_failure_cause_total"))
      | select((.value.count // 0) > 0)
      | "  \(.key): count=\(.value.count)"
    ' "$json" 2>/dev/null || true
    echo "bridge_status (non-zero count):"
    jq -r '
      .metrics | to_entries[]
      | select(.key|test("^sag_dataplane_bridge_status_total"))
      | select((.value.count // 0) > 0)
      | "  \(.key): count=\(.value.count)"
    ' "$json" 2>/dev/null || true
    echo "http_reqs: $(jq -r '.metrics.http_reqs | "count=\(.count // 0) rate=\(.rate // 0)"' "$json" 2>/dev/null || true)"
  } >>"$REPORT"
}

remediation_hints() {
  cat >>"$REPORT" <<'EOF'

---- 问题定位线索（按指标 → 代码/组件）----
- sag_dataplane_failure_cause_total{cause:forbidden} 或 bridge/http 403 高:
    → policy DENY / agent 鉴权: sag-policy、sag-auth、stealth-tunnel-agent grpc_server（policy_eval / resolve_user_identity）、Redis 降级键是否启用。
- cause:policy_unavailable / HTTP 503 文案含 policy:
    → policy 过载或超时、agent 到 policy 的 HTTP 超时与并发门限。
- cause:gateway_502 / bridge_status 502:
    → http-tunnel-bridge 转发、connector 隧道、或 poll 最终失败。
- cause:timeout / status:0 / api 超时多:
    → 整条链路超时链: SAG_BRIDGE_*、SAG_FORWARD_TIMEOUT_MS、connector/bridge gRPC deadline。
- bridge_status 202 高且 poll 后失败:
    → Redis 队列、SAG_BRIDGE_SOFT_INFLIGHT、worker 并发；看 bridge metrics（enqueue、queue depth）。
- cause:connector_unhealthy / no_tunnel_route:
    → intra sag-connector、路由同步、DB DSN 指向 edge。

EOF
}

REPORT="${ARTIFACTS_DIR}/k6-tiered-${RUN_ID}-report.txt"
OUT700="${ARTIFACTS_DIR}/k6-tiered-${RUN_ID}-dp-${FIRST_RPS}.json"
OUT900="${ARTIFACTS_DIR}/k6-tiered-${RUN_ID}-dp-${SECOND_RPS}.json"

{
  echo "SAG tiered dataplane run id: ${RUN_ID}"
  echo "threshold: dataplane_get success_rate >= ${SUCCESS_THRESHOLD} -> run ${SECOND_RPS}"
  echo "DATAPLANE_URL=${DATAPLANE_URL}"
  echo "tier_duration_each_stage=${SAG_TIER_DURATION} (x4 stages same RPS)"
  echo "SAG_DP_ACCEPT_202=${SAG_DP_ACCEPT_202} SAG_DP_POLL_202=${SAG_DP_POLL_202} SAG_DP_ACCEPT_429_SHED=${SAG_DP_ACCEPT_429_SHED}"
  date -u "+%Y-%m-%dT%H:%M:%SZ"
} >"$REPORT"

run_tier "$FIRST_RPS" "$OUT700"
append_analysis "$OUT700" "tier ${FIRST_RPS}"

rate700="$(read_dataplane_success_rate "$OUT700")"
if [[ -z "$rate700" ]]; then
  echo "WARN: could not read sag_api_success_rate{api:dataplane_get} from $OUT700" | tee -a "$REPORT"
  rate700="0"
fi

run_second=0
if awk -v r="$rate700" -v t="$SUCCESS_THRESHOLD" 'BEGIN { exit !(r >= t) }'; then
  run_second=1
  echo "" | tee -a "$REPORT"
  echo "SUCCESS: rate ${rate700} >= ${SUCCESS_THRESHOLD}; will run ${SECOND_RPS} after ${COOLDOWN_SEC}s cooldown." | tee -a "$REPORT"
else
  echo "" | tee -a "$REPORT"
  echo "SKIP ${SECOND_RPS}: rate ${rate700} < ${SUCCESS_THRESHOLD} (threshold not met)." | tee -a "$REPORT"
fi

remediation_hints

if [[ "$run_second" -eq 1 ]]; then
  sleep "$COOLDOWN_SEC"
  run_tier "$SECOND_RPS" "$OUT900"
  append_analysis "$OUT900" "tier ${SECOND_RPS}"
  rate900="$(read_dataplane_success_rate "$OUT900")"
  echo "" | tee -a "$REPORT"
  echo "tier ${SECOND_RPS} dataplane_get success_rate: ${rate900}" | tee -a "$REPORT"
fi

echo ""
echo "Done. Report: ${REPORT}"
echo "JSON: ${OUT700}" $([[ "$run_second" -eq 1 ]] && echo "${OUT900}")

#!/usr/bin/env bash
# 数据面阶梯：1000 RPS -> (success>=90%) -> sleep 300s -> 1500 RPS. Success mode: apisix_routed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SAG_CLOUD_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
K6_SCRIPT="${SCRIPT_DIR}/load-dataplane-k6.js"
ARTIFACTS_DIR="${SAG_ARTIFACTS_DIR:-${SAG_CLOUD_ROOT}/artifacts}"
TS="$(date +%Y%m%d-%H%M%S)"
RUN_ID="${SAG_RUN_ID:-${TS}}"

FIRST_RPS="${FIRST_RPS:-1000}"
SECOND_RPS="${SECOND_RPS:-1500}"
SUCCESS_THRESHOLD="${SUCCESS_THRESHOLD:-0.90}"
COOLDOWN_SEC="${COOLDOWN_SEC:-300}"
SAG_TIER_DURATION="${SAG_TIER_DURATION:-2m}"

SAG_EDGE_HOST="${SAG_EDGE_HOST:-172.16.9.107}"
H="${SAG_EDGE_HOST#http://}"
H="${H#https://}"
H="${H%/}"

DATAPLANE_URL="${DATAPLANE_URL:-https://${H}:10080/dev/}"
SAG_APP_ID="${SAG_APP_ID:-app-001}"
SAG_REQ_TIMEOUT="${SAG_REQ_TIMEOUT:-90s}"
SAG_PRE_ALLOCATED_VUS="${SAG_PRE_ALLOCATED_VUS:-2500}"
SAG_MAX_VUS="${SAG_MAX_VUS:-12000}"
SAG_INSECURE_SKIP_TLS_VERIFY="${SAG_INSECURE_SKIP_TLS_VERIFY:-1}"
SAG_GRACEFUL_STOP="${SAG_GRACEFUL_STOP:-20s}"
SAG_DP_SUCCESS_MODE="${SAG_DP_SUCCESS_MODE:-apisix_routed}"
SAG_DP_ACCEPT_202="${SAG_DP_ACCEPT_202:-1}"
SAG_DP_POLL_202="${SAG_DP_POLL_202:-1}"
SAG_DP_ACCEPT_429_SHED="${SAG_DP_ACCEPT_429_SHED:-1}"

for cmd in k6 jq; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "ERROR: need $cmd" >&2; exit 1; }
done
mkdir -p "$ARTIFACTS_DIR"

export DATAPLANE_URL SAG_EDGE_HOST SAG_APP_ID SAG_REQ_TIMEOUT SAG_PRE_ALLOCATED_VUS SAG_MAX_VUS
export SAG_INSECURE_SKIP_TLS_VERIFY SAG_GRACEFUL_STOP SAG_DP_SUCCESS_MODE
export SAG_DP_ACCEPT_202 SAG_DP_POLL_202 SAG_DP_ACCEPT_429_SHED
export SAG_RUN_MODE="dataplane_only"
export SAG_SCENARIO_TYPE="dataplane_only"
export SAG_LOGIN_EVERY_N="0"
export SAG_CONTROL_EVERY_N="0"
export SAG_POLICY_LIST_EVERY_N="0"
export SAG_GATE_PROFILE="dataplane_routed"

run_tier() {
  local rps="$1"
  local out_json="$2"
  export SAG_START_QPS="$rps" SAG_STAGE1_QPS="$rps" SAG_STAGE2_QPS="$rps" SAG_STAGE3_QPS="$rps" SAG_STAGE4_QPS="$rps"
  export SAG_STAGE1_DURATION="$SAG_TIER_DURATION" SAG_STAGE2_DURATION="$SAG_TIER_DURATION"
  export SAG_STAGE3_DURATION="$SAG_TIER_DURATION" SAG_STAGE4_DURATION="$SAG_TIER_DURATION"
  echo "========== k6 dataplane_only ${rps} iter/s apisix_routed -> ${out_json} =========="
  (cd "$SAG_CLOUD_ROOT" && k6 run --summary-export "$out_json" "$K6_SCRIPT") || true
}

read_rate() {
  jq -r '.metrics["sag_api_success_rate{api:dataplane_get}"].values.rate // .metrics["sag_api_success_rate{api:dataplane_get}"].value // empty' "$1"
}

REPORT="${ARTIFACTS_DIR}/k6-gated-${RUN_ID}-report.txt"
OUT1="${ARTIFACTS_DIR}/k6-gated-${RUN_ID}-dp-${FIRST_RPS}.json"
OUT2="${ARTIFACTS_DIR}/k6-gated-${RUN_ID}-dp-${SECOND_RPS}.json"

{
  echo "run_id=${RUN_ID} mode=${SAG_DP_SUCCESS_MODE}"
  echo "threshold=${SUCCESS_THRESHOLD} cooldown=${COOLDOWN_SEC}s"
  date -u "+%Y-%m-%dT%H:%M:%SZ"
} >"$REPORT"

run_tier "$FIRST_RPS" "$OUT1"
rate1="$(read_rate "$OUT1")"
echo "tier ${FIRST_RPS} rate=${rate1}" >>"$REPORT"

if [[ -z "$rate1" ]] || ! awk -v r="$rate1" -v t="$SUCCESS_THRESHOLD" 'BEGIN { exit !(r >= t) }'; then
  echo "SKIP ${SECOND_RPS}: rate ${rate1:-?} < ${SUCCESS_THRESHOLD}" | tee -a "$REPORT"
  exit 1
fi

echo "sleep ${COOLDOWN_SEC}s" | tee -a "$REPORT"
sleep "$COOLDOWN_SEC"
run_tier "$SECOND_RPS" "$OUT2"
rate2="$(read_rate "$OUT2")"
echo "tier ${SECOND_RPS} rate=${rate2}" >>"$REPORT"
echo "Done. Report: $REPORT"

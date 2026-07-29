#!/usr/bin/env bash
set -euo pipefail

PROM_BASE="${PROM_BASE:-http://127.0.0.1:9091}"
UI_BASE="${UI_BASE:-http://127.0.0.1:3001}"
PRODUCTION_ARTIFACT="${PRODUCTION_ARTIFACT:-}"
MAX_P95_SECONDS="${MAX_P95_SECONDS:-2.5}"

[[ -n "$PRODUCTION_ARTIFACT" && -f "$PRODUCTION_ARTIFACT" ]] || {
  echo "PRODUCTION_ARTIFACT must point to a full-chain sag.production-gate/v1 artifact" >&2
  exit 2
}

python3 - "$PRODUCTION_ARTIFACT" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8-sig") as f: a=json.load(f)
errors=[]
if a.get("schema_version") != "sag.production-gate/v1": errors.append("schema")
if a.get("scenario") != "full_chain": errors.append("scenario")
r=a.get("results",{}); e=a.get("evidence",{})
if (r.get("business_success_rate") or 0) < .99: errors.append("business_success_rate")
if r.get("dropped_iterations") != 0: errors.append("dropped_iterations")
for name in ("auth_rate","policy_rate","audit_rate","redis_queue_rate","idempotency_rate","workload_rate"):
    if (e.get(name) or 0) < .99: errors.append(name)
if errors:
    raise SystemExit("artifact is not full-chain evidence: " + ", ".join(errors))
print("full-chain artifact accepted")
PY

query_value() {
  local expression="$1"
  curl -fsS -G "${PROM_BASE}/api/v1/query" --data-urlencode "query=${expression}" |
    python3 -c 'import json,sys; d=json.load(sys.stdin); r=d.get("data",{}).get("result",[]); print(r[0]["value"][1] if r else "")'
}

check_p95() {
  local name="$1" expression="$2" value
  value="$(query_value "$expression")"
  [[ -n "$value" ]] || { echo "missing p95 series: $name" >&2; return 1; }
  python3 - "$name" "$value" "$MAX_P95_SECONDS" <<'PY'
import math,sys
name,value,limit=sys.argv[1],float(sys.argv[2]),float(sys.argv[3])
if not math.isfinite(value) or value > limit:
    raise SystemExit(f"{name} p95={value}s exceeds {limit}s")
print(f"PASS {name} p95={value}s")
PY
}

check_p95 control-plane-admin 'avg(http_request_duration_seconds{job="control-plane-admin",quantile="0.95",path!="/metrics"})'
check_p95 sag-auth 'avg(http_request_duration_seconds{job="sag-auth",quantile="0.95",path!="/metrics"})'
check_p95 sag-policy 'avg(http_request_duration_seconds{job="sag-policy",quantile="0.95",path!="/metrics"})'
check_p95 http-tunnel-bridge 'avg(http_request_duration_seconds{job="http-tunnel-bridge",quantile="0.95",path!="/metrics"})'
check_p95 stealth-tunnel-agent 'avg(agent_forward_duration_seconds{job="stealth-tunnel-agent",quantile="0.95"})'
check_p95 sag-connector 'avg(connector_forward_duration_seconds{job="sag-connector",quantile="0.95"})'
check_p95 zentinel-proxy 'avg(http_request_duration_seconds{job="zentinel-proxy",quantile="0.95",path!="/metrics"})'
check_p95 apisix 'histogram_quantile(0.95, sum(rate(apisix_http_latency_bucket{job="apisix",type="request"}[5m])) by (le))'
check_p95 mock-workload 'histogram_quantile(0.95, sum(rate(mock_request_duration_seconds_bucket{service="mock-workload"}[5m])) by (le))'

curl -fsSI "${UI_BASE}/workflow" >/dev/null
curl -fsSI "${UI_BASE}/ops/workflow" >/dev/null
echo "workflow p95 validation passed"

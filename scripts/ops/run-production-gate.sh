#!/usr/bin/env bash
set -euo pipefail

scenario="${1:-full_chain}"
target_rps="${2:-500}"
repeats="${3:-3}"
soak_minutes="${4:-120}"
steady_minutes="${SAG_GATE_STEADY_MINUTES:-10}"
output_dir="${SAG_GATE_OUTPUT_DIR:-artifacts/production-gate}"

if [[ "${1:-}" == "--self-test" ]]; then
  python3 - <<'PY'
def validate(a):
    errors=[]
    req=lambda ok,msg: errors.append(msg) if not ok else None
    req(a.get("scenario")=="full_chain","scenario")
    req(a["results"].get("business_success_rate",0)>=.99,"business")
    req(a["results"].get("dropped_iterations") == 0,"dropped")
    req(all(a["evidence"].get(x,0)>=.99 for x in ("auth_rate","policy_rate","audit_rate","redis_queue_rate","idempotency_rate","workload_rate")),"evidence")
    req(a["results"].get("unexpected_status_total",0)==0,"status")
    return errors
fixture={"scenario":"full_chain","results":{"business_success_rate":1,"dropped_iterations":0,"unexpected_status_total":0},"evidence":{x:1 for x in ("auth_rate","policy_rate","audit_rate","redis_queue_rate","idempotency_rate","workload_rate")}}
assert not validate(fixture)
fixture["results"]["unexpected_status_total"]=1
assert validate(fixture)
fixture["results"]["unexpected_status_total"]=0
fixture["evidence"]["audit_rate"]=0
assert validate(fixture)
print("production gate shell self-test passed")
PY
  exit 0
fi

[[ "$scenario" == "full_chain" ]] || { echo "only full_chain can qualify production capacity" >&2; exit 2; }
(( repeats >= 3 )) || { echo "at least three repeats are required" >&2; exit 2; }
(( steady_minutes >= 10 && steady_minutes <= 15 )) || { echo "steady test must be 10-15 minutes" >&2; exit 2; }
(( soak_minutes >= 120 && soak_minutes <= 240 )) || { echo "soak must be 120-240 minutes" >&2; exit 2; }

for command in k6 python3 git; do
  command -v "$command" >/dev/null || { echo "missing command: $command" >&2; exit 2; }
done
for name in SAG_PERF_ENVIRONMENT SAG_IMAGE_DIGESTS_JSON SAG_RESOURCE_EVIDENCE_JSON SAG_DEPENDENCY_EVIDENCE_JSON \
  DATAPLANE_URL AUTH_BASE_URL POLICY_BASE_URL CONTROL_BASE_URL SAG_AUTH_USERNAME SAG_AUTH_PASSWORD; do
  [[ -n "${!name:-}" ]] || { echo "required environment variable missing: $name" >&2; exit 2; }
done
for path in "$SAG_IMAGE_DIGESTS_JSON" "$SAG_RESOURCE_EVIDENCE_JSON" "$SAG_DEPENDENCY_EVIDENCE_JSON"; do
  [[ -f "$path" ]] || { echo "evidence file missing: $path" >&2; exit 2; }
done
git_sha="$(git rev-parse HEAD)"
[[ "$git_sha" =~ ^[0-9a-f]{40}$ ]] || { echo "real Git SHA required" >&2; exit 2; }

mkdir -p "$output_dir"
run_id="$(date -u +%Y%m%dT%H%M%SZ)"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
k6_script="$script_dir/load-dataplane-k6.js"

export SAG_RUN_MODE=strict SAG_SCENARIO_TYPE=full_chain SAG_PRODUCTION_GATE=1
export SAG_INSECURE_SKIP_TLS_VERIFY=0 SAG_LOGIN_EVERY_N=1 SAG_SKIP_VERIFY_AFTER_LOGIN=0
export SAG_MUTATION_MODE=1 SAG_REQUIRE_REDIS_QUEUE=1 SAG_DP_POLL_202=1 SAG_EXPECT_DATAPLANE_STATUS="${SAG_EXPECT_DATAPLANE_STATUS:-200}"
export SAG_AUDIT_SAMPLE_EVERY_N="${SAG_AUDIT_SAMPLE_EVERY_N:-100}"
export SAG_TARGET_RPS="$target_rps" SAG_PRE_ALLOCATED_VUS="${SAG_PRE_ALLOCATED_VUS:-$target_rps}" SAG_MAX_VUS="${SAG_MAX_VUS:-$((target_rps * 4))}"

build_and_validate() {
  local summary="$1" artifact="$2" duration="$3"
  SUMMARY="$summary" ARTIFACT="$artifact" DURATION="$duration" GIT_SHA="$git_sha" TARGET_RPS="$target_rps" python3 - <<'PY'
import json, os, re, sys
def metric(s,name,field):
    return s.get("metrics",{}).get(name,{}).get(field)
with open(os.environ["SUMMARY"],encoding="utf-8-sig") as f: summary=json.load(f)
with open(os.environ["SAG_IMAGE_DIGESTS_JSON"],encoding="utf-8-sig") as f: digests=json.load(f)
with open(os.environ["SAG_RESOURCE_EVIDENCE_JSON"],encoding="utf-8-sig") as f: resources=json.load(f)
with open(os.environ["SAG_DEPENDENCY_EVIDENCE_JSON"],encoding="utf-8-sig") as f: dependencies=json.load(f)
statuses={}
errors={}
for name,value in summary.get("metrics",{}).items():
    match=re.match(r"sag_dataplane_bridge_status_total\{status:([^,}]+)",name)
    if match: statuses[match.group(1)]=value.get("count",0)
    if re.match(r"sag_(?:api_business_reject|api_system_failure|correlation_mismatch|stale_result|mutation_side_effect_mismatch|unexpected_business_status)_total",name): errors[name]=value.get("count",0)
artifact={
 "schema_version":"sag.production-gate/v1","qualification":"unqualified-run","scenario":"full_chain",
 "run":{"git_sha":os.environ["GIT_SHA"],"image_digests":digests,"environment":os.environ["SAG_PERF_ENVIRONMENT"],"k6_exit_code":0,"raw_k6_summary":os.path.abspath(os.environ["SUMMARY"])},
 "config_snapshot":{"target_rps":int(os.environ["TARGET_RPS"]),"duration":os.environ["DURATION"],"expected_status":int(os.environ["SAG_EXPECT_DATAPLANE_STATUS"]),"mutation":True,"require_redis_queue":True,"insecure_skip_tls_verify":False},
 "results":{"target_rps":int(os.environ["TARGET_RPS"]),"actual_completed_rps":metric(summary,"iterations","rate"),"business_success_rate":metric(summary,"sag_business_success_rate","value"),"dropped_iterations":metric(summary,"dropped_iterations","count"),"latency_ms":{"p50":metric(summary,"http_req_duration","med"),"p95":metric(summary,"http_req_duration","p(95)"),"p99":metric(summary,"http_req_duration","p(99)")},"business_error_distribution":errors,"http_status_distribution":statuses},
 "evidence":{"auth_rate":metric(summary,"sag_auth_evidence_rate","value"),"policy_rate":metric(summary,"sag_policy_evidence_rate","value"),"audit_rate":metric(summary,"sag_audit_evidence_rate","value"),"redis_queue_rate":metric(summary,"sag_redis_queue_evidence_rate","value"),"idempotency_rate":metric(summary,"sag_idempotency_evidence_rate","value"),"workload_rate":metric(summary,"sag_workload_evidence_rate","value"),"resources":resources,"dependencies":dependencies}}
with open(os.environ["ARTIFACT"],"w",encoding="utf-8") as f: json.dump(artifact,f,indent=2)
errors_out=[]
need=lambda ok,msg: errors_out.append(msg) if not ok else None
r=artifact["results"]; e=artifact["evidence"]; c=artifact["config_snapshot"]
need(r["business_success_rate"] is not None and r["business_success_rate"]>=.99,"business success")
need(r["dropped_iterations"] == 0,"dropped iterations")
need(r["actual_completed_rps"] is not None and r["actual_completed_rps"]>=r["target_rps"]*.98,"completed RPS")
need(r["latency_ms"]["p95"] is not None and r["latency_ms"]["p95"]<=2500,"p95")
need(r["latency_ms"]["p99"] is not None and r["latency_ms"]["p99"]<=5000,"p99")
need(all((e.get(x) or 0)>=.99 for x in ("auth_rate","policy_rate","audit_rate","redis_queue_rate","idempotency_rate","workload_rate")),"chain evidence")
need(e.get("audit_rate")==1,"sampled audit")
need(all(float(v)==0 for v in r["business_error_distribution"].values()),"business errors")
need(all(k==str(c["expected_status"]) or float(v)==0 for k,v in r["http_status_distribution"].items()),"unexpected status")
need(resources.get("status")=="complete" and resources.get("process_rss_within_budget") is True,"resource evidence")
need(float(resources.get("load_generator_cpu_pct",101))<=85 and float(resources.get("load_generator_network_utilization_pct",101))<=80,"generator headroom")
need(dependencies.get("status")=="complete" and float(dependencies.get("apisix_requests_delta",0))>0,"APISIX evidence")
need(float(dependencies.get("audit_dropped_total",1))==0 and float(dependencies.get("authorization_errors_total",1))==0,"audit/auth evidence")
need(float(dependencies.get("pg_pool_wait_p95_ms",51))<=50 and float(dependencies.get("redis_pel_oldest_ms",1001))<=1000,"dependency SLO")
need(isinstance(digests,list) and digests and all(re.search(r"sha256:[0-9a-fA-F]{64}",str(x)) for x in digests),"image digests")
if errors_out:
    print("artifact rejected: "+", ".join(errors_out),file=sys.stderr); sys.exit(1)
print("artifact passed: "+os.environ["ARTIFACT"])
PY
}

run_one() {
  local label="$1" duration="$2"
  local summary="$output_dir/$run_id-$label-summary.json" artifact="$output_dir/$run_id-$label-artifact.json"
  export SAG_TEST_DURATION="$duration"
  k6 run --summary-export "$summary" "$k6_script"
  build_and_validate "$summary" "$artifact" "$duration"
  printf '%s\n' "$artifact"
}

artifacts=()
for ((i=1;i<=repeats;i++)); do artifacts+=("$(run_one "repeat-$i" "${steady_minutes}m")"); done
artifacts+=("$(run_one soak "${soak_minutes}m")")
printf 'production gate passed; artifacts:\n'
printf '  %s\n' "${artifacts[@]}"

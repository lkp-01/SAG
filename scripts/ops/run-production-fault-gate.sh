#!/usr/bin/env bash
set -euo pipefail

all_scenarios=(
  kill_bridge
  kill_agent
  kill_connector
  auth_policy_replica
  postgres_failover
  redis_failover
  apisix_workload
  network_impairment
)

validate_artifact() {
  python3 - "$1" <<'PY'
import json, re, sys

path = sys.argv[1]
with open(path, encoding="utf-8-sig") as handle:
    artifact = json.load(handle)

scenarios = {
    "kill_bridge": {"true": ["lb_stopped_unready_traffic", "alternate_complete_path_takeover"], "zero": ["accepted_mutation_redispatch_total"]},
    "kill_agent": {"true": ["bridge_stopped_unready_traffic", "connector_new_epoch_observed"], "zero": ["old_epoch_response_accepted_total"]},
    "kill_connector": {"true": ["agent_became_unready"], "zero": ["unsafe_retry_total", "absolute_deadline_violation_total"]},
    "auth_policy_replica": {"true": ["ready_replica_takeover"], "zero": ["unauthorized_allow_total", "fail_open_total"]},
    "postgres_failover": {"true": ["auth_policy_fail_closed", "pool_wait_bounded", "audit_buffer_bounded"], "zero": ["connection_storm_total"]},
    "redis_failover": {"true": ["bounded_sync_fallback", "pel_recovered"], "zero": ["lost_job_total"]},
    "apisix_workload": {"true": ["connector_error_semantics_correct", "response_memory_bounded"], "zero": ["unknown_response_total"]},
    "network_impairment": {"true": ["absolute_deadline_preserved", "cancellation_release_slo_met"], "zero": ["deadline_violation_total"]},
}

errors = []
def require(condition, message):
    if not condition:
        errors.append(message)

scenario = artifact.get("scenario")
run = artifact.get("run", {})
traffic = artifact.get("traffic", {})
results = artifact.get("results", {})
evidence = artifact.get("evidence", {})
resources = artifact.get("resources", {})
assertions = artifact.get("assertions", {})

require(artifact.get("schema_version") == "sag.production-fault-gate/v1", "unsupported or missing schema_version")
require(scenario in scenarios, "unknown fault scenario")
require(run.get("runner_exit_code") == 0, "scenario runner exited non-zero")
require(bool(re.fullmatch(r"[0-9a-fA-F]{40}", str(run.get("git_sha", "")))), "real Git SHA is required")
require(run.get("environment") not in (None, "", "unspecified"), "named isolated environment is required")
require(run.get("fault_injected") is True and run.get("service_restored") is True, "fault injection/restore evidence is incomplete")

submitted = traffic.get("submitted_total", 0)
classified = traffic.get("classified_total", -1)
classifications = traffic.get("final_classifications")
require(isinstance(classifications, dict), "per-request final classifications are missing")
classification_sum = sum(classifications.values()) if isinstance(classifications, dict) else -1
require(submitted > 0 and classified == submitted and classification_sum == submitted, "submitted, classified, and classification totals must match")

for field in ("unknown_job_total", "duplicate_side_effect_total", "incorrect_authorization_total", "unready_accept_total", "permanent_pel_total"):
    require(results.get(field) == 0, f"{field} must be present and zero")
require(results.get("business_slo_met") is True, "business SLO was not met")
require(results.get("rto_ms") is not None and results.get("rto_limit_ms") is not None and results["rto_ms"] <= results["rto_limit_ms"], "RTO exceeded or evidence is missing")

for field in ("expected_business_status_rate", "correct_response_body_rate", "auth_participation_rate", "policy_participation_rate", "audit_completion_rate"):
    require(evidence.get(field, 0) >= .99, f"{field} below 0.99 or missing")
require(evidence.get("audit_completion_rate") == 1.0, "audit completion evidence must be exactly 1.0")
require(evidence.get("tls_verified") is True, "TLS verification evidence is missing")
require(resources.get("hard_permits_peak") is not None and resources.get("hard_permits_limit") is not None and resources["hard_permits_peak"] <= resources["hard_permits_limit"], "hard permit limit exceeded or missing")
require(resources.get("pg_connections_peak") is not None and resources.get("pg_connections_budget") is not None and resources["pg_connections_peak"] <= resources["pg_connections_budget"], "PostgreSQL connection budget exceeded or missing")
require(resources.get("response_memory_bounded") is True, "response memory bound is not proven")

if scenario in scenarios:
    for field in scenarios[scenario]["true"]:
        require(assertions.get(field) is True, f"{field} must be true")
    for field in scenarios[scenario]["zero"]:
        require(assertions.get(field) == 0, f"{field} must be present and zero")

if errors:
    print(f"FAIL {path}", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    sys.exit(1)
print(f"PASS {path}")
PY
}

self_test() {
  local fixture
  fixture="$(mktemp)"
  trap 'rm -f "$fixture"' RETURN
  python3 - "$fixture" <<'PY'
import json, sys
artifact = {
  "schema_version":"sag.production-fault-gate/v1", "scenario":"kill_bridge",
  "run":{"runner_exit_code":0,"git_sha":"a"*40,"environment":"isolated-test","fault_injected":True,"service_restored":True},
  "traffic":{"submitted_total":100,"classified_total":100,"final_classifications":{"business_success":98,"expected_unavailable":2}},
  "results":{"unknown_job_total":0,"duplicate_side_effect_total":0,"incorrect_authorization_total":0,"unready_accept_total":0,"permanent_pel_total":0,"business_slo_met":True,"rto_ms":500,"rto_limit_ms":1000},
  "evidence":{"expected_business_status_rate":1,"correct_response_body_rate":1,"auth_participation_rate":1,"policy_participation_rate":1,"audit_completion_rate":1,"tls_verified":True},
  "resources":{"hard_permits_peak":10,"hard_permits_limit":20,"pg_connections_peak":8,"pg_connections_budget":16,"response_memory_bounded":True},
  "assertions":{"lb_stopped_unready_traffic":True,"alternate_complete_path_takeover":True,"accepted_mutation_redispatch_total":0}
}
with open(sys.argv[1], "w", encoding="utf-8") as handle: json.dump(artifact, handle)
PY
  validate_artifact "$fixture"
  python3 - "$fixture" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle: artifact=json.load(handle)
artifact["results"]["duplicate_side_effect_total"]=1
with open(sys.argv[1], "w", encoding="utf-8") as handle: json.dump(artifact, handle)
PY
  if validate_artifact "$fixture" >/dev/null 2>&1; then
    echo "self-test accepted a duplicate mutation side effect" >&2
    exit 1
  fi
  echo "production fault gate shell self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit 0
fi
if [[ "${1:-}" == "--validate" ]]; then
  [[ -n "${2:-}" ]] || { echo "--validate requires an artifact" >&2; exit 2; }
  validate_artifact "$2"
  exit 0
fi

scenario="${1:-all}"
traffic_rps="${2:-350}"
output_dir="${SAG_FAULT_GATE_OUTPUT_DIR:-artifacts/production-fault-gate}"
environment_name="${SAG_PERF_ENVIRONMENT:-}"
runner="${SAG_FAULT_SCENARIO_RUNNER:-}"

[[ "${SAG_FAULT_GATE_ACK:-}" == "AUTHORIZED_ISOLATED_ENVIRONMENT" ]] || { echo "set SAG_FAULT_GATE_ACK=AUTHORIZED_ISOLATED_ENVIRONMENT only for an approved destructive test environment" >&2; exit 2; }
[[ -n "$environment_name" ]] || { echo "SAG_PERF_ENVIRONMENT is required" >&2; exit 2; }
[[ -x "$runner" ]] || { echo "SAG_FAULT_SCENARIO_RUNNER must be an executable environment adapter" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }
command -v git >/dev/null || { echo "git is required" >&2; exit 2; }
git_sha="$(git rev-parse HEAD 2>/dev/null)"
[[ "$git_sha" =~ ^[0-9a-fA-F]{40}$ ]] || { echo "a recognized Git worktree and real commit SHA are required" >&2; exit 2; }

selected=()
if [[ "$scenario" == "all" ]]; then
  selected=("${all_scenarios[@]}")
else
  for known in "${all_scenarios[@]}"; do [[ "$scenario" == "$known" ]] && selected+=("$known"); done
  ((${#selected[@]} == 1)) || { echo "unknown scenario: $scenario" >&2; exit 2; }
fi

mkdir -p "$output_dir"
run_id="$(date -u +%Y%m%dT%H%M%SZ)"
artifacts=()
for fault in "${selected[@]}"; do
  artifact="$output_dir/$run_id-$fault.json"
  "$runner" --scenario "$fault" --traffic-rps "$traffic_rps" --artifact "$artifact" --environment "$environment_name" --git-sha "$git_sha"
  validate_artifact "$artifact"
  artifacts+=("$artifact")
done

python3 - "$output_dir/$run_id-result.json" "$environment_name" "$traffic_rps" "${artifacts[@]}" <<'PY'
import json, sys
path, environment, rps, *artifacts = sys.argv[1:]
result={"schema_version":"sag.production-fault-gate-result/v1","qualification":"passed","environment":environment,"traffic_rps":int(rps),"artifacts":artifacts}
with open(path,"w",encoding="utf-8") as handle: json.dump(result,handle,indent=2)
print(f"Production fault gate passed: {path}")
PY

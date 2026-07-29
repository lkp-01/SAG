#!/usr/bin/env bash
set -uo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repository_root"

if ! command -v docker >/dev/null 2>&1; then
  printf 'docker is required to parse the resolved Compose model\n' >&2
  exit 127
fi

if ! command -v python3 >/dev/null 2>&1; then
  printf 'python3 is required to validate the resolved Compose JSON\n' >&2
  exit 127
fi

invariant_temp_dir="$(mktemp -d)"
trap 'rm -rf -- "$invariant_temp_dir"' EXIT

overall_status=0

check_model() {
  local label="$1"
  shift
  local output_file="$invariant_temp_dir/${label}.json"
  local compose_args=(compose)
  local file
  for file in "$@"; do
    compose_args+=(-f "$file")
  done
  compose_args+=(config --format json --no-env-resolution)

  docker "${compose_args[@]}" >"$output_file"
  local compose_status=$?
  if [[ "$compose_status" -ne 0 ]]; then
    printf '[%s] Compose rendering failed with exit code %s\n' "$label" "$compose_status" >&2
    overall_status=1
    return
  fi

  if ! python3 - "$label" "$output_file" <<'PY'
import json
import re
import sys

label, path = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    model = json.load(handle)

violations = []
services = model.get("services", {})


def add(message):
    violations.append(f"[{label}] {message}")


def blank(value):
    return value is None or str(value).strip() == ""


for service_name, service in services.items():
    for port in service.get("ports") or []:
        target = str(port.get("target", ""))
        sensitive = (
            service_name.startswith("http-tunnel-bridge")
            or service_name in {"redis", "etcd"}
            or (service_name == "apisix" and target == "9180")
        )
        if not sensitive or blank(port.get("published")):
            continue
        host_ip = port.get("host_ip")
        if blank(host_ip) or host_ip in {"0.0.0.0", "::"}:
            add(
                f"service '{service_name}' publishes sensitive port {target} "
                f"as {port.get('published')} on all interfaces"
            )

    if service_name.startswith("http-tunnel-bridge"):
        environment = service.get("environment") or {}
        if str(environment.get("SAG_GRPC_MTLS_ENABLED", "")).lower() != "true":
            add(f"Bridge '{service_name}' does not set SAG_GRPC_MTLS_ENABLED=true")
        for name in (
            "SAG_GRPC_TLS_CA",
            "SAG_GRPC_TLS_CLIENT_CERT",
            "SAG_GRPC_TLS_CLIENT_KEY",
            "SAG_GRPC_TLS_SERVER_NAME",
        ):
            if blank(environment.get(name)):
                add(f"Bridge '{service_name}' has an empty or missing {name}")

    restart = service.get("restart")
    if blank(restart) or restart == "no":
        add(f"service '{service_name}' has no production restart policy")

    healthcheck = service.get("healthcheck")
    if not healthcheck or healthcheck.get("disable") is True:
        add(f"service '{service_name}' has no enabled healthcheck")

    limits = (((service.get("deploy") or {}).get("resources") or {}).get("limits") or {})
    if blank(limits.get("memory")) or blank(limits.get("cpus")):
        add(f"service '{service_name}' must set both CPU and memory resource limits")

    environment = service.get("environment") or {}
    known_values = {
        "postgres",
        "dev-jwt-secret",
        "your-admin-key",
        "Admin@123",
        "demo-readonly-token",
        "sag-agent-sync-dev-token",
        "dev-policy-internal-token",
        "sag-admin",
        "sag-local-secret",
        "changeme",
    }
    for name, raw_value in environment.items():
        if not re.search(
            r"(PASSWORD|SECRET|TOKEN|API_KEY|PRIVATE_KEY|CREDENTIAL|POSTGRES_DSN)",
            name,
            re.IGNORECASE,
        ):
            continue
        value = str(raw_value)
        if value in known_values or re.search(
            r"(REPLACE_WITH|postgres:postgres@|example-secret|test-secret)",
            value,
            re.IGNORECASE,
        ):
            add(f"service '{service_name}' resolves {name} to a known example credential")

redis = services.get("redis")
if redis:
    if not any(volume.get("target") == "/data" for volume in redis.get("volumes") or []):
        add("Redis has no persistent volume mounted at /data")
    command = " ".join(str(item) for item in redis.get("command") or [])
    if not re.search(r"(?:--appendonly\s+yes|appendonly\s+yes)", command, re.IGNORECASE):
        add("Redis does not enable AOF (appendonly yes)")
    redis_password = (redis.get("environment") or {}).get("REDIS_PASSWORD")
    if blank(redis_password) or not re.search(r"(?:--requirepass|requirepass)", command, re.IGNORECASE):
        add("Redis does not enforce a non-empty password")

if violations:
    print(f"Production invariant violations for {label} ({len(violations)}):", file=sys.stderr)
    for violation in violations:
        print(f" - {violation}", file=sys.stderr)
    raise SystemExit(1)
PY
  then
    overall_status=1
  fi
}

check_model edge \
  docker-compose.edge.yml \
  docker-compose.hscale-edge.yml \
  docker-compose.release.edge.yml

check_model intra \
  docker-compose.intra.yml \
  docker-compose.release.intra.yml

if [[ "$overall_status" -ne 0 ]]; then
  exit "$overall_status"
fi

printf 'Production Compose invariants passed for Edge and Intra configurations.\n'

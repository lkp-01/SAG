#!/usr/bin/env bash
set -euo pipefail

jobs="${1:-100}"
if ! [[ "$jobs" =~ ^[1-9][0-9]*$ ]] || (( jobs > 10000 )); then
  echo "usage: $0 [jobs: 1..10000]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
suffix="$$-$(date +%s)"
container="sag-queue-recovery-$suffix"
volume="sag-queue-recovery-$suffix"
password="queue-test-$suffix"
prefix="sag:recovery:$suffix"
stream="$prefix:queue"
dlq="$prefix:dlq"
group="bridge-workers"
declare -a entry_ids queue_ids idempotency_keys

cleanup() {
  if [[ "$(docker inspect --format '{{ index .Config.Labels "sag.queue-recovery" }}' "$container" 2>/dev/null || true)" == "true" ]]; then
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
  if [[ "$(docker volume inspect --format '{{ index .Labels "sag.queue-recovery" }}' "$volume" 2>/dev/null || true)" == "true" ]]; then
    docker volume rm "$volume" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

redis_cmd() {
  docker exec -e "REDISCLI_AUTH=$password" "$container" \
    redis-cli --no-auth-warning --raw -n 2 "$@"
}

wait_redis() {
  local attempt
  for attempt in $(seq 1 40); do
    if [[ "$(docker exec -e "REDISCLI_AUTH=$password" "$container" redis-cli --no-auth-warning ping 2>/dev/null || true)" == "PONG" ]]; then
      return 0
    fi
    sleep 0.25
  done
  echo "Redis did not become healthy" >&2
  return 1
}

stop_at_checkpoint() {
  local checkpoint="$1"
  # appendfsync everysec permits roughly one second of acknowledged writes to be lost.
  # Two fsync intervals make this deterministic; this does not claim RPO=0.
  sleep 2
  docker kill "$container" >/dev/null
  echo "checkpoint=$checkpoint redis=SIGKILL"
  docker start "$container" >/dev/null
  wait_redis
}

docker volume create --label sag.queue-recovery=true "$volume" >/dev/null
docker run -d --name "$container" \
  --label sag.queue-recovery=true \
  -e "REDIS_PASSWORD=$password" \
  -p 127.0.0.1::6379 \
  -v "$volume:/data" \
  redis:7-alpine sh -ec \
  'printf '\''appendonly yes\nappendfsync everysec\nrequirepass %s\n'\'' "$REDIS_PASSWORD" > /tmp/sag-redis.conf; exec redis-server /tmp/sag-redis.conf' \
  >/dev/null
wait_redis

for ((index = 0; index < jobs; index++)); do
  queue_id="job-$suffix-$index"
  idempotency_key="idem-$suffix-$index"
  entry_id="$(redis_cmd XADD "$stream" '*' queue_id "$queue_id" idempotency_key "$idempotency_key")"
  redis_cmd HSET "$prefix:job:$queue_id" status pending idempotency_key "$idempotency_key" >/dev/null
  entry_ids+=("$entry_id")
  queue_ids+=("$queue_id")
  idempotency_keys+=("$idempotency_key")
done
redis_cmd XGROUP CREATE "$stream" "$group" 0 >/dev/null

# The one-shot consumer exits with every entry pending: worker death at delivered.
redis_cmd XREADGROUP GROUP "$group" worker-delivered COUNT "$jobs" STREAMS "$stream" '>' >/dev/null
pending="$(redis_cmd XPENDING "$stream" "$group" | head -n 1)"
[[ "$pending" == "$jobs" ]] || { echo "expected PEL=$jobs, got $pending" >&2; exit 1; }
stop_at_checkpoint delivered

# Recover, dispatch each unique mutation once, and persist terminal state without ACK.
for ((index = 0; index < jobs; index++)); do
  redis_cmd XCLAIM "$stream" "$group" worker-recovered 1 "${entry_ids[$index]}" JUSTID >/dev/null
  claim="$(redis_cmd SET "$prefix:effect:${idempotency_keys[$index]}" 1 NX)"
  [[ "$claim" == "OK" ]] || { echo "duplicate mutation dispatch: ${idempotency_keys[$index]}" >&2; exit 1; }
  redis_cmd HSET "$prefix:job:${queue_ids[$index]}" status done result ok >/dev/null
done
stop_at_checkpoint result-persisted

# Terminal replay verifies durable state and stops immediately before ACK.
for ((index = 0; index < jobs; index++)); do
  status="$(redis_cmd HGET "$prefix:job:${queue_ids[$index]}" status)"
  effect="$(redis_cmd GET "$prefix:effect:${idempotency_keys[$index]}")"
  [[ "$status" == "done" && "$effect" == "1" ]] || { echo "terminal replay validation failed" >&2; exit 1; }
done
stop_at_checkpoint before-ack

# Only terminal entries are acknowledged; no mutation is dispatched on replay.
for ((index = 0; index < jobs; index++)); do
  status="$(redis_cmd HGET "$prefix:job:${queue_ids[$index]}" status)"
  [[ "$status" == "done" ]] || { echo "unknown terminal state: ${queue_ids[$index]}" >&2; exit 1; }
  redis_cmd XACK "$stream" "$group" "${entry_ids[$index]}" >/dev/null
  redis_cmd XDEL "$stream" "${entry_ids[$index]}" >/dev/null
done

pending="$(redis_cmd XPENDING "$stream" "$group" | head -n 1)"
remaining="$(redis_cmd XLEN "$stream")"
dlq_count="$(redis_cmd XLEN "$dlq")"
[[ "$pending" == "0" && "$remaining" == "0" ]] || {
  echo "recovery incomplete: PEL=$pending stream=$remaining" >&2
  exit 1
}

port_line="$(docker port "$container" 6379/tcp | head -n 1)"
host_port="${port_line##*:}"
export SAG_TEST_REDIS_URL="redis://:$password@127.0.0.1:$host_port/15"
cargo_args=()
if rustup toolchain list 2>/dev/null | grep -q 'stable-x86_64-pc-windows-gnu'; then
  cargo_args+=(+stable-x86_64-pc-windows-gnu)
fi
(cd "$repo_root" && cargo "${cargo_args[@]}" test -p http-tunnel-bridge --test queue_recovery redis_queue_kill_point_matrix -- --ignored --exact)

echo "queue recovery passed: completed=$jobs indeterminate=0 dlq=$dlq_count unknown=0 pel=0 duplicate_dispatch=0"

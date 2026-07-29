#!/usr/bin/env bash
# §5 缓存指标快查（policy / agent）
set -euo pipefail

fetch() {
  local url="$1" label="$2"
  echo "=== $label ($url) ==="
  if curl -fsS --max-time 3 "$url" 2>/dev/null | grep -E '^cache_(hit|miss)_total|^policy_eval_cache_hit_rate|^agent_degrade_redis' | head -20; then
    :
  else
    echo "(no matching lines or service unreachable)"
  fi
  echo ""
}

fetch "http://127.0.0.1:8081/metrics" "sag-policy"
fetch "http://127.0.0.1:9104/metrics" "stealth-tunnel-agent"
echo "See docs/ops/cache-read-runbook.md"

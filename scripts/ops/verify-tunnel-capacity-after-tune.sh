#!/usr/bin/env bash
# 重建后自检（Intra 宿主机执行；Redis 在 Edge，见 docs/ops/tunnel-capacity-bootstrap.md）
set -euo pipefail
echo "=== (Edge VM) Redis DB 2 — run manually if needed ==="
echo "    docker exec sag-redis redis-cli -n 2 PING"

echo "=== connector ulimit ==="
docker exec sag-connector sh -c 'ulimit -n' || true

echo "=== connector metrics (tunnel drops, if binary exposes) ==="
docker exec sag-connector sh -c 'command -v wget >/dev/null && wget -qO- http://127.0.0.1:9103/metrics | grep -E "connector_tunnel_drop|connector_tunnel_reconnect" | head -20' \
  || docker exec sag-connector sh -c 'command -v curl >/dev/null && curl -fsS http://127.0.0.1:9103/metrics | grep -E "connector_tunnel_drop|connector_tunnel_reconnect" | head -20' \
  || echo "(skip metrics: no wget/curl in container)"

echo "=== done ==="

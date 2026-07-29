#!/usr/bin/env bash
# B — 宿主机内核调优（Edge / Intra 各执行一次；需 root）
# 用法: sudo bash scripts/ops/apply-tunnel-host-sysctl.sh
# 干跑: sudo bash scripts/ops/apply-tunnel-host-sysctl.sh --dry-run
#
# 与 docs/ops/tunnel-capacity-bootstrap.md 配套；不适用的发行版请改数值或跳过。

set -euo pipefail
DRY_RUN=false
if [[ "${1:-}" == "--dry-run" ]]; then DRY_RUN=true; fi

CONF=$(mktemp)
trap 'rm -f "$CONF"' EXIT

cat >"$CONF" <<'EOF'
# SAG tunnel / high-connection hosts (Edge + Intra)
fs.file-max = 2097152
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 8192
net.ipv4.ip_local_port_range = 1024 65535
net.ipv4.tcp_fin_timeout = 30
net.ipv4.tcp_tw_reuse = 1
EOF

if $DRY_RUN; then
  echo "=== dry-run: would apply ==="
  cat "$CONF"
  echo "=== end ==="
  exit 0
fi

if [[ "$(id -u)" -ne 0 ]]; then
  echo "Run as root: sudo $0" >&2
  exit 1
fi

cp -a "$CONF" /etc/sysctl.d/99-sag-tunnel-capacity.conf
sysctl --system >/dev/null || sysctl -p /etc/sysctl.d/99-sag-tunnel-capacity.conf
echo "Applied /etc/sysctl.d/99-sag-tunnel-capacity.conf"
sysctl fs.file-max net.core.somaxconn net.ipv4.tcp_max_syn_backlog 2>/dev/null || true

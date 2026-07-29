#!/usr/bin/env bash
set -eu
set -o pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE="${ROOT}/proxy/core"
CFG="${ROOT}/proxy/zentinel-proxy/config/dataplane-verify.kdl"
cd "${CORE}"
export SAG_WINDOWS_HOST_IP="${SAG_WINDOWS_HOST_IP:-127.0.0.1}"
# Optionally rewrite bridge upstream for cross-host dev:
if [[ -n "${ZENTINEL_BRIDGE_TARGET:-}" ]]; then
  echo "override bridge target is not auto-patched; edit ${CFG} or export manually"
fi
exec cargo run -p zentinel-proxy --bin zentinel -- --config "${CFG}"

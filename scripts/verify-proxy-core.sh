#!/usr/bin/env bash
# Exit 0 if proxy/core is present for zentinel cargo; else print hint and exit 1.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -f "$ROOT/proxy/core/Cargo.toml" ]]; then
  echo "ok: $ROOT/proxy/core/Cargo.toml"
  exit 0
fi
echo "missing: $ROOT/proxy/core/Cargo.toml" >&2
echo "Fix: git submodule update --init --recursive   OR copy proxy/core from a machine that can clone GitHub." >&2
echo "See: DEPLOYMENT_README_FRESH_UBUNTU.md section 9.5" >&2
exit 1

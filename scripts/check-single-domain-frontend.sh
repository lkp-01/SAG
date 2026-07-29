#!/usr/bin/env bash
set -euo pipefail

BASE="${1:-http://127.0.0.1:3001}"
AUTH_BASE="${2:-http://127.0.0.1:8080}"
USER="${SAG_CHECK_USER:-admin}"
PASS="${SAG_CHECK_PASS:-Admin@123}"

echo "check single-domain frontend via ${BASE}"
curl -fsS "${BASE}/login" >/dev/null && echo "ok: /login"
curl -fsS "${BASE}/portal" >/dev/null && echo "ok: /portal"
curl -fsS "${BASE}/ops" >/dev/null && echo "ok: /ops"
curl -fsS "${BASE}/boss" >/dev/null && echo "ok: /boss"

token="$(curl -fsS -X POST "${AUTH_BASE}/api/v1/auth/login" -H "Content-Type: application/json" -d "{\"username\":\"${USER}\",\"password\":\"${PASS}\"}" | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
if [[ -z "${token}" ]]; then
  echo "fail: login token missing"
  exit 1
fi
echo "ok: auth token acquired"

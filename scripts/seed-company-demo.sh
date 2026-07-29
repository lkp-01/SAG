#!/usr/bin/env bash
set -euo pipefail

AUTH_BASE="${AUTH_BASE:-http://127.0.0.1:8080}"
ADMIN_BASE="${ADMIN_BASE:-http://127.0.0.1:8090}"
POLICY_BASE="${POLICY_BASE:-http://127.0.0.1:8081}"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-Admin@123}"
CONNECTOR_ENDPOINT="${CONNECTOR_ENDPOINT:-connector-local-001:stream}"
UPSTREAM_ENDPOINT="${UPSTREAM_ENDPOINT:-mock-workload:18080}"
UPSTREAM_SCHEME="${UPSTREAM_SCHEME:-http}"

echo "[1/4] login"
TOKEN="$(curl -sS -X POST "${AUTH_BASE}/api/v1/auth/login" \
  -H "Content-Type: application/json" \
  -d "{\"username\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASSWORD}\"}" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
[ -n "${TOKEN}" ] || { echo "login failed"; exit 1; }

upsert_user() {
  local username="$1"
  local roles_json="$2"
  local display="$3"
  local title="$4"
  local password="${5:-Admin@123}"
  curl -sS -X POST "${AUTH_BASE}/api/v1/users" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"username\":\"${username}\",\"password\":\"${password}\",\"roles\":${roles_json},\"display_name\":\"${display}\",\"title\":\"${title}\",\"enabled\":true}" >/dev/null
}

upsert_app() {
  local app_id="$1"
  local name="$2"
  local desc="$3"
  curl -sS -X POST "${ADMIN_BASE}/api/v1/apps" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"app_id\":\"${app_id}\",\"display_name\":\"${name}\",\"description\":\"${desc}\",\"enabled\":true}" >/dev/null
}

post_route() {
  local host="$1"
  local app_id="$2"
  curl -sS -X POST "${ADMIN_BASE}/api/v1/agent/routes" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"host\":\"${host}\",\"app_id\":\"${app_id}\",\"connector_endpoint\":\"${CONNECTOR_ENDPOINT}\",\"require_healthy_tunnel\":true}" >/dev/null || true
  curl -sS -X PUT "${ADMIN_BASE}/api/v1/agent/intranet-upstreams?app_id=${app_id}" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"upstream\":\"${UPSTREAM_ENDPOINT}\",\"scheme\":\"${UPSTREAM_SCHEME}\"}" >/dev/null
}

echo "[2/5] seed users/apps"
upsert_user "admin" '["admin"]' "平台管理员" "SAG 管理员"
upsert_user "boss" '["boss"]' "老板" "经营看板"
upsert_user "alice" '["tech"]' "研发负责人 Alice" "研发"
upsert_user "bob" '["ops"]' "运维负责人 Bob" "运维"
upsert_user "fiona" '["finance"]' "财务 Fiona" "财务"
upsert_user "vendor" '["vendor"]' "外包 Vendor" "外协"

upsert_app "app-dev" "研发门户" "代码与研发协作入口"
upsert_app "app-ci" "持续集成" "构建与发布流水线"
upsert_app "app-finance" "财务系统" "财务审批与报表"
upsert_app "app-oa" "OA办公" "办公审批与流程"
upsert_app "app-hr" "人事系统" "人事与考勤"
upsert_app "app-bi" "老板看板" "经营可视化"
upsert_app "app-vendor" "外包交付" "外协交付空间"

echo "[3/5] seed routes/upstreams"
post_route "dev.internal.com" "app-dev"
post_route "ci.internal.com" "app-ci"
post_route "finance.internal.com" "app-finance"
post_route "oa.internal.com" "app-oa"
post_route "hr.internal.com" "app-hr"
post_route "bi.internal.com" "app-bi"
post_route "vendor.internal.com" "app-vendor"

upsert_policy() {
  local id="$1"
  local body="$2"
  curl -sS -X DELETE "${POLICY_BASE}/api/v1/policies/${id}" \
    -H "Authorization: Bearer ${TOKEN}" >/dev/null || true
  curl -sS -X POST "${POLICY_BASE}/api/v1/policies" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "${body}" >/dev/null
}

echo "[4/5] seed policies"
upsert_policy "p-allow-admin-all" '{"id":"p-allow-admin-all","effect":"ALLOW","subjects":["role:admin"],"app_id":"*","path_prefix":"/","priority":6000}'
upsert_policy "p-allow-boss-all" '{"id":"p-allow-boss-all","effect":"ALLOW","subjects":["role:boss"],"app_id":"*","path_prefix":"/","priority":5000}'
upsert_policy "p-allow-tech-dev" '{"id":"p-allow-tech-dev","effect":"ALLOW","subjects":["role:tech"],"app_id":"app-dev","path_prefix":"/","priority":3000}'
upsert_policy "p-allow-tech-ci" '{"id":"p-allow-tech-ci","effect":"ALLOW","subjects":["role:tech"],"app_id":"app-ci","path_prefix":"/","priority":3000}'
upsert_policy "p-allow-tech-oa" '{"id":"p-allow-tech-oa","effect":"ALLOW","subjects":["role:tech"],"app_id":"app-oa","path_prefix":"/","priority":2500}'
upsert_policy "p-allow-finance-core" '{"id":"p-allow-finance-core","effect":"ALLOW","subjects":["role:finance"],"app_id":"app-finance","path_prefix":"/","priority":3200}'
upsert_policy "p-allow-finance-oa" '{"id":"p-allow-finance-oa","effect":"ALLOW","subjects":["role:finance"],"app_id":"app-oa","path_prefix":"/","priority":2500}'
upsert_policy "p-allow-vendor-only" '{"id":"p-allow-vendor-only","effect":"ALLOW","subjects":["role:vendor"],"app_id":"app-vendor","path_prefix":"/","priority":2800}'
upsert_policy "p-allow-sandbox-app001" '{"id":"p-allow-sandbox-app001","effect":"ALLOW","subjects":["role:tech","role:finance","role:vendor"],"app_id":"app-001","path_prefix":"/","priority":4500}'
upsert_policy "p-deny-vendor-finance" '{"id":"p-deny-vendor-finance","effect":"DENY","subjects":["role:vendor"],"app_id":"app-finance","path_prefix":"/","priority":9000}'
upsert_policy "p-deny-vendor-hr" '{"id":"p-deny-vendor-hr","effect":"DENY","subjects":["role:vendor"],"app_id":"app-hr","path_prefix":"/","priority":9000}'
upsert_policy "p-deny-tech-finance" '{"id":"p-deny-tech-finance","effect":"DENY","subjects":["role:tech"],"app_id":"app-finance","path_prefix":"/","priority":8500}'
upsert_policy "p-deny-tech-hr" '{"id":"p-deny-tech-hr","effect":"DENY","subjects":["role:tech"],"app_id":"app-hr","path_prefix":"/","priority":8500}'
upsert_policy "p-deny-tech-bi" '{"id":"p-deny-tech-bi","effect":"DENY","subjects":["role:tech"],"app_id":"app-bi","path_prefix":"/","priority":8500}'
upsert_policy "p-deny-tech-vendor" '{"id":"p-deny-tech-vendor","effect":"DENY","subjects":["role:tech"],"app_id":"app-vendor","path_prefix":"/","priority":8500}'

echo "[5/5] done"
echo "seed-company-demo.sh completed"

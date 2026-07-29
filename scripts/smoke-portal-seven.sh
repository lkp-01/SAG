#!/usr/bin/env bash
# 七条门户路径冒烟（与「用户门户」网关探测一致：app-001 + /dev/ /ci/ …）。
# 一键体检用 /api/test；本脚本用七条真实 path。每层：N=Zentinel、T=Bridge、S=APISIX、可选 P=admin-next。
#
# Edge 本机：
#   chmod +x scripts/smoke-portal-seven.sh && ./scripts/smoke-portal-seven.sh
#
# 指定 Edge + Intra + 测 3001（复现浏览器）：
#   EDGE_BASE_URL=http://172.16.9.107 INTRA_APISIX_DATA_BASE_URL=http://192.168.9.26:9080 \
#   ADMIN_NEXT_BASE_URL=http://172.16.9.107:3001 ./scripts/smoke-portal-seven.sh
#
# 可选：SMOKE_BEARER_TOKEN=...

set -uo pipefail

HDR_APP="${HDR_APP:-app-001}"
HDR_USER="${HDR_USER:-u-admin}"
HDR_ROLES="${HDR_ROLES:-admin}"
EDGE_BASE_URL="${EDGE_BASE_URL:-http://127.0.0.1}"
EDGE_BASE_URL="${EDGE_BASE_URL%/}"

if [[ "$EDGE_BASE_URL" =~ ^https?://([^/:]+) ]]; then
  EH="${BASH_REMATCH[1]}"
else
  EH="127.0.0.1"
fi

BRIDGE="${BRIDGE_URL:-http://${EH}:9000}"
BRIDGE="${BRIDGE%/}"
ZENT="${ZENTINEL_URL:-https://${EH}:10080}"
ZENT="${ZENT%/}"

APISIX="${APISIX_DATA_BASE_URL:-${INTRA_APISIX_DATA_BASE_URL:-http://127.0.0.1:9080}}"
APISIX="${APISIX%/}"

ADMIN_NEXT="${ADMIN_NEXT_BASE_URL:-}"
ADMIN_NEXT="${ADMIN_NEXT%/}"

hdr_args=( -H "x-sag-app-id: ${HDR_APP}" -H "x-sag-user-id: ${HDR_USER}" -H "x-sag-user-roles: ${HDR_ROLES}" )
if [[ -n "${SMOKE_BEARER_TOKEN:-}" ]]; then
  hdr_args+=( -H "Authorization: Bearer ${SMOKE_BEARER_TOKEN}" )
fi

tiles=(
  "/dev/|研发门户"
  "/ci/|持续集成"
  "/finance/|财务系统"
  "/oa/|OA办公"
  "/hr/|人事系统"
  "/bi/|老板看板"
  "/vendor/|外包交付"
)

fail=0
echo "smoke-portal-seven.sh — ZENTINEL=$ZENT BRIDGE=$BRIDGE APISIX=$APISIX${ADMIN_NEXT:+ ADMIN_NEXT=$ADMIN_NEXT}"
echo ""

for entry in "${tiles[@]}"; do
  path="${entry%%|*}"
  name="${entry#*|}"
  echo "=== path=$path ($name) ==="
  code=$(curl -sS -k --http1.1 --tlsv1.2 -o /tmp/sag-p7.body -w "%{http_code}" "${hdr_args[@]}" "${ZENT}${path}" || echo "000")
  body=$(head -c 200 /tmp/sag-p7.body 2>/dev/null | tr '\r\n' ' ')
  echo "  N Zentinel  HTTP $code  $body"
  [[ "$code" =~ ^2 ]] || { echo "  FAIL N"; ((fail++)) || true; }

  code=$(curl -sS -o /tmp/sag-p7.body -w "%{http_code}" "${hdr_args[@]}" "${BRIDGE}${path}" || echo "000")
  body=$(head -c 200 /tmp/sag-p7.body 2>/dev/null | tr '\r\n' ' ')
  echo "  T Bridge    HTTP $code  $body"
  [[ "$code" =~ ^2 ]] || { echo "  FAIL T"; ((fail++)) || true; }

  code=$(curl -sS -o /tmp/sag-p7.body -w "%{http_code}" "${hdr_args[@]}" "${APISIX}${path}" || echo "000")
  body=$(head -c 200 /tmp/sag-p7.body 2>/dev/null | tr '\r\n' ' ')
  echo "  S APISIX    HTTP $code  $body"
  [[ "$code" =~ ^2 ]] || { echo "  FAIL S"; ((fail++)) || true; }

  if [[ -n "$ADMIN_NEXT" ]]; then
    # 与门户 page.tsx 一致：/api-zentinel/dev/（有尾斜杠）；curl -L 处理可能的 308
    code=$(curl -sS -L -k -o /tmp/sag-p7.body -w "%{http_code}" "${hdr_args[@]}" "${ADMIN_NEXT}/api-zentinel${path}" || echo "000")
    body=$(head -c 200 /tmp/sag-p7.body 2>/dev/null | tr '\r\n' ' ')
    echo "  P admin-next HTTP $code  $body"
    [[ "$code" =~ ^2 ]] || { echo "  FAIL P"; ((fail++)) || true; }
  fi
  echo ""
done

if [[ "$fail" -eq 0 ]]; then
  echo "=== SUMMARY: all passed ==="
  [[ -z "$ADMIN_NEXT" ]] && echo "Tip: set ADMIN_NEXT_BASE_URL=http://<Edge>:3001 to test Next rewrites."
  exit 0
fi
echo "=== SUMMARY: $fail failure(s) ==="
exit 1

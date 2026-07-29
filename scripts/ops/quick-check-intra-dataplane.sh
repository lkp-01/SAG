#!/usr/bin/env bash
# Intra：绕过隧道，直连 mock 与 APISIX:9080（dataplane-optimization-plan §3 P0）。
# mock: python:slim 无 curl → Python urllib。
# apisix: 控制面下发的 route 带 vars `http_x_sag_app_id == app_id`，必须带 **x-sag-app-id**（默认 app-001），否则 404。
set -euo pipefail
MOCK_CONTAINER="${MOCK_CONTAINER:-sag-mock}"
APISIX_CONTAINER="${APISIX_CONTAINER:-sag-apisix}"
APISIX_TEST_HOST="${APISIX_TEST_HOST:-app.internal.com}"
SAG_APP_ID="${SAG_APP_ID:-app-001}"
TEST_PATH="${TEST_PATH:-/dev/}"
CURL_SIDECAR_IMAGE="${CURL_SIDECAR_IMAGE:-curlimages/curl:8.11.1}"

APISIX_URL="http://127.0.0.1:9080${TEST_PATH}"

echo "=== mock direct (${MOCK_CONTAINER}; python:slim has no curl, use urllib) ==="
docker exec -e "MOCK_TC_PATH=${TEST_PATH}" "${MOCK_CONTAINER}" python -c \
  "import os,urllib.request as u;p=os.environ['MOCK_TC_PATH'];r=u.urlopen('http://127.0.0.1:18080'+p,timeout=20);print('mock_http_code=',r.status)"

echo "=== APISIX data plane (${APISIX_CONTAINER}, Host: ${APISIX_TEST_HOST}, x-sag-app-id: ${SAG_APP_ID}) ==="
if docker exec "${APISIX_CONTAINER}" sh -lc "command -v curl >/dev/null 2>&1"; then
  docker exec "${APISIX_CONTAINER}" sh -lc \
    "curl -sS -o /dev/null -w 'apisix_http_code=%{http_code}\n' '${APISIX_URL}' -H 'Host: ${APISIX_TEST_HOST}' -H 'x-sag-app-id: ${SAG_APP_ID}'"
elif docker exec "${APISIX_CONTAINER}" sh -lc "command -v wget >/dev/null 2>&1"; then
  OUT=$(docker exec "${APISIX_CONTAINER}" sh -lc \
    "wget -qS --spider --header='Host: ${APISIX_TEST_HOST}' --header='x-sag-app-id: ${SAG_APP_ID}' '${APISIX_URL}' 2>&1" || true)
  CODE=$(echo "${OUT}" | grep -oE 'HTTP/[0-9.]+[[:space:]]+[0-9]+' | tail -n1 | awk '{print $2}')
  if [ -n "${CODE}" ]; then
    echo "apisix_http_code=${CODE} (wget)"
  else
    echo "WARN: wget ran but could not parse status. Last lines:" >&2
    echo "${OUT}" | tail -n5 >&2
    exit 3
  fi
else
  echo "No curl/wget in ${APISIX_CONTAINER}; using docker sidecar ${CURL_SIDECAR_IMAGE} (--network container:...) ..."
  docker run --rm --network "container:${APISIX_CONTAINER}" "${CURL_SIDECAR_IMAGE}" \
    -sS -o /dev/null -w 'apisix_http_code=%{http_code}\n' \
    -H "Host: ${APISIX_TEST_HOST}" \
    -H "x-sag-app-id: ${SAG_APP_ID}" \
    "${APISIX_URL}"
fi

echo "Done. If apisix is still 404: control-plane may not have reconciled routes to etcd; see DUAL_HOST_OPERATIONS.md. Else: docker logs ${APISIX_CONTAINER} --tail 80"

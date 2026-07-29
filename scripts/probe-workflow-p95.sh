#!/usr/bin/env bash
set -euo pipefail

PROM_BASE="${PROM_BASE:-http://127.0.0.1:9091}"

q() {
  local expr="$1"
  curl -sS -G "${PROM_BASE}/api/v1/query" --data-urlencode "query=${expr}"
}

echo "=== Prometheus health ==="
curl -sS "${PROM_BASE}/-/healthy" || true
echo

echo "=== Target up state ==="
for job in control-plane-admin sag-auth sag-policy http-tunnel-bridge stealth-tunnel-agent sag-connector zentinel-proxy apisix mock-workload; do
  echo "--- up{job=\"${job}\"} ---"
  q "up{job=\"${job}\"}"
  echo
done

echo "=== P95 capability probe per service ==="
echo "[control-plane-admin]"
q "avg(http_request_duration_seconds{job=\"control-plane-admin\",quantile=\"0.95\",path!=\"/metrics\"})"
q "histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=\"control-plane-admin\"}[5m])) by (le))"
echo

echo "[sag-auth]"
q "avg(http_request_duration_seconds{job=\"sag-auth\",quantile=\"0.95\",path!=\"/metrics\"})"
q "histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=\"sag-auth\"}[5m])) by (le))"
echo

echo "[sag-policy]"
q "avg(http_request_duration_seconds{job=\"sag-policy\",quantile=\"0.95\",path!=\"/metrics\"})"
q "histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=\"sag-policy\"}[5m])) by (le))"
echo

echo "[http-tunnel-bridge]"
q "avg(http_request_duration_seconds{job=\"http-tunnel-bridge\",quantile=\"0.95\",path!=\"/metrics\"})"
q "histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=\"http-tunnel-bridge\"}[5m])) by (le))"
echo

echo "[stealth-tunnel-agent]"
q "avg(agent_forward_duration_seconds{job=\"stealth-tunnel-agent\",quantile=\"0.95\"})"
q "histogram_quantile(0.95, sum(rate(agent_forward_duration_seconds_bucket{job=\"stealth-tunnel-agent\"}[5m])) by (le))"
echo

echo "[sag-connector]"
q "avg(connector_forward_duration_seconds{job=\"sag-connector\",quantile=\"0.95\"})"
q "histogram_quantile(0.95, sum(rate(connector_forward_duration_seconds_bucket{job=\"sag-connector\"}[5m])) by (le))"
echo

echo "[zentinel-proxy]"
q "avg(http_request_duration_seconds{job=\"zentinel-proxy\",quantile=\"0.95\",path!=\"/metrics\"})"
q "histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{job=\"zentinel-proxy\",path!=\"/metrics\"}[5m])) by (le))"
echo

echo "[apisix]"
q "avg(apisix_http_latency{job=\"apisix\",type=\"request\",quantile=\"0.95\"})"
q "avg(apisix_http_latency{job=\"apisix\",quantile=\"0.95\"})"
q "histogram_quantile(0.95, sum(rate(apisix_http_latency_bucket{job=\"apisix\",type=\"request\"}[5m])) by (le))"
q "histogram_quantile(0.95, sum(rate(apisix_http_latency_bucket{job=\"apisix\"}[5m])) by (le))"
q "histogram_quantile(0.95, sum(rate(apisix_upstream_latency_bucket{job=\"apisix\"}[5m])) by (le))"
echo

echo "[mock-workload]"
q "histogram_quantile(0.95, sum(rate(mock_request_duration_seconds_bucket{service=\"mock-workload\"}[5m])) by (le))"
echo

echo "=== done ==="

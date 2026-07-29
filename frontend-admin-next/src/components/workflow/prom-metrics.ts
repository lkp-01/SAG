import { pickScalar, promQuery } from "@/lib/prom";
import type { WorkflowServiceId } from "@/components/workflow/workflow-model";

export type ServiceLiveMetrics = {
  health: "up" | "down" | "unknown";
  qps: number | null;
  errRate: number | null;
  p95Ms: number | null;
};

type WorkflowMetricSpec = {
  id: WorkflowServiceId;
  healthJob: string;
  qpsQuery?: string;
  errRateQuery?: string;
  p95Query?: string;
};

function finiteOrNull(v: number) {
  return Number.isFinite(v) ? v : null;
}

function scalarOrNull(rows: Awaited<ReturnType<typeof promQuery>>) {
  return finiteOrNull(pickScalar(rows, NaN));
}

function secondsToMs(rows: Awaited<ReturnType<typeof promQuery>>) {
  const value = scalarOrNull(rows);
  return value == null ? null : value * 1000;
}

function withZero(expr: string) {
  return `(${expr}) or vector(0)`;
}

function mkHealth(up: number): ServiceLiveMetrics["health"] {
  if (up === 1) return "up";
  if (up === 0) return "down";
  return "unknown";
}

const specs: WorkflowMetricSpec[] = [
  {
    id: "control-plane-admin",
    healthJob: "control-plane-admin",
    qpsQuery: withZero('sum(rate(http_requests_total{service="control-plane-admin"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(http_requests_total{service="control-plane-admin",status=~"5..|408"}[5m])) / clamp_min(sum(rate(http_requests_total{service="control-plane-admin"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(http_request_duration_seconds{job="control-plane-admin",quantile="0.95",path!="/metrics"}) or avg(http_request_duration_seconds{service="control-plane-admin",quantile="0.95",path!="/metrics"}) or histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service="control-plane-admin"}[5m])) by (le))'
  },
  {
    id: "sag-auth",
    healthJob: "sag-auth",
    qpsQuery: withZero('sum(rate(http_requests_total{service="sag-auth"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(http_requests_total{service="sag-auth",status=~"5..|408"}[5m])) / clamp_min(sum(rate(http_requests_total{service="sag-auth"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(http_request_duration_seconds{job="sag-auth",quantile="0.95",path!="/metrics"}) or avg(http_request_duration_seconds{service="sag-auth",quantile="0.95",path!="/metrics"}) or histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service="sag-auth"}[5m])) by (le))'
  },
  {
    id: "sag-policy",
    healthJob: "sag-policy",
    qpsQuery: withZero('sum(rate(http_requests_total{service="sag-policy"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(http_requests_total{service="sag-policy",status=~"5..|408"}[5m])) / clamp_min(sum(rate(http_requests_total{service="sag-policy"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(http_request_duration_seconds{job="sag-policy",quantile="0.95",path!="/metrics"}) or avg(http_request_duration_seconds{service="sag-policy",quantile="0.95",path!="/metrics"}) or histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service="sag-policy"}[5m])) by (le))'
  },
  {
    id: "stealth-tunnel-agent",
    healthJob: "stealth-tunnel-agent",
    qpsQuery: withZero('sum(rate(agent_forward_total{job="stealth-tunnel-agent"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(agent_forward_total{job="stealth-tunnel-agent",result=~"connector_.*"}[5m])) / clamp_min(sum(rate(agent_forward_total{job="stealth-tunnel-agent"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(agent_forward_duration_seconds{job="stealth-tunnel-agent",quantile="0.95"}) or histogram_quantile(0.95, sum(rate(agent_forward_duration_seconds_bucket{job="stealth-tunnel-agent"}[5m])) by (le))'
  },
  {
    id: "http-tunnel-bridge",
    healthJob: "http-tunnel-bridge",
    qpsQuery: withZero('sum(rate(http_requests_total{service="http-tunnel-bridge"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(http_requests_total{service="http-tunnel-bridge",status=~"5..|408"}[5m])) / clamp_min(sum(rate(http_requests_total{service="http-tunnel-bridge"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(http_request_duration_seconds{job="http-tunnel-bridge",quantile="0.95",path!="/metrics"}) or avg(http_request_duration_seconds{service="http-tunnel-bridge",quantile="0.95",path!="/metrics"}) or histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service="http-tunnel-bridge"}[5m])) by (le))'
  },
  {
    id: "sag-connector",
    healthJob: "sag-connector",
    qpsQuery: withZero('sum(rate(connector_forward_total{job="sag-connector"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(connector_forward_total{job="sag-connector",status=~"5..|408"}[5m])) / clamp_min(sum(rate(connector_forward_total{job="sag-connector"}[5m])), 1e-9)'
    ),
    p95Query:
      'avg(connector_forward_duration_seconds{job="sag-connector",quantile="0.95"}) or histogram_quantile(0.95, sum(rate(connector_forward_duration_seconds_bucket{job="sag-connector"}[5m])) by (le))'
  },
  {
    id: "zentinel",
    healthJob: "zentinel-proxy",
    qpsQuery: withZero(
      'sum(rate(http_requests_total{job="zentinel-proxy",path!="/metrics"}[1m])) or sum(rate(zentinel_requests_total[1m])) or sum(rate(pingora_http_requests_total[1m]))'
    ),
    errRateQuery: withZero(
      '(sum(rate(http_requests_total{job="zentinel-proxy",path!="/metrics",status=~"5..|408"}[5m])) / clamp_min(sum(rate(http_requests_total{job="zentinel-proxy",path!="/metrics"}[5m])), 1e-9)) or (sum(rate(zentinel_requests_total{status=~"5..|408"}[5m])) / clamp_min(sum(rate(zentinel_requests_total[5m])), 1e-9)) or (sum(rate(pingora_http_responses_total{status=~"5..|408"}[5m])) / clamp_min(sum(rate(pingora_http_responses_total[5m])), 1e-9))'
    ),
    p95Query:
      'avg(http_request_duration_seconds{job="zentinel-proxy",path!="/metrics",quantile="0.95"}) or histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{job="zentinel-proxy",path!="/metrics"}[5m])) by (le)) or histogram_quantile(0.95, sum(rate(zentinel_request_duration_seconds_bucket[5m])) by (le)) or histogram_quantile(0.95, sum(rate(pingora_http_request_duration_seconds_bucket[5m])) by (le))'
  },
  {
    id: "apisix",
    healthJob: "apisix",
    qpsQuery: withZero('sum(rate(apisix_http_requests_total{job="apisix"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(apisix_http_status{job="apisix",code=~"5..|408"}[5m])) / clamp_min(sum(rate(apisix_http_requests_total{job="apisix"}[5m])), 1e-9)'
    ),
    p95Query:
      '(avg(apisix_http_latency{job="apisix",type="request",quantile="0.95"}) / 1000) or (avg(apisix_http_latency{job="apisix",quantile="0.95"}) / 1000) or (histogram_quantile(0.95, sum(rate(apisix_http_latency_bucket{job="apisix",type="request"}[5m])) by (le)) / 1000) or (histogram_quantile(0.95, sum(rate(apisix_http_latency_bucket{job="apisix"}[5m])) by (le)) / 1000) or (histogram_quantile(0.95, sum(rate(apisix_upstream_latency_bucket{job="apisix"}[5m])) by (le)) / 1000)'
  },
  {
    id: "mock-workload",
    healthJob: "mock-workload",
    qpsQuery: withZero('sum(rate(mock_requests_total{service="mock-workload"}[1m]))'),
    errRateQuery: "vector(0)",
    p95Query:
      'avg(mock_request_duration_seconds{service="mock-workload",quantile="0.95"}) or histogram_quantile(0.95, sum(rate(mock_request_duration_seconds_bucket{service="mock-workload"}[5m])) by (le))'
  },
  {
    id: "prometheus",
    healthJob: "prometheus",
    qpsQuery: withZero('sum(rate(prometheus_http_requests_total{job="prometheus",handler!="/metrics"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(prometheus_http_requests_total{job="prometheus",code=~"5..|408",handler!="/metrics"}[5m])) / clamp_min(sum(rate(prometheus_http_requests_total{job="prometheus",handler!="/metrics"}[5m])), 1e-9)'
    ),
    p95Query: withZero('max(scrape_duration_seconds{job="prometheus"})')
  },
  {
    id: "grafana",
    healthJob: "grafana",
    qpsQuery: withZero('sum(rate(grafana_http_request_total{job="grafana"}[1m]))'),
    errRateQuery: withZero(
      'sum(rate(grafana_http_request_total{job="grafana",status_code=~"5..|408"}[5m])) / clamp_min(sum(rate(grafana_http_request_total{job="grafana"}[5m])), 1e-9)'
    ),
    p95Query: withZero('max(scrape_duration_seconds{job="grafana"})')
  }
];

export async function fetchWorkflowMetrics(): Promise<Record<WorkflowServiceId, ServiceLiveMetrics>> {
  const rows = await Promise.all(
    specs.map(async (spec) => {
      const qUp = `max(up{job="${spec.healthJob}"})`;
      const [upRows, qpsRows, errRows, p95Rows] = await Promise.all([
        promQuery(qUp).catch(() => []),
        spec.qpsQuery ? promQuery(spec.qpsQuery).catch(() => []) : Promise.resolve([]),
        spec.errRateQuery ? promQuery(spec.errRateQuery).catch(() => []) : Promise.resolve([]),
        spec.p95Query ? promQuery(spec.p95Query).catch(() => []) : Promise.resolve([])
      ]);
      return [
        spec.id,
        {
          health: mkHealth(pickScalar(upRows, NaN)),
          qps: scalarOrNull(qpsRows),
          errRate: scalarOrNull(errRows),
          p95Ms: secondsToMs(p95Rows)
        }
      ] as const;
    })
  );

  return Object.fromEntries(rows) as Record<WorkflowServiceId, ServiceLiveMetrics>;
}


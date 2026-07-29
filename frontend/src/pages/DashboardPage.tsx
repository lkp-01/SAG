import { useEffect, useMemo, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { authApi, controlApi, policyApi } from "@/lib/api";

type PromVector = { metric: Record<string, string>; value: [number, string] };
type PromQueryResult = { status: string; data: { resultType: string; result: PromVector[] } };

async function promQuery(query: string): Promise<PromVector[]> {
  const u = `/api-prom/api/v1/query?query=${encodeURIComponent(query)}`;
  const res = await fetch(u);
  const text = await res.text();
  if (!res.ok) throw new Error(`Prometheus ${res.status}: ${text}`);
  const json = JSON.parse(text) as PromQueryResult;
  if (json.status !== "success") throw new Error(`Prometheus query failed: ${text}`);
  return json.data.result ?? [];
}

function pickScalar(result: PromVector[], defaultValue = 0): number {
  const v = result?.[0]?.value?.[1];
  const n = v ? Number(v) : NaN;
  return Number.isFinite(n) ? n : defaultValue;
}

export function DashboardPage() {
  const [routes, setRoutes] = useState<number>(0);
  const [policies, setPolicies] = useState<number>(0);
  const [users, setUsers] = useState<number>(0);

  const [mgmtQps, setMgmtQps] = useState<number | null>(null);
  const [mgmtErrRate, setMgmtErrRate] = useState<number | null>(null);
  const [mgmtP95, setMgmtP95] = useState<number | null>(null);

  const [zentQps, setZentQps] = useState<number | null>(null);
  const [zentErrRate, setZentErrRate] = useState<number | null>(null);
  const [zentP95, setZentP95] = useState<number | null>(null);
  const [promErr, setPromErr] = useState<string>("");

  const grafanaUrl = useMemo(() => import.meta.env.VITE_GRAFANA_URL ?? "http://127.0.0.1:3000", []);

  useEffect(() => {
    (async () => {
      try {
        const [r, p, u] = await Promise.all([controlApi.listRoutes(), policyApi.list(), authApi.listUsers()]);
        setRoutes(r.length);
        setPolicies(p.length);
        setUsers(u.length);
      } catch {
        // Ignore; other pages already show details.
      }
    })();
  }, []);

  useEffect(() => {
    let cancelled = false;
    setPromErr("");
    (async () => {
      try {
        // Management plane metrics (our Rust services instrumented via `metrics` crate).
        const qMgmtQps = 'sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy"}[1m]))';
        const qMgmtErr =
          'sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy",status=~"4..|5.."}[5m])) / clamp_min(sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy"}[5m])), 1e-9)';
        const qMgmtP95 =
          'histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=~"control-plane-admin|sag-auth|sag-policy"}[5m])) by (le))';

        // Zentinel metrics (proxy/core metrics stack; exposed on :9090/metrics).
        const qZentQps = 'sum(rate(zentinel_requests_total[1m]))';
        const qZentErr =
          'sum(rate(zentinel_requests_total{status=~"4..|5.."}[5m])) / clamp_min(sum(rate(zentinel_requests_total[5m])), 1e-9)';
        const qZentP95 =
          'histogram_quantile(0.95, sum(rate(zentinel_request_duration_seconds_bucket[5m])) by (le))';

        const [r1, r2, r3, r4, r5, r6] = await Promise.all([
          promQuery(qMgmtQps),
          promQuery(qMgmtErr),
          promQuery(qMgmtP95),
          promQuery(qZentQps),
          promQuery(qZentErr),
          promQuery(qZentP95),
        ]);
        if (cancelled) return;
        setMgmtQps(pickScalar(r1));
        setMgmtErrRate(pickScalar(r2));
        setMgmtP95(pickScalar(r3));
        setZentQps(pickScalar(r4));
        setZentErrRate(pickScalar(r5));
        setZentP95(pickScalar(r6));
      } catch (e) {
        if (cancelled) return;
        setPromErr(String(e));
        setMgmtQps(null);
        setMgmtErrRate(null);
        setMgmtP95(null);
        setZentQps(null);
        setZentErrRate(null);
        setZentP95(null);
      }
    })();
    const iv = setInterval(() => {
      // refresh
      if (!cancelled) {
        // trigger by re-running effect via state? simplest: call same closure via IIFE again
        (async () => {
          try {
            const qMgmtQps = 'sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy"}[1m]))';
            const qMgmtErr =
              'sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy",status=~"4..|5.."}[5m])) / clamp_min(sum(rate(http_requests_total{service=~"control-plane-admin|sag-auth|sag-policy"}[5m])), 1e-9)';
            const qMgmtP95 =
              'histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket{service=~"control-plane-admin|sag-auth|sag-policy"}[5m])) by (le))';
            const qZentQps = 'sum(rate(zentinel_requests_total[1m]))';
            const qZentErr =
              'sum(rate(zentinel_requests_total{status=~"4..|5.."}[5m])) / clamp_min(sum(rate(zentinel_requests_total[5m])), 1e-9)';
            const qZentP95 =
              'histogram_quantile(0.95, sum(rate(zentinel_request_duration_seconds_bucket[5m])) by (le))';

            const [r1, r2, r3, r4, r5, r6] = await Promise.all([
              promQuery(qMgmtQps),
              promQuery(qMgmtErr),
              promQuery(qMgmtP95),
              promQuery(qZentQps),
              promQuery(qZentErr),
              promQuery(qZentP95),
            ]);
            if (cancelled) return;
            setMgmtQps(pickScalar(r1));
            setMgmtErrRate(pickScalar(r2));
            setMgmtP95(pickScalar(r3));
            setZentQps(pickScalar(r4));
            setZentErrRate(pickScalar(r5));
            setZentP95(pickScalar(r6));
            setPromErr("");
          } catch (e) {
            if (cancelled) return;
            setPromErr(String(e));
            setMgmtQps(null);
            setMgmtErrRate(null);
            setMgmtP95(null);
            setZentQps(null);
            setZentErrRate(null);
            setZentP95(null);
          }
        })();
      }
    }, 5000);
    return () => {
      cancelled = true;
      clearInterval(iv);
    };
  }, []);

  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
      <Card>
        <CardHeader>
          <CardTitle>资源规模</CardTitle>
        </CardHeader>
        <CardContent className="space-y-1 text-sm text-slate-600">
          <div>用户：{users}</div>
          <div>路由：{routes}</div>
          <div>策略：{policies}</div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>实时流量（管理面）</CardTitle>
        </CardHeader>
        <CardContent className="space-y-1 text-sm text-slate-600">
          <div>QPS：{mgmtQps == null ? "—" : mgmtQps.toFixed(2)}</div>
          <div>错误率（4xx+5xx）：{mgmtErrRate == null ? "—" : `${(mgmtErrRate * 100).toFixed(2)}%`}</div>
          <div>P95 延迟：{mgmtP95 == null ? "—" : `${(mgmtP95 * 1000).toFixed(0)} ms`}</div>
          {promErr ? <div className="text-xs text-rose-600">Prometheus 未就绪：{promErr}</div> : null}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>实时流量（Zentinel）</CardTitle>
        </CardHeader>
        <CardContent className="space-y-1 text-sm text-slate-600">
          <div>QPS：{zentQps == null ? "—" : zentQps.toFixed(2)}</div>
          <div>错误率（4xx+5xx）：{zentErrRate == null ? "—" : `${(zentErrRate * 100).toFixed(2)}%`}</div>
          <div>P95 延迟：{zentP95 == null ? "—" : `${(zentP95 * 1000).toFixed(0)} ms`}</div>
          <div className="text-xs text-slate-500">来源：`zentinel_requests_total` / `zentinel_request_duration_seconds_*`</div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>观测入口</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2 text-sm text-slate-600">
          <div>Prometheus：默认映射到宿主机 `http://127.0.0.1:9091`</div>
          <a className="underline" href={grafanaUrl} target="_blank" rel="noreferrer">
            打开 Grafana
          </a>
        </CardContent>
      </Card>
    </div>
  );
}


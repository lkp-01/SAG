"use client";

import { useEffect, useMemo, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { TopBar } from "@/components/app-shell/TopBar";
import { fetchWorkflowMetrics } from "@/components/workflow/prom-metrics";
import type { WorkflowServiceId } from "@/components/workflow/workflow-model";

type MetricsMap = Record<
  WorkflowServiceId,
  { health: "up" | "down" | "unknown"; qps: number | null; errRate: number | null; p95Ms: number | null }
>;

export default function DashboardPage() {
  const [metrics, setMetrics] = useState<MetricsMap | null>(null);
  const [promErr, setPromErr] = useState<string>("");

  const refreshEveryMs = useMemo(() => Number(process.env.NEXT_PUBLIC_REFRESH_MS ?? "5000"), []);

  useEffect(() => {
    let cancelled = false;
    async function runOnce() {
      try {
        const data = await fetchWorkflowMetrics();
        if (cancelled) return;
        setMetrics(data);
        setPromErr("");
      } catch (e) {
        if (cancelled) return;
        setPromErr(String(e));
        setMetrics(null);
      }
    }

    runOnce();
    const iv = window.setInterval(runOnce, Number.isFinite(refreshEveryMs) ? refreshEveryMs : 5000);
    return () => {
      cancelled = true;
      window.clearInterval(iv);
    };
  }, [refreshEveryMs]);

  function fmt(v: number | null | undefined, digits = 2) {
    if (v == null || !Number.isFinite(v)) return "—";
    return v.toFixed(digits);
  }

  function avg(rows: Array<number | null | undefined>) {
    const xs = rows.filter((x): x is number => x != null && Number.isFinite(x));
    if (!xs.length) return null;
    return xs.reduce((a, b) => a + b, 0) / xs.length;
  }

  const mgmtQps = avg([
    metrics?.["control-plane-admin"]?.qps,
    metrics?.["sag-auth"]?.qps,
    metrics?.["sag-policy"]?.qps
  ]);
  const mgmtErrRate = avg([
    metrics?.["control-plane-admin"]?.errRate,
    metrics?.["sag-auth"]?.errRate,
    metrics?.["sag-policy"]?.errRate
  ]);
  const mgmtP95 = avg([
    metrics?.["control-plane-admin"]?.p95Ms,
    metrics?.["sag-auth"]?.p95Ms,
    metrics?.["sag-policy"]?.p95Ms
  ]);
  const zentQps = metrics?.zentinel.qps ?? null;
  const zentErrRate = metrics?.zentinel.errRate ?? null;
  const zentP95 = metrics?.zentinel.p95Ms ?? null;
  const apisixQps = metrics?.apisix.qps ?? null;
  const apisixErrRate = metrics?.apisix.errRate ?? null;
  const apisixP95 = metrics?.apisix.p95Ms ?? null;

  return (
    <>
      <TopBar title="概览" />
      <div className="flex-1 p-4 md:p-6">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
          <Card>
            <CardHeader>
              <CardTitle>实时流量（管理面）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>QPS：{fmt(mgmtQps)}</div>
              <div>错误率（4xx+5xx）：{mgmtErrRate == null ? "—" : `${(mgmtErrRate * 100).toFixed(2)}%`}</div>
              <div>P95 延迟：{mgmtP95 == null ? "—" : `${mgmtP95.toFixed(0)} ms`}</div>
              {promErr ? <div className="text-xs text-destructive">Prometheus 未就绪：{promErr}</div> : null}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>实时流量（Zentinel）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-1 text-sm text-muted-foreground">
              <div>QPS：{fmt(zentQps)}</div>
              <div>错误率（4xx+5xx）：{zentErrRate == null ? "—" : `${(zentErrRate * 100).toFixed(2)}%`}</div>
              <div>P95 延迟：{zentP95 == null ? "—" : `${zentP95.toFixed(0)} ms`}</div>
              <div className="text-xs text-muted-foreground">来源：`zentinel-*` 指标（job: `zentinel-proxy`）。</div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>实时流量（APISIX）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm text-muted-foreground">
              <div>QPS：{fmt(apisixQps)}</div>
              <div>错误率（4xx+5xx）：{apisixErrRate == null ? "—" : `${(apisixErrRate * 100).toFixed(2)}%`}</div>
              <div>P95 延迟：{apisixP95 == null ? "—" : `${apisixP95.toFixed(0)} ms`}</div>
              <div>刷新间隔：{Math.round(refreshEveryMs / 1000)}s（`NEXT_PUBLIC_REFRESH_MS` 可覆盖）。</div>
            </CardContent>
          </Card>
        </div>
      </div>
    </>
  );
}


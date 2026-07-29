"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { RoleGate } from "@/components/auth/RoleGate";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { fetchWorkflowMetrics } from "@/components/workflow/prom-metrics";
import type { WorkflowServiceId } from "@/components/workflow/workflow-model";

type MetricsMap = Record<
  WorkflowServiceId,
  { health: "up" | "down" | "unknown"; qps: number | null; errRate: number | null; p95Ms: number | null }
>;

export default function BossPage() {
  const [metrics, setMetrics] = useState<MetricsMap | null>(null);
  const [err, setErr] = useState("");
  const refreshEveryMs = useMemo(() => Number(process.env.NEXT_PUBLIC_REFRESH_MS ?? "5000"), []);

  useEffect(() => {
    let cancelled = false;
    async function runOnce() {
      try {
        const data = await fetchWorkflowMetrics();
        if (cancelled) return;
        setMetrics(data);
        setErr("");
      } catch (e) {
        if (cancelled) return;
        setErr(String(e));
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

  function avg(rows: Array<number | null | undefined>) {
    const xs = rows.filter((x): x is number => x != null && Number.isFinite(x));
    if (!xs.length) return null;
    return xs.reduce((a, b) => a + b, 0) / xs.length;
  }

  const qps = avg([
    metrics?.zentinel?.qps,
    metrics?.apisix?.qps,
    metrics?.["http-tunnel-bridge"]?.qps,
    metrics?.["sag-connector"]?.qps
  ]);
  const errRate = avg([
    metrics?.zentinel?.errRate,
    metrics?.apisix?.errRate,
    metrics?.["http-tunnel-bridge"]?.errRate,
    metrics?.["sag-connector"]?.errRate
  ]);
  const p95 = avg([
    metrics?.zentinel?.p95Ms,
    metrics?.apisix?.p95Ms,
    metrics?.["http-tunnel-bridge"]?.p95Ms,
    metrics?.["sag-connector"]?.p95Ms
  ]);

  return (
    <RoleGate need="boss">
      <TopBar title="老板视图" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
          <Card>
            <CardHeader>
              <CardTitle>总吞吐</CardTitle>
            </CardHeader>
            <CardContent>{qps == null ? "—" : `${qps.toFixed(2)} qps`}</CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>错误率</CardTitle>
            </CardHeader>
            <CardContent>{errRate == null ? "—" : `${(errRate * 100).toFixed(2)}%`}</CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>P95 延迟</CardTitle>
            </CardHeader>
            <CardContent>{p95 == null ? "—" : `${p95.toFixed(0)} ms`}</CardContent>
          </Card>
        </div>
        <Card>
          <CardHeader>
            <CardTitle>决策入口</CardTitle>
            <CardDescription>{err || `指标来自 Prometheus（刷新间隔 ${Math.round(refreshEveryMs / 1000)}s）。`}</CardDescription>
          </CardHeader>
          <CardContent className="flex gap-2">
            <Button asChild>
              <Link href="/portal">查看用户体验</Link>
            </Button>
            <Button asChild variant="outline">
              <Link href="/ops">进入运维台</Link>
            </Button>
          </CardContent>
        </Card>
      </div>
    </RoleGate>
  );
}

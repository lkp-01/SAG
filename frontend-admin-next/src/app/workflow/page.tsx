"use client";

import { useEffect, useMemo, useState } from "react";
import { ReactFlow, Background, Controls, type Edge, type Node } from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { TopBar } from "@/components/app-shell/TopBar";
import { WorkflowNode, type WorkflowNodeData } from "@/components/workflow/WorkflowNode";
import { fetchWorkflowMetrics } from "@/components/workflow/prom-metrics";
import { workflowServices, type WorkflowServiceId } from "@/components/workflow/workflow-model";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { controlApi, dataPlaneProbe } from "@/lib/api";
import type { FaultEvent } from "@/lib/types";

function debugLog(location: string, message: string, data: Record<string, unknown>, hypothesisId: string, runId = "initial") {
  // #region agent log
  fetch("http://127.0.0.1:7701/ingest/1ccb5b12-5073-4437-a0e2-a9913a1fb79d", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "ac5396" },
    body: JSON.stringify({ sessionId: "ac5396", runId, hypothesisId, location, message, data, timestamp: Date.now() })
  }).catch(() => {});
  // #endregion
}

export default function WorkflowPage() {
  const refreshEveryMs = useMemo(() => Number(process.env.NEXT_PUBLIC_REFRESH_MS ?? "5000"), []);
  const probePath = useMemo(() => {
    const raw = process.env.NEXT_PUBLIC_WORKFLOW_PROBE_PATH ?? process.env.NEXT_PUBLIC_PATH_REQ ?? "/dev/";
    if (!raw.startsWith("/")) return `/${raw}`;
    return raw;
  }, []);
  const [err, setErr] = useState<string>("");
  const [metrics, setMetrics] = useState<Record<WorkflowServiceId, { health: "up" | "down" | "unknown"; qps: number | null; errRate: number | null; p95Ms: number | null }> | null>(null);
  const [faultEvents, setFaultEvents] = useState<FaultEvent[]>([]);
  const [probeAppId, setProbeAppId] = useState<string>("app-001");

  useEffect(() => {
    let cancelled = false;
    async function runOnce() {
      const t0 = performance.now();
      try {
        let selectedAppId = probeAppId;
        try {
          const routes = await controlApi.listRoutes();
          if (routes.length > 0 && routes[0].app_id) {
            selectedAppId = routes[0].app_id;
            if (selectedAppId !== probeAppId) setProbeAppId(selectedAppId);
          }
        } catch {
          // Keep existing probe app when route lookup is unavailable.
        }
        const [m, northProbe, events] = await Promise.all([
          fetchWorkflowMetrics(),
          dataPlaneProbe("/api-zentinel", probePath, selectedAppId).catch(() => null),
          controlApi.listFaultEvents({ from_ts_ms: Date.now() - 10 * 60 * 1000, limit: 100 }).catch(() => [])
        ]);
        // #region agent log
        debugLog("workflow/page.tsx:runOnce", "workflow metrics fetched", {
          ms: Math.round(performance.now() - t0),
          northProbeStatus: northProbe?.status ?? null,
          probeAppId: selectedAppId
        }, "H10");
        // #endregion
        const routeMissing = northProbe?.status === 502 && (northProbe.body ?? "").includes("no tunnel route for app_id");
        if (northProbe && northProbe.status >= 500 && !routeMissing) {
          m.zentinel = { ...m.zentinel, health: "down" };
        }
        if (cancelled) return;
        setMetrics(m);
        setFaultEvents(events);
        setErr("");
      } catch (e) {
        // #region agent log
        debugLog("workflow/page.tsx:runOnce", "workflow fetch failed", {
          ms: Math.round(performance.now() - t0),
          error: String(e)
        }, "H10");
        // #endregion
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
  }, [refreshEveryMs, probePath]);

  const nodeTypes = useMemo(() => ({ workflow: WorkflowNode }), []);
  const threshold = { p95: 800, criticalRate: 0.01, warnRate: 0.005 };
  const alertItems = useMemo(() => {
    if (!metrics) return [];
    return (Object.entries(metrics) as Array<[WorkflowServiceId, { health: "up" | "down" | "unknown"; qps: number | null; errRate: number | null; p95Ms: number | null }]>)
      .flatMap(([svc, m]) => {
        const out: Array<{ service: string; level: "warn" | "critical"; reason: string }> = [];
        if ((m.p95Ms ?? 0) > threshold.p95 * 1.8) out.push({ service: svc, level: "critical", reason: `P95 ${(m.p95Ms ?? 0).toFixed(0)}ms > 基线1.8x` });
        else if ((m.p95Ms ?? 0) > threshold.p95) out.push({ service: svc, level: "warn", reason: `P95 ${(m.p95Ms ?? 0).toFixed(0)}ms > 基线` });
        if ((m.errRate ?? 0) > threshold.criticalRate) out.push({ service: svc, level: "critical", reason: `5xx/超时率 ${((m.errRate ?? 0) * 100).toFixed(2)}% > 1%` });
        else if ((m.errRate ?? 0) > threshold.warnRate) out.push({ service: svc, level: "warn", reason: `5xx/超时率 ${((m.errRate ?? 0) * 100).toFixed(2)}% > 0.5%` });
        if (m.health === "down") out.push({ service: svc, level: "critical", reason: "健康状态 down" });
        return out;
      });
  }, [metrics]);

  const nodes = useMemo(() => {
    const lookup = new Map(workflowServices.map((s) => [s.id, s]));
    const data = (id: WorkflowServiceId): WorkflowNodeData => {
      const s = lookup.get(id)!;
      const m = metrics?.[id];
      return {
        title: s.title,
        subtitle: s.subtitle,
        health: m?.health ?? "unknown",
        qps: m?.qps ?? null,
        errRate: m?.errRate ?? null,
        p95Ms: m?.p95Ms ?? null
      };
    };

    const base: Node[] = [
      { id: "control-plane-admin", type: "workflow", position: { x: 0, y: 0 }, data: data("control-plane-admin") },
      { id: "sag-auth", type: "workflow", position: { x: 0, y: 140 }, data: data("sag-auth") },
      { id: "sag-policy", type: "workflow", position: { x: 0, y: 280 }, data: data("sag-policy") },

      { id: "stealth-tunnel-agent", type: "workflow", position: { x: 300, y: 140 }, data: data("stealth-tunnel-agent") },
      // Move bridge up slightly to reduce visual overlap with crossing tunnel links.
      { id: "http-tunnel-bridge", type: "workflow", position: { x: 620, y: 80 }, data: data("http-tunnel-bridge") },
      { id: "zentinel", type: "workflow", position: { x: 940, y: 0 }, data: data("zentinel") },
      { id: "sag-connector", type: "workflow", position: { x: 940, y: 300 }, data: data("sag-connector") },

      { id: "apisix", type: "workflow", position: { x: 1200, y: 280 }, data: data("apisix") },
      { id: "mock-workload", type: "workflow", position: { x: 1500, y: 280 }, data: data("mock-workload") },

      { id: "prometheus", type: "workflow", position: { x: 940, y: 540 }, data: data("prometheus") },
      { id: "grafana", type: "workflow", position: { x: 1240, y: 540 }, data: data("grafana") }
    ];
    return base;
  }, [metrics]);

  const edges = useMemo<Edge[]>(() => {
    return [
      { id: "admin->agent", source: "control-plane-admin", target: "stealth-tunnel-agent", animated: true, label: "routes sync" },
      { id: "policy->agent", source: "sag-policy", target: "stealth-tunnel-agent", animated: true, label: "PDP evaluate" },
      { id: "bridge->agent", source: "http-tunnel-bridge", target: "stealth-tunnel-agent", animated: true, label: "gRPC forward" },
      { id: "zentinel->bridge", source: "zentinel", target: "http-tunnel-bridge", animated: true, label: "upstream" },
      { id: "agent->connector", source: "stealth-tunnel-agent", target: "sag-connector", animated: true, label: "mTLS tunnel" },
      { id: "connector->apisix", source: "sag-connector", target: "apisix", animated: false },
      { id: "apisix->mock", source: "apisix", target: "mock-workload", animated: false },
      { id: "prom->graf", source: "prometheus", target: "grafana", animated: false }
    ];
  }, []);

  return (
    <>
      <TopBar title="工作流健康" />
      <div className="flex-1 p-4 md:p-6">
        <Card className="mb-4">
          <CardHeader>
            <CardTitle>工作流视图（实时刷新）</CardTitle>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            <div>数据来源：Prometheus（`up` + `http_requests_total` + `zentinel_requests_total` 等）。</div>
            <div>健康判定补充：若北向探测 `N1` 返回 `5xx`，`zentinel` 会强制标记为异常。</div>
            <div>刷新间隔：{Math.round(refreshEveryMs / 1000)}s。</div>
            <div>北向探测 path：{probePath}</div>
            <div>北向探测 app_id：{probeAppId}</div>
            {err ? <div className="mt-2 text-xs text-destructive">Prometheus 未就绪：{err}</div> : null}
          </CardContent>
        </Card>
        <Card className="mb-4">
          <CardHeader>
            <CardTitle>秒级故障高亮</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            {alertItems.length === 0 ? <div className="text-muted-foreground">当前无异常（按阈值自动判定）</div> : alertItems.slice(0, 8).map((a, idx) => (
              <div key={`${a.service}-${idx}`} className={a.level === "critical" ? "text-destructive" : "text-amber-600"}>
                [{a.level.toUpperCase()}] {a.service}: {a.reason}
              </div>
            ))}
            <div className="text-xs text-muted-foreground">
              最近10分钟 fault_events：{faultEvents.length} 条（可在审计中心按 service/path/trace 追溯）。
            </div>
          </CardContent>
        </Card>

        <div className="h-[640px] rounded-lg border bg-background">
          {!metrics ? (
            <div className="space-y-3 p-4">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-5/6" />
              <Skeleton className="h-12 w-4/6" />
            </div>
          ) : (
            <ReactFlow nodes={nodes} edges={edges} nodeTypes={nodeTypes} fitView>
              <Background />
              <Controls />
            </ReactFlow>
          )}
        </div>
      </div>
    </>
  );
}


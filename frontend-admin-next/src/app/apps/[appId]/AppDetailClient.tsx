"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import ReactECharts from "echarts-for-react";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { controlApi } from "@/lib/api";
import type { AppMetricsSeries, RouteRow } from "@/lib/types";
import { promQuery } from "@/lib/prom";

type TreeNode = {
  name: string;
  value?: string;
  children?: TreeNode[];
};

function buildTree(appId: string, routes: RouteRow[], hotRoutes: Array<{ route: string; qps: number }>): TreeNode {
  const root: TreeNode = { name: appId, children: [] };
  const hostNodes = routes.map((r) => ({
    name: r.host,
    value: r.connector_endpoint,
    children: [
      { name: `connector: ${r.connector_endpoint}` },
      { name: `require_healthy: ${r.require_healthy_tunnel ? "true" : "false"}` }
    ]
  }));

  const hot: TreeNode = {
    name: "Zentinel 热点路由（全局）",
    children: hotRoutes.map((x) => ({ name: `${x.route}  (${x.qps.toFixed(2)} qps)` }))
  };

  root.children = [{ name: "隧道路由（control-plane-admin）", children: hostNodes }, hot];
  return root;
}

export function AppDetailClient({ appId }: { appId: string }) {
  const [routes, setRoutes] = useState<RouteRow[]>([]);
  const [hotRoutes, setHotRoutes] = useState<Array<{ route: string; qps: number }>>([]);
  const [series, setSeries] = useState<AppMetricsSeries | null>(null);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState("");

  useEffect(() => {
    (async () => {
      try {
        const [all, m] = await Promise.all([controlApi.listRoutes(), controlApi.getAppsMetrics(appId, 120)]);
        setRoutes(all.filter((r) => r.app_id === appId));
        setSeries(m.series[0] ?? null);
        setErr("");
      } catch (e) {
        setErr(String(e));
        setRoutes([]);
        setSeries(null);
      } finally {
        setLoading(false);
      }
    })();
  }, [appId]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const q = 'topk(10, sum by (route) (rate(zentinel_requests_total{job="zentinel-proxy"}[5m])))';
        const r = await promQuery(q);
        if (cancelled) return;
        const list = r?.map((v) => ({ route: v.metric?.route ?? "(unknown)", qps: Number(v.value?.[1] ?? "0") })) ?? [];
        setHotRoutes(list.filter((x) => Number.isFinite(x.qps)));
      } catch {
        if (cancelled) return;
        setHotRoutes([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const tree = useMemo(() => buildTree(appId, routes, hotRoutes), [appId, routes, hotRoutes]);
  const option = useMemo(
    () => ({
      tooltip: { trigger: "item", triggerOn: "mousemove" },
      series: [
        {
          type: "tree",
          data: [tree],
          top: "2%",
          left: "8%",
          bottom: "2%",
          right: "20%",
          symbolSize: 10,
          label: { position: "left", verticalAlign: "middle", align: "right", fontSize: 12 },
          leaves: { label: { position: "right", verticalAlign: "middle", align: "left" } },
          expandAndCollapse: true,
          initialTreeDepth: 2,
          animationDuration: 200,
          animationDurationUpdate: 300
        }
      ]
    }),
    [tree]
  );

  const trendOption = useMemo(() => {
    const points = series?.points ?? [];
    return {
      tooltip: { trigger: "axis" },
      xAxis: { type: "category", data: points.map((p) => new Date(p.ts_minute * 1000).toLocaleTimeString()) },
      yAxis: { type: "value" },
      series: [
        { name: "QPS", type: "line", smooth: true, data: points.map((p) => Number(p.qps_avg.toFixed(3))) },
        { name: "请求量", type: "bar", data: points.map((p) => p.request_count) }
      ]
    };
  }, [series]);

  return (
    <>
      <TopBar title={`应用详情：${appId}`} />
      <div className="flex-1 p-4 md:p-6">
        {err ? (
          <Card className="mb-4 border-destructive/50">
            <CardHeader>
              <CardTitle>需要登录或后端未就绪</CardTitle>
            </CardHeader>
            <CardContent className="text-sm text-muted-foreground">
              <div className="mb-2">错误：{err}</div>
              <Link className="underline" href="/login">
                去登录（获取 JWT）
              </Link>
            </CardContent>
          </Card>
        ) : null}

        <div className="mb-4 grid grid-cols-2 gap-3 md:grid-cols-4">
          {loading
            ? Array.from({ length: 4 }).map((_, i) => <Skeleton key={i} className="h-20 w-full" />)
            : [
                ["请求次数", series?.latest?.request_count ?? 0],
                ["PV / UV", `${series?.latest?.pv_count ?? 0} / ${series?.latest?.uv_count ?? 0}`],
                ["独立IP", series?.latest?.unique_ip_count ?? 0],
                ["实时QPS", series?.latest?.qps_avg?.toFixed(2) ?? "0.00"]
              ].map(([k, v]) => (
                <Card key={String(k)}>
                  <CardHeader className="pb-2">
                    <CardTitle className="text-xs">{k}</CardTitle>
                  </CardHeader>
                  <CardContent className="text-lg font-semibold">{v}</CardContent>
                </Card>
              ))}
        </div>

        <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
          <Card className="lg:col-span-2">
            <CardHeader>
              <CardTitle>API/路由树</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="h-[560px]">
                <ReactECharts option={option} style={{ height: "100%", width: "100%" }} />
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>说明</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm text-muted-foreground">
              <div>
                当前树的“隧道路由”来自 `control-plane-admin` 的 routes 列表；“热点路由”来自 Zentinel 指标
                `zentinel_requests_total`（暂为全局维度）。
              </div>
            </CardContent>
          </Card>
        </div>
        <Card className="mt-4">
          <CardHeader>
            <CardTitle>近时段趋势</CardTitle>
          </CardHeader>
          <CardContent>
            {loading ? (
              <Skeleton className="h-[300px] w-full" />
            ) : (
              <div className="h-[300px]">
                <ReactECharts option={trendOption} style={{ height: "100%", width: "100%" }} />
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </>
  );
}

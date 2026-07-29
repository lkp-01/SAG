"use client";

import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { usePathname } from "next/navigation";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { controlApi } from "@/lib/api";
import { Input } from "@/components/ui/input";
import type { AppMetricsPoint, AppRecord, AppTreeNode, RouteRow } from "@/lib/types";

const APPS_PAGE_CACHE_KEY = "sag.ops.apps.cache.v1";

function debugLog(location: string, message: string, data: Record<string, unknown>, hypothesisId: string, runId = "initial") {
  // #region agent log
  fetch("http://127.0.0.1:7701/ingest/1ccb5b12-5073-4437-a0e2-a9913a1fb79d", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "ac5396" },
    body: JSON.stringify({
      sessionId: "ac5396",
      runId,
      hypothesisId,
      location,
      message,
      data,
      timestamp: Date.now()
    })
  }).catch(() => {});
  // #endregion
}

export default function AppsPage() {
  const pathname = usePathname();
  const [routes, setRoutes] = useState<RouteRow[]>([]);
  const [metrics, setMetrics] = useState<Record<string, AppMetricsPoint | null>>({});
  const [appsMeta, setAppsMeta] = useState<AppRecord[]>([]);
  const [appForm, setAppForm] = useState<AppRecord>({
    app_id: "",
    display_name: "",
    description: "",
    enabled: true
  });
  const [loading, setLoading] = useState(true);
  const [metricsLoading, setMetricsLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [err, setErr] = useState("");

  function writeCache(next: { routes: RouteRow[]; metrics: Record<string, AppMetricsPoint | null>; appsMeta: AppRecord[] }) {
    try {
      localStorage.setItem(APPS_PAGE_CACHE_KEY, JSON.stringify(next));
    } catch {}
  }

  function readCache(): { routes: RouteRow[]; metrics: Record<string, AppMetricsPoint | null>; appsMeta: AppRecord[] } | null {
    try {
      const raw = localStorage.getItem(APPS_PAGE_CACHE_KEY);
      if (!raw) return null;
      return JSON.parse(raw) as { routes: RouteRow[]; metrics: Record<string, AppMetricsPoint | null>; appsMeta: AppRecord[] };
    } catch {
      return null;
    }
  }

  async function reload(initial = false) {
    const t0 = performance.now();
    try {
      if (initial) {
        if (routes.length === 0) setLoading(true);
        if (Object.keys(metrics).length === 0) setMetricsLoading(true);
      } else {
        setRefreshing(true);
      }
      const treeP = (async (): Promise<{ metricsMap: Record<string, AppMetricsPoint | null>; routesList: RouteRow[] }> => {
        const s = performance.now();
        const out = await controlApi.listAppsTree(false);
        debugLog("apps/page.tsx:reload", "listAppsTree done", { ms: Math.round(performance.now() - s), apps: out.length, initial }, "H6");
        const allRoutes = (out as AppTreeNode[]).flatMap((item) => item.routes);
        const map: Record<string, AppMetricsPoint | null> = {};
        (out as AppTreeNode[]).forEach((item) => {
          map[item.app_id] = item.latest ?? null;
        });
        setRoutes(allRoutes);
        setMetrics(map);
        setLoading(false);
        setMetricsLoading(false);
        return { metricsMap: map, routesList: allRoutes };
      })();
      const appsP = (async (): Promise<AppRecord[]> => {
        const s = performance.now();
        const out = await controlApi.listApps().catch(() => []);
        debugLog("apps/page.tsx:reload", "listApps done", { ms: Math.round(performance.now() - s), count: out.length, initial }, "H1");
        setAppsMeta(out);
        return out;
      })();
      const { metricsMap, routesList } = await treeP;
      const appsMetaNext = await appsP;
      writeCache({ routes: routesList, metrics: metricsMap, appsMeta: appsMetaNext });
      // #region agent log
      void (async () => {
        const s = performance.now();
        const out = await controlApi.listAppsTree(true).catch(() => null);
        if (!out) return;
        const map: Record<string, AppMetricsPoint | null> = {};
        (out as AppTreeNode[]).forEach((item) => {
          map[item.app_id] = item.latest ?? null;
        });
        setMetrics(map);
        writeCache({ routes: routesList, metrics: map, appsMeta: appsMetaNext });
        debugLog("apps/page.tsx:reload", "listAppsTree latest backfill", { ms: Math.round(performance.now() - s), apps: out.length }, "H13");
      })();
      // #endregion
      setErr("");
    } catch (e) {
      setErr(String(e));
      setRoutes([]);
      setMetrics({});
      setAppsMeta([]);
      setMetricsLoading(false);
      setLoading(false);
    } finally {
      debugLog("apps/page.tsx:reload", "reload finished", { ms: Math.round(performance.now() - t0), initial }, "H12");
      if (initial) {
        setLoading((prev) => (routes.length > 0 ? false : prev));
      } else {
        setRefreshing(false);
      }
    }
  }

  async function saveApp() {
    try {
      await controlApi.upsertApp(appForm);
      setAppForm({ app_id: "", display_name: "", description: "", enabled: true });
      await reload(false);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function removeApp(appId: string) {
    try {
      await controlApi.deleteApp(appId);
      await reload(false);
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => {
    const cached = readCache();
    if (cached) {
      setRoutes(cached.routes);
      setMetrics(cached.metrics);
      setAppsMeta(cached.appsMeta);
      setLoading(false);
      setMetricsLoading(false);
      debugLog(
        "apps/page.tsx:useEffect",
        "cache hydrated",
        { routes: cached.routes.length, appsMeta: cached.appsMeta.length, metrics: Object.keys(cached.metrics).length },
        "H12"
      );
    }
    reload(true);
    const iv = window.setInterval(() => {
      void reload(false);
    }, 60000);
    return () => window.clearInterval(iv);
  }, []);

  const apps = useMemo(() => {
    const m = new Map<string, RouteRow[]>();
    for (const r of routes) {
      const arr = m.get(r.app_id) ?? [];
      arr.push(r);
      m.set(r.app_id, arr);
    }
    for (const meta of appsMeta) {
      if (!m.has(meta.app_id)) {
        m.set(meta.app_id, []);
      }
    }
    return [...m.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }, [appsMeta, routes]);
  const detailPrefix = pathname?.startsWith("/ops/") ? "/ops/apps" : "/apps";

  function pct(v: number) {
    return `${(v * 100).toFixed(2)}%`;
  }

  const metaMap = useMemo(() => new Map(appsMeta.map((a) => [a.app_id, a])), [appsMeta]);

  return (
    <>
      <TopBar title="应用与 API" />
      <div className="flex-1 p-4 md:p-6">
        <div className="mb-3 flex items-center justify-end gap-2">
          <span className="text-xs text-muted-foreground">{refreshing ? "刷新中..." : "自动刷新：60s"}</span>
          <Button size="sm" variant="outline" onClick={() => void reload(false)}>
            手动刷新
          </Button>
        </div>
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

        <Card className="mb-4">
          <CardHeader>
            <CardTitle className="text-sm">应用管理</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            <div className="grid grid-cols-1 gap-2 md:grid-cols-5">
              <Input
                placeholder="app_id"
                value={appForm.app_id}
                onChange={(e) => setAppForm((p) => ({ ...p, app_id: e.target.value }))}
              />
              <Input
                placeholder="展示名称"
                value={appForm.display_name}
                onChange={(e) => setAppForm((p) => ({ ...p, display_name: e.target.value }))}
              />
              <Input
                placeholder="描述"
                value={appForm.description}
                onChange={(e) => setAppForm((p) => ({ ...p, description: e.target.value }))}
              />
              <Input
                placeholder="enabled(true/false)"
                value={String(appForm.enabled)}
                onChange={(e) => setAppForm((p) => ({ ...p, enabled: e.target.value !== "false" }))}
              />
              <Button onClick={() => void saveApp()}>保存应用</Button>
            </div>
            <div className="flex flex-wrap gap-2">
              {appsMeta.map((a) => (
                <div key={a.app_id} className="flex items-center gap-2 rounded-md border px-2 py-1 text-xs">
                  <span className="font-medium">{a.app_id}</span>
                  <span className="text-muted-foreground">{a.display_name}</span>
                  <Button size="sm" variant="secondary" onClick={() => setAppForm(a)}>
                    编辑
                  </Button>
                  <Button size="sm" variant="destructive" onClick={() => void removeApp(a.app_id)}>
                    删除
                  </Button>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>

        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          {loading
            ? Array.from({ length: 6 }).map((_, i) => (
                <Card key={i}>
                  <CardHeader>
                    <Skeleton className="h-5 w-32" />
                  </CardHeader>
                  <CardContent className="space-y-2">
                    <Skeleton className="h-4 w-full" />
                    <Skeleton className="h-4 w-full" />
                    <Skeleton className="h-4 w-full" />
                  </CardContent>
                </Card>
              ))
            : apps.map(([appId, rs]) => {
                const mt = metrics[appId];
                const meta = metaMap.get(appId);
                return (
                  <Link key={appId} href={`${detailPrefix}/${encodeURIComponent(appId)}`} prefetch={false}>
                    <Card className="hover:bg-accent/30">
                      <CardHeader>
                        <CardTitle className="text-sm">
                          {appId}
                          {meta?.display_name ? <span className="ml-2 text-xs text-muted-foreground">({meta.display_name})</span> : null}
                        </CardTitle>
                      </CardHeader>
                      <CardContent className="space-y-1 text-xs text-muted-foreground">
                        <div>路由数：{rs.length}</div>
                        {rs.length === 0 ? <div className="text-amber-600">未配置路由，当前仅显示应用元数据</div> : null}
                        <div>请求次数：{mt?.request_count ?? (metricsLoading ? "加载中…" : "—")}</div>
                        <div>PV / UV：{mt ? `${mt.pv_count} / ${mt.uv_count}` : metricsLoading ? "加载中…" : "—"}</div>
                        <div>独立IP：{mt?.unique_ip_count ?? (metricsLoading ? "加载中…" : "—")}</div>
                        <div>4xx：{mt?.err4xx_count ?? (metricsLoading ? "加载中…" : "—")}（{mt ? pct(mt.err4xx_rate) : metricsLoading ? "加载中…" : "—"}）</div>
                        <div>5xx：{mt?.err5xx_count ?? (metricsLoading ? "加载中…" : "—")}（{mt ? pct(mt.err5xx_rate) : metricsLoading ? "加载中…" : "—"}）</div>
                        <div>实时QPS：{mt?.qps_avg?.toFixed(2) ?? (metricsLoading ? "加载中…" : "—")}</div>
                        <div className="mt-1">示例 host：{rs[0]?.host ?? "—"}</div>
                      </CardContent>
                    </Card>
                  </Link>
                );
              })}
        </div>
      </div>
    </>
  );
}


"use client";

import { useEffect, useMemo, useState } from "react";
import { TopBar } from "@/components/app-shell/TopBar";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Skeleton } from "@/components/ui/skeleton";
import { controlApi } from "@/lib/api";
import type { AppTreeNode, IntranetUpstreamRow, RouteRow } from "@/lib/types";
import { dismissTour, isTourDismissed } from "@/lib/tour";

export default function ControlPage() {
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState("");
  const [apps, setApps] = useState<AppTreeNode[]>([]);
  const [selectedApp, setSelectedApp] = useState<string>("");
  const [editing, setEditing] = useState<RouteRow | null>(null);
  const [newUpstream, setNewUpstream] = useState<IntranetUpstreamRow>({
    app_id: "",
    upstream: "mock-workload:18080",
    scheme: "http"
  });
  const [tourVisible, setTourVisible] = useState(false);

  const filteredRoutes = useMemo(
    () => apps.find((x) => x.app_id === selectedApp)?.routes ?? [],
    [apps, selectedApp]
  );

  async function reload() {
    try {
      setLoading(true);
      const tree = await controlApi.listAppsTree();
      setApps(tree);
      if (!selectedApp && tree.length) setSelectedApp(tree[0].app_id);
      setErr("");
    } catch (e) {
      setErr(String(e));
      setApps([]);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    reload();
    setTourVisible(!isTourDismissed());
  }, []);

  async function saveRoute() {
    if (!editing) return;
    try {
      await controlApi.upsertRoute(editing);
      await reload();
      setEditing(null);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function removeRoute(host: string) {
    try {
      await controlApi.deleteRoute(host);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function saveUpstream() {
    if (!selectedApp) return;
    try {
      await controlApi.upsertIntranet({ ...newUpstream, app_id: selectedApp });
      setErr("");
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <TopBar title="控制面板（可视化）" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        {err ? (
          <Alert variant="destructive">
            <AlertTitle>请求失败</AlertTitle>
            <AlertDescription>{err}</AlertDescription>
          </Alert>
        ) : null}
        {tourVisible ? (
          <Alert>
            <AlertTitle>首次引导</AlertTitle>
            <AlertDescription className="flex items-center justify-between gap-2">
              <span>左侧选择应用，右侧管理路由和上游映射；后续会补完整 Tour 分步引导。</span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  dismissTour();
                  setTourVisible(false);
                }}
              >
                我知道了
              </Button>
            </AlertDescription>
          </Alert>
        ) : null}
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-[300px_1fr]">
          <Card>
            <CardHeader>
              <CardTitle>应用树</CardTitle>
              <CardDescription>按应用分组查看路由与指标摘要</CardDescription>
            </CardHeader>
            <CardContent className="space-y-2">
              {loading ? (
                <>
                  <Skeleton className="h-10 w-full" />
                  <Skeleton className="h-10 w-full" />
                  <Skeleton className="h-10 w-full" />
                </>
              ) : (
                apps.map((app) => (
                  <button
                    key={app.app_id}
                    className={`w-full rounded-md border px-3 py-2 text-left text-sm ${
                      selectedApp === app.app_id ? "border-primary bg-primary/5" : "border-border"
                    }`}
                    onClick={() => {
                      setSelectedApp(app.app_id);
                      setNewUpstream((prev) => ({ ...prev, app_id: app.app_id }));
                    }}
                  >
                    <div className="font-medium">{app.app_id}</div>
                    <div className="text-xs text-muted-foreground">
                      路由 {app.routes.length} / QPS {app.latest?.qps_avg?.toFixed(2) ?? "—"}
                    </div>
                  </button>
                ))
              )}
            </CardContent>
          </Card>
          <div className="space-y-4">
            <Card>
              <CardHeader className="flex flex-row items-center justify-between">
                <div>
                  <CardTitle>路由表</CardTitle>
                  <CardDescription>应用：{selectedApp || "未选择"}</CardDescription>
                </div>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    onClick={() =>
                      setEditing({
                        host: "app.internal.com",
                        app_id: selectedApp || "app-001",
                        connector_endpoint: "connector-local-001:stream",
                        require_healthy_tunnel: true
                      })
                    }
                  >
                    创建路由
                  </Button>
                  <Button size="sm" variant="outline" onClick={reload}>
                    刷新
                  </Button>
                </div>
              </CardHeader>
              <CardContent>
                {loading ? (
                  <>
                    <Skeleton className="mb-2 h-8 w-full" />
                    <Skeleton className="mb-2 h-8 w-full" />
                    <Skeleton className="h-8 w-full" />
                  </>
                ) : (
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>host</TableHead>
                        <TableHead>connector_endpoint</TableHead>
                        <TableHead>healthy</TableHead>
                        <TableHead>操作</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {filteredRoutes.map((r) => (
                        <TableRow key={r.host}>
                          <TableCell>{r.host}</TableCell>
                          <TableCell>{r.connector_endpoint}</TableCell>
                          <TableCell>{String(r.require_healthy_tunnel)}</TableCell>
                          <TableCell>
                            <div className="flex gap-2">
                              <Button size="sm" variant="secondary" onClick={() => setEditing(r)}>
                                编辑
                              </Button>
                              <Button size="sm" variant="destructive" onClick={() => void removeRoute(r.host)}>
                                删除
                              </Button>
                            </div>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                )}
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle>上游映射</CardTitle>
                <CardDescription>为当前应用维护 intranet upstream</CardDescription>
              </CardHeader>
              <CardContent className="grid grid-cols-1 gap-2 md:grid-cols-4">
                <Input value={selectedApp} disabled />
                <Input
                  value={newUpstream.upstream}
                  onChange={(e) => setNewUpstream((p) => ({ ...p, upstream: e.target.value }))}
                  placeholder="upstream host:port"
                />
                <Input
                  value={newUpstream.scheme}
                  onChange={(e) =>
                    setNewUpstream((p) => ({ ...p, scheme: (e.target.value || "http") as "http" | "https" }))
                  }
                  placeholder="http/https"
                />
                <Button onClick={() => void saveUpstream()}>保存映射</Button>
              </CardContent>
            </Card>
          </div>
        </div>
        {editing ? (
          <Card className="border-primary/50">
            <CardHeader>
              <CardTitle>{editing.host ? "编辑路由" : "创建路由"}</CardTitle>
            </CardHeader>
            <CardContent className="grid grid-cols-1 gap-2 md:grid-cols-5">
              <Input value={editing.host} onChange={(e) => setEditing({ ...editing, host: e.target.value })} placeholder="host" />
              <Input value={editing.app_id} onChange={(e) => setEditing({ ...editing, app_id: e.target.value })} placeholder="app_id" />
              <Input
                value={editing.connector_endpoint}
                onChange={(e) => setEditing({ ...editing, connector_endpoint: e.target.value })}
                placeholder="connector endpoint"
              />
              <Input
                value={String(editing.require_healthy_tunnel)}
                onChange={(e) => setEditing({ ...editing, require_healthy_tunnel: e.target.value !== "false" })}
                placeholder="true/false"
              />
              <div className="flex gap-2">
                <Button onClick={() => void saveRoute()}>保存</Button>
                <Button variant="outline" onClick={() => setEditing(null)}>
                  取消
                </Button>
              </div>
            </CardContent>
          </Card>
        ) : null}
      </div>
    </>
  );
}

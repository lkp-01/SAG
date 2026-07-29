"use client";

import { useEffect, useMemo, useState } from "react";
import { TopBar } from "@/components/app-shell/TopBar";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { controlApi } from "@/lib/api";
import type { ApiRouteRecord, AppRecord } from "@/lib/types";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsApiRoutesPage() {
  return (
    <RoleGate need="ops">
      <ApiRoutesInner />
    </RoleGate>
  );
}

function ApiRoutesInner() {
  const [apps, setApps] = useState<AppRecord[]>([]);
  const [selectedApp, setSelectedApp] = useState<string>("");
  const [rows, setRows] = useState<ApiRouteRecord[]>([]);
  const [err, setErr] = useState("");
  const [form, setForm] = useState<ApiRouteRecord>({
    id: "",
    app_id: "",
    method: "GET",
    path: "/api/test",
    enabled: true,
    description: ""
  });

  async function reload() {
    try {
      const a = await controlApi.listApps().catch(() => []);
      setApps(a);
      const appId = selectedApp || a[0]?.app_id || "";
      if (!selectedApp && appId) setSelectedApp(appId);
      const list = await controlApi.listApiRoutes(appId || undefined);
      setRows(list);
      setErr("");
    } catch (e) {
      setErr(String(e));
      setRows([]);
    }
  }

  useEffect(() => {
    reload();
  }, []);

  useEffect(() => {
    if (!selectedApp) return;
    void (async () => {
      try {
        const list = await controlApi.listApiRoutes(selectedApp);
        setRows(list);
        setErr("");
      } catch (e) {
        setErr(String(e));
        setRows([]);
      }
    })();
  }, [selectedApp]);

  const appOptions = useMemo(() => apps.map((a) => a.app_id), [apps]);

  async function save() {
    try {
      const payload: ApiRouteRecord = {
        ...form,
        app_id: form.app_id || selectedApp
      };
      await controlApi.upsertApiRoute(payload);
      setForm({ id: "", app_id: "", method: "GET", path: "/api/test", enabled: true, description: "" });
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function remove(id: string) {
    try {
      await controlApi.deleteApiRoute(id);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <>
      <TopBar title="API 路由管理" />
      <div className="flex-1 space-y-4 p-4 md:p-6">
        {err ? (
          <Card className="border-destructive/50">
            <CardHeader>
              <CardTitle className="text-sm">请求失败</CardTitle>
            </CardHeader>
            <CardContent className="text-sm text-muted-foreground">{err}</CardContent>
          </Card>
        ) : null}

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">筛选</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            {appOptions.map((id) => (
              <Button key={id} size="sm" variant={selectedApp === id ? "default" : "outline"} onClick={() => setSelectedApp(id)}>
                {id}
              </Button>
            ))}
            <Button size="sm" variant="outline" onClick={reload}>
              刷新
            </Button>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">新建 / 编辑</CardTitle>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-2 md:grid-cols-6">
            <Input placeholder="id(可空)" value={form.id} onChange={(e) => setForm((p) => ({ ...p, id: e.target.value }))} />
            <Input
              placeholder={`app_id(默认 ${selectedApp || "-"})`}
              value={form.app_id}
              onChange={(e) => setForm((p) => ({ ...p, app_id: e.target.value }))}
            />
            <Input placeholder="method" value={form.method} onChange={(e) => setForm((p) => ({ ...p, method: e.target.value }))} />
            <Input placeholder="path" value={form.path} onChange={(e) => setForm((p) => ({ ...p, path: e.target.value }))} />
            <Input
              placeholder="enabled(true/false)"
              value={String(form.enabled)}
              onChange={(e) => setForm((p) => ({ ...p, enabled: e.target.value !== "false" }))}
            />
            <div className="flex gap-2">
              <Button onClick={() => void save()}>保存</Button>
              <Button
                variant="outline"
                onClick={() => setForm({ id: "", app_id: "", method: "GET", path: "/api/test", enabled: true, description: "" })}
              >
                清空
              </Button>
            </div>
            <Input
              className="md:col-span-6"
              placeholder="description"
              value={form.description}
              onChange={(e) => setForm((p) => ({ ...p, description: e.target.value }))}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-sm">路由列表（{selectedApp || "全部"}）</CardTitle>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>id</TableHead>
                  <TableHead>app_id</TableHead>
                  <TableHead>method</TableHead>
                  <TableHead>path</TableHead>
                  <TableHead>enabled</TableHead>
                  <TableHead>操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => (
                  <TableRow key={r.id}>
                    <TableCell>{r.id}</TableCell>
                    <TableCell>{r.app_id}</TableCell>
                    <TableCell>{r.method}</TableCell>
                    <TableCell>{r.path}</TableCell>
                    <TableCell>{String(r.enabled)}</TableCell>
                    <TableCell>
                      <div className="flex gap-2">
                        <Button size="sm" variant="secondary" onClick={() => setForm(r)}>
                          编辑
                        </Button>
                        <Button size="sm" variant="destructive" onClick={() => void remove(r.id)}>
                          删除
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </div>
    </>
  );
}


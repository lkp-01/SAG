import { useEffect, useState } from "react";
import { controlApi } from "@/lib/api";
import type { RouteRow } from "@/lib/types";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

type Props = { onError: (msg: string) => void };

export function RoutesPage({ onError }: Props) {
  const [rows, setRows] = useState<RouteRow[]>([]);
  const [form, setForm] = useState<RouteRow>({
    host: "app.internal.com",
    app_id: "app-001",
    connector_endpoint: "connector-local-001:stream",
    require_healthy_tunnel: true,
  });

  const reload = async () => {
    try {
      setRows(await controlApi.listRoutes());
    } catch (e) {
      onError(String(e));
    }
  };

  useEffect(() => {
    reload();
  }, []);

  const submit = async () => {
    try {
      await controlApi.upsertRoute(form);
      await reload();
    } catch (e) {
      onError(String(e));
    }
  };

  const remove = async (host: string) => {
    try {
      await controlApi.deleteRoute(host);
      await reload();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>路由管理</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-4">
          <Input
            value={form.host}
            onChange={(e) => setForm({ ...form, host: e.target.value })}
            placeholder="host"
          />
          <Input
            value={form.app_id}
            onChange={(e) => setForm({ ...form, app_id: e.target.value })}
            placeholder="app_id"
          />
          <Input
            value={form.connector_endpoint}
            onChange={(e) => setForm({ ...form, connector_endpoint: e.target.value })}
            placeholder="connector endpoint"
          />
          <div className="flex gap-2">
            <Button onClick={submit}>新增/更新</Button>
            <Button variant="secondary" onClick={reload}>
              刷新
            </Button>
          </div>
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>host</TableHead>
              <TableHead>app_id</TableHead>
              <TableHead>connector_endpoint</TableHead>
              <TableHead>healthy</TableHead>
              <TableHead>操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((r) => (
              <TableRow key={r.host}>
                <TableCell>{r.host}</TableCell>
                <TableCell>{r.app_id}</TableCell>
                <TableCell>{r.connector_endpoint}</TableCell>
                <TableCell>{String(r.require_healthy_tunnel)}</TableCell>
                <TableCell>
                  <div className="flex gap-2">
                    <Button size="sm" variant="secondary" onClick={() => setForm(r)}>
                      编辑
                    </Button>
                    <Button size="sm" variant="destructive" onClick={() => remove(r.host)}>
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
  );
}

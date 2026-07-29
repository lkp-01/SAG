import { useEffect, useState } from "react";
import { controlApi } from "@/lib/api";
import type { IntranetUpstreamRow } from "@/lib/types";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

type Props = { onError: (msg: string) => void };

const CACHE_KEY = "sag.console.upstreams.cache";

export function UpstreamsPage({ onError }: Props) {
  const [form, setForm] = useState<IntranetUpstreamRow>({
    app_id: "app-001",
    upstream: "mock-workload:18080",
    scheme: "http",
  });
  const [rows, setRows] = useState<IntranetUpstreamRow[]>([]);

  useEffect(() => {
    const raw = localStorage.getItem(CACHE_KEY);
    if (raw) {
      try {
        setRows(JSON.parse(raw) as IntranetUpstreamRow[]);
      } catch {
        setRows([]);
      }
    }
  }, []);

  const save = async () => {
    try {
      await controlApi.upsertIntranet(form);
      const next = [form, ...rows.filter((r) => r.app_id !== form.app_id)].slice(0, 20);
      setRows(next);
      localStorage.setItem(CACHE_KEY, JSON.stringify(next));
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>上游映射</CardTitle>
        <CardDescription>
          后端当前仅提供 upsert 接口（`PUT /api/v1/agent/intranet-upstreams?app_id=...`）。此页会保留最近成功提交记录，便于运维回看。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-4">
          <Input
            value={form.app_id}
            onChange={(e) => setForm({ ...form, app_id: e.target.value })}
            placeholder="app_id"
          />
          <Input
            value={form.upstream}
            onChange={(e) => setForm({ ...form, upstream: e.target.value })}
            placeholder="upstream host:port"
          />
          <Input
            value={form.scheme}
            onChange={(e) => setForm({ ...form, scheme: e.target.value as "http" | "https" })}
            placeholder="http/https"
          />
          <Button onClick={save}>保存映射</Button>
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>app_id</TableHead>
              <TableHead>upstream</TableHead>
              <TableHead>scheme</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((r) => (
              <TableRow key={r.app_id}>
                <TableCell>{r.app_id}</TableCell>
                <TableCell>{r.upstream}</TableCell>
                <TableCell>{r.scheme}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}
